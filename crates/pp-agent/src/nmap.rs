//! Discovery by nmap, for ranges that ask for it.
//!
//! The built-in sweep in `probe.rs` can only report that something answered on
//! a port and repeat the first line it volunteered. That is enough to know a
//! host exists and almost never enough to know what it is: on a real /24, 26
//! hosts answering on 22 produced 26 rows saying "SSH-2.0-dropbear" or nothing
//! at all. nmap speaks each protocol instead of listening to it, so the same
//! hosts come back as "Dropbear sshd 2022.82" and "OpenSSH 9.2p1" — the
//! difference between a list to scroll past and a list to act on.
//!
//! It is opt-in per range because nmap is a second binary that may not be
//! installed, and because a version scan is far more traffic than a connect
//! is. When it cannot run, the caller falls back to the sweep and the rows say
//! so; a sweep that quietly lost its service detection looks exactly like a
//! network on which nothing could be identified.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use pp_proto::{DiscoveredHost, DiscoveredService, DiscoveryScan, Scanner};
use quick_xml::events::BytesStart;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use tokio::process::Command;

/// Longest a scan may run before we give up on it and sweep instead.
///
/// Generous, because the scan is deliberately throttled (see `POLITE`) and the
/// thing being protected against is a scan that has hung, not one that is taking
/// its time. `-Pn` means every address in the range is port scanned whether it
/// answers a ping or not, `-sV` then holds a conversation with each open port,
/// and a UDP scan waits out a timeout on every silent one.
///
/// Must stay below `manifest.discovery_secs` with room to spare, or a sweep that
/// hits the limit is immediately followed by the next one and the throttling is
/// undone by running continuously.
const NMAP_TIMEOUT: Duration = Duration::from_secs(2700);

/// Timing arguments that make the sweep a quiet background presence.
///
/// The default timing is tuned for a person waiting on the result, which is the
/// wrong trade for something that runs unattended on a network people are using.
/// Two separate problems with being quick about it: the obvious one is the
/// traffic, and the one that actually cost accuracy is that probes under load
/// time out and nmap records `tcpwrapped` - a Brother printer that had reported
/// its exact model from an HTTP title came back as two anonymous open ports once
/// UDP and scripts were added to the same scan. Slowing down gets that back, so
/// this is not purely a courtesy.
///
///   - `--max-rate` is the throttle. It caps packets per second outright, which
///     is both the thing a switch or a small router actually notices and the one
///     knob here whose cost can be predicted rather than discovered: the work is
///     bounded by packets, so halving the rate roughly doubles the wall clock.
///   - `--max-retries` above the default-for-speed of 1, because a dropped probe
///     is the normal case for UDP, and giving up after one try is what makes a
///     UDP scan unreliable rather than merely slow.
/// Explicitly *not* `-T2`, nmap's own "polite", which was tried first and is the
/// wrong tool at this scope: it pins parallelism to one probe at a time, and a
/// /24 with version, OS, UDP and script detection had not finished after
/// thirty-four minutes.
///
/// And explicitly *not* `--host-timeout`, which was the more instructive
/// mistake. Combined with a throttle it is actively destructive: a host's elapsed
/// time includes waiting its turn under the rate limit, so a per-host budget
/// expires on perfectly healthy hosts - and nmap's response to a host that
/// exceeds it is to discard everything it learned and report nothing for that
/// address. `-T2` with a 300s budget returned a /24 as **one** host, which the
/// portal then stored over the forty-four it had, because a list of one is not an
/// empty list and nothing about the result said it was truncated.
///
/// That is the failure mode to design against here: not a scan that is slow, but
/// one that looks successful while reporting almost nothing. `NMAP_TIMEOUT` is
/// the single bound, and exceeding it falls back to the built-in sweep with a
/// note on every row saying so - loudly wrong instead of quietly wrong.
const POLITE: &[&str] = &["--max-rate", "100", "--max-retries", "3"];
/// Cap on the identification carried in `hint`. Longer than the sweep's banner
/// allowance because a product name and version is worth more than the first
/// 80 bytes of whatever a socket said.
const MAX_HINT: usize = 120;

/// Below this, nmap's uptime guess is discarded rather than reported.
///
/// The guess extrapolates backwards from a host's TCP timestamp clock, which
/// assumes that clock started at boot. Plenty of stacks randomise the timestamp
/// offset per connection instead, and against those nmap computes a few seconds
/// - so the first sweep of this fleet had a FreeBSD router and three Proxmox
/// hosts all claiming to have booted twenty seconds ago, while their real
/// uptimes were weeks. A wrong uptime is worse than no uptime, because the whole
/// point of showing it is to catch a machine that has not rebooted in a year.
/// An hour is chosen because a genuine sub-hour uptime is indistinguishable from
/// the artifact, and the artifact is far more common.
const MIN_UPTIME: i64 = 3600;
/// Cap on one script's output, and on how many host-level scripts are kept.
///
/// `ssl-cert` alone prints a whole certificate and `upnp-info` a full device
/// description. This is evidence for a person to read in a detail pane, not a
/// log to archive, so it is trimmed to the part that identifies something.
/// The identifying scripts asked for by `use_scripts`.
///
/// Every name checked against an installed nmap, because one it does not
/// recognise aborts the whole scan rather than being skipped - the first attempt
/// asked for `mdns-service-discovery`, which does not exist (it is
/// `dns-service-discovery`) and took the entire sweep down with it. The failure
/// is at least loud: the range falls back to the built-in sweep and the note on
/// every row quotes what nmap said.
///
/// None of these probes for weaknesses or tries credentials, which is why the
/// set is named rather than taken from nmap's `-sC` default category.
const SCRIPTS: &str = "ssl-cert,http-title,snmp-info,snmp-sysdescr,smb-os-discovery,upnp-info,dns-service-discovery";

const MAX_SCRIPT: usize = 400;
const MAX_SCRIPTS: usize = 6;

/// Scan `scan.cidr` with nmap and report what answered.
///
/// The range is expected to have been size-checked by the caller: nmap will
/// cheerfully accept a /8 and spend a week on it.
pub async fn sweep(scan: &DiscoveryScan, light: &[String]) -> Result<Vec<DiscoveredHost>> {
    // Two passes when there is anything to be gentle with: the full treatment on
    // everything unaccounted for, and a quieter one on the addresses the portal
    // already knows.
    //
    // Two invocations rather than one, because nmap's port list is global to a run
    // and there is no way to say "these ports for those hosts". The second pass is
    // cheap - a handful of hosts, no OS detection, no scripts - and skipping it
    // entirely would cost the Network tab the open ports of every machine in the
    // fleet, which is the one thing that would notice one of them exposing
    // something it should not.
    if light.is_empty() {
        return one_pass(scan, &[], true).await;
    }
    let mut hosts = one_pass(scan, light, true).await?;
    match one_pass(scan, &[], false).await {
        Ok(known) => hosts.extend(known),
        // The accounted hosts are the ones we already know most about, so losing
        // this pass is a thin row rather than a blind spot. The unaccounted hosts
        // are the point of the sweep and they are already in hand.
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "the light pass over known hosts failed"),
    }
    Ok(hosts)
}

/// One nmap invocation.
///
/// `exclude` is skipped entirely. `full` asks for OS detection, scripts and
/// version detection on every port; without it the run is version detection only,
/// and port 22 is dropped from the list - see `Command::Discover::light`.
/// The arguments for one nmap invocation.
///
/// Separated from running it because every mistake this file has shipped was an
/// argument mistake: a port form nmap refused outright, a script name it did not
/// have, a timing flag that silently discarded hosts. None of those were reachable
/// from a test while the list was assembled inside the function that spawns the
/// process. An empty result means this pass has nothing to scan.
fn plan(scan: &DiscoveryScan, exclude: &[String], full: bool) -> Vec<String> {
    let list = |ps: &[u16]| {
        ps.iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };

    let tcp = &scan.ports;
    if tcp.is_empty() && scan.udp_ports.is_empty() {
        return Vec::new();
    }
    // A bare list when only TCP is wanted, and the `T:`/`U:` form only when both
    // are. The prefixed form obliges nmap to be told the TCP scan type explicitly -
    // without one it refuses the whole scan - so the plain list is both simpler and
    // one less thing to get wrong in the common case.
    let ports = if scan.udp_ports.is_empty() {
        list(tcp)
    } else if tcp.is_empty() {
        format!("U:{}", list(&scan.udp_ports))
    } else {
        format!("T:{},U:{}", list(tcp), list(&scan.udp_ports))
    };

    // `-Pn` because half the point of this is embedded hardware, and a device that
    // drops ICMP but answers on 502 is exactly the one nobody wrote down.
    //
    // Reverse DNS is left on. It used to be disabled with `-n` on the grounds that
    // 254 PTR lookups are the slowest part of a scan, which was a fair trade when
    // nothing displayed a name - but a PTR record is somebody having already
    // written down what a machine is, and that is the cheapest identification on
    // offer. nmap resolves only the hosts that answered, in parallel.
    let mut args: Vec<String> = ["-oX", "-", "-Pn", "--open"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    if full {
        // Version detection is the only part of a scan that talks to the service
        // rather than to the port, and therefore the only part anything logs or
        // objects to. On the hosts nobody has accounted for it is the whole point -
        // it is what turns "22 is open" into "Dropbear sshd 2025.89". On the hosts
        // we already know it buys nothing and costs us a banner grab that OpenSSH
        // penalises and an empty HTTP request that Proxmox's proxy records as a
        // client problem, once an hour, forever.
        //
        // Leaving it off is also what lets the quiet pass keep every port including
        // 22: a SYN scan never completes the handshake, so the service is never
        // spoken to at all. The first version of this dropped port 22 instead,
        // which stopped the penalties but also lost "22 is open" on our own
        // machines - solving it in the wrong place.
        args.push("-sV".into());
        args.push("--version-light".into());
        // OS fingerprinting needs raw sockets, and the agent runs as root on Linux.
        // It costs time and returns a guess rather than a fact, which is why the
        // accuracy is carried alongside the name everywhere it is shown.
        // `--osscan-guess` reports the best matches it has rather than refusing to
        // answer: without it a host that is merely unusual gets nothing at all,
        // which reads as "nothing to see".
        args.push("-O".into());
        args.push("--osscan-guess".into());
    }
    args.extend(POLITE.iter().map(|s| s.to_string()));

    // Only the addresses that fall inside this range. The portal sends every
    // address it accounts for, which includes each machine's docker bridges and
    // VPN endpoints - real addresses of ours, just not on the network being swept.
    // nmap ignores an exclusion it never meets, so this is about the command
    // staying readable and the list staying bounded rather than correctness: it
    // grows by one entry per container bridge otherwise.
    let here: Vec<&String> = match scan.cidr.parse::<ipnet::IpNet>() {
        Ok(net) => exclude
            .iter()
            .filter(|a| {
                a.parse::<std::net::IpAddr>()
                    .map(|ip| net.contains(&ip))
                    .unwrap_or(false)
            })
            .collect(),
        // An unparseable CIDR is the caller's problem and `range` will say so;
        // excluding everything offered is the safe reading here.
        Err(_) => exclude.iter().collect(),
    };
    if !here.is_empty() {
        args.push("--exclude".into());
        args.push(here.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(","));
    }

    if !scan.udp_ports.is_empty() {
        // `-sS` because the `T:`/`U:` port form requires the TCP scan type to be
        // named rather than defaulted. A SYN scan needs raw sockets, the same
        // privilege `-O` relies on.
        args.push("-sS".into());
        args.push("-sU".into());
    }

    // A named set, not nmap's `-sC` category: these put a name to hardware with no
    // readable version banner, and none of them probes for weaknesses or tries
    // credentials, which has no place in a sweep that runs unattended.
    if scan.use_scripts && full {
        args.push("--script".into());
        args.push(SCRIPTS.into());
    }

    args.push("-p".into());
    args.push(ports);
    args.push(scan.cidr.clone());
    args
}

async fn one_pass(
    scan: &DiscoveryScan,
    exclude: &[String],
    full: bool,
) -> Result<Vec<DiscoveredHost>> {
    let args = plan(scan, exclude, full);
    // Nothing to ask for - every port was filtered out of this pass.
    if args.is_empty() {
        return Ok(Vec::new());
    }

    let run = Command::new("nmap")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();

    let out = tokio::time::timeout(NMAP_TIMEOUT, run)
        .await
        .map_err(|_| {
            anyhow::anyhow!("nmap was still scanning after {}s", NMAP_TIMEOUT.as_secs())
        })?
        .context("could not run `nmap`; is it installed and on PATH?")?;

    if !out.status.success() {
        // A refused argument or an unusable target leaves nothing on stdout,
        // so there is no partial result worth keeping here.
        anyhow::bail!(
            "nmap exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    parse(&String::from_utf8_lossy(&out.stdout))
}

/// Pull the hosts out of `nmap -oX -` output.
///
/// The XML is the only output shape nmap commits to keeping stable - the
/// human-readable form has moved between point releases - and the element names
/// below were taken from what nmap 7.97 actually emits rather than from the
/// documentation.
fn parse(xml: &str) -> Result<Vec<DiscoveredHost>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut saw_run = false;
    let mut closed = false;
    let mut hosts: Vec<DiscoveredHost> = Vec::new();
    let mut host: Option<Host> = None;
    let mut port: Option<Port> = None;
    // `<cpe>` carries its value as text rather than an attribute, so it needs a
    // flag across two events. Which thing it describes is decided by whether a
    // port is open at the time: inside `<service>` it is that service, and
    // inside `<osclass>`, which nmap emits after the ports are closed, it is
    // the host.
    let mut in_cpe = false;

    loop {
        let event = reader
            .read_event()
            .context("nmap produced something that is not XML")?;

        let (e, empty) = match &event {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) => {
                match e.name().as_ref() {
                    b"cpe" => in_cpe = false,
                    b"port" => close_port(&mut host, port.take()),
                    b"host" => finish(&mut hosts, host.take()),
                    b"nmaprun" => closed = true,
                    _ => {}
                }
                continue;
            }
            Event::Eof => break,
            Event::Text(t) if in_cpe => {
                // A CPE is an ASCII identifier with no entities in it, so this
                // only has to decode and normalise, the same way `attr` does.
                let text = t
                    .xml_content(XmlVersion::Explicit1_0)
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default();
                if !text.is_empty() {
                    match (port.as_mut().and_then(|p| p.service.as_mut()), host.as_mut()) {
                        (Some(s), _) => s.cpe.push(text),
                        (None, Some(h)) if h.identity.os_cpe.len() < 4 => {
                            if !h.identity.os_cpe.contains(&text) {
                                h.identity.os_cpe.push(text);
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            _ => continue,
        };

        match e.name().as_ref() {
            b"nmaprun" => saw_run = true,
            b"cpe" => in_cpe = true,
            b"host" => host = Some(Host::default()),
            // `<address>` appears more than once per host: the MAC address is
            // reported alongside the IP on a local segment, and taking the
            // last one would report a fleet of MAC addresses.
            b"address" => {
                if let Some(h) = host.as_mut() {
                    let kind = attr(e, "addrtype");
                    if h.ip.is_empty() && (kind == "ipv4" || kind == "ipv6") {
                        h.ip = attr(e, "addr");
                    }
                    // The hardware address, and the vendor that owns its OUI.
                    // Only present on the scanner's own segment, and the most
                    // reliable identifier in the whole record: assigned, not
                    // inferred. It is what tells you a silent web server is a
                    // Yamaha rather than a guess about its HTTP banner.
                    if kind == "mac" {
                        h.identity.mac = attr(e, "addr");
                        h.identity.mac_vendor = attr(e, "vendor");
                    }
                }
            }
            // `<osmatch name accuracy>`, best first. Kept as a list because a
            // 90% and an 88% match are not the same thing as one answer, and
            // showing only the winner hides how close the field was.
            b"osmatch" => {
                if let Some(h) = host.as_mut() {
                    let name = attr(e, "name");
                    let accuracy: u8 = attr(e, "accuracy").parse().unwrap_or(0);
                    if name.is_empty() {
                        continue;
                    }
                    if h.identity.os.is_empty() {
                        h.identity.os = name;
                        h.identity.os_accuracy = accuracy;
                    } else if h.identity.os_alternatives.len() < 4 {
                        h.identity
                            .os_alternatives
                            .push(format!("{name} ({accuracy}%)"));
                    }
                }
            }
            // Inferred from TCP timestamps, so approximate and often absent.
            // Worth having anyway: a device claiming four hundred days of
            // uptime has not been patched in four hundred days.
            b"uptime" => {
                if let Some(h) = host.as_mut() {
                    h.identity.uptime_secs =
                        attr(e, "seconds").parse().ok().filter(|s| *s >= MIN_UPTIME);
                }
            }
            // Both protocols, each kept as itself. A udp port reported as a tcp
            // one sends somebody to the wrong socket, so the distinction is
            // carried rather than flattened - anything that is neither is
            // dropped, since there is nowhere truthful to put it.
            b"port" if matches!(attr(e, "protocol").as_str(), "tcp" | "udp") => {
                let udp = attr(e, "protocol") == "udp";
                port = attr(e, "portid").parse().ok().map(|number| Port {
                    number,
                    udp,
                    open: false,
                    service: None,
                });
                if empty {
                    close_port(&mut host, port.take());
                }
            }
            // `<state>` only ever appears inside a port; the host's own
            // reachability is `<status>`, which `-Pn` answers "up" regardless.
            b"state" => {
                if let Some(p) = port.as_mut() {
                    p.open = attr(e, "state") == "open";
                }
            }
            b"service" => {
                if let Some(p) = port.as_mut() {
                    let (n, udp) = (p.number, p.udp);
                    p.service = Some(service(e, n, udp));
                }
            }
            // `<osclass>` is the answer to "what is this", where `<osmatch>` is
            // the answer to "what is it running". Only the first is kept: nmap
            // emits them best-first, and a device that is 96% a printer and 94%
            // a router is not usefully described as both.
            b"osclass" => {
                if let Some(h) = host.as_mut() {
                    if h.identity.device_type.is_empty() {
                        h.identity.device_type = attr(e, "type");
                        h.identity.vendor = attr(e, "vendor");
                        h.identity.os_family = attr(e, "osfamily");
                    }
                }
            }
            // A PTR record is somebody having already written down what this
            // machine is, which makes it the most valuable field here and the
            // one that was being thrown away by passing `-n`.
            b"hostname" => {
                if let Some(h) = host.as_mut() {
                    let name = attr(e, "name");
                    if !name.is_empty() && !h.identity.hostnames.contains(&name) {
                        h.identity.hostnames.push(name);
                    }
                }
            }
            // Script output is an attribute, not element text, and belongs to
            // whichever thing is open at the time: inside a `<port>` it is that
            // port's, and inside `<hostscript>` it is the host's.
            b"script" => {
                let id = attr(e, "id");
                let text = clip(&attr(e, "output"), MAX_SCRIPT);
                if !id.is_empty() && !text.is_empty() {
                    match port.as_mut().and_then(|p| p.service.as_mut()) {
                        Some(s) => s.scripts.push((id, text)),
                        None => {
                            if let Some(h) = host.as_mut() {
                                if h.identity.scripts.len() < MAX_SCRIPTS {
                                    h.identity.scripts.push((id, text));
                                }
                            }
                        }
                    }
                }
            }
            // nmap reports a scan it abandoned part-way with an exit status of
            // its own, having already printed the hosts it got to. Those are
            // real, but the absence of the rest is not evidence of an empty
            // network, so the whole scan is failed and the caller sweeps.
            b"finished" => {
                if attr(e, "exit") == "error" {
                    anyhow::bail!("nmap gave up: {}", attr(e, "errormsg"));
                }
            }
            _ => {}
        }
    }

    if !saw_run {
        anyhow::bail!("no <nmaprun> in nmap's output");
    }
    // An XML pull parser reaching the end of a truncated document reports the
    // end of the document, not an error - so output that stops half way would
    // otherwise be read as a range where the rest of the hosts are absent.
    // nmap writes the closing tag only once it has finished.
    if !closed {
        anyhow::bail!("nmap's output stops part-way through; the scan did not finish");
    }
    Ok(hosts)
}

/// A host being assembled, before it is known to have any open port.
#[derive(Default)]
struct Host {
    ip: String,
    open: Vec<u16>,
    open_udp: Vec<u16>,
    services: Vec<DiscoveredService>,
    identity: pp_proto::HostIdentity,
}

struct Port {
    number: u16,
    udp: bool,
    open: bool,
    service: Option<DiscoveredService>,
}

/// What nmap learned about one port, keeping only what it established.
///
/// `method` is the load-bearing attribute: "probed" means nmap held a
/// conversation, "table" means it looked the port number up in
/// `nmap-services` and guessed. The guess is worthless here - the port number
/// is already in `open_ports`, and presenting `nmap-services` back as a finding
/// would turn "445 is open" into the claim that Windows file sharing is running
/// there.
fn service(e: &BytesStart, port: u16, udp: bool) -> DiscoveredService {
    let probed = attr(e, "method") == "probed";
    let name = match (probed, attr(e, "name")) {
        (true, n) if !n.is_empty() => {
            // nmap's own convention for a protocol inside TLS, and the only
            // thing that distinguishes https from http here.
            if attr(e, "tunnel") == "ssl" {
                format!("ssl/{n}")
            } else {
                n
            }
        }
        _ => String::new(),
    };

    DiscoveredService {
        port,
        name,
        product: attr(e, "product"),
        version: attr(e, "version"),
        // Only from a real probe, for the same reason as `name`: nmap fills
        // these in from evidence, and a table lookup has none to offer.
        extra: if probed { attr(e, "extrainfo") } else { String::new() },
        cpe: Vec::new(),
        scripts: Vec::new(),
        protocol: if udp { "udp".into() } else { "tcp".into() },
    }
}

/// Trim to `n` characters, collapsing the runs of whitespace that nmap's
/// multi-line script output is mostly made of.
fn clip(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(n) {
        Some((at, _)) => format!("{}...", &flat[..at]),
        None => flat,
    }
}

/// A short identification taken from script output.
///
/// For the hosts that matter most here: a printer or a scanner serves a web page
/// with its model number in the title and answers SNMP with its own description,
/// while naming no product on any port - so before this it was a row of open
/// ports and nothing else. A Brother ADS-2700W on this fleet went from
/// unidentifiable to naming itself and its firmware revision.
///
/// Only two scripts are read, both of which lead with what the device calls
/// itself. The rest is left for the detail pane, where a person can read it.
fn from_scripts(services: &[DiscoveredService]) -> String {
    for want in ["snmp-sysdescr", "http-title"] {
        for (name, text) in services.iter().flat_map(|s| s.scripts.iter()) {
            if name != want {
                continue;
            }
            // nmap appends where it was redirected and, for SNMP, the uptime.
            // Both are about the request rather than the device.
            let head = text
                .split(" Requested resource was")
                .next()
                .unwrap_or(text)
                .split(" System uptime:")
                .next()
                .unwrap_or(text)
                .trim();
            // Its two ways of saying it found nothing, which are not findings.
            if head.is_empty()
                || head.starts_with("Site doesn't have a title")
                || head.starts_with("Did not follow redirect")
            {
                continue;
            }
            return clip(head, MAX_HINT);
        }
    }
    String::new()
}

fn close_port(host: &mut Option<Host>, port: Option<Port>) {
    let (Some(h), Some(p)) = (host.as_mut(), port) else {
        return;
    };
    if !p.open {
        return;
    }
    if p.udp {
        h.open_udp.push(p.number);
    } else {
        h.open.push(p.number);
    }
    // A port with nothing established is carried by `open_ports` alone. Keeping
    // an empty row per port would make a host nmap could not identify look
    // identically detailed to one it could.
    if let Some(svc) = p.service.filter(|s| !s.name.is_empty() || !s.product.is_empty()) {
        h.services.push(svc);
    }
}

fn finish(hosts: &mut Vec<DiscoveredHost>, host: Option<Host>) {
    let Some(h) = host else { return };
    // `-Pn` makes every address in the range "up", so an open port is the only
    // evidence that anything is there. Either protocol counts: a device whose
    // only open port is 161/udp is precisely the kind of thing a UDP scan was
    // turned on to find, and testing tcp alone would have discarded it.
    if h.ip.is_empty() || (h.open.is_empty() && h.open_udp.is_empty()) {
        return;
    }

    hosts.push(DiscoveredHost {
        ip: h.ip,
        open_ports: h.open,
        open_udp: h.open_udp,
        // Best evidence first. Software nmap spoke to and named, then the
        // reverse-DNS name - somebody already wrote that down, which beats
        // anything inferred - then the OUI vendor, which is at least assigned
        // rather than guessed and is often all a silent device offers.
        hint: [
            hint(&h.services),
            from_scripts(&h.services),
            h.identity.hostnames.first().cloned().unwrap_or_default(),
            h.identity.mac_vendor.clone(),
        ]
        .into_iter()
        .find(|c| !c.is_empty())
        .unwrap_or_default(),
        identity: h.identity,
        // The portal decides this; the agent only knows the devices it was
        // told about.
        unmanaged: true,
        services: h.services,
        scanner: Scanner::Nmap,
        scan_note: String::new(),
    });
}

/// One line naming the host, from the software nmap put a name to.
///
/// Products only. A protocol name on its own ("ssh") says less than the port
/// already does, and a host nmap could not identify is left with an empty hint
/// rather than a plausible-sounding one.
fn hint(services: &[DiscoveredService]) -> String {
    let mut named: Vec<String> = Vec::new();
    for svc in services.iter().filter(|s| !s.product.is_empty()) {
        let one = if svc.version.is_empty() {
            svc.product.clone()
        } else {
            format!("{} {}", svc.product, svc.version)
        };
        if !named.contains(&one) {
            named.push(one);
        }
    }

    let joined = named.join(", ");
    match joined.char_indices().nth(MAX_HINT) {
        Some((cut, _)) => format!("{}...", &joined[..cut]),
        None => joined,
    }
}

fn attr(e: &BytesStart, key: &str) -> String {
    e.try_get_attribute(key)
        .ok()
        .flatten()
        // nmap declares XML 1.0; the versions differ only in how control
        // characters inside an attribute are normalised.
        .and_then(|a| a.normalized_value(XmlVersion::Explicit1_0).ok())
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `nmap 7.97 -oX -` run, keeping every shape the
    /// parser has to survive: a MAC address reported next to the IP, a port
    /// identified by conversation, a port named only from nmap's port table, an
    /// open port with no `<service>` at all, a closed port inside the same
    /// host, and a host with nothing open which `-Pn` still reports as up.
    const REAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nmaprun>
<nmaprun scanner="nmap" args="nmap -oX - -Pn -n --open -sV --version-light -p 22,80,443,502 192.168.6.0/24" start="1790410973" version="7.97" xmloutputversion="1.05">
<scaninfo type="connect" protocol="tcp" numservices="4" services="22,80,443,502"/>
<verbose level="0"/>
<host starttime="1790410974" endtime="1790410980"><status state="up" reason="user-set" reason_ttl="0"/>
<address addr="192.168.6.31" addrtype="ipv4"/>
<address addr="B8:27:EB:1A:2B:3C" addrtype="mac" vendor="Raspberry Pi Foundation"/>
<hostnames>
</hostnames>
<ports><extraports state="closed" count="2">
<extrareasons reason="conn-refused" count="2" proto="tcp" ports="443,502"/>
</extraports>
<port protocol="tcp" portid="22"><state state="open" reason="syn-ack" reason_ttl="64"/><service name="ssh" product="Dropbear sshd" version="2022.82" extrainfo="protocol 2.0" method="probed" conf="10"><cpe>cpe:/a:matt_johnston:dropbear_ssh_server:2022.82</cpe></service></port>
<port protocol="tcp" portid="80"><state state="open" reason="syn-ack" reason_ttl="64"/><service name="http" product="lighttpd" method="probed" conf="10"/></port>
</ports>
</host>
<host starttime="1790410974" endtime="1790410981"><status state="up" reason="user-set" reason_ttl="0"/>
<address addr="192.168.6.44" addrtype="ipv4"/>
<ports>
<port protocol="tcp" portid="443"><state state="open" reason="syn-ack" reason_ttl="128"/><service name="https" method="table" conf="3"/></port>
<port protocol="tcp" portid="502"><state state="open" reason="syn-ack" reason_ttl="128"/></port>
<port protocol="tcp" portid="22"><state state="closed" reason="conn-refused" reason_ttl="64"/></port>
</ports>
</host>
<host starttime="1790410974" endtime="1790410981"><status state="up" reason="user-set" reason_ttl="0"/>
<address addr="192.168.6.99" addrtype="ipv4"/>
<ports><extraports state="filtered" count="4">
<extrareasons reason="no-response" count="4" proto="tcp" ports="22,80,443,502"/>
</extraports>
</ports>
</host>
<runstats><finished time="1790410990" timestr="Sat Sep 26 01:23:00 2026" elapsed="17.27" summary="Nmap done" exit="success"/><hosts up="256" down="0" total="256"/>
</runstats>
</nmaprun>
"#;

    /// The whole reason for running nmap: a port that answered has to come back
    /// with the software behind it, and that has to reach `hint` as something
    /// better than a banner.
    #[test]
    fn reads_products_and_versions_per_port() {
        let hosts = parse(REAL).expect("parses");
        let first = &hosts[0];
        assert_eq!(first.ip, "192.168.6.31");
        assert_eq!(first.open_ports, vec![22, 80]);
        assert_eq!(first.scanner, Scanner::Nmap);

        let ssh = &first.services[0];
        assert_eq!((ssh.port, ssh.name.as_str()), (22, "ssh"));
        assert_eq!(ssh.product, "Dropbear sshd");
        assert_eq!(ssh.version, "2022.82");
        // A product with no version is still a product.
        assert_eq!(first.services[1].product, "lighttpd");
        assert_eq!(first.services[1].version, "");

        assert_eq!(first.hint, "Dropbear sshd 2022.82, lighttpd");
    }

    /// The CPE is the only machine-readable identifier nmap emits, and it
    /// arrives as element text rather than an attribute - so it is the one field
    /// here that a parser looking only at attributes silently drops.
    #[test]
    fn reads_the_cpe_of_a_service() {
        let hosts = parse(REAL).expect("parses");
        let ssh = &hosts[0].services[0];
        assert_eq!(
            ssh.cpe,
            vec!["cpe:/a:matt_johnston:dropbear_ssh_server:2022.82"]
        );
    }

    /// nmap extrapolates uptime from a host's TCP timestamp clock, and against a
    /// stack that randomises the timestamp offset per connection it computes a
    /// few seconds. Reporting that would say a router which has been up for
    /// weeks booted during the scan, which is worse than saying nothing.
    #[test]
    fn discards_an_uptime_that_is_the_timestamp_artifact() {
        let xml = REAL.replace(
            r#"<address addr="192.168.6.31" addrtype="ipv4"/>"#,
            r#"<address addr="192.168.6.31" addrtype="ipv4"/><uptime seconds="19" lastboot="x"/>"#,
        );
        assert_eq!(parse(&xml).expect("parses")[0].identity.uptime_secs, None);

        let xml = REAL.replace(
            r#"<address addr="192.168.6.31" addrtype="ipv4"/>"#,
            r#"<address addr="192.168.6.31" addrtype="ipv4"/><uptime seconds="1470518" lastboot="x"/>"#,
        );
        assert_eq!(
            parse(&xml).expect("parses")[0].identity.uptime_secs,
            Some(1_470_518)
        );
    }

    /// A guess from nmap's port table is not a finding. Reporting it would turn
    /// "443 is open" into "this is running HTTPS", which is precisely the
    /// invented detail the built-in sweep is honest about not having.
    #[test]
    fn refuses_to_report_a_service_nmap_only_guessed() {
        let hosts = parse(REAL).expect("parses");
        let guessed = hosts.iter().find(|h| h.ip == "192.168.6.44").expect("host");
        // Both ports are still reported as open - that part was observed.
        assert_eq!(guessed.open_ports, vec![443, 502]);
        assert!(
            guessed.services.is_empty(),
            "method=\"table\" is a lookup table, not evidence: {:?}",
            guessed.services
        );
        assert_eq!(guessed.hint, "", "nothing was identified, so nothing is named");
    }

    /// `-Pn` reports every address in the range as up, so a host element is not
    /// evidence of a host. Counting them would turn a /24 into 254 discovered
    /// devices, and the closed port inside a host that does have open ones must
    /// not be listed either.
    #[test]
    fn only_counts_hosts_with_an_open_port() {
        let hosts = parse(REAL).expect("parses");
        assert_eq!(hosts.len(), 2, "192.168.6.99 has nothing open");
        assert!(!hosts.iter().any(|h| h.ip == "192.168.6.99"));
        assert!(
            !hosts.iter().any(|h| h.open_ports.contains(&22) && h.ip == "192.168.6.44"),
            "22 was closed on that host"
        );
    }

    /// The MAC address nmap reports on a local segment is an `<address>`
    /// element too. Taking the wrong one reports a fleet of MAC addresses,
    /// which the portal then cannot match against anything it knows.
    #[test]
    fn keeps_the_ip_not_the_mac() {
        let hosts = parse(REAL).expect("parses");
        assert!(hosts.iter().all(|h| h.ip.contains('.') && !h.ip.contains(':')));
    }

    /// Output that is not nmap XML has to fail rather than come back as an
    /// empty network: "nothing answered" and "nothing was asked" must never
    /// arrive at the portal looking the same.
    #[test]
    fn unparseable_output_is_an_error_not_an_empty_result() {
        assert!(parse("nmap: unrecognized option --version-light\n").is_err());
        assert!(parse("").is_err(), "empty output is not an empty network");
        assert!(
            parse("<nmaprun><host><address addr=\"10.0.0.1\" addrtype=\"ipv4\"/>").is_err(),
            "truncated XML is a failed scan"
        );
    }

    /// A scan nmap abandoned half way has already printed some hosts. Trusting
    /// them would report the part of the range it reached as the whole of it.
    #[test]
    fn a_scan_nmap_abandoned_is_not_a_result() {
        let cut = REAL.replace(
            r#"summary="Nmap done" exit="success""#,
            r#"summary="Nmap done" exit="error" errormsg="Failed to resolve target""#,
        );
        let err = parse(&cut).expect_err("aborted scan");
        assert!(err.to_string().contains("Failed to resolve"), "{err}");
    }

    /// A bare port number cannot say which protocol it means, so 161/udp
    /// reported among the tcp ports becomes a tcp port nothing is listening on.
    /// The two lists are separate for that reason, and each service says which
    /// it belongs to.
    #[test]
    fn never_reports_a_udp_port_as_tcp() {
        let xml = r#"<nmaprun><host><address addr="10.0.0.1" addrtype="ipv4"/><ports>
<port protocol="udp" portid="161"><state state="open"/><service name="snmp" method="probed"/></port>
<port protocol="tcp" portid="80"><state state="open"/><service name="http" method="probed"/></port>
<port protocol="sctp" portid="9"><state state="open"/><service name="discard" method="probed"/></port>
</ports></host><runstats><finished exit="success"/></runstats></nmaprun>"#;
        let hosts = parse(xml).expect("parses");
        assert_eq!(hosts[0].open_ports, vec![80]);
        assert_eq!(hosts[0].open_udp, vec![161]);

        let by_port = |n: u16| {
            hosts[0]
                .services
                .iter()
                .find(|s| s.port == n)
                .map(|s| s.protocol.as_str())
        };
        assert_eq!(by_port(80), Some("tcp"));
        assert_eq!(by_port(161), Some("udp"));
        // Anything that is neither is dropped rather than filed under a
        // protocol it does not belong to.
        assert_eq!(hosts[0].services.len(), 2);
    }

    /// The quiet pass asks which ports are open and nothing else.
    ///
    /// The two wrong ways to do this are both invisible once shipped: skipping those
    /// hosts loses the open-port list for every machine in the fleet, and dropping
    /// individual ports loses whether those ports are open while still talking to
    /// every other one.
    #[test]
    fn the_quiet_pass_asks_for_port_state_only() {
        let scan = DiscoveryScan {
            cidr: "192.168.6.0/24".into(),
            ports: vec![22, 80, 443, 8006],
            udp_ports: vec![],
            site: String::new(),
            use_nmap: true,
            use_scripts: true,
        };

        let full = plan(&scan, &[], true);
        assert!(full.contains(&"22,80,443,8006".to_string()), "{full:?}");
        assert!(full.contains(&"-O".to_string()));
        assert!(full.contains(&"--script".to_string()));

        // Every port, 22 included: without version detection nothing is spoken to,
        // so there is no reason to give up knowing whether the port is open.
        let quiet = plan(&scan, &[], false);
        assert!(quiet.contains(&"22,80,443,8006".to_string()), "{quiet:?}");
        // Port state only. Each of these three is a conversation with the service,
        // and a conversation is the thing that gets logged and penalised.
        assert!(!quiet.contains(&"-sV".to_string()));
        assert!(!quiet.contains(&"-O".to_string()));
        assert!(!quiet.contains(&"--script".to_string()));
    }

    /// The addresses handed over are excluded from the full pass, so a host is
    /// swept by exactly one of the two.
    #[test]
    fn known_addresses_are_excluded_from_the_full_pass() {
        let scan = DiscoveryScan {
            cidr: "192.168.6.0/24".into(),
            ports: vec![22, 80],
            udp_ports: vec![],
            site: String::new(),
            use_nmap: true,
            use_scripts: false,
        };
        // The third is a docker bridge on one of our own machines: a real address
        // of ours, on a network this sweep never touches, and it should not appear
        // in the command.
        let light = vec![
            "192.168.6.43".to_string(),
            "192.168.6.66".to_string(),
            "172.17.0.1".to_string(),
        ];
        let full = plan(&scan, &light, true);
        let at = full.iter().position(|a| a == "--exclude").expect("--exclude");
        assert_eq!(full[at + 1], "192.168.6.43,192.168.6.66");
        // And the quiet pass excludes nothing, or it would sweep no one.
        assert!(!plan(&scan, &[], false).contains(&"--exclude".to_string()));
    }

    /// A printer names itself in an HTTP title and an SNMP description and
    /// nowhere else, so without this it is a row of open ports. The two noise
    /// replies nmap gives when it found nothing are not identifications.
    #[test]
    fn takes_an_identification_from_script_output() {
        let svc = |scripts: Vec<(&str, &str)>| DiscoveredService {
            port: 80,
            name: "http".into(),
            product: String::new(),
            version: String::new(),
            extra: String::new(),
            cpe: Vec::new(),
            protocol: "tcp".into(),
            scripts: scripts
                .into_iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        };

        assert_eq!(
            from_scripts(&[svc(vec![(
                "http-title",
                "Brother ADS-2700W Requested resource was /general/status.html",
            )])]),
            "Brother ADS-2700W"
        );
        // SNMP is preferred over a title, and its uptime tail is dropped.
        assert_eq!(
            from_scripts(&[svc(vec![
                ("http-title", "Login"),
                (
                    "snmp-sysdescr",
                    "Brother NC-07w, Firmware Ver.E System uptime: 155d13h14m53.15s",
                ),
            ])]),
            "Brother NC-07w, Firmware Ver.E"
        );
        assert_eq!(
            from_scripts(&[svc(vec![
                ("http-title", "Site doesn't have a title (text/html)."),
                ("ssl-cert", "Subject: commonName=x"),
            ])]),
            ""
        );
    }

    /// The four fields that answer "what is this" rather than "what is it
    /// running", each of which was previously either thrown away or never
    /// requested: a PTR name, a device class, and script output.
    #[test]
    fn reads_the_identifying_fields() {
        let xml = r#"<nmaprun><host><address addr="10.0.0.5" addrtype="ipv4"/>
<hostnames><hostname name="printer.lan" type="PTR"/></hostnames>
<ports><port protocol="tcp" portid="443"><state state="open"/>
<service name="http" product="nginx" method="probed"/>
<script id="ssl-cert" output="Subject: commonName=nas.lan&#10;Issuer: self"/>
</port></ports>
<os><osmatch name="Linux 5.0" accuracy="96"><osclass type="printer" vendor="Brother" osfamily="embedded" accuracy="96"/></osmatch></os>
<hostscript><script id="snmp-info" output="  enterprise: net-snmp&#10;  engineIDFormat: unknown"/></hostscript>
</host><runstats><finished exit="success"/></runstats></nmaprun>"#;
        let h = &parse(xml).expect("parses")[0];
        assert_eq!(h.identity.hostnames, vec!["printer.lan"]);
        assert_eq!(h.identity.device_type, "printer");
        assert_eq!(h.identity.vendor, "Brother");
        assert_eq!(h.identity.os_family, "embedded");
        // Host-level script output stays with the host, port-level with the
        // port that served it - a box with two web servers has two answers.
        assert_eq!(h.identity.scripts[0].0, "snmp-info");
        assert!(h.identity.scripts[0].1.starts_with("enterprise: net-snmp"));
        let svc = &h.services[0];
        assert_eq!(svc.scripts[0].0, "ssl-cert");
        assert_eq!(svc.scripts[0].1, "Subject: commonName=nas.lan Issuer: self");
    }

    /// nmap escapes attribute values, and a product name really does contain an
    /// ampersand. An undecoded `&amp;` shown to an operator is a small lie
    /// about what the device said.
    #[test]
    fn decodes_escaped_attribute_values() {
        let xml = r#"<nmaprun><host><address addr="10.0.0.1" addrtype="ipv4"/><ports>
<port protocol="tcp" portid="80"><state state="open"/><service name="http"
  product="Cisco IOS http config &amp; admin" version="1.0 &quot;beta&quot;" method="probed"/></port>
</ports></host><runstats><finished exit="success"/></runstats></nmaprun>"#;
        let hosts = parse(xml).expect("parses");
        assert_eq!(hosts[0].services[0].product, "Cisco IOS http config & admin");
        assert_eq!(hosts[0].services[0].version, "1.0 \"beta\"");
    }
}

