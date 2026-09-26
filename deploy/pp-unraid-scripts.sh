#!/bin/bash
# Report every Unraid User Script to PatchPanel, from one cron entry.
#
# Install as a User Script of its own, scheduled every 15 minutes or so. It
# touches none of the other scripts: it reads the logs the plugin already
# writes and reports what it finds, so scripts added later are picked up with
# no further work.
#
# Unraid's GraphQL API cannot see User Scripts at all - the plugin is a third
# party addition and `userScripts`, `scripts`, `cron` and `plugins` are all
# rejected by the schema - so a watcher on the box is the only way to see them
# from outside.
#
# Bash rather than sh: script names contain spaces, and arrays are the
# difference between handling that and hoping.

PP_URL="${PP_URL:-http://localhost}"
PP_TOKEN="${PP_TOKEN:-}"
TMPDIR_SCRIPTS=/tmp/user.scripts/tmpScripts
PREFIX="${PP_PREFIX:-unraid/}"

# How often each script is expected to run, in hours.
#
# The plugin keeps its schedule in the webGUI rather than on disk - there is no
# `schedule` file beside the script and no cron entry naming it - so this is
# the one thing that cannot be discovered and has to be stated. A script with
# no entry here is still reported; it simply can never be called late, because
# nothing has said what late would mean.
expected_hours() {
    case "$1" in
        backup_routers)     echo 24  ;;
        "update ip records") echo 1  ;;
        "update ssl cert")  echo 168 ;;
        delete.ds_store)    echo 24  ;;
        *)                  echo 0   ;;
    esac
}

json_escape() {
    # Backslash first, then quotes, then carriage returns and tabs, and
    # newlines last. Whole log files come through here now, not one-line
    # summaries, and a raw control character in a JSON string is invalid.
    printf '%s' "$1" \
        | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' \
        | tr -d '\r' \
        | sed -e 's/\t/\\t/g' \
        | sed -e ':a' -e 'N' -e '$!ba' -e 's/\n/\\n/g'
}

# Report one run. `at` is what makes this honest: these runs happened hours
# ago, and recording them as "now" would mean a script could never be overdue
# as long as this watcher was alive.
report() {  # name ok detail at_rfc3339 every_hours [logfile]
    local body out rc code auth=() logjson=""
    # The tail only. These logs reach hundreds of kilobytes and the portal
    # keeps the last 32 KB regardless, so sending the whole file every fifteen
    # minutes would spend bandwidth to have it discarded at the far end.
    if [ -n "${6:-}" ] && [ -r "$6" ]; then
        # `tail -c` lands mid-line, so drop the fragment it starts with - a log
        # that opens on half a word reads like corruption rather than a trim.
        # `tail -n +2` only removes anything when there was a cut to make.
        logjson=",\"log\":\"$(json_escape "$(tail -c 30000 "$6" | tail -n +2)")\""
    fi
    body=$(printf '{"name":"%s","ok":%s,"detail":"%s","every_hours":%s%s%s}' \
        "$(json_escape "$1")" "$2" "$(json_escape "$3")" "$5" \
        "${4:+,\"at\":\"$4\"}" "$logjson")
    [ -n "$PP_TOKEN" ] && auth=(-H "Authorization: Bearer $PP_TOKEN")

    out=$(curl -sS -m 10 -w '\n%{http_code}' -X POST "$PP_URL/api/jobs" \
        -H 'Content-Type: application/json' "${auth[@]}" -d "$body" 2>&1)
    rc=$?
    code=$(printf '%s' "$out" | tail -n1)

    # This script exists to report. Swallowing a reporting failure is right for
    # a library embedded in somebody's backup job - reporting is not that job -
    # but here it is the whole job failing, and staying quiet about it produces
    # exactly the silence this was built to remove.
    if [ "$rc" -ne 0 ] || [ "$code" != "200" ]; then
        echo "  FAILED to report '$1': ${rc:+curl exit $rc, }HTTP ${code:-none}"
        echo "    $(printf '%s' "$out" | head -n1)"
        failed_reports=$((failed_reports + 1))
        return 1
    fi
    if [ "$2" = true ]; then
        echo "  reported $1 - ok${4:+, ran $4}"
    else
        echo "  reported $1 - FAILED${4:+, started $4}"
    fi
    return 0
}

# "Sep 20, 2026  04:40.11" -> RFC 3339 in this box's zone.
#
# The plugin's own footer, which beats the log file's mtime: mtime moves when
# anything writes, including a run that then died, while this line is only
# printed on a clean finish.
to_rfc3339() {
    local when="${1//,/}"           # Sep 20 2026  04:40.11
    when="${when%.*}:${when##*.}"   # ...04:40:11
    date -d "$when" --rfc-3339=seconds 2>/dev/null | tr ' ' 'T'
}

seen=0
late=0
running=0
failed_reports=0

# Which of these directories is this script.
#
# The plugin runs scripts as /tmp/user.scripts/tmpScripts/<name>/script, so $0
# names us. Without this the watcher reads its own log while still writing it,
# finds a Script Start and no Script Finished, and dutifully reports itself as
# having died - every single run.
SELF=""
case "$0" in
    */tmpScripts/*/script) SELF=$(basename "$(dirname "$0")") ;;
esac

# Reachability first, and loudly. The likeliest reason this reports nothing is
# that a short name the portal knows itself by does not resolve from this box -
# they are often on different subnets - and that deserves an instruction rather
# than an empty log.
echo "PatchPanel at $PP_URL"
if ! curl -fsS -m 10 "$PP_URL/api/auth-mode" >/dev/null 2>&1; then
    echo "CANNOT REACH $PP_URL from this box." >&2
    echo "Set PP_URL at the top of this script to something it can resolve -" >&2
    echo "an FQDN or an IP address - and run it again." >&2
    exit 1
fi

if [ ! -d "$TMPDIR_SCRIPTS" ]; then
    echo "No $TMPDIR_SCRIPTS yet: the User Scripts plugin has not run anything" >&2
    echo "since this box last booted, so there is nothing to report." >&2
    exit 0
fi

for dir in "$TMPDIR_SCRIPTS"/*/; do
    [ -d "$dir" ] || continue
    name=$(basename "$dir")
    log="$dir/log.txt"

    [ -n "$SELF" ] && [ "$name" = "$SELF" ] && continue

    # No log is not "never ran". /tmp is tmpfs, so a reboot wipes every log on
    # the box - reporting "never" here would turn one restart into an alarm on
    # every script at once. Say nothing; the portal keeps what it already knew.
    [ -r "$log" ] || continue

    started=$(grep -a 'Script Start' "$log" | tail -1)
    finished=$(grep -a 'Script Finished' "$log" | tail -1)
    every=$(expected_hours "$name")
    seen=$((seen + 1))

    if [ -n "$finished" ]; then
        stamp=$(to_rfc3339 "${finished#*Script Finished }")
        # The last few lines of output are usually the script's own verdict,
        # and are what makes the portal row worth reading rather than just
        # green or red.
        detail=$(tail -n 8 "$log" \
            | grep -av 'Full logs for this script' \
            | grep -av 'Script Finished' \
            | grep -av 'Script Start' \
            | grep -av '^[[:space:]]*$' | tail -n 2 | tr '\n' ' ')
        report "$PREFIX$name" true "${detail:-completed}" "$stamp" "$every" "$log"
    elif [ -n "$started" ]; then
        # Started with no finish yet means one of two very different things,
        # and only the process table can tell them apart: still working, or
        # died part-way. Guessing from the log alone would mark every
        # long-running script as failed the moment this watcher woke up.
        if pgrep -f "tmpScripts/$name/script" >/dev/null 2>&1; then
            echo "  $name is running now - leaving its last result alone"
            running=$((running + 1))
            continue
        fi
        stamp=$(to_rfc3339 "${started#*Script Start }")
        report "$PREFIX$name" false "started and did not finish" "$stamp" "$every" "$log"
        late=$((late + 1))
    fi
done

# The watcher reports itself last, so a watcher that dies is one clear row
# rather than four scripts that mysteriously go quiet together.
report "${PREFIX}script-watcher" true     "$seen checked, $late unfinished, $running running" "" 1

echo "$seen script(s) checked, $late unfinished, $running running, $failed_reports report(s) failed"
[ "$failed_reports" -eq 0 ]
