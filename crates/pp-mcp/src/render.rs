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
