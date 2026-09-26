#!/bin/sh
# Assert that the routers are configured to leave evidence when they die.
#
# On 2026-09-25 the winetown router rebooted uncleanly: the log stops mid-line
# at 16:59 and resumes with a kernel boot at 17:02, with no shutdown sequence
# in between. Nothing could say why, because `dumpdev` was set to NO - crash
# dumps were disabled, so a panic would have written nothing to the 8GB of
# swap sitting unused, and `savecore` had nothing to recover. The evidence was
# not lost; it was never collected.
#
# That is a configuration fact, not a patch fact, so the OPNsense probe cannot
# see it - that probe asks the firmware API about versions and deliberately
# nothing else. This reports it the way PatchPanel already accepts things it
# cannot reach itself: as a job, with the findings as facts.
#
# The useful property of a job over a dashboard query is silence. If this stops
# running - key revoked, box rebuilt, script disabled - `pp_init`'s cadence
# argument means PatchPanel notices the absence, which is the failure mode that
# otherwise looks exactly like everything being fine.
#
# Install as an Unraid User Script. Needs the same root SSH keys the router
# backup job already uses.

PP_URL="${PP_URL:-http://patchpanel}"

PP_LIB=/boot/config/pp-report.sh
if curl -fsS -m 5 "$PP_URL/pp-report.sh" -o "$PP_LIB.new" 2>/dev/null &&
   grep -q "^pp_init()" "$PP_LIB.new"; then mv "$PP_LIB.new" "$PP_LIB"; fi
rm -f "$PP_LIB.new"
if [ -r "$PP_LIB" ]; then . "$PP_LIB"; else
  pp_init() { :; }; pp_fact() { :; }; pp_ok() { :; }
  pp_fail() { :; }; pp_finish() { :; }
fi

pp_init unraid/router_config_audit 24

ROUTERS="winetown:192.168.6.1 seacastle:192.168.2.1 millcreek:192.168.3.1"
SSH="ssh -o BatchMode=yes -o ConnectTimeout=8 -o StrictHostKeyChecking=accept-new"

# root's login shell on OPNsense is csh, which cannot parse $(...). Feeding the
# script to `sh` on its stdin sidesteps the login shell entirely - passing it as
# an argument would be interpreted by csh and silently produce nothing.
REMOTE='
echo "dumpdev=$(sysrc -n dumpdev 2>/dev/null)"
echo "savecore=$(sysrc -n savecore_enable 2>/dev/null)"
echo "armed=$(dumpon -l 2>/dev/null)"
echo "dumps=$(ls /var/crash/*.core /var/crash/vmcore.* 2>/dev/null | wc -l | tr -d " ")"
'

bad=0
unreachable=0
checked=0

for entry in $ROUTERS; do
    name="${entry%%:*}"
    host="${entry##*:}"

    # One round trip per router. Each value is printed on its own line so a
    # partial answer is still readable rather than an unparseable blob.
    out=$(echo "$REMOTE" | $SSH "root@$host" sh 2>/dev/null)

    if [ -z "$out" ]; then
        echo "$name: UNREACHABLE"
        pp_fact "$name.dumpdev" "unreachable"
        unreachable=$((unreachable + 1))
        continue
    fi

    checked=$((checked + 1))
    dumpdev=$(echo "$out" | sed -n 's/^dumpdev=//p')
    savecore=$(echo "$out" | sed -n 's/^savecore=//p')
    armed=$(echo "$out" | sed -n 's/^armed=//p')
    dumps=$(echo "$out" | sed -n 's/^dumps=//p')

    pp_fact "$name.dumpdev" "${dumpdev:-unset}"
    [ -n "$dumps" ] && [ "$dumps" != "0" ] && pp_fact "$name.crashdumps" "$dumps"

    # Unset is as bad as NO. FreeBSD's own default is AUTO, but OPNsense ships
    # NO, so an absent setting here means dumps are off rather than implied.
    case "$dumpdev" in
        ""|NO|no|NONE|none)
            echo "$name: FAILED - crash dumps disabled (dumpdev=${dumpdev:-unset})"
            bad=$((bad + 1))
            continue
            ;;
    esac

    if [ "$savecore" = "NO" ]; then
        echo "$name: FAILED - dumpdev set but savecore_enable=NO, nothing would recover the dump"
        bad=$((bad + 1))
        continue
    fi

    # Configured but not armed means it will only take effect after a reboot -
    # worth saying, because that is the window in which it still cannot help.
    if [ -z "$armed" ]; then
        echo "$name: WARNING - dumpdev: $dumpdev configured but not armed until reboot"
    else
        echo "$name: ok - dumpdev: $dumpdev, savecore: ${savecore:-default}, armed: $armed"
    fi

    if [ -n "$dumps" ] && [ "$dumps" != "0" ]; then
        echo "$name: $dumps crash dump(s) waiting in /var/crash - somebody should read them"
    fi
done

echo "===== done: $((checked - bad)) ok, $bad failed, $unreachable unreachable ====="

if [ "$unreachable" -gt 0 ]; then
    detail="$((checked - bad))/$checked ok, $unreachable unreachable"
else
    detail="$((checked - bad))/$checked routers will capture a panic"
fi

pp_finish "$bad" "$detail"
[ "$bad" -eq 0 ]
