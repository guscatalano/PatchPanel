# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Read `README.md` first — it covers what PatchPanel is, the manifest format, the
safety model, and how to enroll agents. This file covers the things you only
learn by reading across several files, and the ways this repo will bite you.

## Commands

```bash
cargo build --release                  # both binaries
cargo test --workspace                 # ~60 tests, all pure — no fixtures, no network
cargo test -p pp-portal pools::        # one module
cargo test -p pp-agent sources -- --nocapture   # one test by substring
```

The dashboard is a JavaScript string compiled into the binary, so `cargo build`
happily produces a page that does not work. **Both of these must pass before
any `ui.rs` change is considered done:**

```bash
./check-ui.sh                 # extracts the <script> body, node --check
./check-render.sh patchpanel  # renders it against a live portal's real API data
```

`check-render.sh` drives `loadAgent`, `loadDevices` and `drawNeeds` over a DOM
stub in `check-render.js`, asserts panes and counts line up, and parses **every
inline event handler**, including against deliberately hostile ids and pool
names. A handler broken by an unescaped quote is a dead button that looks
completely normal — this has shipped twice.

## Deploying

Development is against a live fleet. The portal runs in an LXC reachable as
`root@patchpanel`; `cargo` there is not on the default PATH.

```bash
scp -q crates/pp-portal/src/*.rs Cargo.toml root@patchpanel:~/patchpanel/...
ssh root@patchpanel 'export PATH=$PATH:/root/.cargo/bin && cd ~/patchpanel &&
  cargo build --release -p pp-portal &&
  install -m755 target/release/pp-portal /usr/local/bin/pp-portal &&
  systemctl restart patchpanel-portal'
```

Bump `workspace.package.version` for anything user-visible. Changing it also
changes the agent version, so a portal-only fix does not have to bump — but a
bump means agents roll when `agent_version` in the manifest is pointed at it.
Agents are built by `/root/deploy-agents.sh` **inside** the portal container
(which has musl-tools, zig and cargo-zigbuild; the musl targets cannot be
cross-built from the Windows host, so a syntax check there is `cargo test
--workspace`, and the real build happens in the container).

Shipping an agent change takes **three** steps, and skipping either of the last
two leaves the fleet on the old binary with nothing reporting an error:

1. `/root/deploy-agents.sh` — builds and installs the binaries and the portal.
2. `POST /api/builds` one entry per target (`linux/x86_64`, `linux/aarch64`,
   `windows/x86_64`) with the **real** `sha256` of each staged file.
3. `PUT /api/manifest` with `agent_version` set to the new version.

Step 3 answers `{"upgrading": N}`. **`"upgrading": 0` means step 2 was missed** —
the manifest is pointing at a version with no published build, so no agent has
anything to fetch and every one of them silently stays put. The Windows `.exe` is
built on the host and `scp`ed to `/var/lib/patchpanel-portal/agents/`; the deploy
script only hashes whatever is already there.

Agent-side changes only take effect on data that was collected *after* the roll.
A parser fix does not retroactively repair stored rows, so after the fleet
reaches the new version, re-collect: `POST /api/commands/broadcast` with
`{"command":{"kind":"discover"}}` for the discovery sweep. `/api/fleet` returns
agents under **`agents`**, not `machines`.

That container has filled its disk with `target/` artifacts before, which fails
the build silently and drops agents off the fleet. If agents disappear after a
deploy, check `df` there first.

## Architecture

Three crates: `pp-proto` (wire types shared by both ends), `pp-agent`,
`pp-portal`. Agents dial **out** over WebSocket and are never connected to.

### Derive on the portal, not the agent

This is the single most important rule here, and it has been violated and fixed
four times (`unfetchable`, `blocked`, device reports, guest lists).

**Anything an agent holds in memory is lost on reconnect and on self-update.**
An agent that restarts reports an empty list, which is indistinguishable from
"nothing is wrong" — and the dashboard confidently goes green. So any fact that
must survive a restart is computed and stored by the portal from what the agent
*observed*, not tracked by the agent.

The same rule applied to the database: state carries forward rather than being
overwritten by an absence. `db.rs::store_inventory` carries devices and
discovered hosts forward when a scan does not include them, and
`device_last_good` keeps the last successful probe so a failed one cannot erase
a firewall's version, pending count or end-of-life status. **Absence of news is
not news** — a failed probe establishes exactly one fact, reachability, and
nothing else on that row may be overwritten by it.

### Honest counting, and where each number lives

`update_count` alone is never the answer. `db.rs::agents()` derives the per-row
counts from the stored inventory: `actionable_count` (total minus deferred),
`deferred_count` (phased), `held_back_count`, `blocked_count`, `ignored_count`,
`scan_issue_count`. `hide_ignored` filters ignored updates out of every read so
the fleet table and the machine page cannot disagree — and it **returns how many
it removed**, because silently subtracting made an all-ignored machine read
"none", which is the one thing this product must not say.

`api.rs::fleet()` then adds what only the whole fleet knows: `unmanaged_guests`
(matching guest names against known hostnames, running guests only), and
`patch_state` / `patch_note` / `patch_short` — whether pending updates are
somebody's job (`yours`), a pool's (`scheduled`, `queued`), or evidence of a
problem (`missed`, `failed`). A count that looks the same whether the schedule
is working or broken is worse than no count.

### `attention.rs` — the Overview list

Everything on the landing page is derived here and shipped on `/api/fleet`, so
the nav badge, the list and the empty state cannot disagree. Two rules govern
what may be added:

1. **An item appears only if nothing scheduled will resolve it.** A machine with
   99 updates and a pool firing at 04:30 is not on the list. This filter is why
   500+ pending updates renders as a handful of rows.
2. **A row is a problem, not an object.** Three guests missing backups is one
   row carrying three names, not three rows. The list must grow with the number
   of *kinds* of thing that can be wrong, not with the size of the fleet.

Tiers: 1 = the page would otherwise be lying (not scanned, mid-upgrade, silent
machine, unreachable appliance); 2 = something tried and demonstrably failed;
3 = nothing failed but nothing is covering it; 4 = waiting on you, harmless.

A row that can never clear does not belong here — it becomes something to
scroll past, which defeats the list. Guests without an agent were removed for
exactly this reason; held-back upgrades were demoted to tier 4.

### The Network tab — one definition of "accounted for"

`api.rs::accounted_for` maps every address the portal can explain to what
explains it, and both readers use it: the Network tab labels rows with it and the
Overview badge counts what is left. They used to compute it separately with a
comment saying they must agree, which is not a mechanism.

Two things make an address resolvable, and neither is obvious:

- An agent's addresses come from `hardware.ip_addresses`. The sweeping agent
  cannot know them — it only has the manifest it was handed — so it marks every
  machine in the fleet `unmanaged` and the portal corrects that.
- A **device is declared by name and a sweep finds addresses**, so matching on
  `target` alone never matches. `DeviceReport::resolved_ip` carries the address
  the collector actually resolved and probed. Resolving names portal-side instead
  would mean blocking DNS inside an endpoint the dashboard polls every few
  seconds. A device whose name resolves off the swept range simply does not
  appear, which is correct — it was not seen.

The tab lists the **whole** sweep, managed and not, because "what is on this
network" is the question; a list that silently drops the fleet's own machines
cannot answer it and gives no way to tell "not seen" from "seen and hidden".

Sweeps run on the agent's own timer (`manifest.discovery_secs`, default 1800) and
only in sites that own a range, so it is one sweep per range per interval rather
than one per agent. `Inventory::swept_at` is stamped by the sweep, not by the
inventory refresh that carries its results along, and the portal carries it
forward across reconnects the same way it carries `discovered` — a list of hosts
with no honest age on it reads as live when it is up to half an hour old.

A `Discover` broadcast is dispatched only to collectors whose site owns a range.
Sending it fleet-wide made twelve of thirteen agents record a failure every time
somebody pressed Scan now, for a sweep that had worked.

### `pools.rs` — scheduling

A machine belongs to at most one pool, so there is always a single answer to
"why did that reboot". `Schedule::next_after` is the only place cadence is
computed. `RUN_WINDOW` (4h) defines how long a run stays open; a machine still
pending inside the window is `queued`, and only `missed` once it closes —
getting this wrong once made nine machines look like failures. Pools always
dispatch `full: false`, so held-back upgrades genuinely never install on a
schedule.

### `ui.rs` — one file, ~3400 lines, embedded

Nav tabs map to *sets* of sections via `TAB_SECTIONS` (Machines shows the agents
table and the appliance cards), with `MOVED` redirecting hashes from the older
seven-tab layout. `showTab` drives both.

Conventions that matter:

- `esc()` for text, **`jsq()` for any value going into an inline handler.** Do
  not interpolate `JSON.stringify` into an attribute; it closes the quote.
- `setHTML()` rather than `innerHTML` — it refuses to clobber a field the user
  is typing in and raises the "updates paused" pill instead.
- The page polls every few seconds and re-renders, so prefer one delegated
  `document` listener over per-row listeners.
- Visual weight: **one pill per row, and it is the row's verdict**, not a label
  for a field. Red means "do not trust the green here" — something failed, or
  this number is not known to be true. Amber means true and waiting on a person.
  Green is reserved and never appears in a table row.
- `th, td { white-space: nowrap }` is global. Any new cell holding
  variable-length content needs a `max-width` plus ellipsis, or one long value
  puts the entire table into horizontal scroll.

**Editing this file:** it is JavaScript inside a Rust string. Shell heredocs
mangle the backslashes and have corrupted it repeatedly. Use the Edit tool, or a
Python script written to the scratchpad and run by path — not `python - <<'EOF'`
with escapes in it.

### `pp-mcp` — the portal over MCP

JSON-RPC on stdio. It is a *view*, not a second implementation: every tool asks
the portal the same question the dashboard asks and passes the portal's own
judgement through, so a caller cannot get a different answer here than the page
gives. Whenever a tool would need to decide what a number means, that decision
belongs in `api.rs` where both readers get it.

Read-only unless started with `--allow-actions`, and that is enforced in
`tools::call` as well as by leaving the actions out of `tools/list`. The catalogue
is only a hint — a client that listed the tools while actions were enabled, or
that guesses a name, can still ask — so read-only has to mean the server refuses,
not that it declines to advertise. `tools::is_action` is the list.

Actions are grouped by the thing they change rather than mapped one-per-route:
`patchpanel_command` covers the whole agent-command family with the kind as an
enum, `patchpanel_backup_policy` covers exempt/snooze/undo. Thirteen tools instead
of thirty, each still discoverable and validated. `patchpanel_edit_manifest`
refuses a document with no apps, appliances or discovery ranges in it, because the
usual way to destroy a manifest through an API is to send a fragment of one.

## Agent notes

`platform/{linux,windows}.rs` hold the package-manager work. Parse machine-
readable output, never prose: `dpkg --audit`'s wording missed a broken grub, and
`dpkg-query -W -f='${Package} ${Status}'` replaced it.

Anything derived from a vendor's API or docs gets verified against a real
device. OPNsense's documentation was wrong twice, Unraid ships with GraphQL
introspection disabled, and Home Assistant's `/api/config` is far poorer than
`/api/states`. `sources.rs` goes further and fetches every apt source it is
about to suggest, because a plausible-looking suite that 404s will be written to
a machine otherwise — `archive.debian.org` has no `bullseye-security`.

Release profile deliberately does **not** set `panic = "abort"`: the agent
isolates panicking probe and command tasks so one bad device cannot take a host
off the fleet.

---

There is an OpenAI Codex config at `~/.codex/config.toml`. If you want its
MCP servers, prompts or instructions available here, reply `/import` to see what
is importable, then `/import --yes=<digest>` to apply it.
