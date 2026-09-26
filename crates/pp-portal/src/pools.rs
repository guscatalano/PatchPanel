//! Patching pools: which machines get patched, how, and when.
//!
//! A pool is a policy group, deliberately not the same thing as a site. A site
//! says which collector can reach which network; a pool says "these boxes get
//! everything nightly and may reboot, those get security fixes only and never
//! reboot, and that lot nobody touches".
//!
//! Membership is explicit and a machine belongs to at most one pool. A machine
//! matching two rules with different reboot policies is unanswerable when
//! somebody asks why their server restarted, and that question always comes.
//!
//! Scheduling lives here rather than in the agent because the portal is the
//! only place that can stagger a pool, refuse to start a second command on a
//! busy machine, and record what it did with an id somebody can quote back.

use chrono::{DateTime, Datelike, Duration, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::state::SharedState;

/// How much of what is pending a pool installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Everything installable.
    All,
    /// Only what the distribution marks as a security fix.
    Security,
    /// Nothing at all. No command is dispatched: a pool that is meant to be
    /// left alone must be genuinely inert, not "dispatched and hoping it is a
    /// no-op".
    None,
}

/// What to do about a machine that wants a reboot afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RebootPolicy {
    /// Install, leave the flag set, and let it show on the dashboard.
    Never,
    /// Reboot once the patch run has finished and the machine says it needs
    /// one.
    IfNeeded,
}

/// When a pool runs by itself, if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Schedule {
    /// Only when somebody presses the button.
    Manual,
    Daily { hour: u32, minute: u32 },
    /// `dow` is 0 for Monday, matching how people say "weekly on Sunday".
    Weekly { dow: u32, hour: u32, minute: u32 },
}

impl Schedule {
    /// The first moment at or after `from` that this schedule fires.
    ///
    /// Local time, because "03:00" means three in the morning where the
    /// machines are, not wherever UTC happens to be.
    pub fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        let at = |d: DateTime<Local>, h: u32, m: u32| -> Option<DateTime<Local>> {
            d.with_hour(h)?
                .with_minute(m)?
                .with_second(0)?
                .with_nanosecond(0)
        };
        match *self {
            Schedule::Manual => None,
            Schedule::Daily { hour, minute } => {
                let today = at(from, hour, minute)?;
                Some(if today > from {
                    today
                } else {
                    at(from + Duration::days(1), hour, minute)?
                })
            }
            Schedule::Weekly { dow, hour, minute } => {
                // Walk forward a day at a time. Eight iterations at most, and
                // it cannot get daylight-saving arithmetic wrong the way
                // adding seven days in one jump can.
                for ahead in 0..8 {
                    let day = from + Duration::days(ahead);
                    if day.weekday().num_days_from_monday() != dow {
                        continue;
                    }
                    let candidate = at(day, hour, minute)?;
                    if candidate > from {
                        return Some(candidate);
                    }
                }
                None
            }
        }
    }
}

/// A patching policy, and the machines it applies to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pool {
    pub name: String,
    pub scope: Scope,
    pub reboot: RebootPolicy,
    pub schedule: Schedule,
    /// How many machines in this pool may be patched at once. One by default:
    /// a bad update should not be able to take out a whole pool at 03:00.
    pub concurrency: usize,
    /// Never installed on this pool's machines, even by an explicit run.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// When this pool last started a run.
    #[serde(default)]
    pub last_run: Option<DateTime<Utc>>,
}

impl Default for Pool {
    fn default() -> Self {
        // Inert until somebody says otherwise. A pool that patches by default
        // would make creating one a hazard.
        Pool {
            name: String::new(),
            scope: Scope::None,
            reboot: RebootPolicy::Never,
            schedule: Schedule::Manual,
            concurrency: 1,
            exclude: Vec::new(),
            last_run: None,
        }
    }
}

/// How long after its start time a run may still pick up machines it has not
/// reached yet, given it only dispatches `concurrency` at a time.
const RUN_WINDOW: i64 = 4;

/// Is a run that started at `started` still allowed to pick up machines?
///
/// A pool dispatches `concurrency` at a time, so a run is a window rather than
/// an instant: it keeps going until everything has been reached or the window
/// closes.
fn still_working(started: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let elapsed = now.signed_duration_since(started);
    elapsed >= Duration::zero() && elapsed < Duration::hours(RUN_WINDOW)
}

/// What a pool would do to one machine right now, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Planned {
    pub agent: String,
    pub hostname: String,
    pub action: String,
    pub detail: String,
}

/// Work out what a pool would do, without doing any of it.
///
/// Used both by the scheduler and by the preview in the UI - the same code, so
/// what the page promises and what happens at three in the morning cannot
/// drift apart.
pub fn plan(state: &SharedState, pool: &Pool) -> anyhow::Result<Vec<Planned>> {
    let members = state.db.pool_members(&pool.name)?;
    let rows = state.db.agents()?;
    let mut out = Vec::new();

    for id in members {
        let Some(row) = rows.iter().find(|a| a.id.to_string() == id) else {
            continue;
        };
        let say = |action: &str, detail: String| Planned {
            agent: id.clone(),
            hostname: row.hostname.clone(),
            action: action.to_string(),
            detail,
        };

        if pool.scope == Scope::None {
            out.push(say("nothing", "this pool does not patch".into()));
            continue;
        }
        if !row.online {
            out.push(say("skip", "offline".into()));
            continue;
        }
        if row.running.is_some() {
            out.push(say("wait", "something is already running on it".into()));
            continue;
        }

        // A machine part-way through an upgrade cannot install anything, and
        // dispatching to it just produces a failure somebody has to read.
        let stuck = state
            .db
            .inventory(row.id)?
            .and_then(|i| i.mid_upgrade)
            .is_some_and(|m| !m.packages.is_empty());
        if stuck {
            out.push(say("skip", "part-way through an upgrade".into()));
            continue;
        }

        let count = if pool.scope == Scope::Security {
            row.security_count
        } else {
            row.actionable_count
        };
        if count == 0 {
            out.push(say(
                "nothing",
                "nothing installable pending".into(),
            ));
            continue;
        }

        let reboot = if pool.reboot == RebootPolicy::IfNeeded {
            ", then reboot if it asks for one"
        } else {
            ", no reboot"
        };
        out.push(say(
            "patch",
            format!(
                "install {count} {}update(s){reboot}",
                if pool.scope == Scope::Security { "security " } else { "" }
            ),
        ));
    }
    Ok(out)
}

/// Dispatch a pool's work immediately, for the "run now" button.
pub async fn run_now(state: &SharedState, name: &str) -> anyhow::Result<usize> {
    let Some(pool) = state.db.pools()?.into_iter().find(|p| p.name == name) else {
        anyhow::bail!("no such pool");
    };
    let before = plan(state, &pool)?
        .iter()
        .filter(|p| p.action == "patch")
        .count();
    dispatch(state, &pool).await?;
    Ok(before.min(pool.concurrency))
}

/// Run the schedule for as long as the portal is up.
pub fn spawn(state: SharedState) {
    tokio::spawn(async move {
        loop {
            // A minute is fine: schedules are to the minute, and a tick that
            // does nothing costs one query.
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if let Err(e) = tick(&state).await {
                tracing::warn!(error = %format!("{e:#}"), "pool scheduler tick failed");
            }
        }
    });
}

/// How long a run's output is kept. The run itself is kept forever - it is a
/// date and a verdict - but its text is only useful while somebody might still
/// go and read it.
const LOG_RETENTION_DAYS: i64 = 30;

async fn tick(state: &SharedState) -> anyhow::Result<()> {
    let now = Local::now();

    // Cheap and idempotent, so it rides along with the scheduler rather than
    // earning a loop of its own.
    if let Ok(n) = state.db.prune_job_logs(LOG_RETENTION_DAYS) {
        if n > 0 {
            tracing::debug!(runs = n, "pruned old job output");
        }
    }

    // Device syslog is a rolling day, trimmed here for the same reason: cheap,
    // idempotent, and not worth a loop of its own.
    if state.syslog_on {
        let per_source = state.db.log_retention().unwrap_or_default();
        match crate::syslog::prune(&state.log_dir, &per_source) {
            Ok(n) if n > 0 => tracing::debug!(files = n, "trimmed device logs"),
            Err(e) => tracing::warn!(error = %e, "could not trim device logs"),
            _ => {}
        }
    }

    for mut pool in state.db.pools()? {
        if pool.scope == Scope::None || pool.schedule == Schedule::Manual {
            continue;
        }

        // Due when the previous run's next occurrence has passed. A pool that
        // has never run gets its first slot from now, so creating one at 09:00
        // with a 03:00 schedule does not immediately fire.
        let since = pool
            .last_run
            .map(|t| t.with_timezone(&Local))
            .unwrap_or(now - Duration::minutes(1));
        let Some(due) = pool.schedule.next_after(since) else {
            continue;
        };

        // Still working through the machines this run has not reached yet.
        //
        // This used to also require the start to be at or after `due`, which
        // can never hold: `due` is derived from `last_run`, so starting a run
        // pushes `due` a whole period into the future. The effect was that a
        // pool dispatched on exactly one tick - one machine, at concurrency 1 -
        // and the rest waited for the next schedule.
        let in_window = pool.last_run.is_some_and(|t| still_working(t, Utc::now()));

        if now >= due {
            tracing::info!(pool = %pool.name, "pool run starting");
            state.db.set_pool_last_run(&pool.name, Utc::now())?;
            pool.last_run = Some(Utc::now());
        } else if !in_window {
            continue;
        }

        dispatch(state, &pool).await?;
    }
    Ok(())
}

/// Send out as much of a pool's work as its concurrency allows.
async fn dispatch(state: &SharedState, pool: &Pool) -> anyhow::Result<()> {
    let rows = state.db.agents()?;
    let members = state.db.pool_members(&pool.name)?;

    let busy = members
        .iter()
        .filter(|id| {
            rows.iter()
                .find(|a| a.id.to_string() == **id)
                .is_some_and(|a| a.running.is_some())
        })
        .count();
    let mut slots = pool.concurrency.saturating_sub(busy);

    for planned in plan(state, pool)? {
        if slots == 0 {
            break;
        }
        if planned.action != "patch" {
            continue;
        }
        let Ok(agent) = planned.agent.parse() else {
            continue;
        };

        // Not patched already in this run: the command log is the record, and
        // deriving from it means no separate run-state to fall out of step.
        if state.db.patched_since(agent, pool.last_run.unwrap_or_else(Utc::now))? {
            continue;
        }

        let command = pp_proto::Command::ApplyPatches {
            security_only: pool.scope == Scope::Security,
            only: Vec::new(),
            full: false,
        };
        match crate::api::dispatch_now(state, agent, command, &format!("pool:{}", pool.name)) {
            Ok(id) => {
                tracing::info!(pool = %pool.name, host = %planned.hostname, command = %id,
                    "pool dispatched a patch run");
                slots -= 1;
            }
            Err(e) => tracing::warn!(pool = %pool.name, host = %planned.hostname,
                error = %format!("{e:#}"), "pool dispatch failed"),
        }
    }

    // Reboots come after, and only for machines this run actually patched.
    if pool.reboot == RebootPolicy::IfNeeded {
        for id in &members {
            let Ok(agent) = id.parse() else { continue };
            let Some(row) = rows.iter().find(|a| a.id == agent) else {
                continue;
            };
            let since = pool.last_run.unwrap_or_else(Utc::now);
            if !row.reboot_required
                || row.running.is_some()
                || !state.db.patched_since(agent, since)?
                || state.db.rebooted_since(agent, since)?
            {
                continue;
            }
            match crate::api::dispatch_now(
                state,
                agent,
                pp_proto::Command::Reboot { delay_secs: 60 },
                &format!("pool:{}", pool.name),
            )
            {
                Ok(cmd) => tracing::info!(pool = %pool.name, host = %row.hostname, command = %cmd,
                    "pool rebooting a machine that asked for one"),
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "pool reboot failed"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn daily_rolls_to_tomorrow_once_the_time_has_passed() {
        let s = Schedule::Daily { hour: 3, minute: 0 };
        // Before the hour: today.
        let next = s.next_after(local(2026, 9, 8, 1, 0)).unwrap();
        assert_eq!((next.day(), next.hour()), (8, 3));
        // After it: tomorrow, not immediately again.
        let next = s.next_after(local(2026, 9, 8, 5, 0)).unwrap();
        assert_eq!((next.day(), next.hour()), (9, 3));
    }

    #[test]
    fn weekly_finds_the_right_day() {
        // 2026-09-08 is a Tuesday; Sunday is 6 days from Monday.
        let s = Schedule::Weekly { dow: 6, hour: 4, minute: 30 };
        let next = s.next_after(local(2026, 9, 8, 12, 0)).unwrap();
        assert_eq!(next.weekday().num_days_from_monday(), 6);
        assert_eq!((next.hour(), next.minute()), (4, 30));
        // And it is this week's Sunday, not next week's.
        assert_eq!(next.day(), 13);
    }

    #[test]
    fn a_manual_pool_never_comes_due() {
        assert!(Schedule::Manual.next_after(local(2026, 9, 8, 3, 0)).is_none());
    }

    /// The bug this replaced: a run that had started could not continue,
    /// because the test compared it against a deadline that the run itself had
    /// just pushed a week into the future.
    #[test]
    fn a_run_keeps_going_until_its_window_closes() {
        let started = Utc::now() - Duration::hours(1);
        assert!(still_working(started, Utc::now()));

        let old = Utc::now() - Duration::hours(RUN_WINDOW + 1);
        assert!(!still_working(old, Utc::now()));

        // A clock that jumped backwards should not open a window either.
        let future = Utc::now() + Duration::hours(2);
        assert!(!still_working(future, Utc::now()));
    }

    #[test]
    fn a_new_pool_defaults_to_touching_nothing() {
        let p = Pool::default();
        assert_eq!(p.scope, Scope::None);
        assert_eq!(p.reboot, RebootPolicy::Never);
        assert_eq!(p.schedule, Schedule::Manual);
        assert_eq!(p.concurrency, 1);
    }
}
