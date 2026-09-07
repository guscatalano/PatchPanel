# PatchPanel

A central portal plus a cross-platform agent that keeps a mixed fleet up to
date: **OS patches**, **application versions**, the **agent's own version**, and
the **firmware of IoT devices that cannot run an agent at all**.

One manifest describes the desired state. Every agent holds a persistent
WebSocket to the portal and converges on it.

```
                    ┌──────────────────────────────┐
                    │          pp-portal           │
                    │  manifest · SQLite · web UI  │
                    └───────────────┬──────────────┘
                       WebSocket (agent dials out)
            ┌──────────────────┼──────────────────┐
            │                  │                  │
      ┌─────┴─────┐      ┌─────┴─────┐      ┌─────┴─────┐
      │ pp-agent  │      │ pp-agent  │      │ pp-agent  │
      │  Linux    │      │  Windows  │      │  Linux    │
      │ apt/dnf   │      │ winget/WU │      │ site: ot  │
      └───────────┘      └───────────┘      └─────┬─────┘
                                                  │ SNMP / HTTP / TCP
                                       ┌──────────┴──────────┐
                                       │  PLCs · switches ·  │
                                       │  cameras · sensors  │
                                       └─────────────────────┘
```

Agents always dial out, so endpoints need no inbound firewall rules and work
behind NAT. Devices are reached by the agent on their own network segment,
not by the portal — device probing scales with the fleet.

## What it manages

| Scope | Linux | Windows | Applied automatically? |
|---|---|---|---|
| OS patches | apt, dnf | Windows Update (via PSWindowsUpdate) | Only if `patch_policy.auto_apply` |
| Applications | apt, dnf, verified URL | winget, verified URL | **Yes**, on every manifest revision |
| Agent itself | self-update + systemd restart | self-update + SCM restart | Yes, when `agent_version` changes |
| IoT devices | SNMP, HTTP, TCP probes | same | Never — reported only |

Two asymmetries are deliberate:

- **Apps converge automatically, patches do not.** An app spec is a statement
  about what should be installed, so agents act on it. A patch run can restart
  services and demand a reboot, so it stays behind an explicit action or an
  opt-in policy.
- **Device firmware is never pushed.** Flashing a PLC or a camera is
  vendor-specific and physically risky. PatchPanel tells you a device is on the
  wrong firmware; a human decides what to do about it.

## Build

```bash
cargo build --release
```

Produces `target/release/pp-portal` and `target/release/pp-agent`. Both are
single binaries with no runtime to install on targets.

Cross-compiling the agent for Linux from a Windows workstation:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release -p pp-agent --target x86_64-unknown-linux-musl
```

## Run the portal

```bash
pp-portal --bind 0.0.0.0:8080 --db /var/lib/patchpanel-portal/portal.db \
  --admin-token "$ADMIN" --enrollment-token "$ENROLL"
```

Omit the tokens and it generates and prints them — convenient for a first look,
but they change on every restart, so set them for anything real. Both are also
read from `PATCHPANEL_ADMIN_TOKEN` and `PATCHPANEL_ENROLLMENT_TOKEN`, which is
how `deploy/patchpanel-portal.service` supplies them (a command line is visible
to every user via `ps`).

Open `http://localhost:8080/` and paste the admin token.

> **TLS:** the portal speaks plain HTTP/WS. Put it behind nginx or Caddy for
> anything crossing a network you do not own, and point agents at `wss://`.
> Agents verify certificates against the system trust store.

## Enroll an agent

**Linux**

```bash
sudo ./deploy/install-agent.sh ws://portal.example.com:8080/api/agent/ws "$ENROLL" plant-a
```

**Windows** (elevated PowerShell)

```powershell
.\deploy\install-agent.ps1 -Portal ws://portal.example.com:8080/api/agent/ws -Token $ENROLL -Site plant-a
```

For Windows OS patching (as opposed to app updates via winget), the agent needs
the Windows Update module on the target:

```powershell
Install-Module PSWindowsUpdate -Force -Scope AllUsers
```

Without it the agent still reports full inventory and manages apps; it just
cannot apply OS updates, and says so.

The `--site` value decides which devices that agent probes. Agents in different
sites can share one portal without probing each other's networks.

### Enrollment model

A new agent presents the shared enrollment token once and receives its own
durable token, stored in its state directory. From then on only that token is
accepted for that agent id — learning the shared secret later does not let
anyone impersonate a machine that has already enrolled.

To re-enroll a host, delete it in the portal **and** remove `state.json` from
its state directory (`/var/lib/patchpanel` or `%ProgramData%\PatchPanel`).

## Before you touch the manifest

Publishing a manifest takes effect on every connected agent within seconds, and
app specs are applied without further confirmation. `examples/manifest.json`
contains real, working specs — treat it as a reference to copy from, not a file
to publish as-is, or you will install its example packages across your fleet.

Start from the default manifest (empty `apps`, `auto_apply: false`) and add
entries deliberately.

Check a device entry before committing it to the manifest:

```bash
pp-agent probe 10.20.0.2 --kind snmp --arg public
pp-agent probe 10.20.4.31 --kind http
pp-agent probe 10.20.9.14 --kind tcp --arg 554
```

And see what an agent would report, without a portal at all:

```bash
pp-agent inventory
```

## The manifest

See `examples/manifest.json` for a fully commented example. The shape:

```jsonc
{
  "apps": [ { "name": "...", "ensure": "present|latest|absent",
              "source": { "type": "apt|dnf|winget|url", ... },
              "os": ["linux"] } ],
  "devices": [ { "id": "...", "target": "10.20.0.2", "site": "plant-a",
                 "expect_version": "2.10.1",
                 "probe": { "type": "snmp|http|tcp", ... } } ],
  "discovery": [ { "cidr": "10.20.0.0/24", "ports": [80,443,22], "site": "plant-a" } ],
  "patch_policy": { "auto_apply": false, "security_only": true,
                    "allow_reboot": false, "exclude": ["linux-image-generic"] },
  "agent_version": null,
  "heartbeat_secs": 30, "inventory_secs": 3600, "device_probe_secs": 300
}
```

The portal assigns `revision` itself; a submitted one is ignored. Agents compare
revisions to decide whether to reconverge, so every publish is visible to them.

**Probes.** SNMP v2c reads sysDescr by default. HTTP extracts a version with a
JSON pointer (`version_json_pointer`) or a regex capture group
(`version_regex`), and `insecure: true` accepts the self-signed certificates
most appliances ship with. TCP checks reachability and can read a banner. All
three are read-only.

**Discovery** sweeps a CIDR for responsive hosts and flags any that no device
entry covers — the "what is actually on this network" question. Capped at 1024
hosts per range, with short timeouts and bounded concurrency so it does not
overwhelm field switches.

**URL app sources** require a `sha256`, verified before the installer runs. This
is not optional: fetching a binary over the network and running it as root is
the most dangerous thing the agent does.

## Rolling out a new agent version

```bash
# 1. Publish the build
curl -X PUT/POST http://portal:8080/api/builds \
  -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' \
  -d '{"version":"0.2.0","os":"linux","arch":"x86_64",
       "url":"https://builds.example.com/pp-agent-0.2.0-linux-x86_64",
       "sha256":"<64 hex chars>"}'

# 2. Point the manifest at it
#    "agent_version": "0.2.0"
```

Agents running a different version get a `SelfUpdate` on their next connect.
The agent verifies the checksum, swaps the binary, and exits; the supervisor
restarts it. This is why `Restart=always` is in the systemd unit and why
`install-service` configures SCM restart actions — without them a self-update
takes the host off the fleet.

## API

Everything the dashboard does is plain JSON, so rollouts can be scripted and
monitoring can poll. All routes take `Authorization: Bearer <admin-token>`.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/fleet` | Summary counters plus every agent |
| `GET` | `/api/agents/{id}` | One agent: inventory and recent commands |
| `DELETE` | `/api/agents/{id}` | Forget an agent |
| `GET` | `/api/devices` | Every device report, plus unmanaged hosts found by discovery |
| `GET`/`PUT` | `/api/manifest` | Read / publish desired state |
| `POST` | `/api/agents/{id}/commands` | Dispatch to one agent |
| `POST` | `/api/commands/broadcast` | Dispatch to all, or one `site` |
| `GET` | `/api/commands` | Command log with captured output |
| `GET`/`POST` | `/api/builds` | Published agent builds |

Commands: `collect_inventory`, `apply_manifest`, `apply_patches`
(`security_only`, `only`), `probe_devices` (`only`), `discover`, `reboot`
(`delay_secs`), `self_update`.

Commands are **not queued** for offline agents — dispatching to one returns
`409`. A button pressed now should not fire days later when a machine happens
to come back.

## Layout

```
crates/pp-proto     Wire protocol and manifest types, shared by both sides
crates/pp-agent     The agent: platform backends, device probes, self-update
crates/pp-portal    The portal: SQLite store, WebSocket hub, REST API, dashboard
deploy/             systemd units and install scripts
examples/           A commented manifest
```

The agent's OS-specific code is confined to `platform/linux.rs` and
`platform/windows.rs` behind one interface; adding a platform means adding a
module, not touching the session loop.

## Status

Working end-to-end and verified against a live Windows host: enrollment,
inventory (registry + winget), manifest publish with live push, app
convergence, device probes (SNMP/HTTP/TCP), discovery sweeps, and the command
log with streamed output.

Not yet exercised against a real Linux host or real SNMP hardware — the apt/dnf
and PSWindowsUpdate paths are written but untested on live systems. Verify them
on a throwaway VM before pointing this at anything you care about.
