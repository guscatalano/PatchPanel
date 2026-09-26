#!/usr/bin/env bash
# Render the per-machine page against real portal data and check it.
#
# check-ui.sh proves the dashboard parses. This proves it renders: it pulls
# every agent's API response from a live portal, runs loadAgent() over a small
# DOM stub, and asserts the machine page's tabs and panes line up. A page that
# throws halfway through looks identical to a healthy one from the outside.
#
#   ./check-render.sh [portal-host]      (default: patchpanel)
set -euo pipefail
cd "$(dirname "$0")"

PORTAL="${1:-patchpanel}"
OUT="${TMPDIR:-/tmp}/pp-render.js"

if ! command -v node >/dev/null 2>&1; then
  echo "node not found; skipping the render check" >&2
  exit 0
fi

echo "==> fetching fixtures from $PORTAL"
FIXTURES="$(python3 - "$PORTAL" <<'PY'
import json, sys, urllib.request

host = sys.argv[1]
get = lambda p: json.load(urllib.request.urlopen(f"http://{host}{p}", timeout=15))

fleet = get("/api/fleet")
out = {}
for a in fleet["agents"]:
    out[a["hostname"]] = get("/api/agents/" + a["id"])
print(json.dumps(out))
PY
)"

python3 - "$OUT" <<'PY'
import io, sys

prelude = '''// --- DOM stub -------------------------------------------------------------
var DOC = {};
function stubEl(id) {
  return { id, innerHTML: "", value: "", hidden: false, dataset: {},
    textContent: "", className: "", style: {}, offsetHeight: 0,
    classList: { toggle() {}, add() {}, remove() {} },
    querySelectorAll: () => [], focus() {}, scrollIntoView() {} };
}
for (const id of ["app", "gate", "gate-msg", "agent-body"]) DOC[id] = stubEl(id);
var document = {
  getElementById: (id) => DOC[id] || null,
  querySelectorAll: () => [],
  addEventListener() {},
  createElement: () => stubEl("tmp"),
  body: { appendChild() {}, removeChild() {} },
};
var window = { addEventListener() {}, isSecureContext: false,
  scrollY: 0, scrollTo() {} };
var requestAnimationFrame = (f) => { f(); return 0; };
var location = { hash: "", host: "portal.test" };
var localStorage = { getItem: () => "", setItem() {} };
var navigator = {};
var setInterval = () => 0;
var setTimeout = () => 0;
globalThis.fetch = async () => ({ ok: true, status: 200, json: async () => ({ required: false }) });
// --- dashboard ------------------------------------------------------------
'''

s = io.open("crates/pp-portal/src/ui.rs", encoding="utf-8").read()
body = s[s.index("<script>") + len("<script>"): s.index("</script>")]
driver = io.open("check-render.js", encoding="utf-8").read()

io.open(sys.argv[1], "w", encoding="utf-8", newline="\n").write(
    prelude + body + "\n// --- checks ---\n" + driver)
print("built %s" % sys.argv[1])
PY

DEVICES="$(python3 -c "
import json, sys, urllib.request
host = sys.argv[1]
print(json.dumps(json.load(urllib.request.urlopen(f'http://{host}/api/devices', timeout=15))))
" "$PORTAL")"

FLEET="$(python3 -c "
import json, sys, urllib.request
host = sys.argv[1]
print(json.dumps(json.load(urllib.request.urlopen(f'http://{host}/api/fleet', timeout=15))))
" "$PORTAL")"

PP_FIXTURES="$FIXTURES" PP_DEVICES="$DEVICES" PP_FLEET="$FLEET" node "$OUT"
