#!/usr/bin/env bash
# Install the PatchPanel agent as a systemd service.
#
#   sudo ./install-agent.sh ws://portal.example.com:8080/api/agent/ws <enrollment-token> [site]
#
# Re-running this is safe: it upgrades the binary in place and restarts the
# service, keeping the agent's existing identity in /var/lib/patchpanel.

set -euo pipefail

PORTAL="${1:-}"
TOKEN="${2:-}"
SITE="${3:-}"

if [[ -z "$PORTAL" || -z "$TOKEN" ]]; then
  echo "usage: $0 <portal-ws-url> <enrollment-token> [site]" >&2
  exit 64
fi

if [[ $EUID -ne 0 ]]; then
  echo "this script must run as root (it installs a system service)" >&2
  exit 1
fi

BIN_SRC="$(dirname "$0")/../target/release/pp-agent"
BIN_DST="/usr/local/bin/pp-agent"
UNIT="/etc/systemd/system/patchpanel-agent.service"

if [[ ! -f "$BIN_SRC" ]]; then
  echo "missing $BIN_SRC — run 'cargo build --release -p pp-agent' first" >&2
  exit 1
fi

# Stop before replacing: on Linux the write would otherwise land under a
# running process, and we want a clean restart anyway.
systemctl stop patchpanel-agent.service 2>/dev/null || true

install -m 0755 "$BIN_SRC" "$BIN_DST"
install -d -m 0755 /etc/patchpanel /var/lib/patchpanel

"$BIN_DST" --config /etc/patchpanel/agent.json enroll \
  --portal "$PORTAL" --token "$TOKEN" --site "$SITE" \
  --state-dir /var/lib/patchpanel

# The config holds the enrollment secret until the portal issues a real token.
chmod 0600 /etc/patchpanel/agent.json

install -m 0644 "$(dirname "$0")/patchpanel-agent.service" "$UNIT"
systemctl daemon-reload
systemctl enable --now patchpanel-agent.service

echo
echo "installed. check it with:"
echo "  systemctl status patchpanel-agent"
echo "  journalctl -u patchpanel-agent -f"
