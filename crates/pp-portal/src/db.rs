//! SQLite-backed store.
//!
//! Every write is small and every read is indexed, so the whole store sits
//! behind one mutex rather than a connection pool. That keeps the code honest
//! about ordering — a manifest bump and the revision it hands to agents cannot
//! interleave — and a fleet of a few thousand agents does not come close to
//! saturating it.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use pp_proto::{AgentId, Command, Inventory, Manifest, SystemInfo};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

/// An agent is "online" if it has been connected or heartbeated recently. Three
/// missed 30s heartbeats is a deliberate compromise: long enough to ride out a
/// blip, short enough that a dead box is obvious on the dashboard.
pub const OFFLINE_AFTER_SECS: i64 = 100;

pub struct Db {
    conn: Mutex<Connection>,
}

/// One row of the fleet view.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentRow {
    pub id: AgentId,
    pub hostname: String,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub agent_version: String,
    pub site: String,
    pub backends: Vec<String>,
    pub applied_revision: u64,
    pub reboot_required: bool,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub online: bool,
    /// Static machine facts, absent for agents that predate the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardware: Option<pp_proto::Hardware>,
    /// When the machine last booted, as the agent reported it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_time: Option<DateTime<Utc>>,
    /// Counts derived from the latest inventory, so the fleet list does not
    /// have to ship every package to render.
    pub package_count: usize,
    pub update_count: usize,
    pub security_count: usize,
    pub drift_count: usize,
    pub device_count: usize,
    pub device_problem_count: usize,
    /// Release-readiness blockers, so the fleet list can warn before someone
    /// presses a button that would break the machine.
    pub release_blockers: usize,
    /// Backends that could not be scanned. Non-zero means `update_count` is a
    /// floor, not a total, and must not be shown as "clean".
    pub scan_issue_count: usize,
    /// Upgrades apt will not apply without a full upgrade.
    pub held_back_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandRow {
    pub id: Uuid,
    pub agent_id: AgentId,
    pub kind: String,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub ok: Option<bool>,
    pub summary: String,
    pub detail: String,
    pub progress: String,
}

/// A published agent build, used to drive self-update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBuild {
    pub version: String,
    pub os: String,
    pub arch: String,
    pub url: String,
    pub sha256: String,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;

        // WAL keeps the dashboard's reads from blocking agent writes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;

        let db = Db {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS agents (
                id                TEXT PRIMARY KEY,
                token             TEXT NOT NULL,
                hostname          TEXT NOT NULL DEFAULT '',
                os                TEXT NOT NULL DEFAULT '',
                os_version        TEXT NOT NULL DEFAULT '',
                arch              TEXT NOT NULL DEFAULT '',
                agent_version     TEXT NOT NULL DEFAULT '',
                site              TEXT NOT NULL DEFAULT '',
                backends          TEXT NOT NULL DEFAULT '[]',
                applied_revision  INTEGER NOT NULL DEFAULT 0,
                reboot_required   INTEGER NOT NULL DEFAULT 0,
                first_seen        TEXT NOT NULL,
                last_seen         TEXT NOT NULL,
                inventory         TEXT
            );
            CREATE INDEX IF NOT EXISTS agents_site ON agents(site);

            CREATE TABLE IF NOT EXISTS manifest (
                id  INTEGER PRIMARY KEY CHECK (id = 1),
                doc TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS commands (
                id          TEXT PRIMARY KEY,
                agent_id    TEXT NOT NULL,
                kind        TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                finished_at TEXT,
                ok          INTEGER,
                summary     TEXT NOT NULL DEFAULT '',
                detail      TEXT NOT NULL DEFAULT '',
                progress    TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS commands_agent ON commands(agent_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS agent_builds (
                version TEXT NOT NULL,
                os      TEXT NOT NULL,
                arch    TEXT NOT NULL,
                url     TEXT NOT NULL,
                sha256  TEXT NOT NULL,
                PRIMARY KEY (version, os, arch)
            );
            "#,
        )?;

        // Added after the first release, so existing databases need it grafted
        // on. Checking with a SELECT is simpler than tracking schema versions
        // for a single nullable column.
        for (col, ddl) in [
            ("hardware", "ALTER TABLE agents ADD COLUMN hardware TEXT"),
            ("boot_time", "ALTER TABLE agents ADD COLUMN boot_time TEXT"),
        ] {
            let probe = format!("SELECT {col} FROM agents LIMIT 1");
            if conn.prepare(&probe).is_err() {
                conn.execute(ddl, [])?;
                tracing::info!(column = col, "added agents column");
            }
        }

        // Seed the manifest so agents always receive a valid document.
        let exists: bool = conn
            .query_row("SELECT 1 FROM manifest WHERE id = 1", [], |_| Ok(true))
            .optional()?
            .unwrap_or(false);
        if !exists {
            let doc = serde_json::to_string_pretty(&Manifest::default())?;
            conn.execute("INSERT INTO manifest (id, doc) VALUES (1, ?1)", params![doc])?;
        }
        Ok(())
    }

    // -- enrollment ---------------------------------------------------------

    /// Look up an agent's stored token, if it has enrolled before.
    pub fn agent_token(&self, id: AgentId) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT token FROM agents WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Create or refresh an agent's row on connect. Returns its durable token.
    pub fn upsert_agent(&self, id: AgentId, sys: &SystemInfo, token: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"
            INSERT INTO agents
                (id, token, hostname, os, os_version, arch, agent_version, site,
                 backends, first_seen, last_seen, hardware, boot_time)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                hostname      = excluded.hostname,
                os            = excluded.os,
                os_version    = excluded.os_version,
                arch          = excluded.arch,
                agent_version = excluded.agent_version,
                site          = excluded.site,
                backends      = excluded.backends,
                last_seen     = excluded.last_seen,
                hardware      = excluded.hardware,
                boot_time     = excluded.boot_time
            "#,
            params![
                id.to_string(),
                token,
                sys.hostname,
                sys.os.to_string(),
                sys.os_version,
                sys.arch,
                sys.agent_version,
                sys.site,
                serde_json::to_string(&sys.backends)?,
                now,
                serde_json::to_string(&sys.hardware)?,
                sys.boot_time.map(|t| t.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn touch(&self, id: AgentId, reboot_required: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE agents SET last_seen = ?2, reboot_required = ?3 WHERE id = ?1",
            params![id.to_string(), Utc::now().to_rfc3339(), reboot_required as i64],
        )?;
        Ok(())
    }

    pub fn set_applied_revision(&self, id: AgentId, revision: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE agents SET applied_revision = ?2 WHERE id = ?1",
            params![id.to_string(), revision as i64],
        )?;
        Ok(())
    }

    // -- inventory ----------------------------------------------------------

    pub fn store_inventory(&self, id: AgentId, inv: &Inventory) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE agents SET inventory = ?2, reboot_required = ?3, last_seen = ?4 WHERE id = ?1",
            params![
                id.to_string(),
                serde_json::to_string(inv)?,
                inv.reboot_required as i64,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn inventory(&self, id: AgentId) -> Result<Option<Inventory>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<Option<String>> = conn
            .query_row(
                "SELECT inventory FROM agents WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.flatten().and_then(|t| serde_json::from_str(&t).ok()))
    }

    pub fn agents(&self) -> Result<Vec<AgentRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, hostname, os, os_version, arch, agent_version, site, backends,
                    applied_revision, reboot_required, first_seen, last_seen, inventory,
                    hardware, boot_time
             FROM agents ORDER BY hostname, id",
        )?;
        let cutoff = Utc::now() - Duration::seconds(OFFLINE_AFTER_SECS);

        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let backends: String = r.get(7)?;
            let first_seen: String = r.get(10)?;
            let last_seen: String = r.get(11)?;
            let inventory: Option<String> = r.get(12)?;
            let hardware: Option<String> = r.get(13)?;
            let boot_time: Option<String> = r.get(14)?;

            let last_seen = parse_time(&last_seen);
            let inv: Option<Inventory> =
                inventory.and_then(|t| serde_json::from_str(&t).ok());

            let (packages, updates, security, drift, devices, device_problems) = match &inv {
                Some(i) => (
                    i.packages.len(),
                    i.updates.len(),
                    i.updates.iter().filter(|u| u.security).count(),
                    i.drift.len(),
                    i.devices.len(),
                    i.devices.iter().filter(|d| !d.reachable || d.drift).count(),
                ),
                None => (0, 0, 0, 0, 0, 0),
            };

            Ok(AgentRow {
                id: id.parse().unwrap_or_default(),
                hostname: r.get(1)?,
                os: r.get(2)?,
                os_version: r.get(3)?,
                arch: r.get(4)?,
                agent_version: r.get(5)?,
                site: r.get(6)?,
                backends: serde_json::from_str(&backends).unwrap_or_default(),
                applied_revision: r.get::<_, i64>(8)? as u64,
                reboot_required: r.get::<_, i64>(9)? != 0,
                first_seen: parse_time(&first_seen),
                last_seen,
                online: last_seen > cutoff,
                hardware: hardware.and_then(|t| serde_json::from_str(&t).ok()),
                boot_time: boot_time.as_deref().map(parse_time),
                package_count: packages,
                update_count: updates,
                security_count: security,
                drift_count: drift,
                device_count: devices,
                device_problem_count: device_problems,
                scan_issue_count: inv.as_ref().map(|i| i.scan_issues.len()).unwrap_or(0),
                held_back_count: inv.as_ref().map(|i| i.held_back.len()).unwrap_or(0),
                release_blockers: inv
                    .as_ref()
                    .and_then(|i| i.release.as_ref())
                    .map(|r| r.blockers())
                    .unwrap_or(0),
            })
        })?;

        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete_agent(&self, id: AgentId) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM agents WHERE id = ?1", params![id.to_string()])?;
        conn.execute(
            "DELETE FROM commands WHERE agent_id = ?1",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }

    // -- manifest -----------------------------------------------------------

    pub fn manifest(&self) -> Result<Manifest> {
        let conn = self.conn.lock().unwrap();
        let doc: String = conn.query_row("SELECT doc FROM manifest WHERE id = 1", [], |r| r.get(0))?;
        serde_json::from_str(&doc).context("stored manifest is not valid")
    }

    /// Store a new manifest, forcing the revision to increment. Callers cannot
    /// set it themselves: agents decide whether to reconverge by comparing
    /// revisions, so a manifest that changes without one is invisible to them.
    pub fn put_manifest(&self, mut manifest: Manifest) -> Result<Manifest> {
        let conn = self.conn.lock().unwrap();
        let current: String =
            conn.query_row("SELECT doc FROM manifest WHERE id = 1", [], |r| r.get(0))?;
        let current: Manifest = serde_json::from_str(&current)?;

        manifest.revision = current.revision + 1;
        conn.execute(
            "UPDATE manifest SET doc = ?1 WHERE id = 1",
            params![serde_json::to_string_pretty(&manifest)?],
        )?;
        Ok(manifest)
    }

    // -- commands -----------------------------------------------------------

    pub fn record_command(&self, id: Uuid, agent_id: AgentId, cmd: &Command) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO commands (id, agent_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                id.to_string(),
                agent_id.to_string(),
                command_kind(cmd),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn append_progress(&self, id: Uuid, line: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // Keep only the tail: a full apt upgrade log is not worth unbounded
        // storage, and the dashboard only shows the end of it anyway.
        conn.execute(
            "UPDATE commands
             SET progress = substr(progress || ?2 || char(10), -20000)
             WHERE id = ?1",
            params![id.to_string(), line],
        )?;
        Ok(())
    }

    pub fn finish_command(
        &self,
        id: Uuid,
        ok: bool,
        summary: &str,
        detail: &str,
        finished_at: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE commands SET ok = ?2, summary = ?3, detail = ?4, finished_at = ?5 WHERE id = ?1",
            params![
                id.to_string(),
                ok as i64,
                summary,
                detail,
                finished_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn commands(&self, agent_id: Option<AgentId>, limit: usize) -> Result<Vec<CommandRow>> {
        let conn = self.conn.lock().unwrap();
        let (sql, filter) = match agent_id {
            Some(id) => (
                "SELECT id, agent_id, kind, created_at, finished_at, ok, summary, detail, progress
                 FROM commands WHERE agent_id = ?1 ORDER BY created_at DESC LIMIT ?2",
                Some(id.to_string()),
            ),
            None => (
                "SELECT id, agent_id, kind, created_at, finished_at, ok, summary, detail, progress
                 FROM commands ORDER BY created_at DESC LIMIT ?2",
                None,
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let map = |r: &rusqlite::Row| -> rusqlite::Result<CommandRow> {
            let id: String = r.get(0)?;
            let agent_id: String = r.get(1)?;
            let created_at: String = r.get(3)?;
            let finished_at: Option<String> = r.get(4)?;
            let ok: Option<i64> = r.get(5)?;
            Ok(CommandRow {
                id: id.parse().unwrap_or_default(),
                agent_id: agent_id.parse().unwrap_or_default(),
                kind: r.get(2)?,
                created_at: parse_time(&created_at),
                finished_at: finished_at.as_deref().map(parse_time),
                ok: ok.map(|v| v != 0),
                summary: r.get(6)?,
                detail: r.get(7)?,
                progress: r.get(8)?,
            })
        };

        let rows = match filter {
            Some(id) => stmt
                .query_map(params![id, limit as i64], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt
                .query_map(params![Option::<String>::None, limit as i64], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    // -- agent builds -------------------------------------------------------

    pub fn put_build(&self, b: &AgentBuild) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_builds (version, os, arch, url, sha256) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(version, os, arch) DO UPDATE SET url = excluded.url, sha256 = excluded.sha256",
            params![b.version, b.os, b.arch, b.url, b.sha256],
        )?;
        Ok(())
    }

    pub fn builds(&self) -> Result<Vec<AgentBuild>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT version, os, arch, url, sha256 FROM agent_builds ORDER BY version DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(AgentBuild {
                version: r.get(0)?,
                os: r.get(1)?,
                arch: r.get(2)?,
                url: r.get(3)?,
                sha256: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The build matching a platform, used when the manifest asks for a
    /// version an agent is not yet running.
    pub fn build_for(&self, version: &str, os: &str, arch: &str) -> Result<Option<AgentBuild>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT version, os, arch, url, sha256 FROM agent_builds
                 WHERE version = ?1 AND os = ?2 AND arch = ?3",
                params![version, os, arch],
                |r| {
                    Ok(AgentBuild {
                        version: r.get(0)?,
                        os: r.get(1)?,
                        arch: r.get(2)?,
                        url: r.get(3)?,
                        sha256: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }
}

/// The command's tag, for display and filtering.
pub fn command_kind(cmd: &Command) -> &'static str {
    match cmd {
        Command::CollectInventory => "collect_inventory",
        Command::ApplyManifest => "apply_manifest",
        Command::ApplyPatches { .. } => "apply_patches",
        Command::SelfUpdate { .. } => "self_update",
        Command::Reboot { .. } => "reboot",
        Command::ProbeDevices { .. } => "probe_devices",
        Command::Discover => "discover",
        Command::Cleanup { .. } => "cleanup",
    }
}

/// Timestamps are written by us and always RFC 3339; a corrupt one should not
/// take down the whole listing.
fn parse_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::UNIX_EPOCH)
}
