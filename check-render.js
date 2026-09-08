// Test driver appended to the extracted dashboard JavaScript by check-render.sh.
//
// `node --check` proves the page parses; it does not prove the machine page
// renders. This runs loadAgent() against real API responses with a DOM stub
// small enough to fit here, and asserts the per-machine tabs come out right.
// A blank device page has shipped before, and it looks exactly like a healthy
// one until you click a machine.

function fakeNodes(html, sel) {
  const attr = sel === "[data-pane]" ? "data-pane" : "data-p";
  const out = [];
  const re = new RegExp(attr + '="([^"]+)"', "g");
  let m;
  while ((m = re.exec(html))) {
    out.push({
      dataset: attr === "data-pane" ? { pane: m[1] } : { p: m[1] },
      hidden: true,
      classList: { toggle(_c, on) { this.on = on; } },
    });
  }
  return out;
}

function el(id) {
  return {
    id,
    innerHTML: "",
    hidden: false,
    dataset: {},
    textContent: "",
    style: {},
    offsetHeight: 0,
    classList: { toggle() {}, add() {}, remove() {} },
    querySelectorAll(sel) { return fakeNodes(this.innerHTML, sel); },
  };
}

const FIXTURES = JSON.parse(process.env.PP_FIXTURES);
let failures = 0;

function check(name, cond, extra) {
  if (cond) { console.log("    ok   " + name); return; }
  failures++;
  console.log("    FAIL " + name + (extra ? " - " + extra : ""));
}

async function run() {
  for (const [host, doc] of Object.entries(FIXTURES)) {
    console.log("  " + host + " (" + doc.os + ")");

    const body = el("agent-body");
    DOC = { "agent-body": body };
    globalThis.fetch = async () => ({
      ok: true, status: 200, json: async () => doc,
    });

    PANE = "overview";
    ROUTE = { tab: "agent", id: doc.id, pane: null };
    await loadAgent(doc.id);

    const html = body.innerHTML;
    check("rendered something", html.length > 500, html.length + " bytes");
    check("has a tab bar", html.includes('class="subnav"'));

    const panes = fakeNodes(html, "[data-pane]").map((n) => n.dataset.pane);
    const tabs = fakeNodes(html, ".subnav button").map((n) => n.dataset.p);
    check("every tab has a pane", tabs.every((t) => panes.includes(t)), tabs + " vs " + panes);
    check("every pane has a tab", panes.every((p) => tabs.includes(p)), panes + " vs " + tabs);
    check("has overview + updates", panes.includes("overview") && panes.includes("updates"));

    const linux = doc.os === "linux";
    const files = (doc.inventory || {}).source_files || [];
    if (linux && files.length) {
      check("linux with sources gets a Sources tab", panes.includes("sources"));
    }
    if (!linux) {
      check("windows gets no Sources tab", !panes.includes("sources"));
    }

    // Deep-linking to a pane this machine does not have must land somewhere
    // real rather than on a blank page.
    PANE = "sources";
    applyPane();
    check("a missing pane falls back", panes.includes(PANE), "landed on " + PANE);

    // Inline handlers are the blind spot: `node --check` sees the source that
    // builds them, never the string that lands in the attribute. A path
    // interpolated as JSON closed its own onclick early and every Save button
    // on the page threw SyntaxError on click, which looks like a button that
    // simply does nothing.
    const bad = [];
    const handlers = html.matchAll(/\son[a-z]+="([^"]*)"/g);
    for (const h of handlers) {
      const code = h[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&");
      try { new Function(code); } catch (e) { bad.push(code.slice(0, 60)); }
    }
    check("every inline handler parses", bad.length === 0, bad.join(" | "));

    // The source comparison renders from a second machine's data, which the
    // normal pass never supplies - so it would sail through every other check
    // and still throw the moment someone picked a machine from the dropdown.
    const other = Object.values(FIXTURES).find((x) => x.id !== doc.id);
    // An empty array is truthy; Windows has no sources pane to compare in.
    if (other && ((doc.inventory || {}).source_files || []).length) {
      AGENTS = Object.values(FIXTURES).map((x) => ({
        id: x.id, hostname: x.hostname, os_version: x.os_version,
      }));
      COMPARE = other.id;
      COMPARE_DATA = other;
      body.__lastHTML = null;
      await loadAgent(doc.id);
      const cmp = body.innerHTML;
      check("comparison renders", cmp.includes("Compared with " + other.hostname),
        cmp.length + " bytes");
      check("comparison lists both machines",
        cmp.includes(">" + doc.hostname + "</th>") && cmp.includes(">" + other.hostname + "</th>"));
      const cbad = [];
      for (const h of cmp.matchAll(/\son[a-z]+="([^"]*)"/g)) {
        try { new Function(h[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&")); }
        catch (e) { cbad.push(h[1].slice(0, 60)); }
      }
      check("comparison handlers parse", cbad.length === 0, cbad.join(" | "));
      COMPARE = null;
      COMPARE_DATA = null;
      body.__lastHTML = null;
      await loadAgent(doc.id);
    }

    // The counts on the tabs have to match what the panes actually contain.
    const updates = ((doc.inventory || {}).updates || []).length;
    const stuck = ((doc.inventory || {}).deferred || []).length;
    // Firmware is counted on the tab too, so the arithmetic has to include it.
    const fw = ((doc.inventory || {}).firmware || []).length;
    const shown = /data-p="updates"[^>]*>Updates(?:<span class="n[^"]*">(\d+)<)?/.exec(html);
    const claimed = shown && shown[1] ? Number(shown[1]) : 0;
    check("updates count matches the table", claimed === updates - stuck + fw,
      claimed + " on the tab, " + (updates - stuck + fw) + " installable plus firmware");
  }

  await checkDevices();

  console.log(failures ? "\n" + failures + " check(s) failed" : "\neverything renders");
  process.exit(failures ? 1 : 0);
}


// The devices table renders from a different code path than the machine page,
// and nothing was checking it. Both times an inline handler shipped broken, it
// was here.
async function checkDevices() {
  console.log("  devices table");

  const rows = JSON.parse(process.env.PP_DEVICES || '{"devices":[],"unmanaged":[]}');
  if (!rows.devices.length) {
    check("has at least one device to render", false, "no devices declared on the portal");
    return;
  }

  DOC = {
    "device-rows": el("device-rows"),
    "unmanaged-rows": el("unmanaged-rows"),
    "devices-empty": el("devices-empty"),
    "devices-eol": el("devices-eol"),
    "unmanaged-empty": el("unmanaged-empty"),
    "dev-hint": el("dev-hint"),
  };
  globalThis.fetch = async () => ({ ok: true, status: 200, json: async () => rows });

  await loadDevices();
  const html = DOC["device-rows"].innerHTML;
  check("rows rendered", html.length > 100, html.length + " bytes");

  const bad = [];
  for (const h of html.matchAll(/\son[a-z]+="([^"]*)"/g)) {
    const code = h[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&").replace(/&lt;/g, "<");
    try { new Function(code); } catch (e) { bad.push(code.slice(0, 70)); }
  }
  check("every device handler parses", bad.length === 0, bad.join(" | "));

  // An id with a quote in it would break the naive escaping, and device ids
  // come from whatever someone typed into the form.
  const nasty = JSON.parse(JSON.stringify(rows));
  nasty.devices = [Object.assign({}, rows.devices[0], {
    id: `it's "quoted" <b>`, label: `a'b"c`,
  })];
  globalThis.fetch = async () => ({ ok: true, status: 200, json: async () => nasty });
  await loadDevices();
  const evil = [];
  for (const h of DOC["device-rows"].innerHTML.matchAll(/\son[a-z]+="([^"]*)"/g)) {
    const code = h[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&").replace(/&lt;/g, "<");
    try { new Function(code); } catch (e) { evil.push(code.slice(0, 70)); }
  }
  check("handlers survive a hostile id", evil.length === 0, evil.join(" | "));
}

run().catch((e) => { console.log("threw: " + (e && e.stack || e)); process.exit(1); });
