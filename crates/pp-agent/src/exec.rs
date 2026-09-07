//! Running external package managers and streaming their output back.
//!
//! Everything PatchPanel changes on a machine ultimately happens through one
//! of these calls, so they all funnel through here: one place that captures
//! output, enforces a timeout, and mirrors progress to the portal.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Where live output goes while a command runs. Detached when the work was
/// started by the agent's own schedule rather than by a portal command.
#[derive(Clone)]
pub struct Progress {
    inner: Option<(Uuid, mpsc::UnboundedSender<(Uuid, String)>)>,
}

impl Progress {
    pub fn detached() -> Self {
        Progress { inner: None }
    }

    pub fn attached(id: Uuid, tx: mpsc::UnboundedSender<(Uuid, String)>) -> Self {
        Progress {
            inner: Some((id, tx)),
        }
    }

    pub fn line(&self, line: &str) {
        if let Some((id, tx)) = &self.inner {
            // A closed channel just means the session dropped; the command
            // itself should still run to completion.
            let _ = tx.send((*id, line.to_string()));
        }
    }
}

pub struct Output {
    pub code: i32,
    pub text: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    /// Fail unless the exit code is 0 or one of `also_ok`. Package managers
    /// love non-zero "informational" codes (`dnf check-update` returns 100
    /// when updates exist), so callers list the ones they expect.
    pub fn require(self, also_ok: &[i32]) -> Result<Output> {
        if self.ok() || also_ok.contains(&self.code) {
            Ok(self)
        } else {
            anyhow::bail!("exited {}: {}", self.code, tail(&self.text, 800))
        }
    }
}

/// Longest any single package operation may run before we give up on it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60 * 45);
/// Cap on captured output so one chatty upgrade cannot blow up memory or the
/// portal's database.
const MAX_CAPTURE: usize = 256 * 1024;

pub async fn run(prog: &str, args: &[&str], progress: &Progress) -> Result<Output> {
    run_with_timeout(prog, args, progress, DEFAULT_TIMEOUT).await
}

pub async fn run_with_timeout(
    prog: &str,
    args: &[&str],
    progress: &Progress,
    timeout: Duration,
) -> Result<Output> {
    tracing::debug!(prog, ?args, "exec");
    progress.line(&format!("$ {} {}", prog, args.join(" ")));

    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Package managers block forever on a prompt if they think a human is
        // watching. These make apt and friends assume nobody is.
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("LC_ALL", "C");
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn `{prog}`"))?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    for (reader, is_err) in [
        (Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Unpin + Send>, false),
        (Box::new(stderr) as Box<dyn tokio::io::AsyncRead + Unpin + Send>, true),
    ] {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = if is_err { format!("stderr: {line}") } else { line };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    let mut text = String::new();
    let collect = async {
        while let Some(line) = rx.recv().await {
            progress.line(&line);
            if text.len() < MAX_CAPTURE {
                text.push_str(&line);
                text.push('\n');
            }
        }
        child.wait().await
    };

    let status = match tokio::time::timeout(timeout, collect).await {
        Ok(status) => status.context("waiting for child")?,
        Err(_) => {
            anyhow::bail!(
                "`{prog}` timed out after {}s; output so far: {}",
                timeout.as_secs(),
                tail(&text, 800)
            );
        }
    };

    Ok(Output {
        // A signal-killed process has no code; -1 marks it as abnormal.
        code: status.code().unwrap_or(-1),
        text,
    })
}

/// True when `prog` resolves on PATH. Used to decide which backends this
/// machine actually has before we try to drive them.
pub fn have(prog: &str) -> bool {
    let (finder, args): (&str, &[&str]) = if cfg!(windows) {
        ("where.exe", &[])
    } else {
        ("sh", &["-c"])
    };
    let mut cmd = std::process::Command::new(finder);
    if cfg!(windows) {
        cmd.arg(prog);
    } else {
        cmd.args(args).arg(format!("command -v {prog}"));
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Keep the last `n` bytes of `s`, on a character boundary.
pub fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut start = s.len() - n;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &s[start..])
}
