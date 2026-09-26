//! Turning the portal's JSON into something worth reading.
//!
//! A tool that hands back the raw API response is a tool that spends thousands
//! of tokens to answer "is anything wrong". The portal already derives the
//! verdicts - what needs a person, whether a count is trustworthy, whether a
//! pool has a machine in hand - so the job here is to lay those out compactly
//! and, above all, to keep the distinctions the portal was careful to make.
//!
//! In particular: a machine that could not be scanned must never render as a
//! number. That is the one thing this whole product exists to get right, and a
//! summariser is exactly where it would quietly be lost.

use serde_json::Value;

pub fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

pub fn n(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

pub fn b(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub fn arr(v: &Value, key: &str) -> Vec<Value> {
    v.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// How long ago, in the shortest form that is still precise enough to act on.
pub fn ago(iso: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return "unknown".into();
    };
    let mins = chrono::Utc::now()
        .signed_duration_since(then.with_timezone(&chrono::Utc))
        .num_minutes();
    match mins {
        m if m < 0 => "in the future".into(),
        m if m < 60 => format!("{m}m ago"),
        m if m < 60 * 48 => format!("{}h ago", m / 60),
        m => format!("{}d ago", m / (60 * 24)),
    }
}

/// One machine, as a single line.
///
/// The update count is the load-bearing part. `not scanned` is not a small
/// formatting choice - a machine whose backends failed reports zero pending
/// updates, and rendering that as "0" is indistinguishable from a clean
/// machine while being the opposite of true.
pub fn machine_line(m: &Value) -> String {
    let name = s(m, "hostname");
    let updates = if n(m, "scan_issue_count") > 0 {
        "not scanned".to_string()
    } else if n(m, "update_count") == 0 {
        let ignored = n(m, "ignored_count");
        if ignored > 0 {
            format!("none ({ignored} ignored)")
        } else {
            "none".into()
        }
    } else {
        let actionable = n(m, "actionable_count");
        let total = n(m, "update_count");
        let mut out = if actionable == total {
            format!("{actionable}")
        } else {
            format!("{actionable} of {total}")
        };
        if n(m, "security_count") > 0 {
            out.push_str(&format!(" ({} security)", n(m, "security_count")));
        }
        out
    };

    let mut notes = Vec::new();
    if !b(m, "online") {
        notes.push(format!("offline, last seen {}", ago(&s(m, "last_seen"))));
    }
    if b(m, "mid_upgrade") {
        notes.push("part-way through an upgrade".into());
    }
    if n(m, "blocked_count") > 0 {
        notes.push(format!("{} blocked", n(m, "blocked_count")));
    }
    if n(m, "held_back_count") > 0 {
        notes.push(format!("{} need a full upgrade", n(m, "held_back_count")));
    }
    if n(m, "deferred_count") > 0 {
        notes.push(format!("{} phased", n(m, "deferred_count")));
    }
    if b(m, "reboot_required") {
        notes.push("needs a reboot".into());
    }
    if n(m, "release_blockers") > 0 {
        notes.push("apt sources would break an upgrade".into());
    }
    let short = s(m, "patch_short");
    if !short.is_empty() {
        notes.push(short);
    }

    let pool = s(m, "pool");
    format!(
        "- {name} ({os}{arch}) pool={} updates={updates}{}",
        if pool.is_empty() { "none" } else { &pool },
        if notes.is_empty() {
            String::new()
        } else {
            format!(" [{}]", notes.join("; "))
        },
        os = s(m, "os_version"),
        arch = {
            let a = s(m, "arch");
            if a.is_empty() {
                String::new()
            } else {
                format!(" {a}")
            }
        }
    )
}

/// The attention list, which is the portal's own answer to "what needs me".
pub fn attention(fleet: &Value) -> String {
    let items = arr(fleet, "attention");
    if items.is_empty() {
        return "Nothing needs a person right now.".into();
    }
    let label = |tier: i64| match tier {
        1 => "TRUST",
        2 => "FAILED",
        3 => "UNCOVERED",
        _ => "WAITING",
    };
    items
        .iter()
        .map(|i| {
            let who = arr(i, "who")
                .iter()
                .map(|w| format!("{} ({})", s(w, "name"), s(w, "why")))
                .collect::<Vec<_>>()
                .join(", ");
            let tail = s(i, "tail");
            format!(
                "[{}] {}{}{}",
                label(n(i, "tier")),
                s(i, "say"),
                if tail.is_empty() {
                    String::new()
                } else {
                    format!(" - {tail}")
                },
                if who.is_empty() {
                    String::new()
                } else {
                    format!("\n    {who}")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The one-line summary, including the part that explains why the attention
/// list is short when the pending count is large.
pub fn gist(fleet: &Value) -> String {
    let g = fleet.get("gist").cloned().unwrap_or(Value::Null);
    let covered = n(&g, "covered");
    format!(
        "{} machines, {} reporting. {} updates to install, {} security.{}",
        n(&g, "machines"),
        n(&g, "reporting"),
        n(&g, "pending"),
        n(&g, "security"),
        if covered > 0 {
            format!(" {covered} of them belong to a pool that patches on a schedule.")
        } else {
            String::new()
        }
    )
}

/// Percent-encode one path segment or query value.
///
/// Hand-written rather than a dependency: the only things that go through it are
/// a sender name, a job name and a device id, all of which come from the portal
/// itself. What they do contain is spaces and dots - "winetown router",
/// "unraid/backup_routers" - and a raw space in a request line is a 400.
pub fn enc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// One discovered host, as a single line.
///
/// A managed machine is named and nothing more; the unexplained ones get whatever
/// the scan established, because those are the rows somebody is reading this to
/// find. nmap's device class is deliberately left out of the line and kept for
/// the detail: on a real network it calls MoCA adapters printers and iMacs
/// phones, at high confidence, and in a one-line summary that reads as a fact.
pub fn host_line(h: &Value) -> String {
    let ip = s(h, "ip");
    let id = h.get("identity").cloned().unwrap_or(Value::Null);
    let ports = arr(h, "open_ports")
        .iter()
        .filter_map(|p| p.as_i64())
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let udp = arr(h, "open_udp")
        .iter()
        .filter_map(|p| p.as_i64())
        .map(|p| format!("{p}/udp"))
        .collect::<Vec<_>>()
        .join(",");

    let what = match h.get("known") {
        Some(k) => format!(
            "{} ({}{})",
            s(k, "name"),
            s(k, "role"),
            if s(k, "via") == "reverse DNS" {
                ", matched by name"
            } else {
                ""
            }
        ),
        None => {
            let ptr = arr(&id, "hostnames")
                .first()
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let hint = s(h, "hint");
            let mut bits: Vec<String> = Vec::new();
            if !ptr.is_empty() {
                bits.push(ptr.clone());
            }
            if !hint.is_empty() && hint != ptr {
                bits.push(hint);
            }
            if bits.is_empty() {
                "UNEXPLAINED".to_string()
            } else {
                format!("UNEXPLAINED - {}", bits.join(" / "))
            }
        }
    };

    format!(
        "{ip:<16} {what:<52} {}{}{}",
        ports,
        if udp.is_empty() { "" } else { " " },
        udp
    )
}

/// Everything one sweep established about one host, with how it was arrived at.
///
/// Each line says where it came from, because only the hardware vendor is
/// assigned rather than inferred - an OS fingerprint reported as a fact is the
/// kind of thing somebody acts on and then spends an afternoon confused by.
pub fn host_detail(h: &Value) -> String {
    let id = h.get("identity").cloned().unwrap_or(Value::Null);
    let mut out = vec![format!("{}  {}", s(h, "ip"), match h.get("known") {
        Some(k) => format!(
            "{} - {}, matched by {}",
            s(k, "name"),
            s(k, "role"),
            s(k, "via")
        ),
        None => "nothing accounts for this address".to_string(),
    })];

    let ptr = arr(&id, "hostnames");
    if !ptr.is_empty() {
        let names: Vec<String> = ptr
            .iter()
            .map(|x| x.as_str().unwrap_or_default().to_string())
            .collect();
        out.push(format!(
            "  reverse DNS   {}  (a PTR record, so whatever was written down when it was set up)",
            names.join(", ")
        ));
    }
    if !s(&id, "mac_vendor").is_empty() {
        out.push(format!(
            "  vendor        {}  (from the hardware address, assigned not guessed)",
            s(&id, "mac_vendor")
        ));
    }
    if !s(&id, "mac").is_empty() {
        out.push(format!("  hardware      {}", s(&id, "mac")));
    }
    if !s(&id, "os").is_empty() {
        let alts = arr(&id, "os_alternatives")
            .iter()
            .map(|x| x.as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!(
            "  OS guess      {} at {}% confidence{}",
            s(&id, "os"),
            n(&id, "os_accuracy"),
            if alts.is_empty() {
                String::new()
            } else {
                format!("; also considered {alts}")
            }
        ));
    }
    if !s(&id, "device_type").is_empty() {
        out.push(format!(
            "  classed as    {} / {} / {}  (a guess of the same standing as the OS match, and \
             wrong often enough to be worth saying so)",
            s(&id, "device_type"),
            s(&id, "vendor"),
            s(&id, "os_family")
        ));
    }
    if let Some(up) = id.get("uptime_secs").and_then(|v| v.as_i64()) {
        out.push(format!(
            "  uptime        about {} day(s)  (inferred from TCP timestamps, approximate)",
            up / 86_400
        ));
    }
    let cpe = arr(&id, "os_cpe");
    if !cpe.is_empty() {
        let list: Vec<String> = cpe
            .iter()
            .map(|x| x.as_str().unwrap_or_default().to_string())
            .collect();
        out.push(format!("  platform      {}", list.join(" ")));
    }

    out.push(String::new());
    out.push(format!(
        "  ports, found by {}:",
        if s(h, "scanner") == "nmap" {
            "nmap"
        } else {
            "the built-in TCP sweep, which cannot name a service"
        }
    ));
    let svcs = arr(h, "services");
    for p in arr(h, "open_ports").iter().chain(arr(h, "open_udp").iter()) {
        let port = p.as_i64().unwrap_or(0);
        let svc = svcs.iter().find(|x| n(x, "port") == port);
        let named = svc
            .map(|x| {
                [s(x, "name"), s(x, "product"), s(x, "version")]
                    .iter()
                    .filter(|f| !f.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let proto = svc.map(|x| s(x, "protocol")).unwrap_or_default();
        out.push(format!(
            "    {port}{:<5} {}{}",
            if proto == "udp" { "/udp" } else { "" },
            if named.is_empty() {
                "open, nothing identified".to_string()
            } else {
                named
            },
            svc.map(|x| {
                let extra = s(x, "extra");
                if extra.is_empty() {
                    String::new()
                } else {
                    format!("  [{extra}]")
                }
            })
            .unwrap_or_default()
        ));
        if let Some(x) = svc {
            for (k, v) in arr(x, "scripts").iter().filter_map(|p| {
                let pair = p.as_array()?;
                Some((pair.first()?.as_str()?, pair.get(1)?.as_str()?))
            }) {
                out.push(format!("      {k}: {v}"));
            }
        }
    }
    for (k, v) in arr(&id, "scripts").iter().filter_map(|p| {
        let pair = p.as_array()?;
        Some((pair.first()?.as_str()?, pair.get(1)?.as_str()?))
    }) {
        out.push(format!("  {k}: {v}"));
    }
    out.join("\n")
}
