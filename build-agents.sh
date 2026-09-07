#!/usr/bin/env bash
# Build every agent binary the portal serves, fully static so they run on any
# Linux regardless of its glibc version.
#
# Run this inside the portal's container (it has the toolchains) then restart
# the portal. Requires: musl-tools, zig, cargo-zigbuild.
set -euo pipefail

DEST="${1:-/var/lib/patchpanel-portal/agents}"
cd "$(dirname "$0")"

echo "==> x86_64 (static musl)"
cargo build --release -p pp-agent --target x86_64-unknown-linux-musl

# aws-lc's C is compiled against the host's glibc headers, so a plain musl
# cross-link fails on __isoc23_* symbols. zig ships musl headers for every
# target, which is why this one goes through cargo-zigbuild.
echo "==> aarch64 (static musl, via zig)"
cargo zigbuild --release -p pp-agent --target aarch64-unknown-linux-musl

install -d -m 0755 "$DEST"
install -m 0755 target/x86_64-unknown-linux-musl/release/pp-agent  "$DEST/pp-agent-x86_64"
install -m 0755 target/x86_64-unknown-linux-musl/release/pp-agent  "$DEST/pp-agent"
install -m 0755 target/aarch64-unknown-linux-musl/release/pp-agent "$DEST/pp-agent-aarch64"
chown -R patchpanel:patchpanel "$DEST" 2>/dev/null || true

echo "==> published:"
for f in "$DEST"/pp-agent*; do
  printf "    %-20s %-6s glibc-refs=%s\n" "$(basename "$f")" "$(du -h "$f" | cut -f1)" \
    "$(strings -a "$f" 2>/dev/null | grep -c GLIBC_ || true)"
done
echo "Windows: build pp-agent.exe on a Windows host and copy it to $DEST/pp-agent.exe"
