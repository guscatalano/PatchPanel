//! An MCP server for a PatchPanel portal.
//!
//! Speaks JSON-RPC over stdin and stdout, which is what an MCP client spawns
//! and talks to. It is a client of the portal's own HTTP API and holds no state
//! of its own, so it can run anywhere that can reach the portal and nothing
//! here has to be kept in step with the database.
//!
//! **Read-only unless told otherwise.** Every tool here answers a question;
//! none of them change a machine. That is the same call the rest of this
//! project makes - patches are reported until somebody asks - and it matters
//! more here, because the thing invoking these tools is a language model and
//! "apply the manifest to all" is not a sentence anybody should be able to
//! arrive at by accident. `--allow-actions` exists for when that is genuinely
//! wanted, and is a deliberate choice rather than a default.
//!
//! The protocol surface is hand-written rather than pulled from an SDK: three
//! methods and a tool table is not much code, and it does not move underneath
//! a fleet tool that has to keep working.

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::{json, Value};

mod render;
mod tools;

/// What the portal is, and what this server is allowed to ask of it.
pub struct Portal {
    client: reqwest::Client,
    base: String,
    token: Option<String>,
    pub allow_actions: bool,
}

impl Portal {
    pub async fn get(&self, path: &str) -> Result<Value> {
        let mut req = self.client.get(format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("asking the portal for {path}"))?;
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if code == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(
                "the portal rejected the token. Set PATCHPANEL_TOKEN to its admin token, \
                 or run the portal with --no-auth on a trusted network."
            );
        }
        if !code.is_success() {
            anyhow::bail!("the portal answered {code} for {path}: {}", body.trim());
        }
        serde_json::from_str(&body).with_context(|| format!("{path} did not return JSON"))
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    pub async fn put(&self, path: &str, body: Value) -> Result<Value> {
        self.send(reqwest::Method::PUT, path, Some(body)).await
    }

    /// DELETE, which a few of the portal's routes use to mean "undo this" -
    /// clearing a backup exemption, dropping a pool, forgetting a machine.
    pub async fn delete(&self, path: &str, body: Option<Value>) -> Result<Value> {
        self.send(reqwest::Method::DELETE, path, body).await
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self.client.request(method, format!("{}{path}", self.base));
        if let Some(b) = body {
            req = req.json(&b);
        }
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.context("calling the portal")?;
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !code.is_success() {
            anyhow::bail!("the portal answered {code}: {}", text.trim());
        }
        // Several of these routes answer with an empty body, which is a success
        // and not a parse failure.
        Ok(serde_json::from_str(&text).unwrap_or(json!({ "ok": true })))
    }
}

#[derive(Parser)]
#[command(
    name = "pp-mcp",
    about = "Expose a PatchPanel portal to an MCP client, read-only by default"
)]
struct Cli {
    /// The portal's base URL.
    #[arg(long, env = "PATCHPANEL_URL", default_value = "http://localhost:8080")]
    portal: String,
    /// Its admin token, if it requires one.
    #[arg(long, env = "PATCHPANEL_TOKEN")]
    token: Option<String>,
    /// Also offer the tools that change machines.
    ///
    /// Off by default on purpose: everything else here answers questions, and a
    /// patch run or a reboot arrived at by inference is a different kind of
    /// mistake from a wrong answer.
    #[arg(long)]
    allow_actions: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let portal = Portal {
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("pp-mcp/", env!("CARGO_PKG_VERSION")))
            .build()?,
        base: cli.portal.trim_end_matches('/').to_string(),
        token: cli.token,
        allow_actions: cli.allow_actions,
    };

    // Line-delimited JSON-RPC on stdio. Nothing is written to stdout that is
    // not a response: a stray print there corrupts the stream, which is why
    // every diagnostic in this binary goes to stderr.
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("pp-mcp: ignoring unparseable line: {e}");
                continue;
            }
        };

        // A notification has no id and takes no reply - `initialized` is one,
        // and answering it is a protocol error rather than a courtesy.
        let Some(id) = req.get("id").cloned() else {
            continue;
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));

        let response = match handle(&portal, method, params).await {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(e) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": format!("{e:#}") }
            }),
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}

async fn handle(portal: &Portal, method: &str, params: Value) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "patchpanel", "version": env!("CARGO_PKG_VERSION") },
            "instructions": tools::INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::catalogue(portal.allow_actions) })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .context("tools/call needs a name")?;
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match tools::call(portal, name, args).await {
                // A failed tool comes back as content with isError, not as a
                // protocol error: the model should be able to read what went
                // wrong and try something else.
                Ok(text) => Ok(json!({ "content": [{ "type": "text", "text": text }] })),
                Err(e) => Ok(json!({
                    "content": [{ "type": "text", "text": format!("{e:#}") }],
                    "isError": true
                })),
            }
        }
        other => anyhow::bail!("this server does not implement {other}"),
    }
}
