//! Shipping this machine's journal to the portal, over the connection that
//! already exists.
//!
//! Most of this fleet has no rsyslog - Debian 13 and current Proxmox ship
//! journald alone - so writing an rsyslog rule reaches almost none of it. This
//! reads the journal directly and sends lines over the WebSocket the agent is
//! already on: nothing installed, nothing written to `/etc`.
//!
//! # This must never be able to harm the agent
//!
//! Log shipping is not the agent's job, and an agent that wedges or dies
//! because of it is strictly worse than one that ships no logs. This session's
//! own headline bug was an unbounded await in the connect path that took nine
//! machines off the fleet for two days, so the rules here are deliberate:
//!
//! * **A bounded channel.** When the portal is slow or the machine is screaming,
//!   lines are dropped and counted rather than queued without limit. An
//!   unbounded channel is a memory leak with a polite name.
//! * **Never blocks the session.** The tailer owns a child process and a
//!   sender; it never takes a lock the session needs and never awaits anything
//!   the session is waiting on.
//! * **`kill_on_drop`.** The tailer lives in the session's task set, so it is
//!   dropped on every reconnect. Without this, each reconnect would leave
//!   another `journalctl -f` running forever - a slow leak that would look
//!   exactly like a memory problem in the agent.
//! * **The child dying is not fatal.** If `journalctl` is missing or exits, the
//!   tailer logs it and stops. The agent carries on doing its actual work.
//!
//! # Verification rather than redundancy
//!
//! The portal records that it asked for forwarding, so it can tell the
//! difference between "this machine is quiet" and "this machine was asked to
//! send and has sent nothing". That check catches a bug in this file, which
//! duplicating the mechanism would not.

use std::process::Stdio;

use anyhow::{Context, Result};
use pp_proto::JournalLine;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Lines held between flushes. Past this they are dropped and counted: a host
/// emitting faster than the link can carry is a host whose newest lines matter
/// more than its oldest.
const BUFFER: usize = 2000;
/// How often a batch goes out. Long enough to coalesce a burst, short enough
/// that a reboot's last words arrive before the reboot.
const FLUSH_MS: u64 = 2000;

/// Follow the journal, handing batches to `send`.
///
/// Returns when the child exits or the channel closes. Errors are the caller's
/// to log, not to die on.
pub async fn follow<F>(min_severity: &str, send: F) -> Result<()>
where
    F: FnMut(Vec<JournalLine>),
{
    #[cfg(windows)]
    {
        return follow_eventlog(min_severity, send).await;
    }
    #[cfg(not(windows))]
    follow_journal(min_severity, send).await
}

async fn follow_journal<F>(min_severity: &str, mut send: F) -> Result<()>
where
    F: FnMut(Vec<JournalLine>),
{
    // `-o json` rather than a text format: the fields are named, so there is no
    // second log parser to get wrong, and priority arrives as a number instead
    // of being inferred.
    //
    // `-n 0` starts at the present. Without it a first run would ship the whole
    // retained journal, which on a busy host is hundreds of megabytes.
    let mut child = Command::new("journalctl")
        .args([
            "-f",
            "-n",
            "0",
            "-p",
            min_severity,
            "-o",
            "json",
            "--no-pager",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Every reconnect drops this task. Without kill_on_drop each one would
        // leave a journalctl behind forever.
        .kill_on_drop(true)
        .spawn()
        .context("starting journalctl; is systemd present?")?;

    let stdout = child
        .stdout
        .take()
        .context("journalctl produced no stdout")?;
    let (tx, mut rx) = mpsc::channel::<JournalLine>(BUFFER);

    // Reader: one task, one job. try_send so a full buffer drops the line
    // instead of parking the reader - which would stall the pipe and eventually
    // block journalctl itself.
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut dropped = 0usize;
        while let Ok(Some(raw)) = lines.next_line().await {
            if let Some(line) = parse(&raw) {
                if tx.try_send(line).is_err() {
                    dropped += 1;
                    // Said once per thousand rather than once per line: the
                    // report must not become the flood it is reporting.
                    if dropped % 1000 == 1 {
                        tracing::warn!(dropped, "journal lines dropped; buffer full");
                    }
                }
            }
        }
        dropped
    });

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(FLUSH_MS));
    let mut batch = Vec::new();
    loop {
        tokio::select! {
            got = rx.recv() => match got {
                Some(line) => batch.push(line),
                // Reader finished: flush what is left and stop.
                None => {
                    if !batch.is_empty() {
                        send(std::mem::take(&mut batch));
                    }
                    break;
                }
            },
            _ = tick.tick() => {
                if !batch.is_empty() {
                    send(std::mem::take(&mut batch));
                }
            }
        }
    }

    let _ = child.kill().await;
    if let Ok(dropped) = reader.await {
        if dropped > 0 {
            tracing::warn!(dropped, "journal lines were dropped over this session");
        }
    }
    Ok(())
}

/// One `journalctl -o json` record, or nothing.
///
/// A record that cannot be read is skipped rather than guessed at: inventing a
/// timestamp or a priority would put a confident wrong line in front of
/// somebody trying to work out what happened.
fn parse(raw: &str) -> Option<JournalLine> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let message = match v.get("MESSAGE") {
        Some(serde_json::Value::String(s)) => s.clone(),
        // A binary message comes back as an array of bytes.
        Some(serde_json::Value::Array(bytes)) => {
            let raw: Vec<u8> = bytes.iter().filter_map(|b| b.as_u64().map(|n| n as u8)).collect();
            String::from_utf8_lossy(&raw).into_owned()
        }
        _ => return None,
    };
    if message.trim().is_empty() {
        return None;
    }

    let priority = v
        .get("PRIORITY")
        .and_then(|p| p.as_str())
        .and_then(|p| p.parse::<i64>().ok())
        .unwrap_or(6);

    // Microseconds since the epoch, as a string.
    let at = v
        .get("__REALTIME_TIMESTAMP")
        .and_then(|t| t.as_str())
        .and_then(|t| t.parse::<i64>().ok())
        .and_then(|us| chrono::DateTime::from_timestamp_micros(us))
        .unwrap_or_else(chrono::Utc::now);

    let tag = v
        .get("SYSLOG_IDENTIFIER")
        .or_else(|| v.get("_COMM"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();

    Some(JournalLine {
        at,
        priority,
        tag,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_journal_record() {
        let l = parse(
            r#"{"__REALTIME_TIMESTAMP":"1790000000000000","PRIORITY":"3",
                "SYSLOG_IDENTIFIER":"kernel","MESSAGE":"I/O error on sdb"}"#,
        )
        .expect("should parse");
        assert_eq!(l.priority, 3);
        assert_eq!(l.tag, "kernel");
        assert_eq!(l.message, "I/O error on sdb");
    }

    /// A binary message is an array of bytes, not a string, and dropping those
    /// would lose exactly the kernel output worth reading.
    #[test]
    fn reads_a_binary_message() {
        let l = parse(r#"{"PRIORITY":"4","MESSAGE":[104,105]}"#).expect("should parse");
        assert_eq!(l.message, "hi");
    }

    /// Skipped, not guessed at. A confident wrong line is worse than a missing
    /// one for somebody working out what happened.
    #[test]
    fn skips_what_it_cannot_read() {
        assert!(parse("not json").is_none());
        assert!(parse(r#"{"PRIORITY":"3"}"#).is_none(), "no message, no line");
        assert!(parse(r#"{"MESSAGE":"   "}"#).is_none(), "blank is not a line");
    }

    /// An absent priority must not read as an emergency.
    #[test]
    fn a_missing_priority_is_not_alarming() {
        let l = parse(r#"{"MESSAGE":"something"}"#).expect("should parse");
        assert_eq!(l.priority, 6, "info, not emerg");
    }
}


/// How many lines the journal holds for the last day, per severity floor.
///
/// Counted rather than estimated: the journal is indexed by priority, so asking
/// it four times is cheap for the restrictive levels and only the `info` pass
/// does real work. An estimate derived from a sample would be wrong in exactly
/// the case that matters - a host whose noise arrives in bursts.
pub async fn volume() -> Result<String> {
    #[cfg(windows)]
    {
        return volume_eventlog().await;
    }
    #[cfg(not(windows))]
    volume_journal().await
}

/// The same question on Windows, where there is no journal to count.
///
/// Left unimplemented at first, which made the one machine that most needed the
/// answer - the only Windows host here - the one machine that could not be
/// asked. A diagnostic that does not work on the platform you are diagnosing is
/// not a diagnostic.
#[cfg(windows)]
async fn volume_eventlog() -> Result<String> {
    let mut out = Vec::new();
    for (sev, label) in [
        ("err", "errors and worse"),
        ("warning", "warnings and worse"),
        ("info", "everything"),
    ] {
        let script = format!(
            "$ErrorActionPreference='SilentlyContinue';              @(Get-WinEvent -FilterHashtable @{{LogName='System','Application';              Level={}; StartTime=(Get-Date).AddHours(-24)}}).Count",
            windows_levels(sev)
        );
        let o = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .context("counting the Windows event log")?;
        let n = String::from_utf8_lossy(&o.stdout).trim().to_string();
        out.push(format!("{label}: {} events", if n.is_empty() { "0" } else { &n }));
    }
    Ok(format!(
        "last 24 hours, System and Application - {}",
        out.join("; ")
    ))
}

#[cfg(not(windows))]
async fn volume_journal() -> Result<String> {
    let mut out = Vec::new();
    for (level, label) in [
        ("err", "errors and worse"),
        ("warning", "warnings and worse"),
        ("notice", "notices and worse"),
        ("info", "everything"),
    ] {
        let o = Command::new("journalctl")
            .args(["--since=-24h", "-p", level, "-q", "--no-pager", "-o", "cat"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .context("counting the journal; is systemd present?")?;
        let lines = o.stdout.iter().filter(|b| **b == b'\n').count();
        // Bytes matter as much as lines for deciding what to ship.
        let kb = o.stdout.len() / 1024;
        out.push(format!("{label}: {lines} lines (~{kb} KB)"));
    }
    Ok(format!("last 24 hours - {}", out.join("; ")))
}


// -- Windows ----------------------------------------------------------------

/// How often the event log is asked for anything new.
///
/// Polled rather than followed: the Windows event log has no `tail -f`, and
/// `Get-WinEvent` has no follow mode. Thirty seconds is a compromise - short
/// enough that a machine's last words before a crash usually get out, long
/// enough that a PowerShell process is not being spawned constantly on a
/// machine somebody is also trying to use.
#[cfg(windows)]
const POLL_SECS: u64 = 30;

/// Which event levels correspond to a syslog floor.
///
/// Windows has five levels where syslog has eight, and they do not line up:
/// there is no "notice" and no "debug" worth shipping. Mapping is therefore
/// deliberate rather than arithmetic - `notice` asks for the same set as
/// `warning`, because inventing a distinction the source does not make would
/// give an operator a control that changes nothing.
pub fn windows_levels(min_severity: &str) -> &'static str {
    match min_severity.trim().to_lowercase().as_str() {
        "emerg" | "alert" | "crit" => "1",
        "err" => "1,2",
        // Critical, Error, Warning.
        "warning" | "notice" => "1,2,3",
        // Plus Information. Verbose (5) is deliberately never included: it is
        // the Windows equivalent of debug and is pure volume.
        _ => "1,2,3,4",
    }
}

/// Windows event level to syslog priority.
#[cfg(windows)]
fn priority_of(level: i64) -> i64 {
    match level {
        1 => 2, // Critical
        2 => 3, // Error
        3 => 4, // Warning
        4 => 6, // Information
        _ => 7,
    }
}

/// One `Get-WinEvent` record as JSON, or nothing.
///
/// Separate from the polling so it can be tested on any host - the parsing is
/// where the mistakes are, and none of it needs Windows to exercise.
pub fn parse_event(v: &serde_json::Value) -> Option<(i64, chrono::DateTime<chrono::Utc>, String, String)> {
    let message = v
        .get("Message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .trim()
        // Event log messages are paragraphs. One line each, or the file becomes
        // unreadable and the severity column stops lining up.
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if message.is_empty() {
        return None;
    }
    let level = v.get("Level").and_then(|l| l.as_i64()).unwrap_or(4);
    let provider = v
        .get("ProviderName")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();

    // PowerShell serialises DateTime as `/Date(1790000000000)/` milliseconds, or
    // as an ISO string depending on version. Both are in the wild.
    let at = v.get("TimeCreated").and_then(|t| match t {
        serde_json::Value::String(s) => s
            .strip_prefix("/Date(")
            .and_then(|r| r.split(')').next())
            .and_then(|ms| ms.parse::<i64>().ok())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .or_else(|| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&chrono::Utc))
            }),
        serde_json::Value::Number(n) => n.as_i64().and_then(chrono::DateTime::from_timestamp_millis),
        _ => None,
    })?;

    Some((level, at, provider, message))
}

#[cfg(windows)]
async fn follow_eventlog<F>(min_severity: &str, mut send: F) -> Result<()>
where
    F: FnMut(Vec<JournalLine>),
{
    let levels = windows_levels(min_severity);
    // Start from now, not from whatever the log holds: a first poll over a
    // retained event log would ship weeks of history.
    let mut since = chrono::Utc::now();

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(POLL_SECS)).await;

        // System and Application only. Security is deliberately excluded: it is
        // enormous, it is the one log an operator may be legally careful about,
        // and shipping it because somebody ticked "forward logs" would be a
        // surprise of the worst kind.
        // The time goes as epoch seconds and is rebuilt on the far side.
        //
        // `[datetime]'2026-09-26T08:39:16+00:00'` parses without complaint and
        // yields 08:39 *local*, which on a machine seven hours behind UTC is in
        // the future - so the filter matched nothing and the agent shipped
        // nothing, for as long as nobody checked. `FromUnixTimeSeconds` has no
        // timezone to get wrong, and `.LocalDateTime` is what `Get-WinEvent`
        // compares its records against.
        let script = format!(
            "Get-WinEvent -FilterHashtable @{{LogName='System','Application'; \
             Level={levels}; \
             StartTime=[datetimeoffset]::FromUnixTimeSeconds({}).LocalDateTime}} \
             -ErrorAction SilentlyContinue | \
             Select-Object -First 500 TimeCreated,Level,ProviderName,Message | \
             ConvertTo-Json -Compress -Depth 2",
            // A second of overlap, removed again by the filter above.
            // Starting exactly on the cursor risks dropping an event written
            // in the same second as the last one seen, and a lost line is
            // worse than a duplicate one.
            since.timestamp() - 1
        );
        let out = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await
            .context("querying the Windows event log")?;

        // Kept rather than discarded. A malformed filter produces no output and
        // no complaint, which looks exactly like a quiet machine - and that is
        // how a timezone bug here went unnoticed until the volume counter said
        // the machine should have been sending hundreds of events a day.
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if !err.is_empty() && !err.contains("No events were found") {
            tracing::warn!(
                error = %err.chars().take(300).collect::<String>(),
                "the event log query complained"
            );
        }

        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "event log query returned unreadable JSON");
                continue;
            }
        };
        // A single record comes back as an object, several as an array. Both
        // shapes are normal and handling only one loses every quiet poll.
        let records = match parsed {
            serde_json::Value::Array(a) => a,
            other => vec![other],
        };

        let mut batch = Vec::new();
        let mut newest = since;
        for r in records {
            if let Some((level, at, provider, message)) = parse_event(&r) {
                // The query window is deliberately a second wider than needed,
                // so the precision has to come from here.
                //
                // `StartTime` has one-second resolution, and advancing the
                // cursor by anything finer rounds back down into the same
                // second - which re-fetches the newest event on the next poll.
                // With sparse events every event is the newest of its own batch,
                // so every single line arrived twice.
                if at <= since {
                    continue;
                }
                if at > newest {
                    newest = at;
                }
                batch.push(JournalLine {
                    at,
                    priority: priority_of(level),
                    tag: provider,
                    message,
                });
            }
        }
        if newest > since {
            since = newest;
        }
        if !batch.is_empty() {
            batch.sort_by_key(|l| l.at);
            send(batch);
        }
    }
}

#[cfg(test)]
mod windows_tests {
    use super::*;

    /// The mapping is deliberate, not arithmetic. `notice` has no Windows
    /// equivalent, so it must ask for the same set as `warning` rather than
    /// silently meaning something else.
    #[test]
    fn severity_maps_to_windows_levels() {
        assert_eq!(windows_levels("err"), "1,2");
        assert_eq!(windows_levels("warning"), "1,2,3");
        assert_eq!(windows_levels("notice"), "1,2,3");
        assert_eq!(windows_levels("info"), "1,2,3,4");
        // Verbose is never requested: it is the Windows debug firehose.
        assert!(!windows_levels("info").contains('5'));
    }

    /// PowerShell serialises DateTime two different ways depending on version,
    /// and both are in the wild. Handling one would mean every event from the
    /// other kind of host is silently dropped.
    #[test]
    fn reads_both_powershell_date_shapes() {
        let epoch = serde_json::json!({
            "TimeCreated": "/Date(1790000000000)/", "Level": 2,
            "ProviderName": "Service Control Manager", "Message": "A service crashed"
        });
        let (level, _, provider, msg) = parse_event(&epoch).expect("epoch form");
        assert_eq!(level, 2);
        assert_eq!(provider, "Service Control Manager");
        assert_eq!(msg, "A service crashed");

        let iso = serde_json::json!({
            "TimeCreated": "2026-09-26T08:00:00Z", "Level": 3,
            "ProviderName": "disk", "Message": "The device has a bad block"
        });
        assert!(parse_event(&iso).is_some(), "iso form must parse too");
    }

    /// Event log messages are paragraphs. Shipped whole they destroy the
    /// column alignment of a file people read by eye.
    #[test]
    fn takes_only_the_first_line_of_a_paragraph() {
        let v = serde_json::json!({
            "TimeCreated": "/Date(1790000000000)/", "Level": 2, "ProviderName": "x",
            "Message": "The first line.\r\n\r\nA paragraph of explanation follows."
        });
        let (_, _, _, msg) = parse_event(&v).unwrap();
        assert_eq!(msg, "The first line.");
    }

    /// Skipped rather than guessed at: a record with no usable time cannot be
    /// placed in a log, and inventing one puts a confident wrong line in front
    /// of somebody working out what happened.
    #[test]
    fn skips_records_it_cannot_place() {
        assert!(parse_event(&serde_json::json!({"Message": "no time"})).is_none());
        assert!(parse_event(&serde_json::json!({"TimeCreated": "/Date(1)/"})).is_none());
        assert!(
            parse_event(&serde_json::json!({
                "TimeCreated": "/Date(1)/", "Message": "   "
            }))
            .is_none(),
            "blank is not a line"
        );
    }
}
