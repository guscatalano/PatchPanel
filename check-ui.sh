#!/usr/bin/env bash
# Syntax-check the dashboard JavaScript that is embedded in ui.rs.
#
# The dashboard is a Rust string, so `cargo build` will happily compile a page
# whose JavaScript does not parse. That has shipped a blank dashboard more than
# once. Run this before deploying the portal.
set -euo pipefail
cd "$(dirname "$0")"

OUT="${TMPDIR:-/tmp}/pp-dash.js"
python3 - "$OUT" <<'PY'
import io, sys
s = io.open("crates/pp-portal/src/ui.rs", encoding="utf-8").read()
body = s[s.index("<script>") + len("<script>"): s.index("</script>")]
io.open(sys.argv[1], "w", encoding="utf-8", newline="\n").write(body)
print("extracted %d bytes" % len(body))
PY

if command -v node >/dev/null 2>&1; then
  node --check "$OUT" && echo "dashboard JavaScript parses cleanly"
else
  echo "node not found; skipping the parse check" >&2
  exit 0
fi
