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
    /// Upgrades nothing will apply, so the pending count can never reach zero.
    pub deferred_count: usize,
    /// Virtual machines this host runs, and how many of them PatchPanel has
    /// never heard from. A hypervisor is the only place an unmanaged machine
    /// is visible at all.
    #[serde(default)]
    pub guest_count: usize,
    #[serde(default)]
    pub unmanaged_guests: usize,
    /// Updates that pressing the button would actually install. This is the
    /// number worth showing: a total that includes updates the archive is
    /// withholding just invites someone to keep clicking a button that
    /// correctly does nothing.
    pub actionable_count: usize,
    /// A command dispatched to this agent that has not reported back yet.
    /// Present means work is in flight and the machine should not be given
    /// more, which is the difference between one patch run and two.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<RunningCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningCommand {
    pub id: Uuid,
    pub kind: String,
    pub started_at: DateTime<Utc>,
}

/// An update somebody decided not to install, at a particular version.
#[derive(Debug, Serialize, Deserialize)]
pub struct IgnoredUpdate {
    pub name: String,
    pub source: String,
    pub version: String,
    pub ignored_at: DateTime<Utc>,
}

/// One remembered probe of a device.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceProbeRow {
    pub checked_at: DateTime<Utc>,
    pub collector: String,
    pub reachable: bool,
    pub firmware: Option<String>,
    /// None when the device could not say, which is not the same as zero.
    pub updates: Option<i64>,
    pub error: Option<String>,
    pub detail: String,
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

            -- One row per probe, so a device has a past and not just a
            -- present. A firewall that was reachable yesterday and is not
            -- today is a different problem from one that never answered, and
            -- the current reading alone cannot tell them apart.
            CREATE TABLE IF NOT EXISTS device_probes (
                device_id  TEXT NOT NULL,
                checked_at TEXT NOT NULL,
                collector  TEXT NOT NULL DEFAULT '',
                reachable  INTEGER NOT NULL DEFAULT 0,
                firmware   TEXT,
                updates    INTEGER,
                error      TEXT,
                detail     TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (device_id, checked_at)
            );
            CREATE INDEX IF NOT EXISTS device_probes_by_time
                ON device_probes (device_id, checked_at DESC);

            -- How long each scan problem has been going on. A mirror that
            -- answers "service unavailable" once and works on the next try is
            -- not a coverage gap, and warning about it teaches people to
            -- ignore the warning that matters.
            CREATE TABLE IF NOT EXISTS scan_issue_seen (
                agent_id   TEXT NOT NULL,
                key        TEXT NOT NULL,
                first_seen TEXT NOT NULL,
                last_seen  TEXT NOT NULL,
                seen_count INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (agent_id, key)
            );

            -- Updates somebody has decided not to install, pinned to the
            -- exact version they decided about. A newer version is a new
            -- decision, so it comes back on its own rather than being hidden
            -- forever by a judgement made about something else.
            CREATE TABLE IF NOT EXISTS update_ignores (
                agent_id   TEXT NOT NULL,
                name       TEXT NOT NULL,
                source     TEXT NOT NULL DEFAULT '',
                version    TEXT NOT NULL,
                ignored_at TEXT NOT NULL,
                PRIMARY KEY (agent_id, name, source, version)
            );

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

    /// Updates this machine's operator has set aside, and at which version.
    pub fn ignores(&self, id: AgentId) -> Result<Vec<IgnoredUpdate>> {
        let conn = self.conn.lock().unwrap();
        Self::ignores_with(&conn, id)
    }

    fn ignores_with(conn: &Connection, id: AgentId) -> Result<Vec<IgnoredUpdate>> {
        let mut stmt = conn.prepare(
            "SELECT name, source, version, ignored_at FROM update_ignores
             WHERE agent_id = ?1 ORDER BY name",
        )?;
        let rows = stmt.query_map(params![id.to_string()], |r| {
            Ok(IgnoredUpdate {
                name: r.get(0)?,
                source: r.get(1)?,
                version: r.get(2)?,
                ignored_at: parse_time(&r.get::<_, String>(3)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn add_ignore(&self, id: AgentId, name: &str, source: &str, version: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO update_ignores (agent_id, name, source, version, ignored_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id.to_string(), name, source, version, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn remove_ignore(&self, id: AgentId, name: &str, version: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM update_ignores WHERE agent_id = ?1 AND name = ?2 AND version = ?3",
            params![id.to_string(), name, version],
        )?)
    }

    /// Drop the updates that have been set aside at exactly this version.
    ///
    /// Matched on the version, not the package: a machine ignoring 4.6.2 is
    /// saying something about 4.6.2, and when 4.7 appears the question is
    /// worth asking again. Applied wherever an inventory is read, so the fleet
    /// counts and the machine page cannot disagree about what is pending.
    fn hide_ignored(conn: &Connection, id: AgentId, inv: &mut Inventory) {
        let Ok(ignores) = Self::ignores_with(conn, id) else {
            return;
        };
        if ignores.is_empty() {
            return;
        }
        inv.updates.retain(|u| {
            !ignores
                .iter()
                .any(|i| i.name == u.name && i.version == u.new_version)
        });
        inv.blocked
            .retain(|n| !ignores.iter().any(|i| &i.name == n));
    }

    /// Which scan problems have lasted long enough to be worth reporting.
    ///
    /// A problem has to survive two scans in a row. Package mirrors return
    /// 503s and time out; an agent that scans every hour would otherwise raise
    /// and clear a coverage warning at random, which is the fastest way to
    /// make people stop reading it. Anything that clears in between starts
    /// counting again from scratch, so a genuinely intermittent source stays
    /// quiet while a broken one is reported and stays reported.
    fn confirm_scan_issues(
        &self,
        conn: &Connection,
        id: AgentId,
        issues: &[pp_proto::ScanIssue],
    ) -> Result<Vec<pp_proto::ScanIssue>> {
        const NEEDED: i64 = 2;
        let now = Utc::now().to_rfc3339();
        let agent = id.to_string();

        let mut keep = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        for issue in issues {
            let key = format!("{}|{}", issue.backend, issue.problem);
            keys.push(key.clone());

            let count: i64 = conn
                .query_row(
                    "SELECT seen_count FROM scan_issue_seen WHERE agent_id = ?1 AND key = ?2",
                    params![agent, key],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            let count = count + 1;

            conn.execute(
                "INSERT INTO scan_issue_seen (agent_id, key, first_seen, last_seen, seen_count)
                 VALUES (?1, ?2, ?3, ?3, ?4)
                 ON CONFLICT(agent_id, key) DO UPDATE SET last_seen = ?3, seen_count = ?4",
                params![agent, key, now, count],
            )?;

            if count >= NEEDED {
                keep.push(issue.clone());
            }
        }

        // Anything that stopped happening forgets its history: the next
        // occurrence is a new problem, not a continuation of an old one.
        let mut stmt = conn.prepare("SELECT key FROM scan_issue_seen WHERE agent_id = ?1")?;
        let stale: Vec<String> = stmt
            .query_map(params![agent], |r| r.get::<_, String>(0))?
            .filter_map(Result::ok)
            .filter(|k| !keys.contains(k))
            .collect();
        drop(stmt);
        for key in stale {
            conn.execute(
                "DELETE FROM scan_issue_seen WHERE agent_id = ?1 AND key = ?2",
                params![agent, key],
            )?;
        }

        Ok(keep)
    }

    /// Record an agent's inventory.
    ///
    /// Device and discovery results are carried forward when the incoming
    /// inventory has none. A restarted agent sends a full inventory before it
    /// has probed anything - it self-updates, comes back, and reports - and
    /// taking that at face value wiped every device off the page after each
    /// deploy. An empty list is "not asked yet", not "nothing is there"; each
    /// report carries its own `checked_at`, so a stale one says so honestly.
    pub fn store_inventory(&self, id: AgentId, inv: &Inventory) -> Result<()> {
        let mut inv = inv.clone();
        if inv.devices.is_empty() || inv.discovered.is_empty() {
            if let Some(prev) = self.inventory(id)? {
                if inv.devices.is_empty() {
                    inv.devices = prev.devices;
                }
                if inv.discovered.is_empty() {
                    inv.discovered = prev.discovered;
                }
            }
        }
        let conn = self.conn.lock().unwrap();

        // Only problems that have persisted are stored, so every path that
        // reads an inventory - the machine page, the fleet counts, the alert
        // tiles - agrees without each having to know the rule.
        inv.scan_issues = self.confirm_scan_issues(&conn, id, &inv.scan_issues)?;
        let inv = &inv;

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

    /// Keep a probe result, unless it repeats the last one.
    ///
    /// Probing runs every few minutes and most results are identical; storing
    /// every one would bury the moments that matter under thousands of rows
    /// saying nothing changed. A row is written when something actually moved
    /// - reachability, version, update count or the error - and otherwise the
    /// existing row's timestamp is left alone as the last time it was seen
    /// this way.
    pub fn record_probe(
        &self,
        collector: &str,
        report: &pp_proto::DeviceReport,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let last: Option<(i64, Option<String>, Option<i64>, Option<String>)> = conn
            .query_row(
                "SELECT reachable, firmware, updates, error FROM device_probes
                 WHERE device_id = ?1 ORDER BY checked_at DESC LIMIT 1",
                params![report.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        let now = (
            report.reachable as i64,
            report.firmware.clone(),
            report.updates_known.then_some(report.updates as i64),
            report.error.clone(),
        );
        if last.as_ref() == Some(&now) {
            return Ok(());
        }

        conn.execute(
            "INSERT OR REPLACE INTO device_probes
                (device_id, checked_at, collector, reachable, firmware, updates, error, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                report.id,
                report.checked_at.to_rfc3339(),
                collector,
                now.0,
                now.1,
                now.2,
                now.3,
                report.detail,
            ],
        )?;
        Ok(())
    }

    /// Move a device's remembered probes to a new id.
    ///
    /// The id is the key everything about a device is filed under, so changing
    /// it without bringing the history along would quietly throw away the
    /// record - which is the one thing a history is for.
    pub fn rename_device(&self, from: &str, to: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE OR REPLACE device_probes SET device_id = ?2 WHERE device_id = ?1",
            params![from, to],
        )?)
    }

    /// A device's probe history, newest first.
    pub fn device_history(&self, id: &str, limit: usize) -> Result<Vec<DeviceProbeRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT checked_at, collector, reachable, firmware, updates, error, detail
             FROM device_probes WHERE device_id = ?1
             ORDER BY checked_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id, limit as i64], |r| {
            Ok(DeviceProbeRow {
                checked_at: parse_time(&r.get::<_, String>(0)?),
                collector: r.get(1)?,
                reachable: r.get::<_, i64>(2)? != 0,
                firmware: r.get(3)?,
                updates: r.get(4)?,
                error: r.get(5)?,
                detail: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
        Ok(raw
            .flatten()
            .and_then(|t| serde_json::from_str::<Inventory>(&t).ok())
            .map(|mut inv| {
                Self::hide_ignored(&conn, id, &mut inv);
                inv
            }))
    }

    /// Commands dispatched but not yet reported on, keyed by agent.
    ///
    /// Only the most recent per agent: an agent runs commands one at a time
    /// from the operator's point of view, and showing the newest is what tells
    /// them whether pressing the button again would duplicate work.
    /// The command this agent is in the middle of, if any.
    pub fn running_for(&self, id: AgentId) -> Result<Option<RunningCommand>> {
        let conn = self.conn.lock().unwrap();
        Ok(Self::running_commands(&conn)?.remove(&id.to_string()))
    }

    fn running_commands(
        conn: &Connection,
    ) -> rusqlite::Result<std::collections::HashMap<String, RunningCommand>> {
        let mut stmt = conn.prepare(
            "SELECT agent_id, id, kind, created_at FROM commands
             WHERE ok IS NULL ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            let agent: String = r.get(0)?;
            let id: String = r.get(1)?;
            let created: String = r.get(3)?;
            Ok((
                agent,
                RunningCommand {
                    id: id.parse().unwrap_or_default(),
                    kind: r.get(2)?,
                    started_at: parse_time(&created),
                },
            ))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (agent, cmd) = row?;
            map.insert(agent, cmd);
        }
        Ok(map)
    }

    /// Close out a disconnected agent's in-flight commands.
    ///
    /// Without this they stay unfinished forever and the machine looks busy
    /// permanently, which would block every future action on it.
    pub fn fail_unfinished(&self, id: AgentId, reason: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE commands SET ok = 0, summary = ?2, finished_at = ?3
             WHERE agent_id = ?1 AND ok IS NULL",
            params![id.to_string(), reason, Utc::now().to_rfc3339()],
        )?;
        Ok(n)
    }

    pub fn agents(&self) -> Result<Vec<AgentRow>> {
        let conn = self.conn.lock().unwrap();
        let running = Self::running_commands(&conn)?;
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
            let inv: Option<Inventory> = inventory
                .and_then(|t| serde_json::from_str::<Inventory>(&t).ok())
                .map(|mut i| {
                    // The fleet counts have to agree with the machine page
                    // about what is pending, so the same filter runs here.
                    if let Ok(agent) = id.parse::<AgentId>() {
                        Self::hide_ignored(&conn, agent, &mut i);
                    }
                    i
                });

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
                deferred_count: inv.as_ref().map(|i| i.deferred.len()).unwrap_or(0),
                guest_count: inv
                    .as_ref()
                    .and_then(|i| i.virt.as_ref())
                    .map(|v| v.guests.len())
                    .unwrap_or(0),
                // Filled in by the caller, which is the only place that knows
                // every hostname; the row itself is built one machine at a time.
                unmanaged_guests: 0,
                actionable_count: {
                    let total = updates;
                    let stuck = inv.as_ref().map(|i| i.deferred.len()).unwrap_or(0);
                    total.saturating_sub(stuck)
                },
                running: running.get(&id).cloned(),
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
        Command::InstallPrerequisites => "install_prerequisites",
        Command::RestartAgent => "restart_agent",
        Command::WriteSource { .. } => "write_source",
        Command::RemoveSource { .. } => "remove_source",
        Command::Cleanup { .. } => "cleanup",
        Command::FinishUpgrade { .. } => "finish_upgrade",
        Command::UpdateFirmware { .. } => "update_firmware",
        Command::DistroUpgrade { check, .. } => {
            // A readiness check changes nothing, so it does not belong in the
            // same bucket as the upgrade itself when reading a history.
            if *check { "distro_check" } else { "distro_upgrade" }
        }
    }
}

/// Timestamps are written by us and always RFC 3339; a corrupt one should not
/// take down the whole listing.
fn parse_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::UNIX_EPOCH)
}
