//! The dashboard, embedded in the binary.
//!
//! Compiling the UI in means the portal is still a single file to deploy, which
//! matters more here than a build pipeline would: this is a thing people run on
//! a box in a cupboard, not a service with a CDN in front of it.

use axum::response::Html;
use axum::routing::get;
use axum::Router;

pub fn routes() -> Router {
    Router::new().route("/", get(|| async { Html(INDEX) }))
}

const INDEX: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>PatchPanel</title>
<style>
  :root {
    color-scheme: light dark;
    --bg: #f6f7f9;
    --panel: #ffffff;
    --line: #dfe3e8;
    --ink: #16191d;
    --muted: #6b7280;
    --accent: #2563eb;
    --ok: #15803d;
    --warn: #b45309;
    --bad: #b91c1c;
    --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #0f1115; --panel: #171a21; --line: #272b34;
      --ink: #e6e8ec; --muted: #9aa3af; --accent: #60a5fa;
      --ok: #4ade80; --warn: #fbbf24; --bad: #f87171;
    }
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--ink);
    font: 14px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  }
  header {
    display: flex; align-items: baseline; gap: 16px; flex-wrap: wrap;
    padding: 14px 20px; border-bottom: 1px solid var(--line); background: var(--panel);
    position: sticky; top: 0; z-index: 10;
  }
  header h1 { font-size: 16px; margin: 0; letter-spacing: -0.01em; }
  header .rev { color: var(--muted); font-family: var(--mono); font-size: 12px; }
  nav { margin-left: auto; display: flex; gap: 4px; }
  nav button {
    background: none; border: 1px solid transparent; color: var(--muted);
    padding: 5px 11px; border-radius: 6px; cursor: pointer; font: inherit;
  }
  nav button.active { color: var(--ink); border-color: var(--line); background: var(--bg); }
  /* How many need attention, without having to open the tab to find out. */
  .badge { display: inline-block; margin-left: 6px; padding: 0 6px; border-radius: 10px;
    font-size: 11px; font-family: var(--mono); line-height: 17px;
    background: var(--warn); color: var(--panel); }
  .badge.bad { background: var(--bad); }
  main { padding: 20px; max-width: 1400px; margin: 0 auto; }
  section { display: none; }
  section.active { display: block; }

  .tiles { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); margin-bottom: 20px; }
  .tile { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px; }
  .tile .n { font-size: 26px; font-weight: 600; line-height: 1.1; font-variant-numeric: tabular-nums; }
  .tile .l { color: var(--muted); font-size: 12px; margin-top: 2px; }
  .tile.warn .n { color: var(--warn); }
  .tile.bad .n { color: var(--bad); }

  .card { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; overflow: hidden; margin-bottom: 20px; }
  .card > h2 { font-size: 13px; margin: 0; padding: 11px 14px; border-bottom: 1px solid var(--line); color: var(--muted); font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em; }
  .scroll { overflow-x: auto; }
  /* Long tables get a viewport of their own rather than pushing the rest of
     the page out of reach - `bit` has 111 pending updates. */
  .scrolly { max-height: 420px; overflow: auto; }
  .scrolly thead th { position: sticky; top: 0; z-index: 1; background: var(--panel); }
  table { border-collapse: collapse; width: 100%; font-size: 13px; }
  th, td { text-align: left; padding: 7px 10px; border-bottom: 1px solid var(--line); white-space: nowrap; vertical-align: top; }
  td .msg { font-size: 11px; }
  th { color: var(--muted); font-weight: 600; font-size: 12px; }
  tr:last-child td { border-bottom: none; }
  tbody tr:hover { background: var(--bg); }
  td.mono, .mono { font-family: var(--mono); font-size: 12px; }

  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-right: 6px; vertical-align: 1px; }
  .dot.on { background: var(--ok); }
  .dot.off { background: var(--muted); }
  .dot.bad { background: var(--bad); }
  .pill { display: inline-block; padding: 1px 7px; border-radius: 20px; font-size: 11px; border: 1px solid var(--line); color: var(--muted); }
  .pill.bad { color: var(--bad); border-color: var(--bad); }
  .pill.warn { color: var(--warn); border-color: var(--warn); }
  .pill.ok { color: var(--ok); border-color: var(--ok); }
  .pill.busy { color: var(--accent); border-color: var(--accent); }
  .cmdid { cursor: pointer; border: 1px solid var(--line); border-radius: 4px; padding: 0 5px; margin-left: 6px; }
  .cmdid:hover { color: var(--accent); border-color: var(--accent); }
  .pill.busy::before {
    content: ""; display: inline-block; width: 7px; height: 7px; margin-right: 5px;
    border-radius: 50%; background: var(--accent); animation: pulse 1.1s ease-in-out infinite;
  }
  @keyframes pulse { 0%,100% { opacity: 1 } 50% { opacity: .25 } }

  button.act {
    font: inherit; font-size: 12px; padding: 3px 8px; margin-right: 3px;
    background: var(--bg); color: var(--ink);
    border: 1px solid var(--line); border-radius: 6px; cursor: pointer;
  }
  button.act:hover { border-color: var(--accent); color: var(--accent); }
  button.act:disabled { opacity: .4; cursor: default; }
  button.primary { background: var(--accent); color: #fff; border-color: var(--accent); }

  textarea {
    width: 100%; min-height: 460px; padding: 12px 14px; border: none; resize: vertical;
    background: var(--panel); color: var(--ink); font-family: var(--mono); font-size: 12.5px; line-height: 1.55;
  }
  textarea:focus { outline: none; }
  .bar { display: flex; gap: 8px; align-items: center; padding: 10px 14px; border-top: 1px solid var(--line); }
  .msg { font-size: 12px; color: var(--muted); }
  .msg.bad { color: var(--bad); }
  .msg.ok { color: var(--ok); }
  .empty { padding: 28px 14px; text-align: center; color: var(--muted); font-size: 13px; }
  details summary { cursor: pointer; color: var(--muted); font-size: 12px; }
  pre { margin: 8px 0 0; padding: 10px; background: var(--bg); border-radius: 6px; font-family: var(--mono); font-size: 11.5px; white-space: pre-wrap; word-break: break-word; max-height: 320px; overflow: auto; }

  .step { padding: 14px; border-bottom: 1px solid var(--line); }
  .step:last-child { border-bottom: none; }
  .step h3 { margin: 0 0 8px; font-size: 13px; font-weight: 600; }
  .step p { margin: 0 0 10px; color: var(--muted); font-size: 12.5px; }
  .cmd { position: relative; }
  .cmd pre { margin: 0; max-height: none; }
  .cmd button {
    position: absolute; top: 6px; right: 6px; font: inherit; font-size: 11px;
    padding: 3px 9px; background: var(--panel); color: var(--muted);
    border: 1px solid var(--line); border-radius: 5px; cursor: pointer;
  }
  .cmd button:hover { color: var(--accent); border-color: var(--accent); }
  .field { display: flex; gap: 8px; align-items: center; margin-bottom: 12px; }
  .field label { font-size: 12.5px; color: var(--muted); }
  .field input {
    padding: 5px 9px; border: 1px solid var(--line); border-radius: 6px;
    background: var(--bg); color: var(--ink); font-family: var(--mono); font-size: 12.5px;
  }
  .note { padding: 10px 12px; border-radius: 8px; font-size: 12.5px; border: 1px solid var(--line); background: var(--bg); margin-bottom: 12px; }

  .diff { font-family: var(--mono); font-size: 12px; line-height: 1.5; border: 1px solid var(--line);
    border-radius: 8px; overflow: auto; max-height: 320px; background: var(--bg); margin: 8px 0; }
  .diff div { padding: 1px 10px; white-space: pre-wrap; word-break: break-word; }
  .diff .del { background: color-mix(in srgb, var(--bad) 16%, transparent); color: var(--bad); }
  .diff .add { background: color-mix(in srgb, var(--ok) 16%, transparent); color: var(--ok); }
  .diff .same { color: var(--muted); }
  .status { font-size: 12.5px; margin-left: 8px; }
  .status.run { color: var(--accent); }
  .status.ok { color: var(--ok); }
  .status.bad { color: var(--bad); }
  #edit-banner { position: sticky; top: 56px; z-index: 5; margin-bottom: 12px; padding: 8px 12px; border-radius: 8px;
    border: 1px solid var(--accent); color: var(--accent); background: var(--panel); font-size: 12.5px; }
  /* Editable in place, but not shouting about it until you are in it. */
  .namefld { background: none; border: 1px solid transparent; border-radius: 6px;
    color: var(--ink); font: inherit; padding: 3px 6px; width: 100%; max-width: 240px; }
  .namefld:hover { border-color: var(--line); }
  .namefld:focus { border-color: var(--accent); background: var(--bg); outline: none; }
  .namefld::placeholder { color: var(--muted); font-style: italic; }
  .fld { padding: 7px 10px; border: 1px solid var(--line); border-radius: 6px;
    background: var(--bg); color: var(--ink); font: inherit; }
  .back { display: inline-block; margin-bottom: 14px; color: var(--accent); cursor: pointer; font-size: 13px; }
  .back:hover { text-decoration: underline; }
  /* The per-machine page carries five very different kinds of information.
     Stacking them made a page you had to scroll past to reach anything, so
     each is its own pane behind a tab. */
  /* Reading a machine page means scrolling, and the way back was at the top
     of it. The header, the name and the tabs stay put. */
  .machine-head { position: sticky; top: 51px; z-index: 6; background: var(--bg);
    padding-top: 10px; margin-bottom: 18px;
    box-shadow: 0 6px 12px -12px rgba(0, 0, 0, .8); }
  .machine-head .hdr { margin-bottom: 10px; }
  .subnav { display: flex; gap: 4px; flex-wrap: wrap; border-bottom: 1px solid var(--line);
    margin: 0; padding-bottom: 8px; background: var(--bg); }
  .subnav button {
    background: none; border: 1px solid transparent; color: var(--muted);
    padding: 6px 12px; border-radius: 6px; cursor: pointer; font: inherit;
  }
  .subnav button:hover { color: var(--ink); }
  .subnav button.active { color: var(--ink); border-color: var(--line); background: var(--panel); }
  .subnav .n { font-family: var(--mono); font-size: 11.5px; color: var(--muted); margin-left: 5px; }
  .subnav button.active .n { color: var(--accent); }
  .subnav .n.warn { color: var(--warn); }
  .hdr { display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap; margin-bottom: 16px; }
  .hdr h2 { margin: 0; font-size: 20px; letter-spacing: -0.01em; }
  .grid2 { display: grid; gap: 20px; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); align-items: start; }
  dl.kv { margin: 0; padding: 4px 0; }
  dl.kv > div { display: flex; gap: 12px; padding: 7px 14px; border-bottom: 1px solid var(--line); }
  dl.kv > div:last-child { border-bottom: none; }
  dl.kv dt { flex: 0 0 130px; color: var(--muted); font-size: 12.5px; }
  dl.kv dd { margin: 0; font-size: 13px; word-break: break-word; }

  #gate { max-width: 380px; margin: 80px auto; }
  #gate input { width: 100%; padding: 9px 11px; margin: 10px 0; border: 1px solid var(--line); border-radius: 7px; background: var(--panel); color: var(--ink); font-family: var(--mono); font-size: 13px; }
</style>
</head>
<body>

<div id="gate" class="card" hidden>
  <h2>Admin token</h2>
  <div style="padding:14px">
    <p class="msg">The portal prints this on startup.</p>
    <input id="token-input" type="password" placeholder="admin token" autocomplete="off">
    <button class="act primary" onclick="saveToken()">Connect</button>
    <span id="gate-msg" class="msg"></span>
  </div>
</div>

<div id="app" hidden>
  <header>
    <h1>PatchPanel</h1>
    <span class="rev" id="rev"></span>
    <span class="pill" id="paused" hidden
      title="You are typing, or have something selected. The page refreshes every few seconds and that would discard it, so it is holding still until you are done.">live updates paused</span>
    <nav>
      <button data-tab="fleet" class="active">Fleet<span class="badge" id="badge-fleet" hidden></span></button>
      <button data-tab="devices">Devices<span class="badge" id="badge-devices" hidden></span></button>
      <button data-tab="add">Add machine</button>
      <button data-tab="manifest">Manifest</button>
      <button data-tab="activity">Activity</button>
    </nav>
  </header>

  <main>
    <section id="fleet" class="active">
      <div class="tiles" id="tiles"></div>
      <div class="card">
        <h2>Agents</h2>
        <div class="scroll"><table>
          <thead><tr>
            <th>Host</th><th>IP</th><th>OS</th><th>Site</th>
            <th>Updates</th><th>Drift</th><th>Devices</th><th>Rev</th><th>Seen</th><th></th>
          </tr></thead>
          <tbody id="agents"></tbody>
        </table></div>
        <div class="empty" id="agents-empty" hidden>No agents have enrolled yet.</div>
      </div>
      <div class="card">
        <h2>Fleet actions</h2>
        <div class="bar">
          <button class="act" title="Re-read packages and updates on every connected machine. Changes nothing."
            onclick="broadcast('collect_inventory')">Rescan all</button>
          <button class="act" title="Make every machine's applications match the manifest."
            onclick="broadcast('apply_manifest')">Apply manifest to all</button>
          <button class="act" title="Re-probe every declared IoT device now."
            onclick="broadcast('probe_devices')">Probe devices</button>
          <button class="act" title="Sweep the manifest's discovery ranges for undeclared devices."
            onclick="broadcast('discover')">Run discovery</button>
          <span id="broadcast-msg" class="msg"></span>
        </div>
      </div>
    </section>

    <section id="agent">
      <div id="agent-body"></div>
    </section>

    <section id="devices">
      <div class="card">
        <h2>Devices</h2>
        <div id="devices-eol" hidden></div>
        <div class="scroll"><table>
          <thead><tr>
            <th>Device</th><th>Target</th><th>Site</th><th>Status</th>
            <th>Firmware</th><th>Updates</th><th>Expected</th><th>Latency</th><th></th><th>Collector</th><th>Checked</th>
          </tr></thead>
          <tbody id="device-rows"></tbody>
        </table></div>
        <div class="empty" id="devices-empty" hidden>
          No devices yet. Add a firewall or NAS under <b>Add machine</b>.
        </div>
      </div>
      <div class="card" id="device-history" hidden></div>

      <div class="card">
        <h2>Seen on the network, not in the manifest</h2>
        <div class="scroll"><table>
          <thead><tr><th>Address</th><th>Open ports</th><th>Banner</th><th>Site</th><th>Found by</th></tr></thead>
          <tbody id="unmanaged-rows"></tbody>
        </table></div>
        <div class="empty" id="unmanaged-empty" hidden>
          Nothing unaccounted for. Add a <code>discovery</code> range to the manifest to sweep for devices.
        </div>
      </div>
    </section>

    <section id="add">
      <div class="card">
        <h2>Add a machine</h2>
        <div class="step">
          <div class="note">
            Enrolling only makes a machine <b>report</b> what it has installed.
            Nothing is installed, upgraded, or rebooted until you publish a
            manifest that asks for it.
          </div>
          <div class="field">
            <label for="add-portal">Portal address</label>
            <input id="add-portal" spellcheck="false" size="24">
            <label for="add-site" style="margin-left:8px">Site</label>
            <input id="add-site" value="homelab" spellcheck="false" size="14">
          </div>
          <div id="add-hint" class="msg"></div>
        </div>
      </div>

      <div class="card">
        <h2>Linux &mdash; run as root</h2>
        <div class="step">
          <div class="cmd"><button onclick="copyCmd('cmd-linux')">Copy</button><pre id="cmd-linux"></pre></div>
        </div>
      </div>

      <div class="card">
        <h2>Windows &mdash; run in an elevated PowerShell</h2>
        <div class="step">
          <div class="cmd"><button onclick="copyCmd('cmd-win')">Copy</button><pre id="cmd-win"></pre></div>
          <p style="margin-top:10px">For Windows Update patching as well as winget app updates, the
             target also needs <code>Install-Module PSWindowsUpdate -Force -Scope AllUsers</code>.</p>
        </div>
      </div>

      <div class="card">
        <h2>What the installer does</h2>
        <div class="step">
          <p>Downloads the agent from this portal, enrols it, installs a service that
             restarts on failure, and starts it. Re-running upgrades in place and keeps
             the machine's existing identity.</p>
        </div>
      </div>
      <div class="card">
        <h2>Appliances &mdash; no agent</h2>
        <div class="step">
          <div class="note">
            A firewall or a NAS is the machine you least want to install software on, and
            often cannot: they run from read-only images, or wipe anything added at the next
            firmware update. PatchPanel asks these over their own API instead. Nothing is
            installed and nothing is written to the device.
          </div>
          <div class="bar" style="padding-left:0;padding-right:0;gap:10px;flex-wrap:wrap">
            <select id="dev-kind" onchange="deviceKindChanged()" class="fld">
              <option value="opnsense">OPNsense firewall</option>
              <option value="unraid">Unraid server</option>
            </select>
            <input id="dev-url" class="fld" style="min-width:320px"
              placeholder="https://router.example.net" oninput="deviceUrlChanged()">
            <input id="dev-name" class="fld" style="width:190px" placeholder="name, e.g. edge firewall">
            <input id="dev-id" class="fld" style="min-width:260px" placeholder="id"
              oninput="this.dataset.touched = '1'">
          </div>

          <div class="bar" style="padding-left:0;padding-right:0;gap:10px;flex-wrap:wrap">
            <label class="msg" for="dev-keyfile" id="dev-keyfile-label">API key file</label>
            <input type="file" id="dev-keyfile" accept=".txt,text/plain"
              onchange="readKeyFile(this)" class="fld">
            <span class="status" id="dev-key-status"></span>
          </div>
          <div class="bar" style="padding-left:0;padding-right:0;gap:10px;flex-wrap:wrap">
            <label class="msg" for="dev-key">or paste the key</label>
            <input id="dev-key" class="fld" style="min-width:340px" placeholder="api key"
              oninput="pastedKey()">
            <input id="dev-secret" class="fld" style="min-width:340px" placeholder="api secret"
              oninput="pastedKey()">
          </div>

          <div class="bar" style="padding-left:0;padding-right:0">
            <button class="act primary" onclick="addDevice()">Add device</button>
            <span class="status" id="dev-add-status"></span>
          </div>
          <div class="msg" id="dev-hint"></div>
        </div>
      </div>


    </section>

    <section id="manifest">
      <div class="card">
        <h2>Desired state</h2>
        <textarea id="manifest-doc" spellcheck="false"></textarea>
        <div class="bar">
          <button class="act primary" onclick="saveManifest()">Publish</button>
          <button class="act" onclick="loadManifest()">Reload</button>
          <span id="manifest-msg" class="msg"></span>
        </div>
      </div>
      <div class="card">
        <h2>Agent builds</h2>
        <div class="scroll"><table>
          <thead><tr><th>Version</th><th>OS</th><th>Arch</th><th>URL</th><th>sha256</th></tr></thead>
          <tbody id="build-rows"></tbody>
        </table></div>
        <div class="empty" id="builds-empty" hidden>
          No builds published. POST to <code>/api/builds</code>, then set
          <code>agent_version</code> in the manifest to roll the fleet.
        </div>
      </div>
    </section>

    <section id="activity">
      <div class="card">
        <h2>Recent commands</h2>
        <div id="commands"></div>
        <div class="empty" id="commands-empty" hidden>Nothing has run yet.</div>
      </div>
    </section>
  </main>
</div>

<script>
const $ = (id) => document.getElementById(id);
let TOKEN = localStorage.getItem("pp_token") || "";
// Read from the URL so a refresh, a bookmark, or a shared link all land on the
// tab you were actually looking at.
const TABS = ["fleet", "devices", "add", "manifest", "activity"];
// `#agent/<uuid>` opens one machine's page; anything else is a tab.
// `#agent/<uuid>` opens a machine, `#agent/<uuid>/<pane>` opens it on one of
// its tabs, so a bookmark or a shared link lands exactly where you were.
function routeOf(hash) {
  const h = (hash || "").replace(/^#/, "");
  if (h.startsWith("agent/")) {
    const [id, pane] = h.slice(6).split("/");
    return { tab: "agent", id, pane: pane || null };
  }
  return { tab: TABS.includes(h) ? h : "fleet", id: null, pane: null };
}
let ROUTE = routeOf(location.hash);
let TAB = ROUTE.tab;
let PANE = ROUTE.pane || "overview";
let AGENTS = [];
let REV = 0;
let AUTH_REQUIRED = true;
// Agents we have just sent a command to. The portal only learns a command is
// running once it records it, and the table refreshes every few seconds, so
// without a local latch the same button can be pressed twice in that window.
const JUST_SENT = new Map();
const SENT_GRACE_MS = 20000;

function isBusy(a) {
  if (a.running) return a.running.kind;
  const t = JUST_SENT.get(a.id);
  if (t && Date.now() - t < SENT_GRACE_MS) return "dispatching";
  if (t) JUST_SENT.delete(a.id);
  return null;
}

// Human wording for a command kind.
const KIND_LABEL = {
  collect_inventory: "scanning",
  apply_patches: "installing updates",
  apply_manifest: "applying manifest",
  self_update: "updating agent",
  probe_devices: "probing devices",
  discover: "discovering",
  cleanup: "cleaning up",
  distro_check: "checking upgrade readiness",
  finish_upgrade: "finishing the upgrade",
  update_firmware: "flashing firmware",
  distro_upgrade: "upgrading the release",
  reboot: "rebooting",
  dispatching: "dispatching",
};

async function api(path, opts = {}) {
  const res = await fetch(path, {
    ...opts,
    headers: { "Authorization": "Bearer " + TOKEN, "Content-Type": "application/json", ...(opts.headers || {}) },
  });
  if (res.status === 401 && AUTH_REQUIRED) {
    gate("That token was not accepted.");
    throw new Error("unauthorized");
  }
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || res.statusText);
  return body;
}

function gate(msg) {
  $("app").hidden = true;
  $("gate").hidden = false;
  $("gate-msg").textContent = msg || "";
  $("gate-msg").className = "msg bad";
}

function saveToken() {
  TOKEN = $("token-input").value.trim();
  localStorage.setItem("pp_token", TOKEN);
  $("gate").hidden = true;
  $("app").hidden = false;
  refresh();
}

document.addEventListener("input", (e) => {
  if (e.target && (e.target.id === "add-site" || e.target.id === "add-portal")) loadAdd();
});

function showTab(tab, id, pane) {
  ROUTE = { tab, id: id || null, pane: pane || null };
  TAB = tab;
  if (tab === "agent") PANE = pane || "overview";
  const want = id ? `agent/${id}${pane ? "/" + pane : ""}` : tab;
  // No nav button is highlighted on a detail page; it is not a tab.
  document.querySelectorAll("nav button").forEach((x) =>
    x.classList.toggle("active", x.dataset.tab === tab));
  document.querySelectorAll("section").forEach((s) =>
    s.classList.toggle("active", s.id === tab));
  if (location.hash.slice(1) !== want) location.hash = want;
  refresh();
}

function openAgent(id) { showTab("agent", id); }

// The machine currently being compared against on the Sources tab, and its
// data. Held outside the render so the five-second refresh does not drop it.
// A scan is not a change, so the history hides them by default - but they are
// the record of when a machine was last actually looked at, which is worth
// being able to see.
let HISTORY_ALL = false;

function setHistoryAll(on) {
  HISTORY_ALL = !!on;
  refresh();
}

let COMPARE = null;
let COMPARE_DATA = null;

async function setCompare(id) {
  COMPARE = id || null;
  COMPARE_DATA = null;
  if (COMPARE) {
    try {
      COMPARE_DATA = await api(`/api/agents/${COMPARE}`);
    } catch (e) {
      COMPARE = null;
      alert(e.message);
    }
  }
  refresh();
}

// Switching pane is a pure DOM toggle: every pane is already rendered, so the
// change is instant and nothing you had open, filtered, or typed is lost.
function showPane(pane) {
  PANE = pane;
  ROUTE.pane = pane;
  const want = `agent/${ROUTE.id}/${pane}`;
  if (location.hash.slice(1) !== want) location.hash = want;
  applyPane();
}

function applyPane() {
  const body = $("agent-body");
  const panes = [...body.querySelectorAll("[data-pane]")];
  if (!panes.length) return;
  // A machine with no apt sources has no Sources pane; fall back rather than
  // showing a blank page to anyone who kept the link.
  if (!panes.some((e) => e.dataset.pane === PANE)) PANE = panes[0].dataset.pane;
  panes.forEach((e) => { e.hidden = e.dataset.pane !== PANE; });
  body.querySelectorAll(".subnav button").forEach((b) =>
    b.classList.toggle("active", b.dataset.p === PANE));
}

document.querySelectorAll("nav button").forEach((b) => {
  b.onclick = () => showTab(b.dataset.tab);
});

window.addEventListener("hashchange", () => {
  const r = routeOf(location.hash);
  if (r.tab !== ROUTE.tab || r.id !== ROUTE.id) { showTab(r.tab, r.id, r.pane); return; }
  // Same machine, different pane (a back button press, usually): no reload.
  if (r.tab === "agent" && (r.pane || "overview") !== PANE) {
    PANE = r.pane || "overview";
    ROUTE.pane = r.pane;
    applyPane();
  }
});

// Re-rendering on a timer is what makes the dashboard live, and also what
// closes an <details> you were reading. Two defences: skip the write entirely
// when nothing changed, and carry the open ones across when it did.
// True while the operator has unsaved text in any editor on the page.
// Re-rendering under them would discard it, and this page refreshes every few
// seconds, which made editing a sources file effectively impossible.
function isEditing() {
  return [...document.querySelectorAll("textarea[data-dirty=\"1\"]")].length > 0;
}

/// Is the operator in the middle of something inside this element?
///
/// Restoring focus and the caret after a rewrite is not enough - the field is
/// a different element afterwards, the page reflows, and anything highlighted
/// is gone. Typing, a dropdown they are working with, or text they have
/// selected to copy all mean the same thing: leave the page alone until they
/// are done with it.
function typingIn(el) {
  if (!el) return false;

  const a = document.activeElement;
  if (a && el.contains(a)) {
    if (a.tagName === "TEXTAREA" || a.tagName === "INPUT" || a.tagName === "SELECT") return true;
    if (a.isContentEditable) return true;
  }

  // Selected text lives only in the DOM nodes it spans; replacing them drops
  // it, which is maddening halfway through copying an error message.
  try {
    const sel = window.getSelection && window.getSelection();
    if (sel && sel.rangeCount && !sel.isCollapsed) {
      const r = sel.getRangeAt(0);
      if (el.contains(r.commonAncestorContainer)) return true;
    }
  } catch (e) {
    // No selection API here; nothing to preserve.
  }
  return false;
}

function setHTML(el, html) {
  if (!el || el.__lastHTML === html) return false;
  if (typingIn(el)) {
    // Say so, rather than leaving a page that has quietly stopped updating.
    showPaused(true);
    return false;
  }
  showPaused(false);
  const open = new Set(
    [...el.querySelectorAll("details[open][data-k]")].map((d) => d.dataset.k)
  );
  const active = document.activeElement;
  const keepId = active && el.contains(active) ? active.id : null;
  const caret = keepId && "selectionStart" in active ? active.selectionStart : null;

  // A running command's output grows every few seconds, and rewriting the
  // element sends its scrollbar back to the top - so a long job like a release
  // upgrade shows you its first minute, over and over, while the part you want
  // is the end. Remember where each log was and put it back; a log that was at
  // the bottom stays at the bottom, which is what following output means.
  const logs = new Map();
  el.querySelectorAll("pre[data-k]").forEach((pre) => {
    logs.set(pre.dataset.k, {
      bottom: pre.scrollHeight - pre.scrollTop - pre.clientHeight < 24,
      top: pre.scrollTop,
    });
  });

  // Replacing the contents collapses the page to nothing for an instant, and
  // the browser clamps the scroll position to the now much shorter document -
  // so reading anything below the fold meant being thrown back to the top
  // every few seconds.
  //
  // Reading scrollY back straight afterwards is not enough: layout has not
  // been recalculated yet, so it still reports the old value and the clamp
  // lands after the check. Hold the element's height across the swap so the
  // document never shrinks, and restore unconditionally either side of the
  // next frame.
  const y = window.scrollY;
  const held = el.offsetHeight;
  if (held) el.style.minHeight = held + "px";

  el.__lastHTML = html;
  el.innerHTML = html;

  window.scrollTo(0, y);
  requestAnimationFrame(() => {
    el.style.minHeight = "";
    if (Math.abs(window.scrollY - y) > 1) window.scrollTo(0, y);
  });

  el.querySelectorAll("pre[data-k]").forEach((pre) => {
    const was = logs.get(pre.dataset.k);
    // Anything still running is followed from the moment it appears.
    if (!was) {
      if (pre.dataset.live === "1") pre.scrollTop = pre.scrollHeight;
      return;
    }
    pre.scrollTop = was.bottom ? pre.scrollHeight : was.top;
  });

  el.querySelectorAll("details[data-k]").forEach((d) => {
    if (open.has(d.dataset.k)) d.open = true;
  });
  if (keepId) {
    const again = document.getElementById(keepId);
    if (again) {
      again.focus();
      if (caret !== null && "setSelectionRange" in again) {
        try { again.setSelectionRange(caret, caret); } catch (e) { /* not a text field */ }
      }
    }
  }
  return true;
}

function tailLog(details) {
  const pre = details.querySelector("pre[data-live=\"1\"]");
  if (pre && details.open) pre.scrollTop = pre.scrollHeight;
}

function showPaused(on) {
  const el = $("paused");
  if (el) el.hidden = !on;
}

// A string safe to drop into an inline handler.
//
// `JSON.stringify` produces double quotes, and a double quote inside a
// double-quoted HTML attribute ends the attribute - the browser then throws
// SyntaxError on click and the button silently does nothing. This quotes with
// apostrophes and escapes for both layers, JavaScript first and HTML second.
function jsq(value) {
  const js = String(value ?? "")
    .replace(/\\/g, "\\\\")
    .replace(/'/g, "\\'")
    .replace(/\r?\n/g, "\\n");
  return "'" + js.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;") + "'";
}

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

function ago(iso) {
  if (!iso) return "-";
  const s = Math.max(0, (Date.now() - new Date(iso)) / 1000);
  if (s < 60) return Math.round(s) + "s ago";
  if (s < 3600) return Math.round(s / 60) + "m ago";
  if (s < 86400) return Math.round(s / 3600) + "h ago";
  return Math.round(s / 86400) + "d ago";
}

function tile(n, label, cls) {
  return `<div class="tile ${n > 0 && cls ? cls : ""}"><div class="n">${n}</div><div class="l">${label}</div></div>`;
}

async function loadFleet() {
  const d = await api("/api/fleet");
  AGENTS = d.agents;
  REV = d.manifest_revision;
  $("rev").textContent = "manifest r" + d.manifest_revision;

  const s = d.summary;

  // A count of machines and devices with something waiting. Deliberately a
  // count of *things needing attention*, not of updates: forty packages on one
  // box is one thing to go and do.
  const badge = (id, n, bad, title) => {
    const el = $(id);
    if (!el) return;
    el.hidden = !n;
    el.textContent = n > 99 ? "99+" : String(n);
    el.className = "badge" + (bad ? " bad" : "");
    el.title = title;
  };
  badge("badge-fleet", s.agents_pending || 0, false,
    `${s.agents_pending || 0} machine(s) with updates to install`);
  badge("badge-devices", (s.devices_pending || 0) + (s.devices_eol || 0), (s.devices_eol || 0) > 0,
    `${s.devices_pending || 0} device(s) with updates` +
    ((s.devices_eol || 0) ? `, ${s.devices_eol} at end of life` : ""));
  $("tiles").innerHTML =
    tile(s.online, "online") +
    tile(s.offline, "offline", "warn") +
    tile(s.pending_security, "security updates", "bad") +
    tile(s.pending_updates, "updates to install", "warn") +
    (s.deferred_updates ? tile(s.deferred_updates, "phased, not yet offered") : "") +
    tile(s.needs_reboot, "need reboot", "warn") +
    tile(s.app_drift, "app drift", "warn") +
    tile(s.stale_manifest, "stale manifest", "warn") +
    tile(s.devices, "devices") +
    tile(s.devices_unreachable, "devices down", "bad") +
    tile(s.devices_drifted, "firmware drift", "warn");

  $("agents-empty").hidden = d.agents.length > 0;
  const agentRows = d.agents.map((a) => {
    const live = a.connected ? "on" : (a.online ? "on" : "off");
    const busy = isBusy(a);
    const canAct = a.connected && !busy;
    const busyPill = busy
      ? ` <span class="pill busy" title="A command is already running on this machine. Wait for it to finish before starting another.">${esc(KIND_LABEL[busy] || busy)}</span>`
      : "";
    // Show the total, then flag the security subset in full words. The old
    // form rendered "1 sec 0" - which reads as a duration, and buried the
    // total behind an unexplained subtraction.
    const unsafe_ = a.release_blockers > 0
      ? ` <span class="pill bad" title="This machine's package sources are misconfigured; installing updates could break it. Open the machine for details.">unsafe</span>`
      : "";
    const actionable = a.actionable_count;
    const stuck = a.deferred_count
      ? ` <span class="msg" title="${a.deferred_count} update(s) the archive is withholding from this machine - a phased rollout. Nothing to do; they arrive on their own.">+${a.deferred_count} phased</span>`
      : "";
    const held = a.held_back_count
      ? ` <span class="pill warn" title="${a.held_back_count} upgrade(s) apt will not apply without a full upgrade - often a kernel">${a.held_back_count} held</span>`
      : "";
    // Zero updates from a machine we could not scan is not "clean", it is
    // "unknown". Showing 0 there is the most dangerous thing this table could do.
    const upd = a.scan_issue_count
      ? `<span class="pill bad" title="Some package backends could not be scanned on this machine, so the real number is unknown. Open the machine for details.">not scanned</span>`
      : (actionable
          ? `${actionable}${a.security_count
              ? ` <span class="pill bad" title="${a.security_count} of these are security updates - patch these first">${a.security_count} security</span>`
              : ""}${held}`
          : (a.deferred_count
              // Everything pending is withheld by the archive, so there is
              // nothing to install and the button is deliberately dead.
              ? `<span class="msg">nothing to install</span>`
              : `<span class="msg">none</span>${held}`));
    const dev = a.device_count
      ? `${a.device_count}${a.device_problem_count ? ` <span class="pill bad">${a.device_problem_count}</span>` : ""}`
      : "-";
    const hw = a.hardware || {};
    // Hardware lives on the machine's own page; in a fleet list it is a very
    // wide column nobody scans. Keep it reachable as a tooltip.
    const spec = [
      hw.cpu_model,
      hw.cpu_threads ? `${hw.cpu_cores || "?"}c/${hw.cpu_threads}t` : "",
      hw.memory_mb ? `${(hw.memory_mb / 1024).toFixed(0)} GB` : "",
      hw.vendor,
    ].filter(Boolean).join(" · ");
    const ips = (hw.ip_addresses || []).length
      ? `${esc(hw.ip_addresses[0])}${hw.ip_addresses.length > 1 ? `<div class="msg">+${hw.ip_addresses.length - 1} more</div>` : ""}`
      : "-";
    return `<tr title="${esc(spec)}">
      <td><span class="dot ${live}"></span><a href="#agent/${a.id}" style="color:inherit">${esc(a.hostname)}</a>${a.reboot_required ? ' <span class="pill warn">reboot</span>' : ""}${
        a.guest_count ? ` <span class="pill" title="Hosts ${a.guest_count} virtual machine(s), ${a.unmanaged_guests} without an agent">${a.guest_count} VMs</span>` : ""}
          <div class="msg">agent ${esc(a.agent_version)}</div></td>
      <td class="mono">${ips}</td>
      <td title="${esc(a.os_version)} ${esc(a.arch)}">${esc(a.os_version.length > 22 ? a.os_version.slice(0, 21) + "…" : a.os_version)}
          <div class="msg mono">${esc(a.arch)}</div></td>
      <td>${esc(a.site) || "-"}</td>
      <td>${upd}${unsafe_}${held ? "" : ""}${stuck}${busyPill}</td>
      <td>${a.drift_count ? `<span class="pill warn">${a.drift_count}</span>` : "-"}</td>
      <td>${dev}</td>
      <td class="mono">${a.applied_revision < REV ? `<span class="pill warn">r${a.applied_revision}</span>` : "r" + a.applied_revision}</td>
      <td>${ago(a.last_seen)}</td>
      <td style="text-align:right; white-space:nowrap">
        <button class="act" ${canAct ? "" : "disabled"}
          title="${busy ? "Busy: " + esc(KIND_LABEL[busy] || busy) : "Re-read installed packages and check for available updates. Changes nothing."}"
          onclick="cmd('${a.id}','collect_inventory')">Rescan</button>
        <button class="act" ${canAct && actionable ? "" : "disabled"}
          title="${busy
            ? "Busy: " + esc(KIND_LABEL[busy] || busy)
            : (actionable
                ? "Install this machine's pending OS updates now. This changes the system and may require a reboot."
                : (a.deferred_count
                    ? "Nothing to install: the " + a.deferred_count + " pending update(s) are phased and the archive is withholding them from this machine."
                    : "Nothing to install."))}"
          onclick="patchNow('${a.id}','${esc(a.hostname)}')">Install updates</button>
        <button class="act" ${canAct ? "" : "disabled"}
          title="${busy ? "Busy: " + esc(KIND_LABEL[busy] || busy) : "Install, upgrade or remove applications so the machine matches the manifest."}"
          onclick="cmd('${a.id}','apply_manifest')">Apply manifest</button>
      </td>
    </tr>`;
  }).join("");
  setHTML($("agents"), agentRows);
}

function since(iso) {
  if (!iso) return "-";
  const s = Math.max(0, (Date.now() - new Date(iso)) / 1000);
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m`;
}

function kv(rows) {
  return `<dl class="kv">` + rows
    .filter(([, v]) => v !== null && v !== undefined && v !== "")
    .map(([k, v]) => `<div><dt>${k}</dt><dd>${v}</dd></div>`)
    .join("") + `</dl>`;
}

// Updates somebody decided not to install.
//
// Pinned to the version they decided about, so this is a judgement about one
// release rather than a package silently disappearing from the fleet's view
// forever. When a newer version is published it comes back by itself.
function ignoredCard(ignored, id) {
  if (!ignored.length) return "";
  return `<div class="card">
    <h2>Ignored &mdash; ${ignored.length}</h2>
    <div class="step">
      <div class="msg">These are not counted as pending. Each is set aside at the exact version
        below; if a newer one is published it appears again as a new update.</div>
    </div>
    <div class="scroll scrolly"><table>
      <thead><tr><th>Package</th><th>Version ignored</th><th>Source</th><th>Since</th><th></th></tr></thead>
      <tbody>${ignored.map((x) => `<tr>
        <td>${esc(x.name)}</td>
        <td class="mono">${esc(x.version)}</td>
        <td class="mono msg">${esc(x.source)}</td>
        <td class="msg">${ago(x.ignored_at)}</td>
        <td><button class="act" style="padding:2px 8px;font-size:11.5px"
          onclick="unignoreUpdate(${jsq(id)}, ${jsq(x.name)}, ${jsq(x.version)})">Show again</button></td>
      </tr>`).join("")}</tbody>
    </table></div>
  </div>`;
}

async function ignoreUpdate(id, name, source, version) {
  if (!confirm(
    "Stop showing " + name + " " + version + " on this machine?\n\n" +
    "It will not be counted as pending, and Install updates will still install it if you " +
    "run one. If a newer version is published it comes back."
  )) return;
  try {
    await api(`/api/agents/${id}/ignores`, {
      method: "POST",
      body: JSON.stringify({ name, source, version }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function unignoreUpdate(id, name, version) {
  try {
    await api(`/api/agents/${id}/ignores`, {
      method: "DELETE",
      body: JSON.stringify({ name, version }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

function scanCard(issues, held, deferred, id, connected) {
  if (!issues.length && !held.length && !deferred.length) return "";
  return `<div class="card">
    <h2>Coverage</h2>
    ${issues.length ? `<div class="step">
      <div class="note" style="border-color:var(--bad)">
        <b>This machine's update count is incomplete.</b> ${issues.length} backend(s)
        could not be scanned, so treat the number as a floor, not a total.
      </div>
      ${issues.map((i) => `<div style="margin-top:10px">
        <h3><span class="pill bad">${esc(i.backend)}</span> ${esc(i.problem)}</h3>
        ${i.remedy ? `<pre>${esc(i.remedy)}</pre>` : ""}
      </div>`).join("")}
      ${issues.some((i) => i.backend === "winget" || i.backend === "windowsupdate" || i.backend === "fwupd")
        ? `<button class="act" ${connected ? "" : "disabled"} style="margin-top:10px"
             title="Installs whatever this machine needs to be fully scannable: fwupd on Linux, PSWindowsUpdate and a system-wide winget on Windows."
             onclick="installPrereqs('${id}')">Install the missing tooling</button>
           <button class="act" ${connected ? "" : "disabled"} style="margin-top:10px"
             title="Backends are detected once at startup, so a restart is needed after installing tooling."
             onclick="restartAgent('${id}')">Restart agent</button>`
        : ""}
    </div>` : ""}
    ${deferred.length ? `<div class="step">
      <h3><span class="pill">phased</span> ${deferred.length} update(s) the archive is withholding</h3>
      <p>Ubuntu rolls an update out to a percentage of machines at a time, and this machine is not
         in the cohort yet. Nothing installs these &mdash; not <code>apt upgrade</code>, not
         <code>full-upgrade</code> &mdash; until the rollout reaches it, which it will on its own.
         Some of them apt merely calls "kept back"; they are listed here because what they are
         waiting on is itself phased, so a full upgrade cannot help them either. There is nothing
         to fix, and a patch run that appears to do nothing is in fact correct.</p>
      <pre>${esc(deferred.join(" "))}</pre>
    </div>` : ""}
    ${held.length ? `<div class="step">
      <h3><span class="pill warn">held back</span> ${held.length} upgrade(s) apt will not apply</h3>
      <p>A plain <code>apt upgrade</code> refuses anything needing new packages installed &mdash;
         typically a kernel metapackage. These stay pending forever until a full upgrade runs,
         which may also remove packages, so it is a deliberate action.</p>
      <ul style="margin:8px 0 0 18px">${held.map((h) => `<li class="mono">${esc(h)}</li>`).join("")}</ul>
      <button class="act" ${connected ? "" : "disabled"}
        title="Runs apt full-upgrade. This can install new packages and remove existing ones."
        onclick="fullUpgrade('${id}')">Run full upgrade</button>
    </div>` : ""}
  </div>`;
}

function cleanupCard(c, id, connected) {
  if (!c || (!(c.packages || []).length && !c.cache_bytes)) return "";
  const mb = (n) => `${(n / 1e6).toFixed(0)} MB`;
  return `<div class="card">
    <h2>Cleanup</h2>
    <div class="step">
      <p>${(c.packages || []).length} package(s) are installed only as dependencies nothing
         needs any more${c.reclaim_bytes ? `, holding ${mb(c.reclaim_bytes)}` : ""}.
         The package cache holds a further ${mb(c.cache_bytes || 0)}, which is always safe to delete.</p>
      ${(c.packages || []).length ? `<pre>${esc(c.packages.join(" "))}</pre>` : ""}
      <button class="act" ${connected ? "" : "disabled"}
        title="Runs apt autoremove and empties the package cache."
        onclick="cleanupNow('${id}')">Clean up</button>
    </div>
  </div>`;
}

// A machine dpkg stopped part-way through an upgrade.
//
// This is the state that looks like nothing is wrong: the update list is
// normal, the machine is up, and every single install fails. It goes at the
// top of the page because until it is resolved nothing else on the machine can
// be done at all.
function midUpgradeCard(mid, id, connected, busy) {
  if (!mid || !(mid.packages || []).length) return "";

  const disks = mid.disks || [];
  const picker = mid.grub_stuck && disks.length
    ? `<div class="step">
        <h3>Which disk should the bootloader be installed to?</h3>
        <p>The recorded answer points at a device that no longer exists, which is why
           <span class="mono">grub</span> could not finish. This is usually the disk the
           machine boots from. Getting it wrong does not damage anything, but the machine
           may not boot until it is corrected, so check it against the sizes below.</p>
        <select id="grub-dev"
          style="padding:6px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font:inherit;font-family:var(--mono)">
          ${disks.map((k) => `<option value="${esc(k.path)}">${esc(k.path)} &mdash; ${esc(k.size)}${k.model ? " " + esc(k.model) : ""}</option>`).join("")}
        </select>
        ${disks.some((k) => (k.by_id || []).length) ? `<div class="msg" style="margin-top:8px">
          Stable names, for recognising the one grub had recorded:
          ${disks.map((k) => (k.by_id || []).map((n) =>
            `<div class="mono">${esc(k.path)} = ${esc(n)}</div>`).join("")).join("")}
        </div>` : ""}
      </div>`
    : "";

  return `<div class="card" style="border-color:var(--bad)">
    <h2>This machine is part-way through an upgrade</h2>
    <div class="step">
      <div class="note" style="border-color:var(--bad)">
        <b>dpkg stopped and will not do anything else until this is resolved.</b>
        ${mid.packages.length} package(s) are unpacked but not configured. Installing updates,
        applying the manifest and finishing the release upgrade all fail while this is true,
        and none of them will say why.
      </div>
      <ul style="margin:10px 0 0 18px">${mid.packages.map((k) => `<li class="mono">${esc(k)}</li>`).join("")}</ul>
    </div>
    ${picker}
    <div class="bar">
      <button class="act primary" ${connected && !busy ? "" : "disabled"}
        title="Runs dpkg --configure -a, then apt-get -f install, then continues the upgrade."
        onclick="finishUpgrade('${id}', ${mid.grub_stuck && disks.length ? "true" : "false"})">Finish the upgrade</button>
      <span class="status" id="finish-status"></span>
    </div>
  </div>`;
}

async function finishUpgrade(id, withDisk) {
  const dev = withDisk ? ($("grub-dev") || {}).value : null;
  if (!confirm(
    "Finish the interrupted upgrade?" +
    (dev ? "\n\nThe bootloader will be installed to " + dev + "." : "") +
    "\n\nThis configures what dpkg left unfinished and then carries on with the upgrade. " +
    "It can take a while, and the machine will need a reboot afterwards."
  )) return;

  const st = $("finish-status");
  const say = (t, c) => { if (st) st.innerHTML = `<span class="${c}">${esc(t)}</span>`; };
  say("working\u2026 this can take several minutes", "run");
  try {
    const res = await cmd(id, "finish_upgrade", dev ? { grub_device: dev } : {});
    if (!res || !res.id) throw new Error("the portal did not accept the command");
    const done = await awaitCommand(id, res.id, 3600000);
    if (!done) { say("still running \u2014 see the History tab", "run"); return; }
    say(done.ok ? "finished \u2014 reboot when convenient" : (done.summary || "still stuck"),
      done.ok ? "ok" : "bad");
  } catch (e) {
    say(e.message, "bad");
  }
}

// What this machine hosts, or what hosts it.
//
// A hypervisor is the one machine whose patch state affects every other
// machine on it, and its guest list is where unmanaged machines show up -
// which is exactly what a fleet tool is otherwise blind to.
function virtCard(virt) {
  if (!virt) return "";
  const guests = virt.guests || [];

  if (!guests.length) {
    // Being a guest is a single fact, not a card's worth of information.
    return `<div class="card"><h2>Virtualization</h2><div class="step">
      ${kv([["Role", esc(virt.role)], ["Platform", esc(virt.platform)]])}
      ${virt.note ? `<div class="msg">${esc(virt.note)}</div>` : ""}
    </div></div>`;
  }

  const running = guests.filter((g) => (g.state || "").toLowerCase().startsWith("running"));
  const unmanaged = guests.filter((g) => !g.managed);
  const unmanagedRunning = unmanaged.filter((g) =>
    (g.state || "").toLowerCase().startsWith("running"));

  return `<div class="card">
    <h2>Virtual machines &mdash; ${guests.length} on this host, ${running.length} running</h2>
    ${unmanagedRunning.length ? `<div class="step">
      <div class="note" style="border-color:var(--warn)">
        <b>${unmanagedRunning.length} running guest(s) have no agent.</b> PatchPanel cannot see
        what they are running or whether they are patched. They are counted here because a
        hypervisor is the one place the gap is visible at all &mdash; from anywhere else an
        unmanaged VM is simply invisible.
      </div>
    </div>` : ""}
    <div class="scroll scrolly"><table>
      <thead><tr><th>Guest</th><th>Id</th><th>Type</th><th>State</th><th>PatchPanel</th></tr></thead>
      <tbody>${guests.map((g) => {
        const on = (g.state || "").toLowerCase().startsWith("running");
        return `<tr${on ? "" : ' style="opacity:.55"'}>
          <td>${esc(g.name)}</td>
          <td class="mono msg">${esc(g.id)}</td>
          <td class="mono msg">${esc(g.kind)}</td>
          <td>${esc(g.state) || "-"}</td>
          <td>${g.managed
            ? '<span class="pill ok">managed</span>'
            : (on ? '<span class="pill warn">no agent</span>' : '<span class="pill">no agent</span>')}</td>
        </tr>`;
      }).join("")}</tbody>
    </table></div>
    <div class="bar"><span class="msg">${esc(virt.platform)} &middot;
      ${guests.filter((g) => g.managed).length} of ${guests.length} have an agent${
        virt.note ? ` &middot; ${esc(virt.note)}` : ""}</span></div>
  </div>`;
}

// What the machine's own logs say about its last restart.
//
// Only shown when it was not a clean one: a machine that shut down properly
// needs no explanation, and a card saying so on every page would be noise.
function bootCard(boot) {
  if (!boot || !boot.unexpected) return "";
  return `<div class="card" style="border-color:var(--warn)">
    <h2>This machine restarted on its own</h2>
    <div class="step">
      <div class="note" style="border-color:var(--warn)">
        <b>${esc(boot.summary)}.</b> The previous shutdown was never started &mdash; nothing
        asked this machine to stop. Below is what its log held from before it went, which is
        the only record of it and will be gone when the journal rotates.
      </div>
      ${boot.detail ? `<pre>${esc(boot.detail)}</pre>` : ""}
    </div>
  </div>`;
}

// Firmware is deliberately its own card, away from the update buttons.
//
// Everything else here can be undone: a package reinstalled, a source file
// rolled back, a release upgrade restored from a snapshot. A firmware write
// cannot, and a failed one can leave hardware that does not come back. So it
// is never part of a patch run and never installed by the manifest - it is
// reported, and flashed only when somebody asks for it by name.
function firmwareCard(list, id, connected, busy) {
  if (!(list || []).length) return "";
  const reboot = list.filter((f) => f.needs_reboot).length;

  return `<div class="card">
    <h2>Firmware &mdash; ${list.length} update(s) available</h2>
    <div class="step">
      <div class="note" style="border-color:var(--warn)">
        <b>Firmware is not installed by patch runs.</b> A package can be rolled back and
        firmware cannot: a write that goes wrong can leave the device unusable. Read the
        vendor's notes, make sure the machine will not lose power part-way, and flash it
        deliberately.
        ${reboot ? `<br><br>${reboot} of these are written now and applied at the next boot.
          Some need a full power cycle rather than a warm reboot.` : ""}
      </div>
    </div>
    <div class="scroll scrolly"><table>
      <thead><tr><th>Device</th><th>Installed</th><th>Available</th><th></th></tr></thead>
      <tbody>${list.map((f) => `<tr>
        <td>${esc(f.device)}${f.summary ? `<div class="msg">${esc(f.summary)}</div>` : ""}
          ${f.caution ? `<div class="msg" style="color:var(--warn)">${esc(f.caution)}</div>` : ""}</td>
        <td class="mono">${esc(f.current) || "-"}</td>
        <td class="mono">${esc(f.available)}</td>
        <td>${f.needs_reboot ? '<span class="pill">at next boot</span>' : ""}</td>
      </tr>`).join("")}</tbody>
    </table></div>
    <div class="bar">
      <button class="act" ${connected && !busy ? "" : "disabled"}
        title="Runs fwupdmgr update. This writes firmware to the devices listed above."
        onclick="updateFirmware('${id}')">Flash all firmware&hellip;</button>
      <span class="status" id="fw-status"></span>
    </div>
  </div>`;
}

async function updateFirmware(id) {
  const typed = prompt(
    "This writes firmware to this machine's hardware.\n\n" +
    "It cannot be undone, and a failure part-way through can leave a device unusable. " +
    "Make sure the machine will not lose power.\n\nType FLASH to continue:"
  );
  if (typed === null) return;
  if (typed.trim() !== "FLASH") {
    alert("Nothing was flashed.");
    return;
  }

  const st = $("fw-status");
  const say = (t, c) => { if (st) st.innerHTML = `<span class="${c}">${esc(t)}</span>`; };
  say("flashing \u2014 do not power this machine off", "run");
  try {
    const res = await cmd(id, "update_firmware", {});
    if (!res || !res.id) throw new Error("the portal did not accept the command");
    const done = await awaitCommand(id, res.id, 1800000);
    if (!done) { say("still running \u2014 see the History tab", "run"); return; }
    say(done.ok ? "written \u2014 reboot to apply it" : (done.summary || "failed"),
      done.ok ? "ok" : "bad");
  } catch (e) {
    say(e.message, "bad");
  }
}

function releaseCard(rel, id, connected, busy) {
  if (!rel) return "";
  const blockers = (rel.findings || []).filter((f) => f.severity === "blocker");
  const warnings = (rel.findings || []).filter((f) => f.severity === "warning");

  const verdict = blockers.length
    ? `<span class="pill bad">not safe to upgrade</span>`
    : (rel.next ? `<span class="pill ok">ready for ${esc(rel.next)}</span>`
                : `<span class="pill">nothing to upgrade to</span>`);

  const finding = (f) => `<div class="step">
      <h3>${f.severity === "blocker" ? '<span class="pill bad">blocker</span>' : '<span class="pill warn">warning</span>'}
        ${esc(f.summary)}</h3>
      ${f.detail ? `<pre>${esc(f.detail)}</pre>` : ""}
    </div>`;

  // The upgrade is offered in two steps on purpose. The check is free and
  // answers the only question worth asking beforehand; the upgrade itself
  // cannot be undone from here, so it asks for the release to be typed out.
  // Name the version, not just the codename. Someone reading "forky" has to
  // already know whether that is a release or what is currently in testing -
  // and if they assume the former, they upgrade a server onto testing.
  const target = rel.next_version
    ? `${esc(rel.distro)} ${esc(rel.next_version)} (${esc(rel.next)})`
    : `${esc(rel.distro)} ${esc(rel.next)}`;

  const upgrade = rel.next ? `<div class="step">
      <h3>Upgrade to ${target}${rel.stable === rel.next ? ' <span class="pill ok">current stable</span>' : ""}</h3>
      <p>PatchPanel finishes the current release, repoints apt at
         <span class="mono">${esc(rel.next)}</span> (keeping a copy of every source file it
         changes), disables third-party repositories that have nothing published for it, and
         runs the upgrade in the two stages Debian documents. It takes a while and the machine
         needs a reboot afterwards.</p>
      <div class="note" style="border-color:var(--warn)">
        <b>This cannot be undone from here.</b> Snapshot the machine first &mdash; on Proxmox,
        <span class="mono">Snapshots &rarr; Take Snapshot</span> &mdash; and read the readiness
        check before starting.
      </div>
      <div class="bar" style="padding-left:0;padding-right:0">
        <button class="act" ${connected && !busy ? "" : "disabled"}
          title="Runs every precondition and changes nothing."
          onclick="distroCheck('${id}', '${esc(rel.next)}')">Check readiness</button>
        <button class="act" ${connected && !busy && !blockers.length ? "" : "disabled"}
          title="${blockers.length ? "Resolve the blockers first" : "Performs the release upgrade"}"
          onclick="distroUpgrade('${id}', '${esc(rel.next)}', '${target.replace(/'/g, "")}')">Upgrade to ${target}&hellip;</button>
        <span class="status" id="distro-status"></span>
      </div>
      <pre id="distro-out" hidden></pre>
    </div>` : "";

  return `<div class="card">
    <h2>Release</h2>
    <div class="step">
      <div class="hdr" style="margin:0">
        <b>${esc(rel.distro)} ${esc(rel.version_id) || "testing"}</b>
        <span class="mono msg">${esc(rel.codename)}</span>
        ${verdict}
        ${rel.stable ? `<span class="msg">current stable is ${esc(rel.stable)}</span>` : ""}
      </div>
      ${blockers.length
        ? `<p style="margin-top:10px">Installing updates on this machine is unsafe until the
             blocker${blockers.length > 1 ? "s" : ""} below ${blockers.length > 1 ? "are" : "is"} resolved.</p>`
        : (rel.next ? `<p style="margin-top:10px">Next release is <b>${esc(rel.next)}</b>.</p>` : "")}
    </div>
    ${blockers.map(finding).join("")}
    ${warnings.map(finding).join("")}
    ${upgrade}
  </div>`;
}

// The readiness check and the upgrade share one reporting path: both are long
// enough that a button which merely dims tells you nothing.
async function runDistro(id, to, check) {
  const out = $("distro-out");
  const st = $("distro-status");
  const say = (text, cls) => {
    if (st) st.innerHTML = `<span class="${cls}">${esc(text)}</span>`;
  };
  say(check ? "checking\u2026" : "upgrading \u2014 this takes a while", "run");
  if (out) { out.hidden = true; out.textContent = ""; }

  let res;
  try {
    res = await cmd(id, "distro_upgrade", { to, check });
    if (!res || !res.id) throw new Error("the portal did not accept the command");
  } catch (e) {
    say(e.message, "bad");
    return;
  }

  // A release upgrade runs for the better part of an hour; the History tab is
  // where it keeps reporting once this page stops waiting.
  const done = await awaitCommand(id, res.id, check ? 120000 : 3600000);
  if (!done) {
    say("still running \u2014 see the History tab (" + res.id.slice(0, 8) + ")", "run");
    return;
  }
  say(done.ok ? (check ? "check complete" : "upgrade complete \u2014 reboot when convenient")
              : (check ? "not ready" : "upgrade failed"), done.ok ? "ok" : "bad");
  if (out) {
    out.hidden = false;
    out.textContent = done.detail || done.summary || "";
  }
}

function distroCheck(id, to) { runDistro(id, to, true); }

// Rebooting is scheduled a minute out rather than immediately: the agent gets
// its reply to the portal before the machine goes, so the action does not read
// as failed, and anyone logged in gets the usual warning with time to object.
const REBOOT_DELAY_SECS = 60;

async function rebootMachine(id, host) {
  if (!confirm(
    "Reboot " + host + " in one minute?\n\n" +
    "Anything running on it stops. It should reconnect to PatchPanel by itself " +
    "once it is back."
  )) return;

  const say = (text, cls) => {
    for (const el of [$("reboot-status"), $("reboot-status-2")]) {
      if (el) el.innerHTML = `<span class="${cls}">${esc(text)}</span>`;
    }
  };
  say("scheduling\u2026", "run");
  try {
    const res = await cmd(id, "reboot", { delay_secs: REBOOT_DELAY_SECS });
    if (!res || !res.id) throw new Error("the portal did not accept the command");
    say("rebooting in " + REBOOT_DELAY_SECS + "s \u2014 it will show as offline, then come back",
      "ok");
  } catch (e) {
    say(e.message, "bad");
  }
}

function distroUpgrade(id, to, label) {
  label = label || to;
  // Typing the release is the last gate. A misclick cannot get through it, and
  // the words are the ones the operator has to have read.
  const typed = prompt(
    "This upgrades the machine to " + label + " and cannot be undone from PatchPanel.\n\n" +
    "Snapshot it first.\n\nType " + to + " to continue:"
  );
  if (typed === null) return;
  if (typed.trim() !== to) {
    alert("Not upgraded - you typed \"" + typed + "\", which is not " + to + ".");
    return;
  }
  runDistro(id, to, false);
}

// Two source lines that differ only in spacing say the same thing to apt, and
// showing them as a change buries the one line that actually moved.
const sameLine = (a, b) =>
  (a ?? "").trim().replace(/\s+/g, " ") === (b ?? "").trim().replace(/\s+/g, " ");

function lineDiff(before, after) {
  const A = before.replace(/\s+$/, "").split("\n");
  const B = after.replace(/\s+$/, "").split("\n");
  const rows = [];
  for (let i = 0; i < Math.max(A.length, B.length); i++) {
    const x = A[i], y = B[i];
    if (sameLine(x, y)) { rows.push({ t: "same", s: y ?? x ?? "" }); continue; }
    if (x !== undefined && x.trim() !== "") rows.push({ t: "del", s: x });
    if (y !== undefined && y.trim() !== "") rows.push({ t: "add", s: y });
  }
  return rows;
}

const sameText = (a, b) => {
  const A = (a ?? "").replace(/\s+$/, "").split("\n");
  const B = (b ?? "").replace(/\s+$/, "").split("\n");
  return A.length === B.length && A.every((l, i) => sameLine(l, B[i]));
};

function renderDiff(before, after) {
  const rows = lineDiff(before, after);
  const changed = rows.filter((r) => r.t !== "same").length;
  if (!changed) return `<div class="msg">No differences.</div>`;
  return `<div class="diff">${rows.map((r) =>
    `<div class="${r.t}">${r.t === "del" ? "- " : r.t === "add" ? "+ " : "  "}${esc(r.s)}</div>`
  ).join("")}</div>`;
}

function sourceEditor(files, id, connected) {
  if (!files.length) return "";

  const block = (f, i) => {
    const notes = (f.notes || []).length
      ? `<ul style="margin:6px 0 10px 18px">${f.notes.map((n) => `<li>${esc(n)}</li>`).join("")}</ul>`
      : "";
    const suggestion = f.suggested
      ? `<div class="note" style="border-color:var(--accent)">
           <b>PatchPanel can correct this file.</b>
           ${notes}
           <div id="sugdiff-${i}">${renderDiff(f.content, f.suggested)}</div>
           <button class="act" onclick="useSuggestion('${i}')">Apply this to the editor</button>
           <span class="msg">nothing is written until you press Save</span>
           <textarea id="sug-${i}" hidden>${esc(f.suggested)}</textarea>
         </div>`
      : "";
    return `
      <div class="step">
        <h3 class="mono">${esc(f.path)}${f.suggested ? ' <span class="pill warn">fix available</span>' : ""}</h3>
        ${suggestion}
        <textarea id="src-${i}" spellcheck="false" style="min-height:130px"
          data-original="${esc(f.content)}" data-dirty="0" data-path="${esc(f.path)}"
          oninput="onEdit(${i})">${esc(f.content)}</textarea>
        <div id="pending-${i}" hidden>
          <div class="msg" style="margin-top:8px">Unsaved changes &mdash; this is what Save will write:</div>
          <div id="editdiff-${i}"></div>
        </div>
        <div class="bar" style="padding-left:0;padding-right:0">
          <button class="act primary" ${connected ? "" : "disabled"} id="save-${i}"
            onclick="saveSource('${id}', ${i})">Save and validate</button>
          <button class="act" onclick="revertSource(${i})">Revert</button>
          <button class="act" ${connected ? "" : "disabled"}
            onclick="deleteSource('${id}', ${i})">Delete file</button>
          <span class="status" id="status-${i}">${statusHTML(f.path)}</span>
        </div>
      </div>`;
  };

  const fixable = files.filter((f) => f.suggested).length;
  return `<div class="card">
    <h2>Apt sources${fixable ? ` &mdash; ${fixable} file(s) with a suggested fix` : ""}</h2>
    <div class="step"><div class="note">
      Saving runs <code>apt-get update</code> on the machine. If apt rejects the new contents the
      agent puts the previous file back and tells you why, so a bad edit undoes itself. A copy is
      kept beside each file as <code>.patchpanel-bak</code>.
    </div></div>
    ${files.map(block).join("")}
  </div>`;
}

// Show what the current editor content would change, so "apply the fix" is
// never a leap of faith.
function onEdit(i) {
  const ta = document.getElementById("src-" + i);
  // Retyping the same line with different spacing is not an edit worth
  // warning about, or worth writing to the machine.
  const dirty = !sameText(ta.value, ta.dataset.original);
  ta.dataset.dirty = dirty ? "1" : "0";

  const pending = document.getElementById("pending-" + i);
  if (pending) {
    pending.hidden = !dirty;
    if (dirty) {
      document.getElementById("editdiff-" + i).innerHTML =
        renderDiff(ta.dataset.original, ta.value);
    }
  }
  const banner = document.getElementById("edit-banner");
  if (banner) banner.hidden = !isEditing();
}

function useSuggestion(i) {
  const ta = document.getElementById("src-" + i);
  const sug = document.getElementById("sug-" + i);
  if (!ta || !sug) return;
  ta.value = sug.value;
  onEdit(i);
  ta.scrollIntoView({ block: "nearest" });
}

function revertSource(i) {
  const ta = document.getElementById("src-" + i);
  if (!ta) return;
  ta.value = ta.dataset.original;
  onEdit(i);
  setStatus(i, "", "", ta.dataset.path);
}

// Shared row filter for the long tables.
// winget cannot always determine what is installed; it prints `Unknown`, or a
// bound like `< 4.6.2`. Those are the packages that quietly never upgrade.
function unreadableVersion(u) {
  if (u.source !== "winget") return false;
  const v = (u.current_version || "").trim();
  return v === "" || v === "Unknown" || v.startsWith("<");
}

function filterRows(inputId, tbodyId) {
  const q = (document.getElementById(inputId)?.value || "").toLowerCase();
  document.querySelectorAll(`#${tbodyId} tr`).forEach((tr) => {
    tr.hidden = q && !(tr.dataset.n || "").includes(q);
  });
}

// Last save outcome per file path, so it survives the refresh timer.
const SAVED = new Map();

function statusHTML(path) {
  const st = SAVED.get(path);
  if (!st) return "";
  return `<span class="${st.cls}">${esc(st.text)}</span>`;
}

function setStatus(i, text, cls, path) {
  const el = document.getElementById("status-" + i);
  if (el) el.innerHTML = text ? `<span class="${cls}">${esc(text)}</span>` : "";
  const key = path || pathOf(i);
  if (!key) return;
  if (text) SAVED.set(key, { text, cls });
  else SAVED.delete(key);
}

function pathOf(i) {
  const ta = document.getElementById("src-" + i);
  return ta ? ta.dataset.path : null;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Wait for the agent to report on a dispatched command, so Save says what
// actually happened instead of silently returning.
async function awaitCommand(agentId, cmdId, timeoutMs = 90000) {
  const until = Date.now() + timeoutMs;
  while (Date.now() < until) {
    await sleep(1500);
    try {
      const rows = await api(`/api/commands?agent=${agentId}&limit=15`);
      const c = rows.find((r) => r.id === cmdId);
      if (c && c.ok !== null && c.ok !== undefined) return c;
    } catch (e) {
      // Keep waiting; a transient failure to poll is not a failed command.
    }
  }
  return null;
}

async function saveSource(id, i) {
  const ta = document.getElementById("src-" + i);
  const path = ta.dataset.path;
  const content = ta.value;
  if (!confirm("Replace " + path + " on this machine? apt validates it, and the old file is restored if it is rejected.")) return;

  const btn = document.getElementById("save-" + i);
  if (btn) btn.disabled = true;
  setStatus(i, "writing " + path + " and running apt-get update\u2026", "run", path);

  let res;
  try {
    res = await cmd(id, "write_source", { path, content });
    if (!res || !res.id) throw new Error("the portal did not accept the command");
  } catch (e) {
    setStatus(i, e.message, "bad", path);
    if (btn) btn.disabled = false;
    return;
  }

  const done = await awaitCommand(id, res.id);
  if (btn) btn.disabled = false;

  if (!done) {
    setStatus(i, "still running \u2014 see the History tab (" + res.id.slice(0, 8) + ")", "run", path);
    return;
  }
  if (done.ok) {
    // Only now does the editor match what is on disk.
    ta.dataset.original = content;
    onEdit(i);
    setStatus(i, "saved \u2014 " + (done.summary || "apt accepted it"), "ok", path);
  } else {
    setStatus(i, "not saved \u2014 " + (done.summary || "apt rejected it, the previous file was restored"), "bad", path);
  }
}

async function deleteSource(id, i) {
  const path = pathOf(i);
  if (!confirm("Delete " + path + "? A backup is kept beside it.")) return;
  setStatus(i, "deleting " + path + "\u2026", "run", path);
  try {
    const res = await cmd(id, "remove_source", { path });
    if (!res || !res.id) throw new Error("the portal did not accept the command");
    const done = await awaitCommand(id, res.id);
    if (!done) { setStatus(i, "still running \u2014 see the History tab", "run", path); return; }
    setStatus(i, (done.ok ? "deleted \u2014 " : "not deleted \u2014 ") + (done.summary || ""),
      done.ok ? "ok" : "bad", path);
  } catch (e) {
    setStatus(i, e.message, "bad", path);
  }
}

// Side-by-side apt sources for two machines.
//
// The fleet-wide diff answers "is this machine unusual"; this answers "why is
// that one working and this one not", which is the question actually being
// asked when two boxes of the same release behave differently.
function compareCard(me, myRepos, myFiles) {
  const others = AGENTS.filter((a) => a.id !== me.id);
  const label = (a) =>
    `${a.hostname}${a.os_version ? ` - ${a.os_version}` : ""}`;
  // Same release first: those are the comparisons where a difference means
  // something.
  const mine = me.os_version || "";
  others.sort((a, b) => {
    const sa = (a.os_version === mine ? 0 : 1), sb = (b.os_version === mine ? 0 : 1);
    return sa - sb || a.hostname.localeCompare(b.hostname);
  });

  const picker = `<div class="step">
      <label class="msg">Compare apt sources with</label>
      <select id="cmp-with" onchange="setCompare(this.value)"
        style="margin-left:8px;padding:6px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font:inherit">
        <option value="">nothing</option>
        ${others.map((a) => `<option value="${a.id}"${a.id === COMPARE ? " selected" : ""}>${esc(label(a))}</option>`).join("")}
      </select>
      ${COMPARE && COMPARE_DATA ? `<button class="act" style="margin-left:8px" onclick="setCompare('${COMPARE}')">Refresh</button>` : ""}
    </div>`;

  if (!COMPARE || !COMPARE_DATA) {
    return `<div class="card"><h2>Compare with another machine</h2>${picker}</div>`;
  }

  const them = COMPARE_DATA;
  const theirInv = them.inventory || {};
  const theirRepos = (theirInv.repositories || []).filter((r) => r.enabled);
  const myEnabled = myRepos.filter((r) => r.enabled);
  const key = (r) => `${r.source} ${(r.uri || "").replace(/\/$/, "")} ${r.suite || ""}`;
  // What to show for a key: an apt repository is written as a `deb` line, and
  // showing anything else invites it being pasted into a source file. The
  // backend name belongs in a column of its own, not at the front of a line
  // that otherwise looks copyable.
  const shown = (k) => {
    const [backend, ...rest] = k.split(" ");
    return backend === "apt" || backend === "apt-src"
      ? `${backend === "apt-src" ? "deb-src" : "deb"} ${rest.join(" ").trim()}`
      : k;
  };

  const mineSet = new Set(myEnabled.map(key));
  const theirSet = new Set(theirRepos.map(key));
  const all = [...new Set([...mineSet, ...theirSet])].sort();
  const both = all.filter((k) => mineSet.has(k) && theirSet.has(k)).length;

  const rows = all.map((k) => {
    const a = mineSet.has(k), b = theirSet.has(k);
    const mark = (on) => on
      ? '<span class="pill ok">yes</span>'
      : '<span class="pill bad">no</span>';
    return `<tr${a && b ? ' style="opacity:.55"' : ""}>
      <td class="mono">${esc(shown(k))}</td>
      <td>${mark(a)}</td>
      <td>${mark(b)}</td>
    </tr>`;
  }).join("");

  // Files with the same path on both machines get a real diff, which is what
  // you want once the table has told you which repo is missing.
  const theirFiles = theirInv.source_files || [];
  const shared = (myFiles || []).filter((f) => theirFiles.some((g) => g.path === f.path));
  const diffs = shared.map((f) => {
    const g = theirFiles.find((x) => x.path === f.path);
    const same = sameText(f.content, g.content);
    return `<div class="step">
      <h3 class="mono">${esc(f.path)} ${same ? '<span class="pill ok">identical</span>' : '<span class="pill warn">differs</span>'}</h3>
      ${same ? "" : renderDiff(g.content, f.content)}
    </div>`;
  }).join("");

  const onlyMine = (myFiles || []).filter((f) => !theirFiles.some((g) => g.path === f.path));
  const onlyTheirs = theirFiles.filter((g) => !(myFiles || []).some((f) => f.path === g.path));

  const sameRelease = them.os_version === me.os_version;
  return `<div class="card">
    <h2>Compared with ${esc(them.hostname)}</h2>
    ${picker}
    <div class="step">
      ${sameRelease
        ? `<div class="msg">Both machines run ${esc(me.os_version)}, so a difference here is a real one.</div>`
        : `<div class="note" style="border-color:var(--warn)">
             <b>Different releases.</b> ${esc(me.hostname)} runs ${esc(me.os_version)} and
             ${esc(them.hostname)} runs ${esc(them.os_version)}. Their archives are
             <i>supposed</i> to differ; read this as a shape comparison, not a checklist.
           </div>`}
    </div>
    <div class="scroll scrolly"><table>
      <thead><tr><th>Enabled repository</th><th>${esc(me.hostname)}</th><th>${esc(them.hostname)}</th></tr></thead>
      <tbody>${rows}</tbody>
    </table></div>
    <div class="bar"><span class="msg">${both} in common, ${all.length - both} on only one of them</span></div>
    ${diffs ? `<h3 style="padding:0 14px">Files on both machines</h3>${diffs}` : ""}
    ${onlyMine.length ? `<div class="step"><div class="msg">Only on ${esc(me.hostname)}:
      ${onlyMine.map((f) => `<span class="mono">${esc(f.path)}</span>`).join(", ")}</div></div>` : ""}
    ${onlyTheirs.length ? `<div class="step"><div class="msg">Only on ${esc(them.hostname)}:
      ${onlyTheirs.map((f) => `<span class="mono">${esc(f.path)}</span>`).join(", ")}</div></div>` : ""}
  </div>`;
}

// A repository key rendered the way it is written in a sources file. The
// internal form starts with the backend name, which reads as a source type and
// is not one.
function asSourceLine(k) {
  const [backend, ...rest] = String(k).split(" ");
  if (backend === "apt") return `deb ${rest.join(" ").trim()}`;
  if (backend === "apt-src") return `deb-src ${rest.join(" ").trim()}`;
  return k;
}

function repoCard(repos, diff) {
  if (!repos.length) return "";
  const odd = new Set(diff.only_here || []);
  const key = (r) => `${r.source} ${(r.uri || "").replace(/\/$/, "")} ${r.suite || ""}`;

  const rows = repos.map((r) => {
    const flags = [];
    if (!r.enabled) flags.push('<span class="pill">disabled</span>');
    if (odd.has(key(r))) flags.push('<span class="pill warn">only on this machine</span>');
    if (r.problem) flags.push(`<span class="pill bad" title="${esc(r.problem)}">cannot deliver</span>`);
    return `<tr${r.enabled ? "" : ' style="opacity:.55"'}>
      <td class="mono">${esc(r.source)}</td>
      <td class="mono">${esc(r.uri)}</td>
      <td class="mono">${esc(r.suite) || "-"}</td>
      <td class="mono msg">${esc((r.components || []).join(" "))}</td>
      <td>${flags.join(" ")}</td>
      <td class="mono msg">${esc(r.origin_file)}</td>
    </tr>`;
  }).join("");

  // A repository whose files have gone is the reason a patch run downloads
  // hundreds of megabytes and then fails, and nothing about the source line
  // itself looks wrong. Say it above the table, not in a tooltip.
  const broken = repos.filter((r) => r.problem);
  const brokenNote = broken.length
    ? `<div class="step"><div class="note" style="border-color:var(--bad)">
         <b>${broken.length} source(s) list packages that are no longer served.</b>
         ${broken.map((r) => `<div style="margin-top:8px">
           <div class="mono">${esc(r.uri)} ${esc(r.suite)}</div>
           <div class="msg">${esc(r.problem)}</div>
           <div class="msg">declared in <span class="mono">${esc(r.origin_file)}</span></div>
         </div>`).join("")}
       </div></div>`
    : "";

  const missing = (diff.missing_here || []).length
    ? `<div class="note" style="margin:12px 14px">
         <b>Missing here.</b> Configured on all ${diff.peers} other ${esc(diff.group || "machine")} machine(s), but not this one:
         <ul style="margin:6px 0 0 18px">${diff.missing_here.map((m) => `<li class="mono">${esc(asSourceLine(m))}</li>`).join("")}</ul>
       </div>`
    : "";

  // Say what the comparison was against. "The same OS" was a lie that made a
  // Debian 12 box look wrong next to a Debian 13 one.
  const summary = diff.peers
    ? `compared against ${diff.peers} other machine(s) running ${esc(diff.group || "the same OS")}`
    : `no other machines running ${esc(diff.group || "this OS")} to compare against`;

  return `<div class="card">
    <h2>Package sources &mdash; ${repos.length}${broken.length ? `, ${broken.length} not serving` : ""}</h2>
    ${brokenNote}
    <div class="scroll scrolly"><table>
      <thead><tr><th>Backend</th><th>URI</th><th>Suite</th><th>Components</th><th></th><th>Declared in</th></tr></thead>
      <tbody>${rows}</tbody>
    </table></div>
    ${missing}
    <div class="bar"><span class="msg">${summary}</span></div>
  </div>`;
}

async function loadAgent(id) {
  const body = $("agent-body");
  if (!id) { body.innerHTML = `<div class="empty">No machine selected.</div>`; return; }

  let d;
  try {
    d = await api(`/api/agents/${id}`);
  } catch (e) {
    body.innerHTML = `<div class="back" onclick="showTab('fleet')">&larr; Fleet</div>
      <div class="card"><div class="empty">${esc(e.message)}</div></div>`;
    return;
  }

  const hw = d.hardware || {};
  const inv = d.inventory || {};
  const updates = inv.updates || [];
  const sec = updates.filter((u) => u.security);
  // Separate what a button press would actually install from what the archive
  // is withholding, so the page never implies there is work to do when there
  // is not.
  const stuckNames = new Set(inv.deferred || []);
  // Observed, not reported: these survived a patch run untouched.
  const blockedNames = new Set(inv.blocked || []);
  const canInstall = updates.filter((u) => !stuckNames.has(u.name));
  const phased = updates.filter((u) => stuckNames.has(u.name));
  const busy = isBusy(d);
  const state = d.connected
    ? '<span class="pill ok">connected</span>'
    : (d.online ? '<span class="pill warn">recently seen</span>' : '<span class="pill bad">offline</span>');

  // Patch history is the command log filtered to the actions that change the
  // machine; a "rescan" is not an event anyone wants in a history.
  const CHANGED = ["apply_patches", "apply_manifest", "self_update", "reboot", "distro_upgrade",
    "finish_upgrade", "update_firmware"];
  const history = (d.commands || []).filter((c) => HISTORY_ALL || CHANGED.includes(c.kind));
  const hidden = (d.commands || []).length - history.length;

  // Never redraw over unsaved edits.
  if (isEditing()) return;

  const files = inv.source_files || [];
  const fixable = files.filter((f) => f.suggested).length;
  const repos = inv.repositories || [];
  const drift = inv.drift || [];
  const issues = inv.scan_issues || [];
  const held = inv.held_back || [];
  const blocked = issues.length + held.length;

  // Which panes this machine has. Windows has no apt sources, so it gets no
  // Sources tab rather than an empty one.
  const panes = [
    { p: "overview", label: "Overview" },
    { p: "updates", label: "Updates",
      n: (canInstall.length + (inv.firmware || []).length) || null, warn: blocked > 0 },
  ];
  if (files.length || repos.length) {
    panes.push({ p: "sources", label: "Sources", n: fixable || null, warn: fixable > 0 });
  }
  panes.push({ p: "packages", label: "Packages", n: (inv.packages || []).length || null });
  panes.push({ p: "history", label: "History", n: history.length || null });

  const subnav = `<div class="subnav">${panes.map((t) =>
    `<button data-p="${t.p}" onclick="showPane('${t.p}')">${t.label}${
      t.n ? `<span class="n${t.warn ? " warn" : ""}">${t.n}</span>` : ""}</button>`).join("")}</div>`;

  const html = `
    <div class="machine-head">
      <div class="back" onclick="showTab('fleet')">&larr; Fleet</div>
      <div class="hdr">
        <h2>${esc(d.hostname)}</h2>${state}
        ${busy ? `<span class="pill busy">${esc(KIND_LABEL[busy] || busy)}</span>` : ""}
        ${d.reboot_required ? '<span class="pill warn">reboot required</span>' : ""}
        ${/ (testing|unstable) /.test(" " + d.os_version + " ")
          ? `<span class="pill warn" title="This machine tracks a rolling suite rather than a released version. It has no version number and no security team of its own.">${esc(d.os_version.includes("unstable") ? "unstable" : "testing")}</span>`
          : ""}
        <span class="msg">${esc(d.os_version)} &middot; ${esc(d.site) || "no site"}</span>
      </div>
      ${subnav}
    </div>
    <div id="edit-banner" hidden>Editing &mdash; live updates are paused for this page until you save or revert.</div>

    <div data-pane="overview" hidden>
      ${midUpgradeCard(inv.mid_upgrade, d.id, d.connected, busy)}

      ${d.reboot_required ? `<div class="card"><div class="step">
        <div class="note" style="border-color:var(--warn)">
          <b>This machine is waiting on a reboot.</b> Updates have been installed that are not
          in effect until it restarts &mdash; on Linux that is usually a kernel or libc, which
          means the running system is still the unpatched one.
        </div>
        <div class="bar" style="padding-left:0;padding-right:0">
          <button class="act primary" ${d.connected && !busy ? "" : "disabled"}
            onclick="rebootMachine('${d.id}', '${esc(d.hostname)}')">Reboot ${esc(d.hostname)}&hellip;</button>
          <span class="status" id="reboot-status-2"></span>
        </div>
      </div></div>` : ""}

      <div class="grid2">
        <div class="card">
          <h2>Hardware</h2>
          ${kv([
            ["CPU", esc(hw.cpu_model) || "-"],
            ["Cores", hw.cpu_threads ? `${hw.cpu_cores || "?"} physical / ${hw.cpu_threads} logical` : "-"],
            ["Memory", hw.memory_mb ? `${(hw.memory_mb / 1024).toFixed(1)} GB` : "-"],
            ["Architecture", `<span class="mono">${esc(d.arch)}</span>`],
            ["System", esc(hw.vendor)],
            ["IP", (hw.ip_addresses || []).map((i) => `<span class="mono">${esc(i)}</span>`).join("<br>") || "-"],
            ["Kernel", `<span class="mono">${esc(hw.kernel)}</span>`],
          ])}
        </div>

        <div class="card">
          <h2>State</h2>
          ${kv([
            ["Booted", d.boot_time
            ? `${since(d.boot_time)} ago <span class="msg">(${new Date(d.boot_time).toLocaleString()})</span>${
                inv.boot && inv.boot.unexpected
                  ? ` <span class="pill bad" title="${esc(inv.boot.summary)}">did not shut down cleanly</span>`
                  : (inv.boot ? ' <span class="pill ok">clean shutdown</span>' : "")}`
            : "-"],
            ["Last seen", `${ago(d.last_seen)}`],
            ["First enrolled", new Date(d.first_seen).toLocaleString()],
            ["Agent version", `<span class="mono">${esc(d.agent_version)}</span>`],
            ["Manifest", d.applied_revision < REV
              ? `<span class="pill warn">r${d.applied_revision}</span> portal is at r${REV}`
              : `r${d.applied_revision} <span class="msg">up to date</span>`],
            ["Package backends", (d.backends || []).map((b) => `<span class="mono">${esc(b)}</span>`).join(", ")],
            ["Agent id", `<span class="mono msg">${esc(d.id)}</span>`],
          ])}
          <div class="bar">
            <button class="act" ${d.connected && !busy ? "" : "disabled"}
              title="Restarts the agent process. Re-detects package backends; does not touch the machine otherwise."
              onclick="restartAgent('${d.id}')">Restart agent</button>
            <button class="act" ${d.connected && !busy ? "" : "disabled"}
              title="Reboots the machine itself, one minute from now."
              onclick="rebootMachine('${d.id}', '${esc(d.hostname)}')">Reboot machine&hellip;</button>
            <span class="status" id="reboot-status"></span>
          </div>
        </div>
      </div>

      ${virtCard(inv.virt)}

      ${bootCard(inv.boot)}

      ${releaseCard(inv.release, d.id, d.connected, busy)}
    </div>

    <div data-pane="updates" hidden>
      <div class="card">
        <h2>Pending updates &mdash; ${canInstall.length} to install${phased.length ? `, ${phased.length} phased` : ""}${blockedNames.size ? `, ${blockedNames.size} blocked` : ""}${sec.length ? `, ${sec.length} security` : ""}${(d.ignored || []).length ? `, ${d.ignored.length} ignored` : ""}</h2>
      ${blockedNames.size ? `<div class="step"><div class="note" style="border-color:var(--bad)">
        <b>${blockedNames.size} update(s) survived the last patch run untouched.</b>
        They were attempted, nothing was installed, and they are still offered at the same
        version &mdash; so pressing Install updates again will do exactly the same thing.
        Each has to be dealt with individually: on Windows that usually means the package
        manager will not replace it in place and it has to be uninstalled and reinstalled;
        on Linux it means apt could not resolve it. The last patch run in the History tab
        names them with whatever the tool said.
      </div></div>` : ""}
        ${phased.length && !canInstall.length ? `<div class="step"><div class="note">
          <b>Nothing to install.</b> All ${phased.length} pending update(s) are phased: the archive is
          withholding them from this machine until the rollout completes. Pressing Install updates
          will correctly do nothing. They will arrive on their own.
        </div></div>` : ""}
        ${updates.length > 12 ? `<div class="step" style="padding-bottom:0">
          <input id="upd-filter" placeholder="filter packages&hellip;" spellcheck="false"
            oninput="filterRows('upd-filter','upd-rows')"
            style="width:100%;padding:7px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font-family:var(--mono);font-size:12.5px">
        </div>` : ""}
        ${updates.length ? `<div class="scroll scrolly"><table>
          <thead><tr><th>Package</th><th>Installed</th><th>Available</th><th>Source</th><th></th></tr></thead>
          <tbody id="upd-rows">${canInstall.concat(phased).map((u) => {
            const stuck = stuckNames.has(u.name);
            return `<tr data-n="${esc((u.name || "").toLowerCase())}"${stuck ? ' style="opacity:.55"' : ""}>
            <td>${esc(u.name)}${u.security ? ' <span class="pill bad">security</span>' : ""}</td>
            <td class="mono"${unreadableVersion(u) ? ' title="winget could not read the installed version, only bound it. That on its own does not stop an upgrade."' : ""}>${esc(u.current_version) || "-"}</td>
            <td class="mono">${esc(u.new_version)}</td>
            <td class="mono">${esc(u.source)}</td>
            <td>${stuck ? '<span class="pill" title="Phased: the archive is withholding this from this machine. Nothing to do.">phased</span>' : ""}${
            blockedNames.has(u.name) ? ' <span class="pill bad" title="A patch run installed everything it could and left this exactly as it was - same version installed, same version on offer. Pressing Install updates again will not change it.">blocked</span>' : ""}
            <button class="act" style="padding:2px 8px;font-size:11.5px"
              title="Stop showing this version. It comes back on its own if a newer one is published."
              onclick="ignoreUpdate(${jsq(d.id)}, ${jsq(u.name)}, ${jsq(u.source)}, ${jsq(u.new_version)})">Ignore</button>
          </td></tr>`;
          }).join("")}</tbody>
        </table></div>` : `<div class="empty">Nothing pending${inv.collected_at ? ` as of ${ago(inv.collected_at)}` : ""}.</div>`}
        <div class="bar">
          <button class="act" ${busy ? "disabled" : (d.connected ? "" : "disabled")} onclick="cmd('${d.id}','collect_inventory')">Rescan</button>
          <button class="act" ${busy || !canInstall.length ? "disabled" : (d.connected ? "" : "disabled")}
            title="${canInstall.length ? "Install the pending updates" : "Nothing to install - everything pending is phased"}"
            onclick="patchNow('${d.id}','${esc(d.hostname)}')">Install updates</button>
          <button class="act" ${busy ? "disabled" : (d.connected ? "" : "disabled")} onclick="cmd('${d.id}','apply_manifest')">Apply manifest</button>
          <span class="msg">${busy
            ? `waiting for ${esc(KIND_LABEL[busy] || busy)} to finish`
            : (inv.collected_at ? `inventory collected ${ago(inv.collected_at)}` : "")}</span>
        </div>
      </div>

      ${firmwareCard(inv.firmware, d.id, d.connected, busy)}

      ${ignoredCard(d.ignored || [], d.id)}

      ${scanCard(issues, held, inv.deferred || [], d.id, d.connected)}

      ${drift.length ? `<div class="card"><h2>Application drift</h2>
        <div class="scroll"><table><thead><tr><th>App</th><th>Wanted</th><th>Found</th></tr></thead>
        <tbody>${drift.map((x) => `<tr><td>${esc(x.app)}</td>
          <td class="mono">${esc(x.desired)}</td><td class="mono">${esc(x.observed)}</td></tr>`).join("")}</tbody>
        </table></div></div>` : ""}
    </div>

    ${files.length || repos.length ? `<div data-pane="sources" hidden>
      ${sourceEditor(files, d.id, d.connected)}
      ${repoCard(repos, d.repo_diff || {})}
      ${compareCard(d, repos, files)}
    </div>` : ""}

    <div data-pane="packages" hidden>
      <div class="card">
        <h2>Installed packages &mdash; ${(inv.packages || []).length}</h2>
        <div class="step">
          <input id="pkg-filter" placeholder="filter&hellip;" spellcheck="false"
            oninput="filterRows('pkg-filter','pkg-rows')"
            style="width:100%;padding:7px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font-family:var(--mono);font-size:12.5px">
        </div>
        <div class="scroll scrolly">
          <table><tbody id="pkg-rows">${(inv.packages || []).map((p) =>
            `<tr data-n="${esc((p.name || "").toLowerCase())}"><td>${esc(p.name)}</td>
             <td class="mono">${esc(p.version)}</td><td class="mono msg">${esc(p.source)}</td></tr>`).join("")}</tbody></table>
        </div>
      </div>

      ${cleanupCard(inv.cleanup, d.id, d.connected)}
    </div>

    <div data-pane="history" hidden>
      <div class="card">
        <h2>History &mdash; ${HISTORY_ALL ? "everything this machine has been asked to do" : "changes made to this machine"}</h2>
        <div class="step">
          <label class="msg" style="cursor:pointer">
            <input type="checkbox" ${HISTORY_ALL ? "checked" : ""} onchange="setHistoryAll(this.checked)">
            include scans and other read-only activity${hidden && !HISTORY_ALL ? ` (${hidden} hidden)` : ""}
          </label>
        </div>
        ${history.length ? history.map((c) => {
          const live = c.ok === null || c.ok === undefined;
          const st = live
            ? '<span class="pill">running</span>'
            : (c.ok ? '<span class="pill ok">ok</span>' : '<span class="pill bad">failed</span>');
          const out = (c.detail || c.progress || "").trim();
          return `<div style="padding:10px 14px;border-bottom:1px solid var(--line)">
            <div>${st} <b>${esc(c.kind)}</b>
              <span class="msg">&middot; ${new Date(c.created_at).toLocaleString()} &middot; ${ago(c.created_at)}</span><span class="mono msg cmdid" title="${esc(c.id)} - click to copy" onclick="copyText('${esc(c.id)}', this)">${esc(c.id.slice(0, 8))}</span></div>
            ${c.summary ? `<div class="mono" style="margin-top:4px">${esc(c.summary)}</div>` : ""}
            ${out ? `<details data-k="${esc(c.id)}" ontoggle="tailLog(this)"><summary>output</summary>
            <pre data-k="${esc(c.id)}" data-live="${live ? 1 : 0}">${esc(out)}</pre></details>` : ""}
          </div>`;
        }).join("") : `<div class="empty">Nothing has changed this machine yet.</div>`}
      </div>
    </div>`;

  // Only touches the DOM when something actually changed, so an open output
   // block, the package filter's text, and its caret all survive the timer.
  if (!setHTML(body, html)) return;
  applyPane();
}

// The credentials for a device being added. Held here rather than in the
// form, so a secret is never in the DOM where a screenshot or a stray copy
// would catch it.
let DEV_KEY = { key: "", secret: "", from: "" };

function deviceKindChanged() {
  const unraid = $("dev-kind").value === "unraid";
  $("dev-secret").hidden = unraid;
  $("dev-keyfile-label").textContent = unraid
    ? "API key file (a text file holding the key)"
    : "API key file (the .txt OPNsense downloads)";
  $("dev-hint").textContent = unraid
    ? "Unraid: Settings > Management Access > API Keys. The VIEWER role with the INFO and OS resources is enough - do not use ADMIN."
    : "OPNsense: System > Access > Users > your user > API keys. Give that user only the `System: Firmware` privilege.";
}

// Derive an id from the address, since it is nearly always the right one.
//
// The full name, not the short one: two sites each have a machine called
// `router`, and collapsing them to the first label makes the second one
// impossible to add.
function deviceUrlChanged() {
  const id = $("dev-id");
  if (id.dataset.touched === "1") return;
  const host = $("dev-url").value.trim()
    .replace(/^https?:\/\//, "").replace(/\/.*$/, "").replace(/:\d+$/, "");
  id.value = host;
  // The short name reads better as a label than the full one does.
  const name = $("dev-name");
  if (name && !name.value) name.placeholder = host.split(".")[0] || "name";
}

// The file an appliance hands you is `key=...` / `secret=...`, or just a bare
// key. Reading it in the browser means the secret goes straight into the
// manifest without anyone having to retype it.
function readKeyFile(input) {
  const file = input.files && input.files[0];
  if (!file) return;
  const reader = new FileReader();
  reader.onload = () => {
    const text = String(reader.result || "");
    const find = (name) => {
      const m = new RegExp("^\\s*" + name + "\\s*=\\s*(\\S+)", "mi").exec(text);
      return m ? m[1] : "";
    };
    const key = find("key") || text.trim().split(/\s+/)[0] || "";
    DEV_KEY = { key, secret: find("secret"), from: file.name };
    const st = $("dev-key-status");
    if (!key) {
      st.innerHTML = '<span class="bad">no key found in that file</span>';
      return;
    }
    st.innerHTML = `<span class="ok">read ${esc(DEV_KEY.secret ? "key and secret" : "key")} from ${esc(file.name)}</span>`;
    $("dev-key").value = "";
    $("dev-secret").value = "";
  };
  reader.readAsText(file);
}

function pastedKey() {
  DEV_KEY = {
    key: $("dev-key").value.trim(),
    secret: $("dev-secret").value.trim(),
    from: "pasted",
  };
  $("dev-key-status").innerHTML = "";
}

async function addDevice() {
  const st = $("dev-add-status");
  const say = (t, c) => { st.innerHTML = `<span class="${c}">${esc(t)}</span>`; };

  const kind = $("dev-kind").value;
  const raw = $("dev-url").value.trim();
  const id = $("dev-id").value.trim();
  if (!raw) return say("give it an address", "bad");
  if (!id) return say("give it an id", "bad");
  if (!DEV_KEY.key) return say("upload or paste an API key", "bad");
  if (kind === "opnsense" && !DEV_KEY.secret) {
    return say("OPNsense needs both a key and a secret", "bad");
  }

  // A bare hostname is what the probe wants; a pasted browser URL is what
  // people have to hand.
  const target = raw.replace(/^https?:\/\//, "").replace(/\/.*$/, "");

  const probe = kind === "opnsense"
    ? { type: "opnsense", api_key: DEV_KEY.key, api_secret: DEV_KEY.secret,
        insecure: true, check_after_hours: 12 }
    : { type: "unraid", api_key: DEV_KEY.key, insecure: true, check_releases: true };

  say("saving\u2026", "run");
  try {
    const m = await api("/api/manifest");
    m.devices = m.devices || [];
    const clash = m.devices.find((d) => d.id === id);
    if (clash) {
      return say(`there is already a device called ${id} (${clash.target || "no target"}). ` +
        `Use its full name to tell them apart.`, "bad");
    }
    m.devices.push({
      id,
      label: $("dev-name").value.trim(),
      target,
      site: "",
      collector: "",
      probe,
    });
    await api("/api/manifest", { method: "PUT", body: JSON.stringify(m) });

    say("added \u2014 probing it now", "run");
    // Probe straight away rather than leaving a blank row until the next
    // sweep; whoever just added it wants to know it works.
    const portal = AGENTS.find((a) => a.hostname === location.hostname.split(".")[0])
      || AGENTS.find((a) => a.connected);
    if (portal) await cmd(portal.id, "probe_devices", { only: [id] });

    DEV_KEY = { key: "", secret: "", from: "" };
    $("dev-url").value = ""; $("dev-id").value = ""; $("dev-name").value = "";
    $("dev-id").dataset.touched = "";
    $("dev-key").value = ""; $("dev-secret").value = "";
    $("dev-keyfile").value = ""; $("dev-key-status").innerHTML = "";
    setTimeout(() => { say("added", "ok"); refresh(); }, 2500);
  } catch (e) {
    say(e.message, "bad");
  }
}

// What this device has looked like over time. Only readings that changed are
// kept, so the list is the story rather than a log of "still fine".
async function deviceHistory(id) {
  const box = $("device-history");
  box.hidden = false;
  box.innerHTML = `<div class="step"><div class="msg">loading&hellip;</div></div>`;
  let rows = [];
  try {
    rows = await api(`/api/devices/${encodeURIComponent(id)}/history`);
  } catch (e) {
    box.innerHTML = `<div class="step"><div class="msg bad">${esc(e.message)}</div></div>`;
    return;
  }

  box.innerHTML = `
    <h2>${esc(id)} &mdash; what changed</h2>
    <div class="step">
      <div class="msg">Every probe is compared with the one before it; a row appears only
        when something actually moved. ${rows.length ? "" : "Nothing recorded yet."}</div>
    </div>
    ${rows.length ? `<div class="scroll scrolly"><table>
      <thead><tr><th>When</th><th>Reachable</th><th>Version</th><th>Updates</th><th>By</th><th>Note</th></tr></thead>
      <tbody>${rows.map((r) => `<tr>
        <td>${new Date(r.checked_at).toLocaleString()} <span class="msg">${ago(r.checked_at)}</span></td>
        <td>${r.reachable ? '<span class="pill ok">yes</span>' : '<span class="pill bad">no</span>'}</td>
        <td class="mono">${esc(r.firmware || "-")}</td>
        <td>${r.updates === null || r.updates === undefined
          ? '<span class="msg">unknown</span>' : r.updates}</td>
        <td class="msg">${esc(r.collector)}</td>
        <td class="msg">${esc(r.error || (r.detail || "").split("\n")[0] || "")}</td>
      </tr>`).join("")}</tbody>
    </table></div>` : ""}
    <div class="bar"><button class="act" onclick="$('device-history').hidden = true">Close</button></div>`;
}

// Rename a device.
//
// Only the label: the id is what probe history is recorded against, and
// changing it would orphan everything remembered about the device.
async function renameDevice(id, label) {
  const clean = (label || "").trim();
  try {
    const m = await api("/api/manifest");
    const dev = (m.devices || []).find((d) => d.id === id);
    if (!dev) return;
    if ((dev.label || "") === clean) return;
    dev.label = clean;
    await api("/api/manifest", { method: "PUT", body: JSON.stringify(m) });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function removeDevice(id) {
  if (!confirm("Stop monitoring " + id + "? Its API key is removed from the manifest too.")) return;
  const m = await api("/api/manifest");
  m.devices = (m.devices || []).filter((d) => d.id !== id);
  await api("/api/manifest", { method: "PUT", body: JSON.stringify(m) });
  refresh();
}

async function probeDevice(collectorId, id) {
  await cmd(collectorId, "probe_devices", { only: [id] });
}

async function loadDevices() {
  const d = await api("/api/devices");
  const eol = d.devices.filter((x) => x.eol);
  const warn = $("devices-eol");
  warn.hidden = eol.length === 0;
  if (eol.length) {
    warn.innerHTML = `<div class="step"><div class="note" style="border-color:var(--bad)">
      <b>${eol.length} device(s) are running a release that is end of life.</b>
      They report nothing pending, and that is true: there will be no more updates for them,
      including for anything found after today. The fix is a release upgrade, not a patch run.
      <ul style="margin:8px 0 0 18px">${eol.map((x) => `<li>
        <b>${esc(x.label || x.id)}</b> &mdash; ${esc(x.firmware || "unknown version")}${
          x.eol_note ? `. ${esc(x.eol_note)}` : ""}</li>`).join("")}</ul>
    </div></div>`;
  }

  $("devices-empty").hidden = d.devices.length > 0;
  setHTML($("device-rows"), d.devices.map((x) => {
    let status = '<span class="pill ok">ok</span>';
    if (!x.probed) status = '<span class="pill">not probed yet</span>';
    else if (!x.reachable) status = `<span class="pill bad">unreachable</span>`;
    else if (x.eol) status = `<span class="pill bad" title="${esc(x.eol_note || "This release receives no further updates.")}">end of life</span>`;
    else if (x.drift) status = `<span class="pill warn">drift</span>`;
    return `<tr>
      <td>
        <input class="namefld" value="${esc(x.label)}" spellcheck="false"
          placeholder="${esc((x.id || "").split(".")[0])}"
          title="Click to rename. The id below is what history is kept against and does not change."
          onchange="renameDevice(${jsq(x.id)}, this.value)"
          onkeydown="if (event.key === 'Enter') this.blur()">
        <div class="mono" style="color:var(--muted)">${esc(x.target)}</div>
      </td>
      <td class="mono">${esc(x.target)}</td>
      <td>${esc(x.site) || "-"}</td>
      <td>${status}${x.error ? `<div class="mono" style="color:var(--muted)">${esc(x.error)}</div>` : ""}</td>
      <td class="mono">${esc(x.firmware || "-")}</td>
      <td>${x.updates_known
        ? (x.updates
            ? `<span class="pill warn">${x.updates} update(s)</span>`
            : (x.eol
                ? '<span class="pill bad" title="Nothing is pending because nothing more will ever be released for it.">none coming</span>'
                : '<span class="pill ok">current</span>'))
        : '<span class="msg">-</span>'}${
        x.reboot_required ? ' <span class="pill warn">reboot</span>' : ""}</td>
      <td class="mono">${esc(x.expect_version || "-")}</td>
      <td>${x.latency_ms != null ? x.latency_ms + "ms" : "-"}</td>
      <td>
        <button class="act" onclick="deviceHistory(${jsq(x.id)})">History</button>
        <button class="act" onclick="probeDevice(${jsq(x.collector)}, ${jsq(x.id)})">Probe</button>
        <button class="act" onclick="removeDevice(${jsq(x.id)})">Remove</button>
      </td>
      <td>${esc(x.collector_host)}${x.collectors > 1
        ? ` <span class="pill warn" title="${x.collectors} machines are all probing this device because no collector is named for it in the manifest. Set a collector to stop the duplicate work.">+${x.collectors - 1} more</span>`
        : ""}</td>
      <td>${x.probed ? ago(x.checked_at) : "-"}</td>
    </tr>`;
  }).join(""));

  $("unmanaged-empty").hidden = d.unmanaged.length > 0;
  setHTML($("unmanaged-rows"), d.unmanaged.map((h) => `<tr>
      <td class="mono">${esc(h.ip)}</td>
      <td class="mono">${h.open_ports.join(", ")}</td>
      <td class="mono">${esc(h.hint) || "-"}</td>
      <td>${esc(h.site) || "-"}</td>
      <td>${esc(h.collector_host)}</td>
    </tr>`).join(""));
}

// The portal cannot know which address you reached it on (it may be bound to
// 0.0.0.0), so the commands are built from the URL in your address bar - which
// is by definition an address that works.
async function loadAdd() {
  if (!$("dev-hint").textContent) deviceKindChanged();
  const site = ($("add-site").value || "default").replace(/[^A-Za-z0-9._-]/g, "");
  const host = $("add-portal").value.trim() || location.host;
  let token = "<enrollment-token>";
  try {
    token = (await api("/api/enrollment")).token || token;
  } catch (e) {
    // Leave the placeholder; the shape of the command is still useful.
  }

  $("cmd-linux").textContent =
    `curl -fsSL http://${host}/install.sh | sh -s --` +
    `${AUTH_REQUIRED ? ` --token ${token}` : ""} --site ${site}`;

  // One line, no backslashes, no scriptblock, and no token to paste: the
  // agent asks the portal for it. `./` works in PowerShell and cannot be
  // mangled the way `.\` can.
  const needsToken = AUTH_REQUIRED ? ` --token ${token}` : "";
  $("cmd-win").textContent =
    `irm http://${host}/download/pp-agent.exe -OutFile pp-agent.exe; ` +
    `./pp-agent.exe setup --portal ${host}${needsToken} --site ${site}`;

  // A bare IP works until DHCP moves the portal, and then every enrolled agent
  // is pointing at nothing. Say so once, here, rather than in a runbook.
  const hint = $("add-hint");
  if (/^\d+\.\d+\.\d+\.\d+(:\d+)?$/.test(host)) {
    hint.textContent =
      "This is an IP address. If this portal is on DHCP, prefer its hostname " +
      "so agents keep working when the address changes.";
    hint.className = "msg bad";
  } else {
    hint.textContent = "Agents will connect to this address, so it must resolve from every machine you enrol.";
    hint.className = "msg";
  }
}

// The async clipboard API needs a secure context, and this portal is plain
// HTTP, so fall back to the old selection trick rather than silently failing.
function copyText(text, el) {
  const flash = () => {
    const o = el.textContent;
    el.textContent = "copied";
    setTimeout(() => (el.textContent = o), 900);
  };
  if (navigator.clipboard && window.isSecureContext) {
    navigator.clipboard.writeText(text).then(flash);
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); flash(); } catch (e) { /* select manually */ }
  document.body.removeChild(ta);
}

function copyCmd(id) {
  const text = $(id).textContent;
  const done = (btn) => { const o = btn.textContent; btn.textContent = "Copied"; setTimeout(() => (btn.textContent = o), 1200); };
  const btn = $(id).parentElement.querySelector("button");
  if (navigator.clipboard && window.isSecureContext) {
    navigator.clipboard.writeText(text).then(() => done(btn));
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); done(btn); } catch (e) { /* user can select manually */ }
  document.body.removeChild(ta);
}

async function loadManifest() {
  const m = await api("/api/manifest");
  $("manifest-doc").value = JSON.stringify(m, null, 2);
  $("manifest-msg").textContent = "revision " + m.revision;
  $("manifest-msg").className = "msg";

  const builds = await api("/api/builds");
  $("builds-empty").hidden = builds.length > 0;
  $("build-rows").innerHTML = builds.map((b) => `<tr>
      <td class="mono">${esc(b.version)}</td><td>${esc(b.os)}</td><td class="mono">${esc(b.arch)}</td>
      <td class="mono">${esc(b.url)}</td><td class="mono">${esc(b.sha256.slice(0, 16))}…</td>
    </tr>`).join("");
}

async function saveManifest() {
  const msg = $("manifest-msg");
  let doc;
  try {
    doc = JSON.parse($("manifest-doc").value);
  } catch (e) {
    msg.textContent = "Invalid JSON: " + e.message;
    msg.className = "msg bad";
    return;
  }
  try {
    const r = await api("/api/manifest", { method: "PUT", body: JSON.stringify(doc) });
    msg.textContent = `Published revision ${r.revision} to ${r.pushed_to} connected agent(s).`;
    msg.className = "msg ok";
    loadManifest();
  } catch (e) {
    msg.textContent = e.message;
    msg.className = "msg bad";
  }
}

async function loadActivity() {
  const rows = await api("/api/commands?limit=40");
  $("commands-empty").hidden = rows.length > 0;
  const host = (id) => (AGENTS.find((a) => a.id === id) || {}).hostname || id.slice(0, 8);
  const html = rows.map((c) => {
    const running = c.ok === null || c.ok === undefined;
    const state = running
      ? '<span class="pill">running</span>'
      : (c.ok ? '<span class="pill ok">ok</span>' : '<span class="pill bad">failed</span>');
    const body = (c.detail || c.progress || "").trim();
    return `<div style="padding:10px 14px;border-bottom:1px solid var(--line)">
      <div>${state} <b>${esc(c.kind)}</b> on ${esc(host(c.agent_id))}
        <span class="msg">· ${ago(c.created_at)}</span><span class="mono msg cmdid" title="${esc(c.id)} - click to copy" onclick="copyText('${esc(c.id)}', this)">${esc(c.id.slice(0, 8))}</span></div>
      ${c.summary ? `<div class="mono" style="margin-top:4px">${esc(c.summary)}</div>` : ""}
      ${body ? `<details data-k="${esc(c.id)}" ontoggle="tailLog(this)"><summary>output</summary>
        <pre data-k="${esc(c.id)}" data-live="${running ? 1 : 0}">${esc(body)}</pre></details>` : ""}
    </div>`;
  }).join("");
  setHTML($("commands"), html);
}

// Patching restarts services and can demand a reboot, so it is the one
// per-machine action that asks first.
async function restartAgent(id) {
  if (!confirm("Restart the agent on this machine? It re-detects package backends and reconnects in a few seconds. Nothing else on the machine is touched.")) return;
  await cmd(id, "restart_agent");
}

async function installPrereqs(id) {
  const warn = "Install the missing update tooling on this machine?\n\n" +
    "On Linux this installs fwupd, so firmware is looked at at all. On Windows it "  +
    "downloads PSWindowsUpdate and the WinGet client and repairs winget for all users.\n\n" +
    "Restart the agent afterwards so the new backend is detected - they are found "  +
    "once at startup.";
  if (!confirm(warn)) return;
  await cmd(id, "install_prerequisites");
}

async function fullUpgrade(id) {
  const warn = "Run a FULL upgrade? This can install new packages and remove existing ones. It is how held-back upgrades such as a kernel get applied.";
  if (!confirm(warn)) return;
  await cmd(id, "apply_patches", { full: true });
}

async function cleanupNow(id) {
  if (!confirm("Remove packages nothing depends on, and empty the package cache?")) return;
  await cmd(id, "cleanup");
}

async function patchNow(id, host) {
  if (!confirm(`Install pending OS updates on ${host}?

This can restart services and may require a reboot.`)) return;
  await cmd(id, "apply_patches");
}

async function cmd(id, kind, extra = {}) {
  // Latch before awaiting: the click that starts the request is the one we
  // need to stop happening twice.
  JUST_SENT.set(id, Date.now());
  refresh();
  try {
    const res = await api(`/api/agents/${id}/commands`, {
      method: "POST",
      body: JSON.stringify({ command: { kind, ...extra } }),
    });
    setTimeout(refresh, 400);
    return res;
  } catch (e) {
    JUST_SENT.delete(id);
    refresh();
    alert(e.message);
  }
}

async function broadcast(kind) {
  const msg = $("broadcast-msg");
  try {
    const r = await api("/api/commands/broadcast", {
      method: "POST",
      body: JSON.stringify({ command: { kind } }),
    });
    msg.textContent = `Sent to ${r.dispatched_to} agent(s).`;
    msg.className = "msg ok";
  } catch (e) {
    msg.textContent = e.message;
    msg.className = "msg bad";
  }
}

async function refresh() {
  try {
    // The fleet call also populates the hostname lookup the activity tab uses.
    await loadFleet();
    if (TAB === "devices") await loadDevices();
    if (TAB === "add") {
      if (!$("add-portal").value) $("add-portal").value = location.host;
      await loadAdd();
    }
    if (TAB === "manifest" && !$("manifest-doc").value) await loadManifest();
    if (TAB === "activity") await loadActivity();
    if (TAB === "agent") await loadAgent(ROUTE.id);
  } catch (e) {
    if (e.message !== "unauthorized") console.error(e);
  }
}

// The portal can be run with authentication turned off for a trusted
// network; ask it before deciding whether to demand a token.
async function boot() {
  try {
    const mode = await fetch("/api/auth-mode").then((r) => r.json());
    AUTH_REQUIRED = mode.required !== false;
  } catch (e) {
    // Unreachable or an older portal: assume auth is on rather than
    // silently dropping the token from requests.
  }
  if (!AUTH_REQUIRED || TOKEN) {
    $("app").hidden = false;
    showTab(TAB);
  } else {
    gate();
    $("gate-msg").className = "msg";
  }
}
boot();
// Polling covers both entry paths, and skips itself while the gate is up.
setInterval(() => { if (!$("app").hidden) refresh(); }, 5000);
</script>
</body>
</html>
"##;
