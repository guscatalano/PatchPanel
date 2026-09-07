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
  table { border-collapse: collapse; width: 100%; font-size: 13px; }
  th, td { text-align: left; padding: 8px 14px; border-bottom: 1px solid var(--line); white-space: nowrap; }
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

  button.act {
    font: inherit; font-size: 12px; padding: 3px 9px; margin-right: 4px;
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

  .back { display: inline-block; margin-bottom: 14px; color: var(--accent); cursor: pointer; font-size: 13px; }
  .back:hover { text-decoration: underline; }
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
    <nav>
      <button data-tab="fleet" class="active">Fleet</button>
      <button data-tab="devices">Devices</button>
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
            <th>Host</th><th>IP</th><th>OS</th><th>Hardware</th><th>Site</th>
            <th>Updates</th><th>Drift</th><th>Devices</th><th>Manifest</th><th>Last seen</th><th></th>
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
        <div class="scroll"><table>
          <thead><tr>
            <th>Device</th><th>Target</th><th>Site</th><th>Status</th>
            <th>Firmware</th><th>Expected</th><th>Latency</th><th>Collector</th><th>Checked</th>
          </tr></thead>
          <tbody id="device-rows"></tbody>
        </table></div>
        <div class="empty" id="devices-empty" hidden>
          No devices declared. Add them under <code>devices</code> in the manifest.
        </div>
      </div>
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
function routeOf(hash) {
  const h = (hash || "").replace(/^#/, "");
  if (h.startsWith("agent/")) return { tab: "agent", id: h.slice(6) };
  return { tab: TABS.includes(h) ? h : "fleet", id: null };
}
let ROUTE = routeOf(location.hash);
let TAB = ROUTE.tab;
let AGENTS = [];
let REV = 0;
let AUTH_REQUIRED = true;

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

function showTab(tab, id) {
  ROUTE = { tab, id: id || null };
  TAB = tab;
  const want = id ? `agent/${id}` : tab;
  // No nav button is highlighted on a detail page; it is not a tab.
  document.querySelectorAll("nav button").forEach((x) =>
    x.classList.toggle("active", x.dataset.tab === tab));
  document.querySelectorAll("section").forEach((s) =>
    s.classList.toggle("active", s.id === tab));
  if (location.hash.slice(1) !== want) location.hash = want;
  refresh();
}

function openAgent(id) { showTab("agent", id); }

document.querySelectorAll("nav button").forEach((b) => {
  b.onclick = () => showTab(b.dataset.tab);
});

window.addEventListener("hashchange", () => {
  const r = routeOf(location.hash);
  if (r.tab !== ROUTE.tab || r.id !== ROUTE.id) showTab(r.tab, r.id);
});

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
  $("tiles").innerHTML =
    tile(s.online, "online") +
    tile(s.offline, "offline", "warn") +
    tile(s.pending_security, "security updates", "bad") +
    tile(s.pending_updates, "updates pending", "warn") +
    tile(s.needs_reboot, "need reboot", "warn") +
    tile(s.app_drift, "app drift", "warn") +
    tile(s.stale_manifest, "stale manifest", "warn") +
    tile(s.devices, "devices") +
    tile(s.devices_unreachable, "devices down", "bad") +
    tile(s.devices_drifted, "firmware drift", "warn");

  $("agents-empty").hidden = d.agents.length > 0;
  $("agents").innerHTML = d.agents.map((a) => {
    const live = a.connected ? "on" : (a.online ? "on" : "off");
    const upd = a.security_count > 0
      ? `<span class="pill bad">${a.security_count} sec</span> ${a.update_count - a.security_count}`
      : (a.update_count || "-");
    const dev = a.device_count
      ? `${a.device_count}${a.device_problem_count ? ` <span class="pill bad">${a.device_problem_count}</span>` : ""}`
      : "-";
    const hw = a.hardware || {};
    const cpu = hw.cpu_model
      ? `${esc(hw.cpu_model)}<div class="msg">${hw.cpu_cores || "?"}c/${hw.cpu_threads || "?"}t · ${hw.memory_mb ? (hw.memory_mb / 1024).toFixed(0) + " GB" : "? GB"}</div>`
      : "-";
    const ips = (hw.ip_addresses || []).length
      ? `${esc(hw.ip_addresses[0])}${hw.ip_addresses.length > 1 ? `<div class="msg">+${hw.ip_addresses.length - 1} more</div>` : ""}`
      : "-";
    return `<tr title="${esc(hw.vendor || "")}">
      <td><span class="dot ${live}"></span><a href="#agent/${a.id}" style="color:inherit">${esc(a.hostname)}</a>${a.reboot_required ? ' <span class="pill warn">reboot</span>' : ""}
          <div class="msg">agent ${esc(a.agent_version)}</div></td>
      <td class="mono">${ips}</td>
      <td>${esc(a.os_version)} <span class="mono">${esc(a.arch)}</span></td>
      <td class="mono">${cpu}</td>
      <td>${esc(a.site) || "-"}</td>
      <td>${upd}</td>
      <td>${a.drift_count ? `<span class="pill warn">${a.drift_count}</span>` : "-"}</td>
      <td>${dev}</td>
      <td class="mono">${a.applied_revision < REV ? `<span class="pill warn">r${a.applied_revision}</span>` : "r" + a.applied_revision}</td>
      <td>${ago(a.last_seen)}</td>
      <td style="text-align:right; white-space:nowrap">
        <button class="act" ${a.connected ? "" : "disabled"}
          title="Re-read installed packages and check for available updates. Changes nothing."
          onclick="cmd('${a.id}','collect_inventory')">Rescan</button>
        <button class="act" ${a.connected ? "" : "disabled"}
          title="Install this machine's pending OS updates now (apt/dnf on Linux, Windows Update). This changes the system and may require a reboot."
          onclick="patchNow('${a.id}','${esc(a.hostname)}')">Install updates</button>
        <button class="act" ${a.connected ? "" : "disabled"}
          title="Install, upgrade or remove applications so the machine matches the manifest's desired state."
          onclick="cmd('${a.id}','apply_manifest')">Apply manifest</button>
      </td>
    </tr>`;
  }).join("");
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
  const state = d.connected
    ? '<span class="pill ok">connected</span>'
    : (d.online ? '<span class="pill warn">recently seen</span>' : '<span class="pill bad">offline</span>');

  // Patch history is the command log filtered to the actions that change the
  // machine; a "rescan" is not an event anyone wants in a history.
  const CHANGED = ["apply_patches", "apply_manifest", "self_update", "reboot"];
  const history = (d.commands || []).filter((c) => CHANGED.includes(c.kind));

  body.innerHTML = `
    <div class="back" onclick="showTab('fleet')">&larr; Fleet</div>
    <div class="hdr">
      <h2>${esc(d.hostname)}</h2>${state}
      ${d.reboot_required ? '<span class="pill warn">reboot required</span>' : ""}
      <span class="msg">${esc(d.os_version)} &middot; ${esc(d.site) || "no site"}</span>
    </div>

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
          ["Booted", d.boot_time ? `${since(d.boot_time)} ago <span class="msg">(${new Date(d.boot_time).toLocaleString()})</span>` : "-"],
          ["Last seen", `${ago(d.last_seen)}`],
          ["First enrolled", new Date(d.first_seen).toLocaleString()],
          ["Agent version", `<span class="mono">${esc(d.agent_version)}</span>`],
          ["Manifest", d.applied_revision < REV
            ? `<span class="pill warn">r${d.applied_revision}</span> portal is at r${REV}`
            : `r${d.applied_revision} <span class="msg">up to date</span>`],
          ["Package backends", (d.backends || []).map((b) => `<span class="mono">${esc(b)}</span>`).join(", ")],
          ["Agent id", `<span class="mono msg">${esc(d.id)}</span>`],
        ])}
      </div>
    </div>

    <div class="card">
      <h2>Pending updates &mdash; ${updates.length}${sec.length ? `, ${sec.length} security` : ""}</h2>
      ${updates.length ? `<div class="scroll"><table>
        <thead><tr><th>Package</th><th>Installed</th><th>Available</th><th>Source</th></tr></thead>
        <tbody>${updates.map((u) => `<tr>
          <td>${esc(u.name)}${u.security ? ' <span class="pill bad">security</span>' : ""}</td>
          <td class="mono">${esc(u.current_version) || "-"}</td>
          <td class="mono">${esc(u.new_version)}</td>
          <td class="mono">${esc(u.source)}</td></tr>`).join("")}</tbody>
      </table></div>` : `<div class="empty">Nothing pending${inv.collected_at ? ` as of ${ago(inv.collected_at)}` : ""}.</div>`}
      <div class="bar">
        <button class="act" ${d.connected ? "" : "disabled"} onclick="cmd('${d.id}','collect_inventory')">Rescan</button>
        <button class="act" ${d.connected ? "" : "disabled"} onclick="patchNow('${d.id}','${esc(d.hostname)}')">Install updates</button>
        <button class="act" ${d.connected ? "" : "disabled"} onclick="cmd('${d.id}','apply_manifest')">Apply manifest</button>
        <span class="msg">${inv.collected_at ? `inventory collected ${ago(inv.collected_at)}` : ""}</span>
      </div>
    </div>

    ${(inv.drift || []).length ? `<div class="card"><h2>Application drift</h2>
      <div class="scroll"><table><thead><tr><th>App</th><th>Wanted</th><th>Found</th></tr></thead>
      <tbody>${inv.drift.map((x) => `<tr><td>${esc(x.app)}</td>
        <td class="mono">${esc(x.desired)}</td><td class="mono">${esc(x.observed)}</td></tr>`).join("")}</tbody>
      </table></div></div>` : ""}

    <div class="card">
      <h2>History &mdash; changes made to this machine</h2>
      ${history.length ? history.map((c) => {
        const st = c.ok === null || c.ok === undefined
          ? '<span class="pill">running</span>'
          : (c.ok ? '<span class="pill ok">ok</span>' : '<span class="pill bad">failed</span>');
        const out = (c.detail || c.progress || "").trim();
        return `<div style="padding:10px 14px;border-bottom:1px solid var(--line)">
          <div>${st} <b>${esc(c.kind)}</b>
            <span class="msg">&middot; ${new Date(c.created_at).toLocaleString()} &middot; ${ago(c.created_at)}</span></div>
          ${c.summary ? `<div class="mono" style="margin-top:4px">${esc(c.summary)}</div>` : ""}
          ${out ? `<details><summary>output</summary><pre>${esc(out)}</pre></details>` : ""}
        </div>`;
      }).join("") : `<div class="empty">Nothing has changed this machine yet.</div>`}
    </div>

    <div class="card">
      <h2>Installed packages &mdash; ${(inv.packages || []).length}</h2>
      <div class="step">
        <input id="pkg-filter" placeholder="filter&hellip;" spellcheck="false"
          style="width:100%;padding:7px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font-family:var(--mono);font-size:12.5px">
      </div>
      <div class="scroll" style="max-height:380px;overflow-y:auto">
        <table><tbody id="pkg-rows">${(inv.packages || []).map((p) =>
          `<tr data-n="${esc((p.name || "").toLowerCase())}"><td>${esc(p.name)}</td>
           <td class="mono">${esc(p.version)}</td><td class="mono msg">${esc(p.source)}</td></tr>`).join("")}</tbody></table>
      </div>
    </div>`;

  const filter = $("pkg-filter");
  if (filter) {
    filter.oninput = () => {
      const q = filter.value.toLowerCase();
      document.querySelectorAll("#pkg-rows tr").forEach((tr) => {
        tr.hidden = q && !tr.dataset.n.includes(q);
      });
    };
  }
}

async function loadDevices() {
  const d = await api("/api/devices");
  $("devices-empty").hidden = d.devices.length > 0;
  $("device-rows").innerHTML = d.devices.map((x) => {
    let status = '<span class="pill ok">ok</span>';
    if (!x.reachable) status = `<span class="pill bad">unreachable</span>`;
    else if (x.drift) status = `<span class="pill warn">drift</span>`;
    return `<tr>
      <td>${esc(x.label || x.id)}<div class="mono" style="color:var(--muted)">${esc(x.id)}</div></td>
      <td class="mono">${esc(x.target)}</td>
      <td>${esc(x.site) || "-"}</td>
      <td>${status}${x.error ? `<div class="mono" style="color:var(--muted)">${esc(x.error)}</div>` : ""}</td>
      <td class="mono">${esc(x.firmware || "-")}</td>
      <td class="mono">${esc(x.expect_version || "-")}</td>
      <td>${x.latency_ms != null ? x.latency_ms + "ms" : "-"}</td>
      <td>${esc(x.collector_host)}</td>
      <td>${ago(x.checked_at)}</td>
    </tr>`;
  }).join("");

  $("unmanaged-empty").hidden = d.unmanaged.length > 0;
  $("unmanaged-rows").innerHTML = d.unmanaged.map((h) => `<tr>
      <td class="mono">${esc(h.ip)}</td>
      <td class="mono">${h.open_ports.join(", ")}</td>
      <td class="mono">${esc(h.hint) || "-"}</td>
      <td>${esc(h.site) || "-"}</td>
      <td>${esc(h.collector_host)}</td>
    </tr>`).join("");
}

// The portal cannot know which address you reached it on (it may be bound to
// 0.0.0.0), so the commands are built from the URL in your address bar - which
// is by definition an address that works.
async function loadAdd() {
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
  $("commands").innerHTML = rows.map((c) => {
    const state = c.ok === null || c.ok === undefined
      ? '<span class="pill">running</span>'
      : (c.ok ? '<span class="pill ok">ok</span>' : '<span class="pill bad">failed</span>');
    const body = (c.detail || c.progress || "").trim();
    return `<div style="padding:10px 14px;border-bottom:1px solid var(--line)">
      <div>${state} <b>${esc(c.kind)}</b> on ${esc(host(c.agent_id))}
        <span class="msg">· ${ago(c.created_at)}</span></div>
      ${c.summary ? `<div class="mono" style="margin-top:4px">${esc(c.summary)}</div>` : ""}
      ${body ? `<details><summary>output</summary><pre>${esc(body)}</pre></details>` : ""}
    </div>`;
  }).join("");
}

// Patching restarts services and can demand a reboot, so it is the one
// per-machine action that asks first.
async function patchNow(id, host) {
  if (!confirm(`Install pending OS updates on ${host}?

This can restart services and may require a reboot.`)) return;
  await cmd(id, "apply_patches");
}

async function cmd(id, kind, extra = {}) {
  try {
    await api(`/api/agents/${id}/commands`, {
      method: "POST",
      body: JSON.stringify({ command: { kind, ...extra } }),
    });
    setTimeout(refresh, 400);
  } catch (e) {
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
