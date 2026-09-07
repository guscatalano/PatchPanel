//! Agent configuration and the small amount of state it must survive a
//! restart with.
//!
//! Config is layered: file, then environment, then CLI flags. The file is what
//! a deployment tool writes; the environment is what a container sets; the
//! flags are what a human uses while debugging.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// WebSocket endpoint, e.g. `wss://patchpanel.example.com/api/agent/ws`.
    pub portal_url: String,
    /// Shared secret presented once, to trade for a per-agent token.
    #[serde(default)]
    pub enrollment_token: String,
    /// Collector site. Decides which devices this agent probes.
    #[serde(default)]
    pub site: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            portal_url: "ws://127.0.0.1:8080/api/agent/ws".into(),
            enrollment_token: String::new(),
            site: String::new(),
            state_dir: default_state_dir(),
        }
    }
}

/// Where an agent running as a system service can always write.
pub fn default_state_dir() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
        PathBuf::from(base).join("PatchPanel")
    } else {
        PathBuf::from("/var/lib/patchpanel")
    }
}

pub fn default_config_path() -> PathBuf {
    if cfg!(windows) {
        default_state_dir().join("agent.json")
    } else {
        PathBuf::from("/etc/patchpanel/agent.json")
    }
}

impl Config {
    /// Read `path` if it exists, then let the environment override it.
    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            serde_json::from_str(&text)
                .with_context(|| format!("parsing config {}", path.display()))?
        } else {
            tracing::warn!(path = %path.display(), "no config file; using defaults");
            Config::default()
        };

        if let Ok(v) = std::env::var("PATCHPANEL_PORTAL_URL") {
            cfg.portal_url = v;
        }
        if let Ok(v) = std::env::var("PATCHPANEL_ENROLLMENT_TOKEN") {
            cfg.enrollment_token = v;
        }
        if let Ok(v) = std::env::var("PATCHPANEL_SITE") {
            cfg.site = v;
        }
        if let Ok(v) = std::env::var("PATCHPANEL_STATE_DIR") {
            cfg.state_dir = PathBuf::from(v);
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing config {}", path.display()))
    }
}

/// Identity and progress that must outlive a restart. Written next to the
/// config so a reinstall that preserves the state dir keeps the agent's
/// enrollment rather than creating a duplicate in the portal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub agent_id: Uuid,
    /// Durable credential issued by the portal at first enrollment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_token: Option<String>,
    /// Highest manifest revision this agent has fully applied.
    #[serde(default)]
    pub applied_revision: u64,
}

impl AgentState {
    fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("state.json")
    }

    /// Load existing identity, or mint one on first run.
    pub fn load_or_init(state_dir: &Path) -> Result<Self> {
        let path = Self::path(state_dir);
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            return serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()));
        }

        let state = AgentState {
            agent_id: Uuid::new_v4(),
            agent_token: None,
            applied_revision: 0,
        };
        state.save(state_dir)?;
        tracing::info!(agent_id = %state.agent_id, "minted new agent identity");
        Ok(state)
    }

    /// Write via a temp file and rename, so a crash mid-write cannot leave the
    /// agent with an unparseable identity and no way back into the portal.
    pub fn save(&self, state_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        let path = Self::path(state_dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
