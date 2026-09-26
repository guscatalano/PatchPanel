//! A syslog receiver, for devices that cannot host an agent.
//!
//! PatchPanel detects an unexpected reboot on a machine by reading its journal.
//! It has no equivalent for a firewall or a NAS, which is why a router can be
//! unreachable and failing its backups for days with nothing here saying why.
//! Almost every such device speaks syslog, so this is the one protocol that
//! reaches all of them without credentials, polling, or anything installed.
//!
//! **This is not a log server, and must not become one.** A /24 pushing syslog
//! produces hundreds of thousands of lines a day; storing them would make the
//! portal a bad archive and a worse patch manager. What is kept is a rolling
//! day, pruned on the scheduler's tick, and the value is meant to come from
//! what gets *extracted* - reboots, disk errors, authentication failures -
//! rather than from the text itself.
//!
//! Lines go to plain files, one per sender, not into the database. A rolling
//! day of syslog is a stream, and a stream does not want transactions, indexes
//! or a write lock shared with the thing that records patch runs. `tail` works
//! on a file; so does `grep`; so does deleting it.
//!
//! Two things are deliberate. It is off unless a bind address is given: an
//! unauthenticated listening socket is a choice, not a default. And a sender
//! that floods is capped rather than allowed to fill the disk - with the drops
//! counted and reported, because a log that silently loses lines is worse than
//! one that admits to a gap.

use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::io::AsyncBufReadExt;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Mutex;

use crate::state::SharedState;

/// Lines per sender per second before it is throttled. A busy firewall with
/// filter logging on can emit thousands; a system log rarely breaks ten.
const RATE_PER_SEC: usize = 200;
/// How long lines are kept. Long enough to scroll back through yesterday,
/// short enough that this never becomes the thing on disk that matters.
pub const RETAIN_HOURS: i64 = 24;
/// The longest anyone may ask for. This is deliberately not a log server, and a
/// ceiling is what keeps that true after somebody reasonable asks for "just a
/// month" of a firewall that emits a gigabyte a day.
pub const MAX_RETAIN_HOURS: i64 = 24 * 7;
/// Flush interval. Batched because one INSERT per datagram would spend the
/// whole database on write locks.
const FLUSH_MS: u64 = 2000;

#[derive(Clone, Debug)]
pub struct LogLine {
    pub at: DateTime<Utc>,
    /// The address it arrived from, which is the only identity that cannot be
    /// spoofed by the message itself.
    pub source: String,
    /// The hostname the sender put in the message. Useful and untrusted.
    pub host: String,
    pub facility: i64,
    pub severity: i64,
    pub tag: String,
    pub msg: String,
}

/// Anything with a PRI, an optional timestamp and host, and a message.
///
/// Both RFC 3164 and RFC 5424 are in the wild and plenty of devices emit
/// neither correctly, so this takes what it can find and keeps the rest of the
/// line as the message. A log line that fails to parse still has to arrive -
/// dropping it would hide exactly the malformed output that signals a problem.
pub fn parse(raw: &str, source: &str) -> LogLine {
    let now = Utc::now();
    let text = raw.trim_end_matches(['\n', '\r', '\0']);
    let mut line = LogLine {
        at: now,
        source: source.to_string(),
        host: String::new(),
        facility: 1,
        severity: 6,
        tag: String::new(),
        msg: text.to_string(),
    };

    // <PRI>
    let rest = if let Some(close) = text.find('>') {
        if text.starts_with('<') {
            if let Ok(pri) = text[1..close].parse::<i64>() {
                line.facility = pri / 8;
                line.severity = pri % 8;
            }
            &text[close + 1..]
        } else {
            text
        }
    } else {
        text
    };

    // RFC 5424 announces itself with a version digit. Seven fields, not six:
    // timestamp, host, app, procid, msgid, structured-data, then the message.
    // Stopping at six leaves the structured-data field (usually a bare "-")
    // glued to the front of every message.
    if let Some(after) = rest.strip_prefix("1 ") {
        let mut parts = after.splitn(7, ' ');
        let ts = parts.next().unwrap_or("");
        line.host = parts.next().unwrap_or("").to_string();
        line.tag = parts.next().unwrap_or("").to_string();
        let _procid = parts.next();
        let _msgid = parts.next();
        let _structured = parts.next();
        line.msg = parts.next().unwrap_or("").trim().to_string();
        if let Ok(t) = DateTime::parse_from_rfc3339(ts) {
            line.at = t.with_timezone(&Utc);
        }
        if line.msg.is_empty() {
            line.msg = text.to_string();
        }
        return line;
    }

    // RFC 3164: `Mmm dd hh:mm:ss host tag: message`. The timestamp carries no
    // year and no zone, so arrival time is used instead - it is the one clock
    // here that is known to be right.
    //
    // The host is only taken when that timestamp was actually there. Taking it
    // unconditionally turns the first word of any unparseable line into a
    // hostname and silently amputates it from the message - so "this is not
    // syslog" was being stored as "is not syslog", which is worse than not
    // parsing it at all.
    match strip_bsd_timestamp(rest) {
        Some(cursor) => {
            let mut parts = cursor.trim_start().splitn(2, ' ');
            let first = parts.next().unwrap_or("");
            let remainder = parts.next().unwrap_or("").trim();
            if !first.is_empty() && !first.ends_with(':') {
                line.host = first.to_string();
                match remainder.find(": ") {
                    Some(i) => {
                        line.tag = remainder[..i].trim().to_string();
                        line.msg = remainder[i + 2..].trim().to_string();
                    }
                    None => line.msg = remainder.to_string(),
                }
            }
        }
        // No timestamp, so no structure to trust: keep every word.
        None => line.msg = rest.trim().to_string(),
    }
    if line.msg.is_empty() {
        line.msg = text.to_string();
    }
    line
}

/// `Mmm dd hh:mm:ss ` at the start, returning what follows it.
///
/// Checked rather than assumed, because whether it is present decides whether
/// the next word is a hostname or the first word of the message.
fn strip_bsd_timestamp(text: &str) -> Option<&str> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    if text.len() < 16 || !MONTHS.contains(&&text[..3]) {
        return None;
    }
    let rest = &text[3..];
    // `dd` may be space-padded, and the clock is fixed width.
    let (day_and_time, tail) = rest.split_at(rest.char_indices().nth(12).map(|(i, _)| i)?);
    let clock = day_and_time.trim_start();
    let (_, time) = clock.split_once(' ')?;
    let time = time.trim();
    if time.len() != 8 || time.as_bytes()[2] != b':' || time.as_bytes()[5] != b':' {
        return None;
    }
    Some(tail)
}

/// Everything a sender has been up to since the last flush.
#[derive(Default)]
struct Bucket {
    lines: Vec<LogLine>,
    /// Lines refused because the sender exceeded its rate. Counted so the gap
    /// can be reported rather than quietly existing.
    dropped: usize,
    window_started: Option<DateTime<Utc>>,
    in_window: usize,
}

type Buffers = Arc<Mutex<HashMap<String, Bucket>>>;

/// A sender's own file, named after the address so a rename in the manifest
/// cannot orphan a log, and sanitised because the name comes off the network.
fn path_for(dir: &Path, source: &str) -> PathBuf {
    let safe: String = source
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    dir.join(format!("{safe}.log"))
}

/// One line, as it is written to disk.
///
/// Fixed leading columns so the file stays greppable by eye and by `awk`, with
/// the arrival time first because that is the clock that is known to be right.
fn format_line(l: &LogLine) -> String {
    format!(
        "{} {:<9} {} {}\n",
        l.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        severity_name(l.severity),
        if l.tag.is_empty() { "-" } else { &l.tag },
        l.msg
    )
}

/// A window on the log as it arrives, for watching rather than searching.
///
/// Every line is already being written to its sender's file, and the files are
/// the record - this is a second, much shorter view of the same writes, kept in
/// memory so a page can follow the whole fleet at once without reading thirteen
/// files on every poll.
///
/// A ring rather than a growing list: a live view is about the present, and the
/// past is on disk. When it wraps, readers are told how many lines they missed
/// instead of being handed a feed with a silent hole in it - a log that loses
/// lines without saying so is worse than one that stops.
pub struct Live {
    inner: std::sync::Mutex<LiveInner>,
}

struct LiveInner {
    lines: std::collections::VecDeque<LiveLine>,
    /// Monotonic, and never reset: it is the cursor readers hold, so reusing a
    /// number would silently hand somebody the wrong lines.
    next_seq: u64,
}

#[derive(Clone, serde::Serialize)]
pub struct LiveLine {
    pub seq: u64,
    pub at: DateTime<Utc>,
    /// The machine or device this came from, resolved the same way the file
    /// list resolves it, so one line reads the same in both places.
    pub who: String,
    pub source: String,
    pub severity: i64,
    pub tag: String,
    pub msg: String,
}

/// How many lines the window holds. Roughly a screenful per machine on this
/// fleet, which is what a person scrolling back through a burst wants; anything
/// longer is a job for the files.
const LIVE_LINES: usize = 3000;

impl Default for Live {
    fn default() -> Self {
        Self {
            inner: std::sync::Mutex::new(LiveInner {
                lines: std::collections::VecDeque::with_capacity(LIVE_LINES),
                next_seq: 1,
            }),
        }
    }
}

impl Live {
    /// Add lines, evicting the oldest once full.
    pub fn push(&self, who: &str, lines: &[LogLine]) {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            // A panic while formatting somebody else's log line must not take
            // the receiver down with it; the files are the record either way.
            Err(poisoned) => poisoned.into_inner(),
        };
        for l in lines {
            let seq = g.next_seq;
            g.next_seq += 1;
            g.lines.push_back(LiveLine {
                seq,
                at: l.at,
                who: who.to_string(),
                source: l.source.clone(),
                severity: l.severity,
                tag: l.tag.clone(),
                msg: l.msg.clone(),
            });
            if g.lines.len() > LIVE_LINES {
                g.lines.pop_front();
            }
        }
    }

    /// Lines after `cursor`, and how many were evicted before the reader got to
    /// them.
    ///
    /// `cursor` of 0 means "I have just arrived": that returns the tail rather
    /// than everything, because a page opening on a busy fleet wants the present
    /// and not three thousand lines of history it did not ask for.
    pub fn since(&self, cursor: u64, limit: usize) -> (Vec<LiveLine>, u64, u64) {
        let g = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let newest = g.next_seq;
        if cursor == 0 {
            let out: Vec<LiveLine> = g.lines.iter().rev().take(limit).rev().cloned().collect();
            return (out, newest, 0);
        }
        let oldest = g.lines.front().map(|l| l.seq).unwrap_or(newest);
        // Everything from `cursor` up to the oldest line still held is gone.
        let missed = oldest.saturating_sub(cursor);
        let mut out: Vec<LiveLine> = g
            .lines
            .iter()
            .filter(|l| l.seq >= cursor)
            .take(limit)
            .cloned()
            .collect();
        // A reader that is further behind than the window is long gets the tail
        // and the count of what it lost.
        if out.len() == limit {
            if let Some(last) = out.last() {
                let next = last.seq + 1;
                return (out, next, missed);
            }
        }
        out.truncate(limit);
        (out, newest, missed)
    }
}

pub fn append(dir: &Path, source: &str, lines: &[LogLine], dropped: usize) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = path_for(dir, source);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut buf = String::new();
    for l in lines {
        buf.push_str(&format_line(l));
    }
    // The gap is part of the record. A log that loses lines silently is worse
    // than one that says where it stopped being complete.
    if dropped > 0 {
        buf.push_str(&format!(
            "{} warning   patchpanel {dropped} line(s) from this sender were dropped: over {RATE_PER_SEC}/s\n",
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ));
    }
    f.write_all(buf.as_bytes())?;
    Ok(())
}

/// Drop anything older than a day, by rewriting each file without it.
///
/// Called from the scheduler's tick. Cheap because the files are small by
/// construction, and it keeps the whole feature to "some files in a directory"
/// rather than a retention subsystem.
pub fn prune(dir: &Path, per_source: &std::collections::HashMap<String, i64>) -> Result<usize> {
    let now = Utc::now();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(0);
    };
    let mut trimmed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        // Each sender may be held to its own window; absent means the default.
        let source = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let hours = per_source
            .get(&source)
            .copied()
            .unwrap_or(RETAIN_HOURS)
            .clamp(1, MAX_RETAIN_HOURS);
        let cutoff = now - chrono::Duration::hours(hours);

        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let kept: String = text
            .lines()
            .filter(|l| match l.split(' ').next() {
                // A line whose timestamp cannot be read is kept: it is more
                // likely a message containing a newline than a line from last
                // week, and discarding evidence on a parse failure is the
                // wrong default.
                Some(ts) => chrono::DateTime::parse_from_rfc3339(ts)
                    .map(|t| t.with_timezone(&Utc) >= cutoff)
                    .unwrap_or(true),
                None => true,
            })
            .map(|l| format!("{l}\n"))
            .collect();
        if kept.len() != text.len() {
            std::fs::write(&path, kept)?;
            trimmed += 1;
        }
    }
    Ok(trimmed)
}

/// Which senders have written anything, with how much and how recently.
pub struct Sender {
    pub source: String,
    pub bytes: u64,
    /// How many lines are held. Size alone is misleading at both ends: a file
    /// with forty bytes in it rounds to "0 KB" and reads as "nothing arrived",
    /// and megabytes says nothing about how much there is to scroll through.
    pub lines: usize,
    pub modified: Option<DateTime<Utc>>,
}

/// Newlines in a file, counted without holding it in memory.
///
/// Cheap enough to do on a polled endpoint: a day of one sender is single-digit
/// megabytes, and this is a scan for one byte value rather than any parsing.
fn count_lines(path: &Path) -> usize {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return 0;
    };
    let mut buf = [0u8; 64 * 1024];
    let mut n = 0;
    while let Ok(read) = f.read(&mut buf) {
        if read == 0 {
            break;
        }
        n += buf[..read].iter().filter(|b| **b == b'\n').count();
    }
    n
}

pub fn senders(dir: &Path) -> Vec<Sender> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let meta = entry.metadata().ok();
        out.push(Sender {
            source: stem.to_string(),
            bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            lines: count_lines(&path),
            modified: meta
                .and_then(|m| m.modified().ok())
                .map(|t| chrono::DateTime::<Utc>::from(t)),
        });
    }
    // By name, not by recency.
    //
    // Sorting by last-written meant every arriving line reordered the list, and
    // the page redraws every few seconds - so rows moved out from under the
    // cursor between deciding to click Read and clicking it. Recency is already
    // on each row as "1m ago"; it does not need to be in the position as well,
    // and a stable order is worth more than a fresh one in a list somebody is
    // trying to operate.
    out.sort_by(|a, b| a.source.cmp(&b.source));
    out
}

/// Every line a sender has on disk, optionally filtered, as one blob.
///
/// Separate from `tail` because an export is a different job: the viewer wants
/// a page, and this wants the file. Streaming would be tidier, but a day of one
/// sender is megabytes rather than gigabytes by construction.
pub fn whole(dir: &Path, source: &str, contains: &str) -> Result<String> {
    let path = path_for(dir, source);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("nothing has been logged by {source}"))?;
    if contains.is_empty() {
        return Ok(text);
    }
    let needle = contains.to_lowercase();
    Ok(text
        .lines()
        .filter(|l| l.to_lowercase().contains(&needle))
        .map(|l| format!("{l}\n"))
        .collect())
}

/// How many lines a sender has, so the viewer can say what it is showing a
/// slice of rather than implying it is the whole thing.
pub fn line_count(dir: &Path, source: &str) -> usize {
    std::fs::read_to_string(path_for(dir, source))
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

/// The last `limit` lines from one sender, newest last.
pub fn tail(dir: &Path, source: &str, limit: usize, contains: &str) -> Result<Vec<String>> {
    let path = path_for(dir, source);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("nothing has been logged by {source}"))?;
    let needle = contains.to_lowercase();
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| needle.is_empty() || l.to_lowercase().contains(&needle))
        .map(str::to_string)
        .collect();
    if lines.len() > limit {
        lines = lines.split_off(lines.len() - limit);
    }
    Ok(lines)
}

pub fn spawn(state: SharedState, bind: SocketAddr, dir: PathBuf) {
    let buffers: Buffers = Arc::new(Mutex::new(HashMap::new()));

    // UDP is what devices use by default; TCP is offered because a firewall
    // sending over a link that drops packets is exactly the firewall whose
    // logs you want.
    {
        let buffers = buffers.clone();
        tokio::spawn(async move {
            match UdpSocket::bind(bind).await {
                Ok(sock) => {
                    tracing::info!(%bind, "syslog receiver listening on udp");
                    let mut buf = vec![0u8; 8192];
                    loop {
                        match sock.recv_from(&mut buf).await {
                            Ok((n, from)) => {
                                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                                accept(&buffers, &text, &from.ip().to_string()).await;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "syslog udp read failed");
                            }
                        }
                    }
                }
                Err(e) => tracing::error!(error = %e, %bind,
                    "could not bind syslog on udp; nothing will be received"),
            }
        });
    }

    {
        let buffers = buffers.clone();
        tokio::spawn(async move {
            match TcpListener::bind(bind).await {
                Ok(listener) => {
                    tracing::info!(%bind, "syslog receiver listening on tcp");
                    loop {
                        let Ok((stream, from)) = listener.accept().await else {
                            continue;
                        };
                        let buffers = buffers.clone();
                        let ip = from.ip().to_string();
                        tokio::spawn(async move {
                            let mut lines = tokio::io::BufReader::new(stream).lines();
                            while let Ok(Some(text)) = lines.next_line().await {
                                accept(&buffers, &text, &ip).await;
                            }
                        });
                    }
                }
                Err(e) => tracing::warn!(error = %e, %bind, "could not bind syslog on tcp"),
            }
        });
    }

    let _ = &state;

    // One writer task. A burst of senders becomes one append per sender per
    // flush rather than a thousand small writes racing each other.
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(FLUSH_MS));
        loop {
            tick.tick().await;
            let batch: Vec<(String, Bucket)> = {
                let mut map = buffers.lock().await;
                map.drain().collect()
            };
            for (source, bucket) in batch {
                if bucket.dropped > 0 {
                    tracing::warn!(
                        %source,
                        dropped = bucket.dropped,
                        "syslog sender exceeded its rate; lines were discarded"
                    );
                }
                if bucket.lines.is_empty() && bucket.dropped == 0 {
                    continue;
                }
                if let Err(e) = append(&dir, &source, &bucket.lines, bucket.dropped) {
                    tracing::warn!(error = %e, %source, "could not write syslog lines");
                }
                // Resolved here rather than on the page: the address is the only
                // identity a sender cannot lie about, and turning it into a name
                // is something only the portal can do.
                let who = owner(&state, &source);
                state.live.push(
                    if who.is_empty() { &source } else { &who },
                    &bucket.lines,
                );
            }
        }
    });
}

async fn accept(buffers: &Buffers, text: &str, source: &str) {
    let now = Utc::now();
    let mut map = buffers.lock().await;
    let bucket = map.entry(source.to_string()).or_default();

    // A one-second window per sender. Crude on purpose: the goal is to keep a
    // misconfigured firewall from filling the disk, not to shape traffic.
    let fresh = bucket
        .window_started
        .is_none_or(|w| now.signed_duration_since(w).num_seconds() >= 1);
    if fresh {
        bucket.window_started = Some(now);
        bucket.in_window = 0;
    }
    bucket.in_window += 1;
    if bucket.in_window > RATE_PER_SEC {
        bucket.dropped += 1;
        return;
    }

    for raw in text.split('\n') {
        if raw.trim().is_empty() {
            continue;
        }
        bucket.lines.push(parse(raw, source));
    }
}

/// Which declared device or known machine an address belongs to.
///
/// Resolved from the address rather than from the hostname in the message: the
/// message is written by the sender and the address is not.
pub fn owner(state: &SharedState, source: &str) -> String {
    if let Ok(rows) = state.db.agents() {
        // Journal lines arrive over the WebSocket and are filed under the
        // machine's own name, so the source is already the answer. Checked
        // first, because the address lookup below cannot match a hostname.
        if let Some(row) = rows.iter().find(|a| a.hostname == source) {
            return row.hostname.clone();
        }
        for row in rows {
            if row
                .hardware
                .as_ref()
                .is_some_and(|hw| hw.ip_addresses.iter().any(|ip| ip == source))
            {
                return row.hostname;
            }
        }
    }
    if let Ok(manifest) = state.db.manifest() {
        for d in &manifest.devices {
            let target = d.target.split(':').next().unwrap_or(&d.target);
            if target == source {
                return if d.label.is_empty() {
                    d.id.clone()
                } else {
                    d.label.clone()
                };
            }
        }
        // Devices are declared by name, and syslog arrives from an address, so
        // a literal comparison never matches for the normal case. Resolving the
        // declared name is what connects "192.168.6.1" to "winetown router".
        for d in &manifest.devices {
            let target = d.target.split(':').next().unwrap_or(&d.target);
            if resolves_to(target, source) {
                return if d.label.is_empty() {
                    d.id.clone()
                } else {
                    d.label.clone()
                };
            }
        }
    }
    String::new()
}

/// Whether a declared hostname currently resolves to this address.
///
/// Best effort and deliberately not cached: it runs when somebody looks at the
/// log list, a handful of names at a time, and a stale answer here would label
/// a device wrongly for as long as the portal stayed up.
fn resolves_to(host: &str, source: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = (host, 0u16).to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| a.ip().to_string() == source)
}

/// Human words for a syslog severity, which is the thing people actually
/// filter on.
pub fn severity_name(severity: i64) -> &'static str {
    match severity {
        0 => "emergency",
        1 => "alert",
        2 => "critical",
        3 => "error",
        4 => "warning",
        5 => "notice",
        6 => "info",
        _ => "debug",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_classic_format() {
        // What an OPNsense or Proxmox box emits by default.
        let l = parse(
            "<38>Sep 26 04:12:01 router.winetown sshd[12345]: Failed password for root from 10.0.0.9",
            "192.168.6.1",
        );
        assert_eq!(l.facility, 4);
        assert_eq!(l.severity, 6);
        assert_eq!(l.host, "router.winetown");
        assert_eq!(l.tag, "sshd[12345]");
        assert!(l.msg.starts_with("Failed password for root"));
    }

    #[test]
    fn reads_the_structured_format() {
        let l = parse(
            "<165>1 2026-09-26T04:12:01.123Z mininas kernel - - - I/O error on sdb",
            "192.168.6.43",
        );
        assert_eq!(l.severity, 5);
        assert_eq!(l.host, "mininas");
        assert_eq!(l.tag, "kernel");
        assert_eq!(l.msg, "I/O error on sdb");
        assert_eq!(l.at.format("%Y-%m-%d %H:%M").to_string(), "2026-09-26 04:12");
    }

    /// A device that emits something malformed is often the device with a
    /// problem, so the line still has to arrive with its text intact.
    #[test]
    fn keeps_a_line_it_cannot_parse() {
        let l = parse("this is not syslog at all", "192.168.6.99");
        assert_eq!(l.msg, "this is not syslog at all");
        assert_eq!(l.source, "192.168.6.99");
    }

    #[test]
    fn a_bare_priority_still_yields_a_message() {
        let l = parse("<11>something broke", "10.0.0.1");
        assert_eq!(l.severity, 3);
        assert_eq!(l.msg, "something broke");
    }
}
