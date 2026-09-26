//! Reading what a job's output actually says.
//!
//! A job reports `ok` and PatchPanel believes it, which is fine when the job
//! knows. Often it does not: the Unraid watcher marks a run green because the
//! plugin printed `Script Finished`, and that footer means the script *ended*,
//! not that it *worked*. A DDNS run that failed on two of four sites ends just
//! as cleanly as one that succeeded on all four.
//!
//! So the output gets read. Two things come out of it: values worth putting on
//! the page, and evidence that contradicts a green result. The second is the
//! important one - reporting success that the output plainly denies is the
//! exact failure this whole product exists to avoid.
//!
//! Nothing here overrides what a job said about itself. A job that reports
//! failure is failed; a job that reports success with failure all over its
//! output is *suspect*, which is a third state and is shown as one.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

#[derive(Serialize, Default, Debug)]
pub struct LogScan {
    /// Counts and values the output stated plainly, ready to show.
    pub values: BTreeMap<String, String>,
    /// Lines that look like something went wrong, verbatim and capped.
    pub failures: Vec<String>,
}

/// Tallies of the shape `3 ok, 0 failed, 1 changed`, anywhere in a line.
fn tally() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(\d+)\s+(ok|failed|changed|skipped|error|errors|warning|warnings|success|succeeded)\b").unwrap())
}

/// `key: value` where the key is a word or two and the value is short. Catches
/// `WAN IP: 1.2.3.4` and `DNS now: 1.2.3.4` without swallowing prose.
fn keyed() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*([A-Za-z][A-Za-z0-9 _-]{1,24}?)\s*[:=]\s*(\S[^\n]{0,60})\s*$").unwrap()
    })
}

/// Markers that mean a line is reporting a problem.
///
/// Deliberately narrow. "error" appearing in ordinary prose must not turn a
/// healthy run red, so this wants the shapes people actually use to flag a
/// failure - a bracketed tag, a prefix with a colon, a shell's own complaint.
fn failure_marker(line: &str) -> bool {
    const MARKERS: &[&str] = &[
        "[fail]",
        "[error]",
        "[fatal]",
        "fatal:",
        "error:",
        "failed:",
        "traceback (most recent call last)",
        "command not found",
        "no such file or directory",
        "permission denied",
        "connection refused",
        "segmentation fault",
    ];
    let lower = line.to_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

/// A tally that names a non-zero failure count, like `2 failed` or `1 error`.
fn counts_failures(line: &str) -> bool {
    tally().captures_iter(line).any(|c| {
        let n: u64 = c[1].parse().unwrap_or(0);
        let word = c[2].to_lowercase();
        n > 0 && matches!(word.as_str(), "failed" | "error" | "errors")
    })
}

pub fn scan(log: &str) -> LogScan {
    let mut out = LogScan::default();
    if log.trim().is_empty() {
        return out;
    }

    for line in log.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }

        // Later lines win: a script that prints a running tally should be
        // represented by its final one, not its first.
        for c in tally().captures_iter(line) {
            out.values
                .insert(c[2].to_lowercase(), c[1].to_string());
        }
        if let Some(c) = keyed().captures(line) {
            let key = c[1].trim().to_lowercase().replace(' ', "_");
            let value = c[2].trim();
            // A URL is not a key and a value. `PatchPanel at http://patchpanel`
            // otherwise reads as `patchpanel_at_http = //patchpanel`, which is
            // nonsense presented as a fact - and the watcher prints its own
            // address on every single run.
            //
            // Checked here rather than in the pattern: this crate has no
            // lookaround, by design, so the expression cannot say it.
            let is_url_split = value.starts_with("//");
            // The plugin's own furniture is not a fact about the job.
            if !is_url_split && !matches!(key.as_str(), "script_start" | "script_finished") {
                out.values.insert(key, value.to_string());
            }
        }

        if (failure_marker(line) || counts_failures(line)) && out.failures.len() < 8 {
            out.failures.push(line.trim().chars().take(200).collect());
        }
    }

    // A count of zero failures is the most reassuring line in a log and the
    // least interesting value on a page. Keep the ones that say something.
    out.values.retain(|k, v| {
        !matches!(k.as_str(), "failed" | "error" | "errors" | "warning" | "warnings")
            || v != "0"
    });
    out.values.retain(|_, v| !v.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDNS_OK: &str = "\
===== DDNS run =====
---- seattle -> example.net/12345 (api) ----
  WAN IP: 203.0.113.9
  DNS now: 203.0.113.9
  [ok] unchanged
===== done: 3 ok, 0 failed, 1 changed =====";

    const DDNS_PARTIAL: &str = "\
===== DDNS run =====
  WAN IP: 203.0.113.9
  [FAIL] '' is not a public IP - refusing to update
  [FAIL] Linode PUT HTTP 500
===== done: 2 ok, 2 failed, 0 changed =====";

    #[test]
    fn reads_the_values_a_run_stated() {
        let s = scan(DDNS_OK);
        assert_eq!(s.values.get("ok").map(String::as_str), Some("3"));
        assert_eq!(s.values.get("changed").map(String::as_str), Some("1"));
        assert_eq!(s.values.get("wan_ip").map(String::as_str), Some("203.0.113.9"));
        // Zero failures is true, reassuring, and not worth a line on the page.
        assert!(!s.values.contains_key("failed"));
        assert!(s.failures.is_empty(), "a clean run has nothing to flag");
    }

    /// The case that matters: the script ended cleanly, so the plugin printed
    /// `Script Finished` and the watcher called it green - while the output
    /// says two sites failed.
    #[test]
    fn a_clean_finish_does_not_hide_failures_in_the_output() {
        let s = scan(DDNS_PARTIAL);
        assert_eq!(s.values.get("failed").map(String::as_str), Some("2"));
        assert_eq!(s.failures.len(), 3, "two [FAIL] lines and the tally");
        assert!(s.failures.iter().any(|f| f.contains("Linode PUT HTTP 500")));
    }

    /// A warning that fires on ordinary prose is one people learn to ignore.
    #[test]
    fn prose_about_errors_is_not_evidence_of_one() {
        let s = scan("checking for errors\nno errors were found\n0 failed");
        assert!(
            s.failures.is_empty(),
            "got {:?} - the word alone must not flag a line",
            s.failures
        );
    }

    #[test]
    fn nothing_is_invented_from_an_empty_log() {
        let s = scan("   \n\n");
        assert!(s.values.is_empty() && s.failures.is_empty());
    }
}

/// Apply one operator-written pattern to a log.
///
/// The `regex` crate has no backtracking, so a pattern someone pastes in
/// cannot hang the portal however badly it is written - which is what makes
/// offering this at all reasonable.
///
/// Named groups `key` and `value` together mean "one fact per match, named by
/// what you captured". That is the shape a log listing four sites and four
/// addresses needs, and the built-in scan cannot express it: repeated
/// `WAN IP:` lines collapse onto each other and only the last survives.
/// Any other named group becomes a fact in its own right.
pub fn apply_rule(pattern: &str, log: &str) -> Result<BTreeMap<String, String>, String> {
    // The crate's own message points at the offending character across several
    // lines, and the first line alone is just "regex parse error:", which
    // tells nobody anything. Flatten the useful part onto one line.
    let re = Regex::new(pattern).map_err(|e| {
        let text = e.to_string();
        let useful: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('^'))
            .collect();
        let msg = useful.join(" ");
        if msg.is_empty() {
            "not a valid pattern".to_string()
        } else {
            msg.chars().take(200).collect()
        }
    })?;
    let names: Vec<&str> = re.capture_names().flatten().collect();
    let paired = names.contains(&"key") && names.contains(&"value");

    let mut out = BTreeMap::new();
    for caps in re.captures_iter(log).take(200) {
        if paired {
            let (Some(k), Some(v)) = (caps.name("key"), caps.name("value")) else {
                continue;
            };
            let key = k.as_str().trim().to_lowercase().replace(' ', "_");
            if !key.is_empty() {
                out.insert(key, v.as_str().trim().to_string());
            }
        } else {
            for name in &names {
                if let Some(m) = caps.name(name) {
                    out.insert((*name).to_string(), m.as_str().trim().to_string());
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    const DDNS: &str = "\
---- seattle -> example.net/12345 (api) ----
  WAN IP: 203.0.113.9
  [ok] unchanged
---- millcreek -> example.net/12346 (ssh) ----
  WAN IP: 198.51.100.4
  [ok] updated";

    /// The case the built-in scan cannot do: four sites, four addresses, all
    /// under the same label. Without paired capture they overwrite each other
    /// and only the last one survives.
    #[test]
    fn paired_captures_keep_every_match() {
        let got = apply_rule(
            r"(?s)---- (?P<key>\S+) ->.*?WAN IP: (?P<value>[0-9.]+)",
            DDNS,
        )
        .unwrap();
        assert_eq!(got.get("seattle").map(String::as_str), Some("203.0.113.9"));
        assert_eq!(got.get("millcreek").map(String::as_str), Some("198.51.100.4"));
    }

    #[test]
    fn plain_named_groups_each_become_a_fact() {
        let got = apply_rule(
            r"(?P<files>\d+) dated files, (?P<size>\S+) total",
            "millcreek: pruned 1 old | 90 dated files, 16M total",
        )
        .unwrap();
        assert_eq!(got.get("files").map(String::as_str), Some("90"));
        assert_eq!(got.get("size").map(String::as_str), Some("16M"));
    }

    /// A bad pattern is the operator's typo, not an outage: it has to come
    /// back as a message they can act on rather than a 500.
    #[test]
    fn a_broken_pattern_explains_itself() {
        let err = apply_rule(r"(?P<unclosed", DDNS).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn a_pattern_that_matches_nothing_invents_nothing() {
        assert!(apply_rule(r"(?P<x>nothing-like-this)", DDNS).unwrap().is_empty());
    }
}


/// Patterns that ship with PatchPanel, applied to every job automatically.
///
/// Written against real logs rather than guessed at, and each one is gated on
/// a marker that only appears in the output it understands - so a pattern for
/// an OPNsense backup run cannot fire on a DDNS log and invent values from a
/// coincidental match.
///
/// These exist because asking somebody to write a regex before their dashboard
/// says anything useful is a bad trade. A rule they write themselves still
/// wins, and these are visible on the page rather than hidden, so nothing here
/// is a value whose origin cannot be traced.
pub struct Builtin {
    pub label: &'static str,
    /// The literal that has to appear in the log for this to be tried at all.
    pub when: &'static str,
    pub pattern: &'static str,
}

pub const BUILTINS: &[Builtin] = &[
    // `---- winetown.example.net ----` then, further down, the file it
    // wrote. Paired, so every router keeps its own filename instead of the
    // last one overwriting the rest.
    Builtin {
        label: "backup file per router",
        when: "[ok] saved",
        pattern: r"---- (?P<key>[A-Za-z0-9_.-]+) ----[\s\S]*?\[ok\] saved (?P<value>\S+)",
    },
    // `===== done: 3 ok, 0 failed | 90 dated files, 16M total =====`
    Builtin {
        label: "archive size",
        when: "dated files",
        pattern: r"(?P<dated_files>\d+) dated files, (?P<archive_size>\S+) total",
    },
    // `---- seacastle -> 2921712/40694240 (ssh) ----` then `WAN IP: 1.2.3.4`.
    // The whole reason paired capture exists: one site per stanza, many
    // stanzas per run, and a plain `WAN IP:` reading keeps only the last.
    Builtin {
        label: "public IP per site",
        when: "WAN IP:",
        pattern: r"---- (?P<key>[A-Za-z0-9_.-]+) ->[\s\S]*?WAN IP: (?P<value>[0-9.]+)",
    },
    // The watcher's own summary line.
    Builtin {
        label: "scripts watched",
        when: "script(s) checked",
        pattern: r"(?P<scripts_checked>\d+) script\(s\) checked, (?P<unfinished>\d+) unfinished",
    },
];

/// Which built-in patterns apply to this log, and what they found.
///
/// Returns the label alongside the values so the page can say where a number
/// came from. A figure with no traceable origin is the kind of thing people
/// stop trusting the moment it looks wrong.
pub fn builtin_values(log: &str) -> Vec<(&'static str, BTreeMap<String, String>)> {
    let mut out = Vec::new();
    for b in BUILTINS {
        if !log.contains(b.when) {
            continue;
        }
        match apply_rule(b.pattern, log) {
            Ok(v) if !v.is_empty() => out.push((b.label, v)),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod builtin_tests {
    use super::*;

    // Verbatim from this fleet's own logs, timestamp prefixes and all.
    const BACKUP: &str = "\
04:40:01  ===== OPNsense backup run: 2026-09-14_04-40-01 =====
04:40:01  target dir: /mnt/user/router_backups
04:40:01  ---- winetown.example.net ----
04:40:01    HTTP 200, 364944 bytes
04:40:09    [ok] saved router.winetown.example.net.backup_2026-09-14_04-40-01.xml
04:40:09  ---- seacastle.example.net ----
04:40:10    HTTP 200, 69090 bytes
04:40:10    [ok] saved router.seacastle.example.net.backup_2026-09-14_04-40-01.xml
04:40:11  ---- pruning (keep 30 per router) ----
04:40:11    winetown: pruned 1 old
04:40:11  ===== done: 3 ok, 0 failed | 90 dated files, 16M total =====";

    const DDNS: &str = "\
21:47:04  ---- seacastle -> 2921712/40694240 (ssh) ----
21:47:04    WAN IP: 203.0.113.7
21:47:04    DNS now: 203.0.113.7
21:47:04    [ok] unchanged
21:47:04  ---- millcreek -> 2921712/40694243 (api) ----
21:47:05    WAN IP: 198.51.100.23
21:47:05    DNS now: 198.51.100.23
21:47:05    [ok] unchanged
20:47:05  ===== done: 6 ok, 0 failed, 0 changed =====";

    fn flat(log: &str) -> BTreeMap<String, String> {
        let mut all = BTreeMap::new();
        for (_, v) in builtin_values(log) {
            all.extend(v);
        }
        all
    }

    #[test]
    fn reads_a_real_backup_run() {
        let v = flat(BACKUP);
        assert_eq!(
            v.get("winetown.example.net").map(String::as_str),
            Some("router.winetown.example.net.backup_2026-09-14_04-40-01.xml")
        );
        assert!(v.contains_key("seacastle.example.net"));
        assert_eq!(v.get("dated_files").map(String::as_str), Some("90"));
        assert_eq!(v.get("archive_size").map(String::as_str), Some("16M"));
        // `---- pruning (keep 30 per router) ----` is a section heading, not a
        // router, and must not become one.
        assert!(!v.contains_key("pruning"), "got {v:?}");
    }

    #[test]
    fn reads_a_real_ddns_run() {
        let v = flat(DDNS);
        assert_eq!(v.get("seacastle").map(String::as_str), Some("203.0.113.7"));
        assert_eq!(v.get("millcreek").map(String::as_str), Some("198.51.100.23"));
    }

    /// The gates matter as much as the patterns: a backup pattern loose on a
    /// DDNS log would produce values that look authoritative and are not.
    #[test]
    fn a_pattern_stays_on_the_log_it_understands() {
        let labels: Vec<&str> = builtin_values(DDNS).into_iter().map(|(l, _)| l).collect();
        assert!(labels.contains(&"public IP per site"));
        assert!(!labels.contains(&"backup file per router"));
        assert!(!labels.contains(&"archive size"));
    }

    /// A script that prints nothing is a script nothing can be read from, and
    /// that has to stay true rather than being filled in with guesses.
    #[test]
    fn silent_output_yields_nothing() {
        assert!(builtin_values("Script Starting Sep 16, 2026  08:47.01").is_empty());
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    /// A URL is not a key and a value. The watcher prints its own destination
    /// on every run, and reading it as `patchpanel_at_http = //patchpanel`
    /// puts a meaningless line on the job forever.
    #[test]
    fn a_url_is_not_a_key_value_pair() {
        let s = scan("PatchPanel at http://patchpanel\n  reported unraid/x - ok");
        assert!(
            !s.values.keys().any(|k| k.contains("http")),
            "got {:?}",
            s.values
        );
    }

    /// While a real pair on the same shape still reads.
    #[test]
    fn an_ordinary_pair_still_reads() {
        let s = scan("  WAN IP: 203.0.113.7");
        assert_eq!(s.values.get("wan_ip").map(String::as_str), Some("203.0.113.7"));
    }
}
