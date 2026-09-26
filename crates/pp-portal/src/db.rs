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
    /// Updates a run was asked to install and that did not move. This is the
    /// most red-worthy fact the system holds - something tried and failed,
    /// silently - and until now it was invisible until you opened the machine.
    #[serde(default)]
    pub blocked_count: usize,
    /// Updates deliberately set aside. They are subtracted from every other
    /// count, so without this number a machine whose only pending updates are
    /// ignored reads "none", which is the one thing this product must not say.
    #[serde(default)]
    pub ignored_count: usize,
    /// Set when dpkg is stuck part-way through an upgrade: nothing else on
    /// this machine can run until it is finished or rolled back.
    #[serde(default)]
    pub mid_upgrade: bool,
    /// Virtual machines this host runs, and how many of them PatchPanel has
    /// never heard from. A hypervisor is the only place an unmanaged machine
    /// is visible at all.
    /// Which pool this machine is in, and when it was last patched. Both live
    /// on the row so the fleet table can answer "is the schedule working"
    /// without opening thirteen pages.
    #[serde(default)]
    pub pool: String,
    #[serde(default)]
    pub last_patched: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_patch_ok: Option<bool>,
    /// Whether anything is expected of a person about this machine's updates:
    /// `yours` (nothing is scheduled), `scheduled` (a pool has it in hand),
    /// `missed` (a run happened and left it behind), `failed` (the last run
    /// failed), or `clean`.
    #[serde(default)]
    pub patch_state: String,
    /// The human half of that: when the next run is, or what went wrong.
    #[serde(default)]
    pub patch_note: String,
    /// The same thing in a few words, for a table cell. A sentence in a cell
    /// wraps to three lines and makes the row unreadable.
    #[serde(default)]
    pub patch_short: String,
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

/// What someone decided about one guest's backups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRule {
    /// Why, in their words. Worth keeping: "why is that not backed up" is a
    /// question that gets asked six months later, by which time the reason is
    /// the only part anyone needs.
    pub reason: String,
    /// How many days may pass before it counts as a gap. 0 means never count
    /// it at all.
    pub every_days: i64,
    /// Quiet until this moment, then back to normal by itself.
    ///
    /// A snooze that does not expire is just hiding, and hiding is how a
    /// dashboard starts lying. This one has a date on it and the row says
    /// what that date is.
    #[serde(default)]
    pub snooze_until: Option<DateTime<Utc>>,
}

/// The newest version a vendor publishes for a watched application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestVersion {
    pub name: String,
    pub version: String,
    pub checked_at: DateTime<Utc>,
    pub error: Option<String>,
    /// Where this answer came from, so a changed check is refetched at once.
    #[serde(default)]
    pub url: String,
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
    /// Who asked: empty or "manual" for a person, "pool:<name>" for a
    /// scheduled run.
    #[serde(default)]
    pub source: String,
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

/// Something outside PatchPanel that is supposed to happen on a schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub name: String,
    /// How often it is meant to run. 0 means nobody said, so it can be late
    /// but never overdue - the portal will not invent an expectation.
    pub every_hours: i64,
    pub first_seen: DateTime<Utc>,
    pub last_at: Option<DateTime<Utc>>,
    pub last_ok: Option<bool>,
    pub last_detail: String,
    /// What it reported about the world, newest values.
    pub facts: std::collections::BTreeMap<String, String>,
    /// Somebody said they do not want to hear about this one. It still runs,
    /// still reports, and still keeps its history - it simply stops asking
    /// for attention.
    #[serde(default)]
    pub muted: bool,
    /// The last run said it worked, and its own output contradicted it.
    #[serde(default)]
    pub last_suspect: bool,
}

/// One recorded run of an external job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRun {
    pub at: DateTime<Utc>,
    pub ok: bool,
    pub detail: String,
    /// What it printed, as far back as it was willing to send. Empty for runs
    /// old enough to have been pruned, and for jobs that never send one.
    #[serde(default)]
    pub log: String,
    /// Reported ok while its own output reported problems.
    #[serde(default)]
    pub suspect: bool,
}

/// One change to one of a job's facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobFactChange {
    pub key: String,
    pub value: String,
    pub at: DateTime<Utc>,
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

            -- The last reading that actually succeeded.
            --
            -- A probe that fails tells you the device did not answer. It does
            -- not tell you the firewall stopped being end of life, or that its
            -- ninety-one pending updates were installed - but overwriting the
            -- report with the failure said exactly that, and every appliance
            -- card went blank the moment the collector had a bad minute.
            -- Absence of news is not news.
            CREATE TABLE IF NOT EXISTS device_last_good (
                device_id TEXT PRIMARY KEY,
                seen_at   TEXT NOT NULL,
                report    TEXT NOT NULL
            );

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

            -- The newest version a vendor publishes, fetched once for the
            -- whole fleet rather than by every agent that runs the software.
            CREATE TABLE IF NOT EXISTS version_latest (
                name       TEXT PRIMARY KEY,
                version    TEXT NOT NULL DEFAULT '',
                checked_at TEXT NOT NULL,
                error      TEXT,
                -- The URL the answer came from. Editing a check has to take
                -- effect now, not in twelve hours' time when the cache expires
                -- - otherwise correcting a wrong source appears to do nothing.
                url        TEXT NOT NULL DEFAULT ''
            );

            -- Patching policy, and which machines it applies to. A machine
            -- belongs to at most one pool: two rules with different reboot
            -- policies make "why did that restart" unanswerable.
            CREATE TABLE IF NOT EXISTS pools (
                name        TEXT PRIMARY KEY,
                scope       TEXT NOT NULL DEFAULT 'none',
                reboot      TEXT NOT NULL DEFAULT 'never',
                schedule    TEXT NOT NULL DEFAULT '{"kind":"manual"}',
                concurrency INTEGER NOT NULL DEFAULT 1,
                exclude     TEXT NOT NULL DEFAULT '[]',
                last_run    TEXT
            );
            CREATE TABLE IF NOT EXISTS pool_members (
                agent_id TEXT PRIMARY KEY,
                pool     TEXT NOT NULL
            );

            -- How often each guest is actually meant to be backed up.
            --
            -- Started life as a yes/no exemption, which forced a choice
            -- between being nagged on everyone else's schedule and vanishing
            -- from the count entirely. Most guests are neither: a scratch VM
            -- genuinely does not need backing up, but plenty of others just
            -- need it less often than the default, and calling those a gap
            -- every week teaches people to ignore the number - which is the
            -- same failure as not counting them at all.
            --
            -- `every_days` is the window: 0 means do not track this one.
            -- Absent means the fleet default.
            CREATE TABLE IF NOT EXISTS backup_exempt (
                host     TEXT NOT NULL,
                guest    TEXT NOT NULL,
                reason   TEXT NOT NULL DEFAULT '',
                since    TEXT NOT NULL,
                PRIMARY KEY (host, guest)
            );

            -- Work PatchPanel does not do, reported by whatever does.
            --
            -- A cron job that backs up the routers and updates dynamic DNS is
            -- invisible from here, and the failure mode that matters is not
            -- the one it can report: a script that errors can shout, but a
            -- script that stopped running entirely - disabled unit, full disk,
            -- rebuilt box - says nothing at all, and silence reads exactly
            -- like success. `every_hours` is what turns that silence into a
            -- statement, by making an absent check-in a thing the portal can
            -- notice on the job's behalf.
            CREATE TABLE IF NOT EXISTS jobs (
                name        TEXT PRIMARY KEY,
                every_hours INTEGER NOT NULL DEFAULT 0,
                first_seen  TEXT NOT NULL,
                last_at     TEXT,
                last_ok     INTEGER,
                last_detail TEXT NOT NULL DEFAULT '',
                facts       TEXT NOT NULL DEFAULT '{}'
            );

            -- Every check-in, so the calendar can show that it ran on the days
            -- it ran. Unlike a device probe, "nothing changed" is the whole
            -- point here, so these are not deduplicated.
            CREATE TABLE IF NOT EXISTS job_runs (
                name   TEXT NOT NULL,
                at     TEXT NOT NULL,
                ok     INTEGER NOT NULL,
                detail TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (name, at)
            );
            CREATE INDEX IF NOT EXISTS job_runs_by_time ON job_runs (at DESC);

            -- Facts a job reports about the world - a public IP, a record
            -- count. Written only when the value actually moves, so the table
            -- is a history of changes rather than of check-ins.
            CREATE TABLE IF NOT EXISTS job_facts (
                name  TEXT NOT NULL,
                key   TEXT NOT NULL,
                value TEXT NOT NULL,
                at    TEXT NOT NULL,
                PRIMARY KEY (name, key, at)
            );

            -- Which machines have been asked to forward their journal.
            --
            -- Held here rather than on the agent, for the reason that has caught
            -- this project four times: an agent restarts and forgets, and a
            -- portal that reads "off" from that amnesia is confidently wrong.
            -- The portal re-asks on every connection instead, so the switch
            -- survives a restart, a self-update and a reboot without the agent
            -- having to remember anything at all.
            CREATE TABLE IF NOT EXISTS log_forward (
                agent_id     TEXT PRIMARY KEY,
                min_severity TEXT NOT NULL DEFAULT 'warning',
                since        TEXT NOT NULL
            );

            -- How long one sender's lines are kept.
            --
            -- Per sender rather than one global number, because the senders are
            -- not alike: a firewall forwarding its filter log produces a day in
            -- an hour, while a quiet host could keep a week in the same space.
            -- Absent means the default.
            CREATE TABLE IF NOT EXISTS log_retention (
                source TEXT PRIMARY KEY,
                hours  INTEGER NOT NULL
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

        // Same for tables that gained a column after they first shipped.
        // `CREATE TABLE IF NOT EXISTS` leaves an existing table alone, so a
        // database created before the column existed would fail every query
        // that reads it - silently, since the caller treats an error as "no
        // data" and shows nothing.
        for (table, col, ddl) in [
            (
                "version_latest",
                "url",
                "ALTER TABLE version_latest ADD COLUMN url TEXT NOT NULL DEFAULT ''",
            ),
            // Who asked for a command. Without it a scheduled run and a button
            // press are indistinguishable afterwards, which makes "is the
            // schedule actually working" a question nobody can answer.
            (
                "commands",
                "source",
                "ALTER TABLE commands ADD COLUMN source TEXT NOT NULL DEFAULT ''",
            ),
            // How often this guest is meant to be backed up. Existing rows
            // are all "never" decisions, and 0 is exactly that.
            (
                "backup_exempt",
                "every_days",
                "ALTER TABLE backup_exempt ADD COLUMN every_days INTEGER NOT NULL DEFAULT 0",
            ),
            // "I know, tell me later." Different from a cadence, which says
            // how often you want the backup, and different from not tracking
            // it, which says never ask again. This one expires on its own.
            // What the run actually printed. Worth keeping next to the run
            // rather than only the two-line summary: the summary says a backup
            // failed, the log says which router refused and why.
            // Jobs somebody has decided not to be told about. Kept listed and
            // still recorded - a decision, not a disappearance - but out of
            // the attention list and out of the badge.
            (
                "jobs",
                "muted",
                "ALTER TABLE jobs ADD COLUMN muted INTEGER NOT NULL DEFAULT 0",
            ),
            // Reported ok, but the output disagreed. Stored rather than
            // recomputed, so the page and the badge cannot read one run two
            // different ways.
            // Left in place for databases that already have it: dropping a
            // column in SQLite means rebuilding the table, and one nothing
            // reads costs nothing.
            (
                "jobs",
                "rules",
                "ALTER TABLE jobs ADD COLUMN rules TEXT NOT NULL DEFAULT '[]'",
            ),
            (
                "jobs",
                "last_suspect",
                "ALTER TABLE jobs ADD COLUMN last_suspect INTEGER NOT NULL DEFAULT 0",
            ),
            (
                "job_runs",
                "suspect",
                "ALTER TABLE job_runs ADD COLUMN suspect INTEGER NOT NULL DEFAULT 0",
            ),
            (
                "job_runs",
                "log",
                "ALTER TABLE job_runs ADD COLUMN log TEXT NOT NULL DEFAULT ''",
            ),
            (
                "backup_exempt",
                "snooze_until",
                "ALTER TABLE backup_exempt ADD COLUMN snooze_until TEXT NOT NULL DEFAULT ''",
            ),
        ] {
            let probe = format!("SELECT {col} FROM {table} LIMIT 1");
            if conn.prepare(&probe).is_err() {
                conn.execute(ddl, [])?;
                tracing::info!(table, column = col, "added column");
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

    // -----------------------------------------------------------------
    // Pools
    // -----------------------------------------------------------------

    pub fn pools(&self) -> Result<Vec<crate::pools::Pool>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, scope, reboot, schedule, concurrency, exclude, last_run
             FROM pools ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            let schedule: String = r.get(3)?;
            let exclude: String = r.get(5)?;
            let last: Option<String> = r.get(6)?;
            Ok(crate::pools::Pool {
                name: r.get(0)?,
                scope: serde_json::from_value(serde_json::Value::String(r.get(1)?))
                    .unwrap_or(crate::pools::Scope::None),
                reboot: serde_json::from_value(serde_json::Value::String(r.get(2)?))
                    .unwrap_or(crate::pools::RebootPolicy::Never),
                schedule: serde_json::from_str(&schedule)
                    .unwrap_or(crate::pools::Schedule::Manual),
                concurrency: r.get::<_, i64>(4)?.max(1) as usize,
                exclude: serde_json::from_str(&exclude).unwrap_or_default(),
                last_run: last.map(|t| parse_time(&t)),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// What is expected of one guest's backups.
    ///
    /// Absent means the fleet default. `every_days` of 0 means this guest is
    /// not tracked at all - a decision, recorded with its reason, rather than
    /// a guest quietly dropped from the list.
    pub fn backup_rules(&self) -> Result<std::collections::HashMap<String, BackupRule>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT host, guest, reason, every_days, snooze_until FROM backup_exempt")?;
        let rows = stmt.query_map([], |r| {
            let until: String = r.get(4)?;
            Ok((
                format!("{}/{}", r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                BackupRule {
                    reason: r.get(2)?,
                    every_days: r.get(3)?,
                    snooze_until: (!until.is_empty()).then(|| parse_time(&until)),
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?)
    }

    pub fn set_backup_rule(
        &self,
        host: &str,
        guest: &str,
        reason: &str,
        every_days: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // Setting a cadence answers the question a snooze was deferring, so
        // it clears one rather than leaving two rules disagreeing.
        conn.execute(
            "INSERT OR REPLACE INTO backup_exempt
                (host, guest, reason, since, every_days, snooze_until)
             VALUES (?1, ?2, ?3, ?4, ?5, '')",
            params![host, guest, reason, Utc::now().to_rfc3339(), every_days],
        )?;
        Ok(())
    }

    /// Go quiet about this guest until a date, keeping everything else.
    pub fn snooze_backup(&self, host: &str, guest: &str, until: DateTime<Utc>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT reason, every_days FROM backup_exempt WHERE host = ?1 AND guest = ?2",
                params![host, guest],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        // A snoozed guest with no rule of its own is still on the fleet
        // default, not exempt - the default has to be written explicitly or
        // `every_days` of 0 would silently mean "never track this again".
        let (reason, every_days) = existing.unwrap_or_else(|| (String::new(), -1));
        conn.execute(
            "INSERT OR REPLACE INTO backup_exempt
                (host, guest, reason, since, every_days, snooze_until)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                host,
                guest,
                reason,
                Utc::now().to_rfc3339(),
                every_days,
                until.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn clear_backup_rule(&self, host: &str, guest: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM backup_exempt WHERE host = ?1 AND guest = ?2",
            params![host, guest],
        )?)
    }

    /// When each machine last had a patch run, and whether it worked.
    pub fn last_patch_runs(
        &self,
    ) -> Result<std::collections::HashMap<String, (DateTime<Utc>, Option<bool>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT agent_id, MAX(created_at), ok FROM commands
             WHERE kind = 'apply_patches' GROUP BY agent_id",
        )?;
        let rows = stmt.query_map([], |r| {
            let ok: Option<i64> = r.get(2)?;
            Ok((
                r.get::<_, String>(0)?,
                (parse_time(&r.get::<_, String>(1)?), ok.map(|v| v != 0)),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?)
    }

    pub fn put_pool(&self, pool: &crate::pools::Pool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // Keep the existing last_run: editing a policy is not a run.
        conn.execute(
            "INSERT INTO pools (name, scope, reboot, schedule, concurrency, exclude)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name) DO UPDATE SET
                scope = ?2, reboot = ?3, schedule = ?4, concurrency = ?5, exclude = ?6",
            params![
                pool.name,
                serde_json::to_value(pool.scope)?.as_str().unwrap_or("none"),
                serde_json::to_value(pool.reboot)?.as_str().unwrap_or("never"),
                serde_json::to_string(&pool.schedule)?,
                pool.concurrency as i64,
                serde_json::to_string(&pool.exclude)?,
            ],
        )?;
        Ok(())
    }

    pub fn delete_pool(&self, name: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM pool_members WHERE pool = ?1", params![name])?;
        Ok(conn.execute("DELETE FROM pools WHERE name = ?1", params![name])? > 0)
    }

    pub fn set_pool_last_run(&self, name: &str, at: DateTime<Utc>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE pools SET last_run = ?2 WHERE name = ?1",
            params![name, at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn pool_members(&self, name: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT agent_id FROM pool_members WHERE pool = ?1 ORDER BY agent_id")?;
        let rows = stmt.query_map(params![name], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every machine's pool, for showing membership without a query per row.
    pub fn pool_of_each(&self) -> Result<std::collections::HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT agent_id, pool FROM pool_members")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?)
    }

    /// Move a machine into a pool, or out of every pool when `pool` is empty.
    pub fn set_pool_member(&self, agent: AgentId, pool: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        if pool.is_empty() {
            conn.execute(
                "DELETE FROM pool_members WHERE agent_id = ?1",
                params![agent.to_string()],
            )?;
        } else {
            conn.execute(
                "INSERT OR REPLACE INTO pool_members (agent_id, pool) VALUES (?1, ?2)",
                params![agent.to_string(), pool],
            )?;
        }
        Ok(())
    }

    /// Has this machine had a patch run since `since`?
    ///
    /// The command log is the record of what a pool has already done, so there
    /// is no separate run state to fall out of step with reality.
    pub fn patched_since(&self, agent: AgentId, since: DateTime<Utc>) -> Result<bool> {
        self.command_since(agent, since, "apply_patches")
    }

    pub fn rebooted_since(&self, agent: AgentId, since: DateTime<Utc>) -> Result<bool> {
        self.command_since(agent, since, "reboot")
    }

    fn command_since(&self, agent: AgentId, since: DateTime<Utc>, kind: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM commands
             WHERE agent_id = ?1 AND kind = ?2 AND created_at >= ?3",
            params![agent.to_string(), kind, since.to_rfc3339()],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// What the vendor currently publishes for each watched application.
    pub fn latest_versions(&self) -> Result<Vec<LatestVersion>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT name, version, checked_at, error, url FROM version_latest")?;
        let rows = stmt.query_map([], |r| {
            Ok(LatestVersion {
                name: r.get(0)?,
                version: r.get(1)?,
                checked_at: parse_time(&r.get::<_, String>(2)?),
                error: r.get(3)?,
                url: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_latest_version(
        &self,
        name: &str,
        version: &str,
        error: Option<&str>,
        url: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO version_latest (name, version, checked_at, error, url)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, version, Utc::now().to_rfc3339(), error, url],
        )?;
        Ok(())
    }

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
    ///
    /// Returns how many updates it removed. Subtracting silently is what made
    /// an all-ignored machine read "none"; the count has to survive so the row
    /// can say "none - 3 ignored" instead.
    fn hide_ignored(conn: &Connection, id: AgentId, inv: &mut Inventory) -> usize {
        let Ok(ignores) = Self::ignores_with(conn, id) else {
            return 0;
        };
        if ignores.is_empty() {
            return 0;
        }
        let before = inv.updates.len();
        inv.updates.retain(|u| {
            !ignores
                .iter()
                .any(|i| i.name == u.name && i.version == u.new_version)
        });
        inv.blocked
            .retain(|n| !ignores.iter().any(|i| &i.name == n));
        before - inv.updates.len()
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
        if inv.devices.is_empty() || inv.discovered.is_empty() || inv.swept_at.is_none() {
            if let Some(prev) = self.inventory(id)? {
                if inv.devices.is_empty() {
                    inv.devices = prev.devices;
                }
                if inv.discovered.is_empty() {
                    inv.discovered = prev.discovered;
                }
                // An agent that has restarted has not swept yet and reports no
                // time, which is not the same as "never swept" - the hosts it
                // carries forward were found at some point, and dropping the
                // timestamp would present them as ageless.
                if inv.swept_at.is_none() {
                    inv.swept_at = prev.swept_at;
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
    /// Remember a reading that worked, so a later failure has something to
    /// fall back to.
    pub fn record_last_good(&self, report: &pp_proto::DeviceReport) -> Result<()> {
        if !report.reachable {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO device_last_good (device_id, seen_at, report)
             VALUES (?1, ?2, ?3)",
            params![
                report.id,
                report.checked_at.to_rfc3339(),
                serde_json::to_string(report)?
            ],
        )?;
        Ok(())
    }

    /// The last successful reading for every device that has ever had one.
    pub fn last_good_probes(&self) -> Result<std::collections::HashMap<String, pp_proto::DeviceReport>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT device_id, report FROM device_last_good")?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let doc: String = r.get(1)?;
            Ok((id, doc))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, doc) = row?;
            if let Ok(report) = serde_json::from_str::<pp_proto::DeviceReport>(&doc) {
                out.insert(id, report);
            }
        }
        Ok(out)
    }

    /// Forget a device entirely, including what it last looked like.
    /// Drop remembered readings for devices no longer declared.
    pub fn prune_last_good(&self, keep: &[String]) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT device_id FROM device_last_good")?;
        let ids: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        drop(stmt);
        let mut gone = 0;
        for id in ids {
            if !keep.iter().any(|k| k == &id) {
                conn.execute(
                    "DELETE FROM device_last_good WHERE device_id = ?1",
                    params![id],
                )?;
                gone += 1;
            }
        }
        Ok(gone)
    }

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
        conn.execute(
            "UPDATE OR REPLACE device_last_good SET device_id = ?2 WHERE device_id = ?1",
            params![from, to],
        )?;
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
                let _ = Self::hide_ignored(&conn, id, &mut inv);
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
            let mut ignored = 0;
            let inv: Option<Inventory> = inventory
                .and_then(|t| serde_json::from_str::<Inventory>(&t).ok())
                .map(|mut i| {
                    // The fleet counts have to agree with the machine page
                    // about what is pending, so the same filter runs here.
                    if let Ok(agent) = id.parse::<AgentId>() {
                        ignored = Self::hide_ignored(&conn, agent, &mut i);
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
                blocked_count: inv.as_ref().map(|i| i.blocked.len()).unwrap_or(0),
                ignored_count: ignored,
                mid_upgrade: inv.as_ref().is_some_and(|i| i.mid_upgrade.is_some()),
                pool: String::new(),
                last_patched: None,
                last_patch_ok: None,
                patch_state: String::new(),
                patch_note: String::new(),
                patch_short: String::new(),
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

    // -- log forwarding -----------------------------------------------------

    /// How long to keep one sender's lines, where it has been set.
    pub fn log_retention(&self) -> Result<std::collections::HashMap<String, i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT source, hours FROM log_retention")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_log_retention(&self, source: &str, hours: Option<i64>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        match hours {
            Some(h) => conn.execute(
                "INSERT OR REPLACE INTO log_retention (source, hours) VALUES (?1, ?2)",
                params![source, h],
            )?,
            None => conn.execute(
                "DELETE FROM log_retention WHERE source = ?1",
                params![source],
            )?,
        };
        Ok(())
    }

    /// Ask this machine to forward, or stop asking.
    pub fn set_log_forward(&self, id: AgentId, on: bool, min_severity: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        if on {
            conn.execute(
                "INSERT OR REPLACE INTO log_forward (agent_id, min_severity, since)
                 VALUES (?1, ?2, ?3)",
                params![id.to_string(), min_severity, Utc::now().to_rfc3339()],
            )?;
        } else {
            conn.execute(
                "DELETE FROM log_forward WHERE agent_id = ?1",
                params![id.to_string()],
            )?;
        }
        Ok(())
    }

    /// What this machine has been asked for, and when it was asked.
    ///
    /// The timestamp matters: "nothing has arrived" only means something once
    /// enough time has passed for something to have arrived. Without it, a
    /// machine that was switched on ten seconds ago is reported as broken.
    pub fn log_forward(&self, id: AgentId) -> Result<Option<(String, DateTime<Utc>)>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT min_severity, since FROM log_forward WHERE agent_id = ?1",
                params![id.to_string()],
                |r| {
                    let since: String = r.get(1)?;
                    Ok((r.get::<_, String>(0)?, parse_time(&since)))
                },
            )
            .optional()?)
    }

    // -- external jobs ------------------------------------------------------

    /// Record a check-in, returning any facts whose value changed.
    ///
    /// `every_hours` is sticky: a script that reports its cadence once should
    /// not have to repeat it, and a later run that omits it must not quietly
    /// turn overdue detection off.
    pub fn record_job_run(
        &self,
        name: &str,
        ok: bool,
        detail: &str,
        every_hours: Option<i64>,
        facts: &std::collections::BTreeMap<String, String>,
        at: Option<DateTime<Utc>>,
        log: &str,
        suspect: bool,
    ) -> Result<Vec<JobFactChange>> {
        let conn = self.conn.lock().unwrap();
        // `at` is for a reporter speaking on another job's behalf: one cron
        // entry that watches four scripts is reporting runs that happened
        // hours ago, and recording those as "now" would mean a job could never
        // be late as long as the watcher was alive.
        let now = at.unwrap_or_else(Utc::now);
        let now_s = now.to_rfc3339();

        let existing: Option<(i64, String, Option<String>)> = conn
            .query_row(
                "SELECT every_hours, facts, last_at FROM jobs WHERE name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        // A watcher repeats itself - it reports the same run every time it
        // wakes up - so an older observation must never walk the clock back.
        let newest = existing
            .as_ref()
            .and_then(|(_, _, last)| last.as_deref())
            .map(parse_time)
            .is_none_or(|prev| now >= prev);

        let mut merged: std::collections::BTreeMap<String, String> = existing
            .as_ref()
            .and_then(|(_, f, _)| serde_json::from_str(f).ok())
            .unwrap_or_default();
        let every = every_hours
            .filter(|h| *h > 0)
            .or_else(|| existing.as_ref().map(|(h, _, _)| *h))
            .unwrap_or(0);

        let mut changed = Vec::new();
        for (k, v) in facts {
            if merged.get(k).map(String::as_str) != Some(v.as_str()) {
                changed.push(JobFactChange {
                    key: k.clone(),
                    value: v.clone(),
                    at: now,
                });
                conn.execute(
                    "INSERT OR REPLACE INTO job_facts (name, key, value, at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![name, k, v, now_s],
                )?;
                merged.insert(k.clone(), v.clone());
            }
        }

        // An observation older than what is already stored still earns its
        // history row - it is evidence a run happened - but it does not become
        // the job's current state.
        let sql = if newest {
            "INSERT INTO jobs
                (name, every_hours, first_seen, last_at, last_ok, last_detail, facts,
                 last_suspect)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(name) DO UPDATE SET
                every_hours = ?2, last_at = ?3, last_ok = ?4, last_detail = ?5, facts = ?6,
                last_suspect = ?7"
        } else {
            // An observation older than the stored one earns its history row
            // but must not become the job's current state - including its
            // suspicion, which belongs to whichever run is newest.
            "INSERT INTO jobs
                (name, every_hours, first_seen, last_at, last_ok, last_detail, facts,
                 last_suspect)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(name) DO UPDATE SET every_hours = ?2, facts = ?6"
        };
        conn.execute(
            sql,
            params![
                name,
                every,
                now_s,
                ok as i64,
                detail,
                serde_json::to_string(&merged)?,
                suspect as i64
            ],
        )?;
        // Keyed by the run's own time, so a watcher reporting the same run
        // every fifteen minutes overwrites one row instead of accumulating
        // ninety-six copies of the same log.
        conn.execute(
            "INSERT OR REPLACE INTO job_runs (name, at, ok, detail, log, suspect)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![name, now_s, ok as i64, detail, log, suspect as i64],
        )?;
        Ok(changed)
    }

    pub fn jobs(&self) -> Result<Vec<Job>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, every_hours, first_seen, last_at, last_ok, last_detail, facts, muted,
                    last_suspect
             FROM jobs ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            let first_seen: String = r.get(2)?;
            let last_at: Option<String> = r.get(3)?;
            let last_ok: Option<i64> = r.get(4)?;
            let facts: String = r.get(6)?;
            Ok(Job {
                name: r.get(0)?,
                every_hours: r.get(1)?,
                first_seen: parse_time(&first_seen),
                last_at: last_at.as_deref().map(parse_time),
                last_ok: last_ok.map(|v| v != 0),
                last_detail: r.get(5)?,
                facts: serde_json::from_str(&facts).unwrap_or_default(),
                muted: r.get::<_, i64>(7)? != 0,
                last_suspect: r.get::<_, i64>(8)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Check-ins since a point in time, oldest first, for the calendar.
    pub fn job_runs_since(&self, since: DateTime<Utc>) -> Result<Vec<(String, DateTime<Utc>, bool, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, at, ok, detail FROM job_runs WHERE at >= ?1 ORDER BY at ASC",
        )?;
        let rows = stmt.query_map(params![since.to_rfc3339()], |r| {
            let at: String = r.get(1)?;
            let ok: i64 = r.get(2)?;
            Ok((r.get(0)?, parse_time(&at), ok != 0, r.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Recent runs of one job, newest first, with whatever each one printed.
    pub fn job_runs(&self, name: &str, limit: usize) -> Result<Vec<JobRun>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT at, ok, detail, log, suspect FROM job_runs
             WHERE name = ?1 ORDER BY at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![name, limit as i64], |r| {
            let at: String = r.get(0)?;
            let ok: i64 = r.get(1)?;
            Ok(JobRun {
                at: parse_time(&at),
                ok: ok != 0,
                detail: r.get(2)?,
                log: r.get(3)?,
                suspect: r.get::<_, i64>(4)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Drop the text of old runs, keeping the runs themselves.
    ///
    /// The fact that something ran on a Tuesday in March is small and worth
    /// keeping forever; what it printed is neither. Losing the text while
    /// keeping the row means the calendar stays complete.
    pub fn prune_job_logs(&self, older_than_days: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = (Utc::now() - Duration::days(older_than_days)).to_rfc3339();
        Ok(conn.execute(
            "UPDATE job_runs SET log = '' WHERE at < ?1 AND log <> ''",
            params![cutoff],
        )?)
    }

    /// When each of a job's facts last changed, newest first.
    pub fn job_fact_history(&self, name: &str, limit: usize) -> Result<Vec<JobFactChange>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT key, value, at FROM job_facts WHERE name = ?1 ORDER BY at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![name, limit as i64], |r| {
            let at: String = r.get(2)?;
            Ok(JobFactChange {
                key: r.get(0)?,
                value: r.get(1)?,
                at: parse_time(&at),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_job_muted(&self, name: &str, muted: bool) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE jobs SET muted = ?2 WHERE name = ?1",
            params![name, muted as i64],
        )?)
    }

    pub fn forget_job(&self, name: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM job_runs WHERE name = ?1", params![name])?;
        conn.execute("DELETE FROM job_facts WHERE name = ?1", params![name])?;
        Ok(conn.execute("DELETE FROM jobs WHERE name = ?1", params![name])?)
    }

    /// How often a job is allowed to be late before it counts as overdue.
    ///
    /// A quarter past due, and never less than an hour: a daily job that runs
    /// at 03:00 and once at 03:20 is not news, and a warning that fires on
    /// ordinary jitter is one people learn to ignore.
    pub fn job_grace(every_hours: i64) -> Duration {
        Duration::minutes((every_hours * 60 / 4).max(60))
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

    /// Record a dispatched command. `source` is who asked: "manual" for a
    /// person, "pool:<name>" for a scheduled run.
    pub fn record_command(
        &self,
        id: Uuid,
        agent_id: AgentId,
        cmd: &Command,
        source: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO commands (id, agent_id, kind, created_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                agent_id.to_string(),
                command_kind(cmd),
                Utc::now().to_rfc3339(),
                source,
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

    /// Everything that ran since a point in time, oldest first.
    ///
    /// A calendar needs a range rather than a page: "the last 500 commands"
    /// is a different window on every fleet and on every day.
    pub fn commands_since(&self, since: DateTime<Utc>) -> Result<Vec<CommandRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, agent_id, kind, created_at, finished_at, ok, summary, detail,
                    progress, source
             FROM commands WHERE created_at >= ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![since.to_rfc3339()], |r| {
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
                source: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn commands(&self, agent_id: Option<AgentId>, limit: usize) -> Result<Vec<CommandRow>> {
        let conn = self.conn.lock().unwrap();
        let (sql, filter) = match agent_id {
            Some(id) => (
                "SELECT id, agent_id, kind, created_at, finished_at, ok, summary, detail,
                        progress, source
                 FROM commands WHERE agent_id = ?1 ORDER BY created_at DESC LIMIT ?2",
                Some(id.to_string()),
            ),
            None => (
                "SELECT id, agent_id, kind, created_at, finished_at, ok, summary, detail,
                        progress, source
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
                source: r.get(9)?,
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
        // On and off are different actions in a history: "who turned this off"
        // is a question somebody eventually asks.
        Command::JournalVolume => "journal_volume",
        Command::ConfigureSyslog { enable: true, .. } => "syslog_on",
        Command::ConfigureSyslog { .. } => "syslog_off",
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
