#!/bin/sh
# PatchPanel job reporting, for scripts PatchPanel does not run itself.
#
# Source it, name the job, say how often it should run, and report at the end:
#
#     . /boot/config/pp-report.sh
#     pp_init router-backup 24
#     pp_fact public_ip "$IP"
#     pp_ok "3 routers backed up"
#
# To have a script keep its own copy current, paste this instead of the `.`
# line - it refreshes the cache, falls back to the cached copy, and finally to
# doing nothing at all:
#
#     PP_LIB=/boot/config/pp-report.sh
#     if curl -fsS -m 5 "$PP_URL/pp-report.sh" -o "$PP_LIB.new" 2>/dev/null &&
#        grep -q "^pp_init()" "$PP_LIB.new"; then mv "$PP_LIB.new" "$PP_LIB"; fi
#     rm -f "$PP_LIB.new"
#     if [ -r "$PP_LIB" ]; then . "$PP_LIB"; else
#       pp_init() { :; }; pp_fact() { :; }; pp_ok() { :; }
#       pp_fail() { :; }; pp_finish() { :; }
#     fi
#
# The last four lines are the important ones. Reporting must never be able to
# break the job it reports on: a portal that is down, or a proxy answering with
# a login page, has to leave the backup running and unreported rather than not
# running at all. The `grep` is what stops an HTML error page that arrived with
# a 200 from being cached and then sourced.
#
# The failure this exists for is not the one a script can report. A script that
# errors can shout; a script that has stopped running - disabled unit, full
# disk, rebuilt box, a cron line someone removed - says nothing at all, and an
# absence of bad news is indistinguishable from good news. `pp_init`'s second
# argument is what turns that silence into something the portal can notice on
# the script's behalf.
#
# POSIX sh on purpose: these live in Unraid User Scripts and similar, which are
# `#!/bin/sh`. Nothing here is a bashism - in particular there is no `trap ERR`,
# which sh accepts and then never fires.
#
# Nothing in here can fail the calling script. Reporting is not the job.

PP_URL="${PP_URL:-http://localhost}"
PP_TOKEN="${PP_TOKEN:-}"
PP_JOB=""
PP_EVERY=0
PP_DONE=0
PP_FACTS=""

# pp_init <job-name> [hours-between-runs]
#
# Call this before anything that can fail, including argument checks and
# missing-credential guards - those early exits are usually a script's quietest
# failure, because they skip the work and therefore skip whatever the script
# normally uses to complain.
pp_init() {
    PP_JOB="$1"
    PP_EVERY="${2:-0}"
    PP_DONE=0
    PP_FACTS=""
    trap 'pp__on_exit $?' EXIT
}

# pp_fact <key> <value>
#
# Something this job knows that PatchPanel cannot see: a public IP, a record
# count, a version. Only changes are kept, so a value sent every run becomes a
# history of when it actually moved.
pp_fact() {
    [ -n "${1:-}" ] || return 0
    PP_FACTS="$PP_FACTS$(pp__obj "$1" "$2")"
}

# pp_ok [detail] / pp_fail [detail]
pp_ok() {
    PP_DONE=1
    pp__send true "${1:-}"
}

pp_fail() {
    PP_DONE=1
    pp__send false "${1:-}"
}

# pp_finish <failure-count> [detail]
#
# For the common shape: a loop that tallies failures and an exit status derived
# from it.
pp_finish() {
    if [ "${1:-0}" -eq 0 ]; then
        pp_ok "${2:-}"
    else
        pp_fail "${2:-$1 failed}"
    fi
}

# --- internals ------------------------------------------------------------

# Whatever happens, the run is accounted for. A script that exits 0 without
# reporting has usually grown a new early return, which is worth saying rather
# than silently recording nothing.
pp__on_exit() {
    [ "$PP_DONE" = 1 ] && return 0
    if [ "${1:-0}" -eq 0 ]; then
        pp__send false "ended without reporting"
    else
        pp__send false "exited $1"
    fi
    return 0
}

# One {"key":"value"} object, correctly quoted.
#
# jq when it is there, because a value holding a quote or a newline would
# otherwise produce a body the portal rejects - and a reporting call that fails
# silently is worse than no reporting at all. The fallback covers the same
# cases by hand.
pp__obj() {
    if command -v jq >/dev/null 2>&1; then
        jq -cn --arg k "$1" --arg v "$2" '{($k):$v}'
    else
        printf '{"%s":"%s"}' "$(pp__esc "$1")" "$(pp__esc "$2")"
    fi
}

pp__esc() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/\t/\\t/g' \
        | sed -e ':a' -e 'N' -e '$!ba' -e 's/\n/\\n/g'
}

pp__send() {
    [ -n "$PP_JOB" ] || return 0
    _pp_auth=""
    [ -n "$PP_TOKEN" ] && _pp_auth="Authorization: Bearer $PP_TOKEN"

    if command -v jq >/dev/null 2>&1; then
        _pp_body=$(printf '%s' "$PP_FACTS" | jq -cs \
            --arg n "$PP_JOB" --argjson ok "$1" --arg d "$2" \
            --argjson e "${PP_EVERY:-0}" \
            '{name:$n, ok:$ok, every_hours:$e, detail:$d, facts:(add // {})}')
    else
        # Objects concatenated rather than merged; the portal takes the last
        # value for a repeated key, which is the same answer jq's `add` gives.
        _pp_facts=$(printf '%s' "$PP_FACTS" | sed -e 's/}{/,/g')
        [ -n "$_pp_facts" ] || _pp_facts="{}"
        _pp_body=$(printf '{"name":"%s","ok":%s,"every_hours":%s,"detail":"%s","facts":%s}' \
            "$(pp__esc "$PP_JOB")" "$1" "${PP_EVERY:-0}" "$(pp__esc "$2")" "$_pp_facts")
    fi

    if [ -n "$_pp_auth" ]; then
        curl -fsS -m 10 -X POST "$PP_URL/api/jobs" \
            -H 'Content-Type: application/json' -H "$_pp_auth" \
            -d "$_pp_body" >/dev/null 2>&1 || true
    else
        curl -fsS -m 10 -X POST "$PP_URL/api/jobs" \
            -H 'Content-Type: application/json' \
            -d "$_pp_body" >/dev/null 2>&1 || true
    fi
    return 0
}
