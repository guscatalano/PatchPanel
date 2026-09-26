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

/// Longest a version scan may run before we give up on it and sweep instead.
///
/// Minutes rather than seconds: `-Pn` means every address in the range is port
/// scanned whether it answers a ping or not, and `-sV` then holds a
/// conversation with each open port. A /24 of mostly dead addresses is the slow
/// case, and it is also the case the operator most wants an answer for.
const NMAP_TIMEOUT: Duration = Duration::from_secs(600);
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

/// Scan `scan.cidr` with nmap and report what answered.
///
/// The range is expected to have been size-checked by the caller: nmap will
/// cheerfully accept a /8 and spend a week on it.
pub async fn sweep(scan: &DiscoveryScan) -> Result<Vec<DiscoveredHost>> {
    let ports = scan
        .ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");

    // `-Pn` because half the point of this is embedded hardware, and a device
    // that drops ICMP but answers on 502 is exactly the one nobody wrote down.
    // `-n` because nothing here uses a name, and 254 PTR lookups against a
    // field DNS server is the slowest part of the scan. No `-O`: OS
    // fingerprinting needs raw sockets, and the agent should not need root to
    // list its neighbours.
    let args = [
        "-oX", "-",
        "-Pn",
        "-n",
        "--open",
        "-sV",
        "--version-light",
        // OS fingerprinting needs raw sockets, and the agent runs as root on
        // Linux, so it is available here. It costs time and returns a guess
        // rather than a fact, which is why the accuracy is carried alongside
        // the name everywhere it is shown.
        "-O",
        // Report the best matches it has rather than refusing to answer. The
        // accuracy travels with the result, so a weak guess is visible as one -
        // whereas without this a host that is merely unusual gets nothing at
        // all, which reads as "nothing to see".
        "--osscan-guess",
        "-p", &ports,
        &scan.cidr,
    ];

    let run = Command::new("nmap")
        .args(args)
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
            // Nothing here scans UDP, but `open_ports` has no room for a
            // protocol, so a udp port slipping in would be reported as a tcp
            // one and send somebody to the wrong socket.
            b"port" if attr(e, "protocol") == "tcp" => {
                port = attr(e, "portid").parse().ok().map(|number| Port {
                    number,
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
                    p.service = Some(service(e, p.number));
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
    services: Vec<DiscoveredService>,
    identity: pp_proto::HostIdentity,
}

struct Port {
    number: u16,
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
fn service(e: &BytesStart, port: u16) -> DiscoveredService {
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
    }
}

fn close_port(host: &mut Option<Host>, port: Option<Port>) {
    let (Some(h), Some(p)) = (host.as_mut(), port) else {
        return;
    };
    if !p.open {
        return;
    }
    h.open.push(p.number);
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
    // evidence that anything is there.
    if h.ip.is_empty() || h.open.is_empty() {
        return;
    }

    hosts.push(DiscoveredHost {
        ip: h.ip,
        open_ports: h.open,
        // A vendor from the OUI beats a guess from a banner: it is assigned
        // rather than inferred, and it is often the only thing that names a
        // silent device at all.
        hint: {
            let from_services = hint(&h.services);
            if from_services.is_empty() && !h.identity.mac_vendor.is_empty() {
                h.identity.mac_vendor.clone()
            } else {
                from_services
            }
        },
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

    /// `open_ports` carries no protocol, so a udp port reported alongside the
    /// tcp ones becomes a tcp port that nothing is listening on. Nothing here
    /// asks nmap for udp today; the day something does, this is the regression.
    #[test]
    fn never_reports_a_udp_port_as_tcp() {
        let xml = r#"<nmaprun><host><address addr="10.0.0.1" addrtype="ipv4"/><ports>
<port protocol="udp" portid="161"><state state="open"/><service name="snmp" method="probed"/></port>
<port protocol="tcp" portid="80"><state state="open"/><service name="http" method="probed"/></port>
</ports></host><runstats><finished exit="success"/></runstats></nmaprun>"#;
        let hosts = parse(xml).expect("parses");
        assert_eq!(hosts[0].open_ports, vec![80]);
        assert_eq!(hosts[0].services.len(), 1);
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

