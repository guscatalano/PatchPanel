//! What needs a person, and nothing else.
//!
//! The fleet page answers "what is pending". That is a different question, and
//! a worse one: this morning 527 of 536 pending updates belonged to a pool that
//! was going to install them tonight. A list that says "536" every day teaches
//! you to stop reading it, and then the one row that mattered is invisible.
//!
//! Two rules shape everything here.
//!
//! **An item appears only if nothing scheduled will resolve it.** A machine
//! with ninety-nine updates and a pool that fires at 04:30 is not on this list.
//! That single filter is the difference between four rows and thirteen.
//!
//! **A row is a problem, not an object.** One backup rule broken across three
//! guests is one row carrying three names, not three rows. Grouped by cause,
//! the list grows with the number of *kinds* of thing that can go wrong - about
//! a dozen - rather than with the size of the fleet. Adding a fourth hypervisor
//! must not make this page longer.

use serde::Serialize;

use crate::db::AgentRow;

/// How long a machine may be quiet before it is worth saying so.
///
/// Agents check in about once a minute, so silence is abnormal fast. The
/// threshold is a reboot's worth of grace and no more: a day was long enough
/// for a machine to be rebooted, fail to come back, and have this page say
/// nothing about it all afternoon.
const SILENT_MINUTES: i64 = 15;


/// One member of a grouped item: the name, where it lives, and why it is here.
///
/// These ship with the item rather than behind a second request, because the
/// count on a row is also the control that opens them - grouping should cost
/// nothing in detail, only in vertical space.
#[derive(Serialize)]
pub struct Who {
    pub name: String,
    pub context: String,
    pub why: String,
    /// Where this particular one lives. A grouped row can only have one
    /// button, and it is usually the wrong destination for any single member -
    /// "See guests" pointed at a tab with no guest list on it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
}

#[derive(Serialize)]
pub struct Item {
    /// 1 the page would otherwise be lying; 2 something tried and failed;
    /// 3 nothing failed, but nothing is covering it; 4 waiting on you, harmless.
    pub tier: u8,
    /// The verdict, as a sentence someone would say out loud.
    pub say: String,
    /// The part that explains it, when a sentence is not enough on its own.
    pub tail: String,
    /// What the button says, and where it goes.
    pub action: String,
    pub link: String,
    pub who: Vec<Who>,
}

/// Collect a group, or nothing at all when it is empty.
///
/// Every group is built the same way - filter the fleet, and if anything
/// survives, describe it once - so a new kind of problem is a few lines rather
/// than a new shape.
fn group(
    tier: u8,
    who: Vec<Who>,
    say: impl Fn(usize) -> String,
    tail: &str,
    action: &str,
    link: &str,
) -> Option<Item> {
    if who.is_empty() {
        return None;
    }
    Some(Item {
        tier,
        say: say(who.len()),
        tail: tail.into(),
        action: action.into(),
        link: link.into(),
        who,
    })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        one.into()
    } else {
        many.into()
    }
}

/// Machines matching a predicate, described by a reason.
fn machines(
    rows: &[AgentRow],
    pick: impl Fn(&AgentRow) -> bool,
    why: impl Fn(&AgentRow) -> String,
) -> Vec<Who> {
    rows.iter()
        .filter(|r| pick(r))
        .map(|r| Who {
            name: r.hostname.clone(),
            context: r.os_version.clone(),
            why: why(r),
            link: format!("#agent/{}", r.id),
        })
        .collect()
}

/// A guest with nothing recent to restore from.
pub struct RiskyGuest {
    pub host: String,
    pub name: String,
    pub kind: String,
    pub why: String,
}

/// An external job and what the portal makes of its last check-in.
pub struct JobTrouble {
    pub name: String,
    pub status: String,
    pub detail: String,
    pub late_by: String,
}

/// A device that is declared but not well.
pub struct DeviceTrouble {
    pub id: String,
    pub label: String,
    pub firmware: String,
    pub reachable: bool,
    pub eol: bool,
    pub eol_note: String,
    pub updates: usize,
    /// Which agent probes it. When every unreachable appliance shares one,
    /// they did not each fail - the thing looking at them did.
    pub collector: String,
}

pub fn collect(
    rows: &[AgentRow],
    revision: u64,
    guests: &[RiskyGuest],
    devices: &[DeviceTrouble],
    jobs: &[JobTrouble],
) -> Vec<Item> {
    let now = chrono::Utc::now();
    let mut out = Vec::new();

    // -- tier 1: the numbers on this page are not known to be true ----------

    out.extend(group(
        1,
        machines(
            rows,
            |r| r.scan_issue_count > 0,
            |r| {
                format!(
                    "{} backend(s) could not be scanned",
                    r.scan_issue_count
                )
            },
        ),
        |n| {
            format!(
                "{n} {} could not be scanned",
                plural(n, "machine", "machines")
            )
        },
        "their update counts on this page are a floor, not a total",
        "Fix scanning",
        "#machines",
    ));

    out.extend(group(
        1,
        machines(rows, |r| r.mid_upgrade, |_| "dpkg is part-way through".into()),
        |n| {
            format!(
                "{n} {} part-way through an upgrade",
                plural(n, "machine is", "machines are")
            )
        },
        "nothing else will run on them until it is finished or rolled back",
        "Finish it",
        "#machines",
    ));

    out.extend(group(
        1,
        machines(
            rows,
            |r| r.release_blockers > 0,
            |r| format!("{} blocker(s)", r.release_blockers),
        ),
        |n| {
            format!(
                "{n} {} apt sources that would break an upgrade",
                plural(n, "machine has", "machines have")
            )
        },
        "an expired or wrong source makes every count on that machine suspect",
        "Review sources",
        "#machines",
    ));

    // A machine that has stopped reporting is not "fine, last seen Tuesday" -
    // everything on its row is the last thing it said, and the longer that is
    // ago the less any of it means.
    out.extend(group(
        1,
        machines(
            rows,
            |r| now.signed_duration_since(r.last_seen).num_minutes() >= SILENT_MINUTES,
            |r| {
                let mins = now.signed_duration_since(r.last_seen).num_minutes();
                if mins < 120 {
                    format!("silent for {mins} minutes")
                } else if mins < 60 * 48 {
                    format!("silent for {} hours", mins / 60)
                } else {
                    format!("silent for {} days", mins / (60 * 24))
                }
            },
        ),
        |n| {
            format!(
                "{n} {} stopped reporting",
                plural(n, "machine has", "machines have")
            )
        },
        "everything this page shows for them is the last thing they said",
        "Open",
        "#machines",
    ));

    // Appliances that stopped answering. Every one of them is reported by an
    // agent, and when they all stopped at once and share that agent, the
    // honest reading is that the collector lost the network - not that five
    // separate devices failed in the same minute.
    let down: Vec<&DeviceTrouble> = devices.iter().filter(|d| !d.reachable).collect();
    let one_collector = down
        .first()
        .map(|f| down.iter().all(|d| d.collector == f.collector))
        .unwrap_or(false);
    out.extend(group(
        1,
        down.iter()
            .map(|d| Who {
                name: d.label.clone(),
                context: d.id.clone(),
                why: "did not answer the last probe".into(),
                link: String::new(),
            })
            .collect(),
        |n| {
            format!(
                "{n} {} not answering",
                plural(n, "appliance is", "appliances are")
            )
        },
        &if down.len() > 1 && one_collector {
            format!(
                "all of them are probed by {} - check that machine's network before the appliances",
                down.first().map(|d| d.collector.as_str()).unwrap_or("one agent")
            )
        } else {
            "what this page shows for them is the last reading that worked".into()
        },
        "Open",
        "#machines",
    ));

    out.extend(group(
        1,
        jobs.iter()
            .filter(|j| j.status == "overdue")
            .map(|j| Who {
                name: j.name.clone(),
                context: j.late_by.clone(),
                why: if j.detail.is_empty() {
                    "last check-in said nothing".into()
                } else {
                    j.detail.clone()
                },
                link: "#schedule".into(),
            })
            .collect(),
        |n| {
            format!(
                "{n} {} stopped checking in",
                plural(n, "job has", "jobs have")
            )
        },
        "nothing else will mention this: a job that stops running reports nothing at all",
        "Open",
        "#schedule",
    ));

    // -- tier 2: something tried, and demonstrably failed -------------------

    // Reported success with output that says otherwise. Red, because red here
    // means "do not trust the green" rather than "severe".
    out.extend(group(
        2,
        jobs.iter()
            .filter(|j| j.status == "suspect")
            .map(|j| Who {
                name: j.name.clone(),
                context: String::new(),
                why: j.detail.clone(),
                link: "#jobs".into(),
            })
            .collect(),
        |n| {
            format!(
                "{n} {} success with output that disagrees",
                plural(n, "job reported", "jobs reported")
            )
        },
        "the script ended cleanly; what it printed says something went wrong",
        "Open",
        "#jobs",
    ));

    out.extend(group(
        2,
        jobs.iter()
            .filter(|j| j.status == "failed")
            .map(|j| Who {
                name: j.name.clone(),
                context: String::new(),
                why: j.detail.clone(),
                link: "#schedule".into(),
            })
            .collect(),
        |n| format!("{n} {} reported a failure", plural(n, "job", "jobs")),
        "",
        "Open",
        "#schedule",
    ));


    out.extend(group(
        2,
        machines(
            rows,
            |r| r.patch_state == "failed",
            |r| {
                if r.pool.is_empty() {
                    "the last run failed".into()
                } else {
                    format!("in the \"{}\" pool", r.pool)
                }
            },
        ),
        |n| {
            format!(
                "the last patch run failed on {n} {}",
                plural(n, "machine", "machines")
            )
        },
        "",
        "Open log",
        "#schedule",
    ));

    // Observed, not reported: a run installed them and the version did not
    // move. Nothing else in the system notices this.
    let blocked: Vec<Who> = machines(
        rows,
        |r| r.blocked_count > 0,
        |r| format!("{} did not move", r.blocked_count),
    );
    let blocked_total: usize = rows.iter().map(|r| r.blocked_count).sum();
    out.extend(group(
        2,
        blocked,
        |n| {
            format!(
                "{blocked_total} {} blocked on {n} {}",
                plural(blocked_total, "update is", "updates are"),
                plural(n, "machine", "machines")
            )
        },
        "a run was asked to install them and the installed version did not change",
        "Show packages",
        "#machines",
    ));

    // -- tier 3: nothing failed, but nothing is covering it -----------------

    out.extend(group(
        3,
        guests
            .iter()
            .map(|g| Who {
                name: g.name.clone(),
                context: format!("{} · {}", g.host, g.kind),
                why: g.why.clone(),
                link: "#backups".into(),
            })
            .collect(),
        |n| {
            format!(
                "{n} running {} no recent backup",
                plural(n, "guest has", "guests have")
            )
        },
        "",
        "Backups",
        "#backups",
    ));

    // Guests without an agent are deliberately left out of this list.
    //
    // In practice most of them are on purpose - a docker host, an appliance
    // VM, a box somebody manages another way - so the row never cleared and
    // became something to scroll past, which is what this list exists not to
    // be. The count is still on the host's fleet row and in its guests pane,
    // where it reads as a fact about that machine rather than as a task.

    out.extend(group(
        3,
        devices
            .iter()
            .filter(|d| d.eol)
            .map(|d| Who {
                name: d.label.clone(),
                context: d.firmware.clone(),
                why: if d.eol_note.is_empty() {
                    "past end of life".into()
                } else {
                    d.eol_note.clone()
                },
                link: String::new(),
            })
            .collect(),
        |n| {
            format!(
                "{n} {} past end of life",
                plural(n, "appliance is", "appliances are")
            )
        },
        "no further patches will be issued for it, whatever the update count says",
        "Open",
        "#machines",
    ));

    // An appliance has no pool and no agent, so anything it is offered is
    // always somebody's job by definition.
    out.extend(group(
        3,
        devices
            .iter()
            .filter(|d| d.updates > 0 && !d.eol)
            .map(|d| Who {
                name: d.label.clone(),
                context: d.firmware.clone(),
                why: format!("{} update(s) offered", d.updates),
                link: String::new(),
            })
            .collect(),
        |n| {
            format!(
                "{n} {} updates waiting",
                plural(n, "appliance has", "appliances have")
            )
        },
        "nothing patches an appliance on a schedule",
        "Open",
        "#machines",
    ));

    // Pending updates that no schedule will ever take. This is the only place
    // a plain update count earns a row.
    out.extend(group(
        3,
        machines(
            rows,
            |r| r.patch_state == "yours",
            |r| {
                let sec = if r.security_count > 0 {
                    format!(", {} security", r.security_count)
                } else {
                    String::new()
                };
                format!("{} update(s){sec}", r.actionable_count)
            },
        ),
        |n| {
            format!(
                "{n} {} updates and nothing will install them",
                plural(n, "machine has", "machines have")
            )
        },
        "no pool patches them on a schedule",
        "Install",
        "#machines",
    ));

    out.extend(group(
        3,
        machines(
            rows,
            |r| r.patch_state == "missed",
            |r| r.patch_short.clone(),
        ),
        |n| {
            format!(
                "{n} {} left behind by the last run",
                plural(n, "machine was", "machines were")
            )
        },
        "the pool ran and these still have updates pending",
        "Install",
        "#schedule",
    ));

    // Held-back upgrades are a standing condition rather than an event: a
    // third of any Debian fleet has some at any moment, and on a rolling
    // release they come and go as dependencies land. True, worth knowing, and
    // no pool will ever take them - but nothing is degrading while it waits,
    // so it belongs below the things that are.
    out.extend(group(
        4,
        machines(
            rows,
            |r| r.held_back_count > 0,
            |r| format!("{} held back", r.held_back_count),
        ),
        |n| {
            format!(
                "{n} {} upgrades only a full upgrade will take",
                plural(n, "machine has", "machines have")
            )
        },
        "a pool runs a plain upgrade, so these wait for you",
        "Open",
        "#machines",
    ));

    // -- tier 4: waiting on you, and nothing is degrading meanwhile ---------

    out.extend(group(
        4,
        machines(
            rows,
            |r| r.reboot_required,
            |r| {
                r.last_patched
                    .map(|t| format!("since {}", t.format("%-d %b")))
                    .unwrap_or_default()
            },
        ),
        |n| {
            format!(
                "{n} {} waiting for a reboot",
                plural(n, "machine is", "machines are")
            )
        },
        "",
        "Reboot",
        "#machines",
    ));

    out.extend(group(
        4,
        machines(
            rows,
            |r| r.applied_revision < revision,
            |r| format!("r{} of r{revision}", r.applied_revision),
        ),
        |n| {
            format!(
                "{n} {} behind the manifest",
                plural(n, "machine is", "machines are")
            )
        },
        "",
        "Apply",
        "#setup",
    ));

    out.extend(group(
        4,
        machines(
            rows,
            |r| r.drift_count > 0,
            |r| format!("{} app(s) differ", r.drift_count),
        ),
        |n| {
            format!(
                "{n} {} applications that differ from the manifest",
                plural(n, "machine has", "machines have")
            )
        },
        "",
        "Open",
        "#machines",
    ));

    out.sort_by_key(|i| i.tier);
    out
}

/// The one-line summary that replaces eleven tiles.
///
/// Its job is to explain why the attention list is short when the pending
/// count is large: most of those updates belong to a pool.
#[derive(Serialize, Default)]
pub struct Gist {
    pub machines: usize,
    pub reporting: usize,
    pub pending: usize,
    pub security: usize,
    /// Pending updates on machines a pool patches on a schedule.
    pub covered: usize,
}

pub fn gist(rows: &[AgentRow]) -> Gist {
    Gist {
        machines: rows.len(),
        reporting: rows.iter().filter(|r| r.online).count(),
        pending: rows.iter().map(|r| r.actionable_count).sum(),
        security: rows.iter().map(|r| r.security_count).sum(),
        covered: rows
            .iter()
            .filter(|r| matches!(r.patch_state.as_str(), "scheduled" | "queued"))
            .map(|r| r.actionable_count)
            .sum(),
    }
}
