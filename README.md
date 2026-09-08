# PatchPanel

Patch management for a mixed fleet — Linux, Windows, hypervisors, and the
appliances you cannot install anything on — with one rule running through it:

**never report "up to date" unless it is actually known to be true.**

A patch tool that says *0 pending* because it could not scan looks exactly like
one that says *0 pending* because the machine is clean. Every feature here
exists because that distinction was got wrong somewhere, and PatchPanel is
built to tell you which of the two you are looking at.

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
      │  Linux    │      │  Windows  │      │ collector │
      │ apt/dnf   │      │ winget/WU │      │           │
      └───────────┘      └───────────┘      └─────┬─────┘
                                                  │ API / SNMP / HTTP
                                       ┌──────────┴──────────┐
                                       │ firewalls · NAS ·   │
                                       │ switches · sensors  │
                                       └─────────────────────┘
```

Agents dial out, so endpoints need no inbound firewall rules and work behind
NAT. Appliances are reached by an agent on their own network segment, never by
the portal, so probing scales with the fleet.

## What it manages

| Scope | Linux | Windows | Applied automatically? |
|---|---|---|---|
| OS patches | apt, dnf | Windows Update via PSWindowsUpdate | Only when you ask |
| Applications | apt, dnf, verified URL | winget, verified URL | **Yes**, on every manifest revision |
| Release upgrades | Debian 11 → 12 → 13 | — | Never; explicit, gated, preflighted |
| Firmware | fwupd / LVFS | (via Windows Update) | **Never** — reported only |
| Appliances | OPNsense, Unraid, SNMP, HTTP, TCP | — | Never — read-only probes |
| The agent itself | self-update + systemd | self-update + SCM | Yes, when `agent_version` changes |

## Honest counting

The interesting part of the problem is not installing updates. It is knowing
what the number on the screen means. PatchPanel separates:

- **installable** — a patch run will install these.
- **phased** — Ubuntu is withholding them from this machine; nothing to do, and
  pressing the button will correctly do nothing. Packages merely "kept back"
  because they depend on a phased one are counted here too, so the fix offered
  is never a button that cannot work.
- **held back** — a full upgrade would install them; a plain one will not.
- **blocked** — a patch run was asked to install this and *demonstrably did
  not*: same version installed, same version offered, afterwards. Measured, not
  guessed. Windows package managers refuse in-place upgrades across installer
  technologies and report only a count, never a name.
- **ignored** — somebody decided against this exact version. It returns by
  itself when a newer one is published.
- **end of life** — reports zero pending, truthfully, because there will never
  be another update. The most dangerous green in the fleet.
- **not scanned** — a backend was missing or broken. Reported as a floor, never
  as a total.

Add to that: repositories whose indices list packages the server no longer has,
sources that fail to resolve, machines part-way through an interrupted upgrade,
and scan problems that only count once they have persisted across two scans —
because a mirror that 503s once and works on the next try is not a coverage gap,
and warnings that flap get ignored.

## Beyond patching

- **Appliances, agentlessly.** A firewall or NAS is the machine you least want
  to install software on. OPNsense and Unraid are asked over their own APIs with
  a read-only, least-privilege key; nothing is installed and nothing is written.
  Add one from the UI by pasting its URL and uploading the API key file the
  appliance gave you.
- **Hypervisors.** A Proxmox or Hyper-V host lists its guests and marks which
  of them PatchPanel has an agent for. An unmanaged VM is invisible from
  everywhere else — its host is the only place the gap can be seen.
- **Why a machine rebooted.** An unexpected restart is detected from the
  previous boot's own logs — kernel panic, OOM killer, soft lockup, or "the log
  simply stops", which means power loss or a hard reset. Windows reads the
  bugcheck from the event log. That evidence is gone when the journal rotates,
  so it is captured at the next scan.
- **Apt source auditing.** Sources are read, compared against machines running
  the *same release* (not merely the same OS), and corrected. Every suggestion
  is verified against the archives before it is offered — the fix is fetched to
  confirm the suite exists and whether its Release file has expired — and every
  write is validated with `apt-get update` and rolled back automatically if apt
  rejects it.
- **Release upgrades.** Debian only, and deliberately awkward: a preflight that
  refuses on pending updates, low disk, broken packages, or a target that is not
  a released version, plus a recovery path for an upgrade dpkg stopped part-way
  through — including choosing the disk a stuck bootloader needs.
- **Firmware.** fwupd devices and their available updates are reported and
  never installed by a patch run. Everything else here can be undone; a
  firmware write cannot.

## Build

```bash
cargo build --release
```

Two single binaries, `pp-portal` and `pp-agent`, with no runtime to install on
targets. For fully static Linux agents that run on any glibc:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release -p pp-agent --target x86_64-unknown-linux-musl
```

`build-agents.sh` builds both architectures (aarch64 goes through
`cargo-zigbuild`, because aws-lc's C needs musl headers a plain cross-gcc does
not have) and publishes them where the portal serves them.

Before changing the dashboard, run the two checks — the UI is an embedded
string, so `cargo build` will happily compile a page that does not work:

```bash
./check-ui.sh                 # the dashboard JavaScript parses
./check-render.sh <portal>    # it renders, against real API responses
```

`check-render.sh` runs the page against a DOM stub and asserts the panes, the
counts, and every inline event handler. A page that throws halfway through
rendering looks identical to a healthy one from the outside.

## Run the portal

```bash
pp-portal --bind 0.0.0.0:8080 --db /var/lib/patchpanel-portal/portal.db \
  --admin-token "$ADMIN" --enrollment-token "$ENROLL"
```

Omit the tokens and it generates and prints them — fine for a first look, but
they change on every restart. Both are read from `PATCHPANEL_ADMIN_TOKEN` and
`PATCHPANEL_ENROLLMENT_TOKEN`, which is how `deploy/patchpanel-portal.service`
supplies them; a command line is visible to every user via `ps`.

`--no-auth` turns off the admin token entirely, for a trusted network where the
portal is not reachable from anywhere else. It is a deliberate choice, not a
default.

> **TLS:** the portal speaks plain HTTP/WS. Put it behind nginx or Caddy for
> anything crossing a network you do not own, and point agents at `wss://`.
> Agents verify certificates against the system trust store.

## Enroll an agent

The **Add machine** tab builds these for you, with the portal's own address
filled in. In short:

```bash
curl -fsSL http://portal.example.com/install.sh | sh -s -- --site plant-a
```

```powershell
irm http://portal.example.com/download/pp-agent.exe -OutFile pp-agent.exe; ./pp-agent.exe setup --portal portal.example.com --site plant-a
```

Windows needs PSWindowsUpdate and a system-wide winget for full coverage, and
Linux needs fwupd for firmware. Both are one button in the UI — *Install the
missing tooling* — rather than a runbook. Until then the machine reports what it
cannot see rather than reporting zero.

`--site` decides which appliances that agent probes. A device with no collector
named is probed by whichever agent runs on the portal itself, so exactly one
machine probes each device without agents having to coordinate.

### Enrollment model

An agent presents the shared enrollment token once and receives its own durable
token. From then on only that token is accepted for that agent id, so learning
the shared secret later does not let anyone impersonate a machine that has
already enrolled.

To re-enroll a host, delete it in the portal **and** remove `state.json` from
its state directory (`/var/lib/patchpanel` or `%ProgramData%\PatchPanel`).

## Before you touch the manifest

Publishing a manifest takes effect on every connected agent within seconds, and
**app specs are applied without further confirmation** — that is what desired
state means. Patches are not: they are reported until someone asks.

`examples/manifest.json` contains real, working specs. Treat it as a reference
to copy from, not a file to publish as-is, or you will install its example
packages across your fleet.

## The manifest

One document describes the desired state. The portal assigns a monotonic
`revision`; agents act only when it is newer than what they applied.

```jsonc
{
  "apps": [
    { "name": "openssh-server", "ensure": "latest", "os": ["linux"],
      "source": { "type": "apt", "package": "openssh-server" } }
  ],

  // Apt source files every matching machine should have. Written through the
  // same validated path as a hand edit: apt has to accept the result, or the
  // previous file comes back.
  "apt_sources": [
    { "distro": "debian", "codename": "bullseye",
      "path": "/etc/apt/sources.list.d/archive.list",
      "content": "deb http://archive.debian.org/debian bullseye main\n" }
  ],

  // Appliances that cannot host an agent. Probes are read-only.
  "devices": [
    { "id": "fw.example.net", "label": "edge firewall",
      "target": "fw.example.net",
      "probe": { "type": "opnsense", "api_key": "...", "api_secret": "...",
                 "insecure": true, "check_after_hours": 12 } }
  ],

  "patch_policy": { "auto_apply": false, "security_only": true,
                    "allow_reboot": false, "exclude": ["linux-image-generic"] },

  "agent_version": "0.4.37",
  "heartbeat_secs": 30,
  "inventory_secs": 3600,
  "device_probe_secs": 300
}
```

`patch_policy.exclude` is never installed, even by an explicit run. That is
different from *ignore*, which hides one version from the count until a newer
one appears.

## Rolling out a new agent version

```bash
# 1. Publish the build
curl -X PUT $PORTAL/api/builds -H 'Content-Type: application/json' -d '{
  "version": "0.4.37", "os": "linux", "arch": "x86_64",
  "url": "http://portal.example.com/download/pp-agent-x86_64",
  "sha256": "<sha256 of the binary>"
}'

# 2. Point the manifest at it: "agent_version": "0.4.37"
```

Agents verify the sha256 before swapping the binary, then exit; systemd or the
Windows SCM restarts them. A failed download or hash mismatch leaves the running
agent untouched.

## Safety model

- **Patches are never applied without being asked** unless
  `patch_policy.auto_apply` says so.
- **One changing command at a time per machine.** Two apt runs collide on the
  dpkg lock, and a reboot during a release upgrade is worse. Scans and an agent
  restart stay available — the restart deliberately, as the way out when
  something is wedged.
- **Every source write is validated and rolls itself back** if apt rejects the
  result, including when apt names the file rather than a URL.
- **Firmware and release upgrades are never automatic**, and both make you
  confirm in a way a misclick cannot satisfy.
- **Device probes are read-only.** The one exception asks a firewall to check
  its own mirrors, is rationed by the device's own record of when it last did,
  and can be turned off.

## Layout

```
crates/
  pp-proto/    wire types: manifest, inventory, commands, device probes
  pp-agent/    the agent: platform backends, sources, probes, self-update
  pp-portal/   axum server, SQLite, WebSocket hub, embedded dashboard
deploy/        systemd unit, install scripts, portal.env example
examples/      a documented manifest to copy from
check-ui.sh    the dashboard JavaScript parses
check-render.sh  the dashboard renders, against a live portal's data
```

## Status

Runs a mixed fleet of Linux, Windows, Proxmox and appliances in production at
home. The parts most likely to bite are the ones that touch a machine: release
upgrades, source rewriting and firmware. Each of those is gated, preflighted,
and reversible where reversal is possible at all — and where it is not, it says
so before you press the button.

MIT licensed.
