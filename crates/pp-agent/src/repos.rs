//! Which package repositories a machine is configured to trust.
//!
//! This is usually the real answer to "why does that box look different" — a
//! host pinned to an old suite, or carrying a third-party repo nobody
//! remembers adding, will never converge on the rest of the fleet no matter
//! how many times you patch it.

use pp_proto::Repository;

pub fn collect() -> Vec<Repository> {
    #[cfg(target_os = "linux")]
    return linux();
    #[cfg(windows)]
    return windows();
    #[cfg(not(any(target_os = "linux", windows)))]
    return Vec::new();
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn linux() -> Vec<Repository> {
    let mut out = Vec::new();
    out.extend(apt_sources());
    out.extend(dnf_repos());
    out
}

/// apt understands two formats: the classic one-line `sources.list` syntax and
/// the newer deb822 `.sources` files. Debian 12+ ships the latter by default,
/// so reading only the old one would report nothing on a modern install.
#[cfg(target_os = "linux")]
fn apt_sources() -> Vec<Repository> {
    let mut out = Vec::new();
    let mut files: Vec<std::path::PathBuf> = Vec::new();

    let list = std::path::Path::new("/etc/apt/sources.list");
    if list.exists() {
        files.push(list.to_path_buf());
    }
    if let Ok(dir) = std::fs::read_dir("/etc/apt/sources.list.d") {
        // apt reads only `.list` and `.sources` here. Everything else in the
        // directory - our own `.patchpanel-bak` and `.pre-<release>` copies
        // included - is inert, and reporting it as a configured repository
        // invents problems that do not exist.
        files.extend(
            dir.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|e| e.to_str()),
                        Some("list") | Some("sources")
                    )
                }),
        );
    }

    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = path.display().to_string();
        match path.extension().and_then(|e| e.to_str()) {
            Some("sources") => out.extend(parse_deb822(&text, &name)),
            _ => out.extend(parse_one_line(&text, &name)),
        }
    }
    out
}

/// `deb [opts] URI SUITE COMPONENT...`
#[cfg(target_os = "linux")]
fn parse_one_line(text: &str, file: &str) -> Vec<Repository> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        // A commented-out entry is still worth reporting: "this repo is here
        // but switched off" is a meaningful difference between machines.
        let (enabled, line) = match line.strip_prefix('#') {
            Some(rest) => (false, rest.trim()),
            None => (true, line),
        };
        if !(line.starts_with("deb ") || line.starts_with("deb-src ")) {
            continue;
        }

        let mut fields = line.split_whitespace();
        let kind = fields.next().unwrap_or_default();
        // Skip a bracketed options group like [arch=amd64 signed-by=...].
        let mut first = fields.next().unwrap_or_default().to_string();
        if first.starts_with('[') {
            while !first.ends_with(']') {
                match fields.next() {
                    Some(f) => first = f.to_string(),
                    None => break,
                }
            }
            first = fields.next().unwrap_or_default().to_string();
        }
        if first.is_empty() {
            continue;
        }

        out.push(Repository {
            problem: None,
            source: if kind == "deb-src" { "apt-src".into() } else { "apt".into() },
            uri: first,
            suite: fields.next().unwrap_or_default().to_string(),
            components: fields.map(str::to_string).collect(),
            enabled,
            origin_file: file.to_string(),
        });
    }
    out
}

/// deb822: stanzas of `Key: value`, separated by blank lines.
#[cfg(target_os = "linux")]
fn parse_deb822(text: &str, file: &str) -> Vec<Repository> {
    let mut out = Vec::new();
    for stanza in text.split("\n\n") {
        let mut types = String::new();
        let mut uris = String::new();
        let mut suites = String::new();
        let mut comps = String::new();
        let mut enabled = true;

        for line in stanza.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim().to_string();
            match key.trim().to_ascii_lowercase().as_str() {
                "types" => types = value,
                "uris" => uris = value,
                "suites" => suites = value,
                "components" => comps = value,
                "enabled" => enabled = !value.eq_ignore_ascii_case("no"),
                _ => {}
            }
        }

        if uris.is_empty() {
            continue;
        }
        let is_src = types.split_whitespace().all(|t| t == "deb-src");
        // A stanza may list several URIs and suites; expand to one row each so
        // the dashboard can diff them.
        for uri in uris.split_whitespace() {
            for suite in suites.split_whitespace() {
                out.push(Repository {
            problem: None,
                    source: if is_src { "apt-src".into() } else { "apt".into() },
                    uri: uri.to_string(),
                    suite: suite.to_string(),
                    components: comps.split_whitespace().map(str::to_string).collect(),
                    enabled,
                    origin_file: file.to_string(),
                });
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn dnf_repos() -> Vec<Repository> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/etc/yum.repos.d") else {
        return out;
    };
    for entry in dir.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = path.display().to_string();
        let (mut id, mut url, mut enabled) = (String::new(), String::new(), true);

        let flush = |out: &mut Vec<Repository>, id: &str, url: &str, enabled: bool, file: &str| {
            if !id.is_empty() {
                out.push(Repository {
            problem: None,
                    source: "dnf".into(),
                    uri: if url.is_empty() { id.to_string() } else { url.to_string() },
                    suite: id.to_string(),
                    components: Vec::new(),
                    enabled,
                    origin_file: file.to_string(),
                });
            }
        };

        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                flush(&mut out, &id, &url, enabled, &file);
                id = line.trim_matches(['[', ']']).to_string();
                url.clear();
                enabled = true;
            } else if let Some(v) = line.strip_prefix("baseurl=") {
                url = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("metalink=") {
                if url.is_empty() {
                    url = v.trim().to_string();
                }
            } else if let Some(v) = line.strip_prefix("enabled=") {
                enabled = v.trim() == "1";
            }
        }
        flush(&mut out, &id, &url, enabled, &file);
    }
    out
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn windows() -> Vec<Repository> {
    let out = std::process::Command::new("winget.exe")
        .args(["source", "list", "--disable-interactivity"])
        .stdin(std::process::Stdio::null())
        .output();

    let Ok(out) = out else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);

    // `winget source list` is a two-column table: Name then Argument (the URL).
    let mut repos = Vec::new();
    let mut seen_header = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.chars().all(|c| c == '-') {
            continue;
        }
        if !seen_header {
            if t.starts_with("Name") {
                seen_header = true;
            }
            continue;
        }
        let mut parts = t.split_whitespace();
        let (Some(name), Some(url)) = (parts.next(), parts.next()) else {
            continue;
        };
        repos.push(Repository {
            problem: None,
            source: "winget".into(),
            uri: url.to_string(),
            suite: name.to_string(),
            components: Vec::new(),
            enabled: true,
            origin_file: "winget source list".into(),
        });
    }
    repos
}
