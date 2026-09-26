#!/bin/sh
# Make an already-installed agent come back after a reboot.
#
# The shipped unit used to wait for network-online.target. On a host where that
# target is never reached - a Proxmox LXC with no wait-online helper is the
# common case - a unit that Wants it is never started at all: systemd neither
# fails it nor retries it, so `Restart=always` has nothing to act on and the
# machine comes back with no agent, silently, indefinitely.
#
# This is written as a drop-in rather than a replacement unit so it applies
# whatever version of the unit is installed, and so it is obvious later what was
# changed and why.
set -eu

DIR=/etc/systemd/system/patchpanel-agent.service.d
mkdir -p "$DIR"
cat > "$DIR/override.conf" <<'EOF'
[Unit]
# Emptying the setting first is required: drop-ins add to a list, so without
# this the original Wants= would survive alongside.
Wants=
After=
After=network.target

[Service]
# Make "always" mean always. systemd's default start limit gives up after a few
# rapid restarts and leaves the unit failed permanently, which for a fleet agent
# is the worst available behaviour: the machine goes quiet and stays quiet.
StartLimitIntervalSec=0
EOF

systemctl daemon-reload
systemctl enable patchpanel-agent.service
systemctl restart patchpanel-agent.service
sleep 2
systemctl is-active patchpanel-agent.service
