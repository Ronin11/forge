#!/bin/sh
# Forge's CPU share follows whether the operator is at the desktop
# (docs/PLUGINS.md, "presence"): a low weight on the forge-worker unit's
# cgroup while they are here, the default weight once they walk away.
# Replaces the old deploy/forge-idle, as a plugin any desktop can supply
# a source for rather than one omarchy-only script and unit.
set -u

SOURCE=
IDLE_AFTER=180
UNIT=forge-worker
ACTIVE_WEIGHT=40
IDLE_WEIGHT=100
ACTIVE_QUOTA=
IDLE_QUOTA=
COMMAND=

config="$FORGE_PLUGIN_DIR/config"
if [ -f "$config" ]; then
    while IFS= read -r cfgline || [ -n "$cfgline" ]; do
        case "$cfgline" in
            '' | '#'*) continue ;;
        esac
        key=${cfgline%%=*}
        val=${cfgline#*=}
        case "$key" in
            SOURCE) SOURCE=$val ;;
            IDLE_AFTER) IDLE_AFTER=$val ;;
            UNIT) UNIT=$val ;;
            ACTIVE_WEIGHT) ACTIVE_WEIGHT=$val ;;
            IDLE_WEIGHT) IDLE_WEIGHT=$val ;;
            ACTIVE_QUOTA) ACTIVE_QUOTA=$val ;;
            IDLE_QUOTA) IDLE_QUOTA=$val ;;
            COMMAND) COMMAND=$val ;;
        esac
    done <"$config"
fi

log() {
    printf 'presence: %s\n' "$1" >&2
}

state_file="$FORGE_PLUGIN_STATE/state"

# Applies `$1` (active|idle|none) as the unit's live CPUWeight and
# CPUQuota (the latter reset to unlimited when that state has none
# configured, so an earlier state's quota never lingers), then records
# it: this is the one place a transition becomes real, both on the
# machine and in the state file `forge doctor`'s presence row reads back.
apply() {
    case "$1" in
        active) weight=$ACTIVE_WEIGHT; quota=$ACTIVE_QUOTA ;;
        idle | none) weight=$IDLE_WEIGHT; quota=$IDLE_QUOTA ;;
        *)
            log "unknown state $1"
            return 1
            ;;
    esac
    systemctl --user set-property --runtime "$UNIT" "CPUWeight=$weight" ||
        log "systemctl set-property CPUWeight=$weight failed"
    if [ -n "$quota" ]; then
        quota_prop="CPUQuota=${quota}%"
    else
        quota_prop="CPUQuota=infinity"
    fi
    systemctl --user set-property --runtime "$UNIT" "$quota_prop" ||
        log "systemctl set-property $quota_prop failed"
    tmp="$FORGE_PLUGIN_STATE/.state.tmp.$$"
    printf 'state=%s\nsince=%s\nweight=%s\n' "$1" "$(date +%s)" "$weight" >"$tmp"
    mv -f "$tmp" "$state_file"
    log "$1 -> $UNIT CPUWeight=$weight${quota:+ CPUQuota=${quota}%}"
}

# swayidle's own two callbacks (below): a fresh process each time,
# re-reading the config parsed above from the same file, applying just
# this one transition, and exiting; the long-lived loop stays swayidle's.
case "${1:-}" in
    apply-idle)
        apply idle
        exit $?
        ;;
    apply-active)
        apply active
        exit $?
        ;;
esac

if [ -z "$SOURCE" ]; then
    if command -v omarchy-shell >/dev/null 2>&1; then
        SOURCE=omarchy
    elif command -v swayidle >/dev/null 2>&1; then
        SOURCE=swayidle
    else
        SOURCE=none
    fi
    log "SOURCE unset; using $SOURCE"
fi

case "$SOURCE" in
    none)
        apply none
        exec sleep infinity
        ;;
    omarchy)
        # The state at start, from the shell's own idle service, so a
        # restart never leaves the wrong weight in place until the next
        # transition happens to come along.
        json=$(timeout 5 omarchy-shell idle status 2>/dev/null || true)
        case "$json" in
            *'"idle":true'*) apply idle ;;
            *) apply active ;;
        esac
        # -n0: transitions from now on; the state above already covered
        # "now". Inhibitor-aware (a playing video is not idle) and no
        # polling: the shell's own idle monitor logs every transition.
        journalctl --user -f -n0 -o cat SYSLOG_IDENTIFIER=omarchy-shell 2>/dev/null |
            grep --line-buffered -oE 'idle-monitor: (idle|active)$' |
            while read -r line; do apply "${line##*: }"; done
        ;;
    swayidle)
        # No way to ask swayidle the state at start; active (the lower
        # weight) is the safe guess until it says otherwise, the same
        # posture deploy/forge-idle took for an unknown omarchy state.
        apply active
        exec swayidle -w \
            timeout "$IDLE_AFTER" "$FORGE_PLUGIN_DIR/presence.sh apply-idle" \
            resume "$FORGE_PLUGIN_DIR/presence.sh apply-active"
        ;;
    command)
        if [ -z "$COMMAND" ]; then
            log "SOURCE=command but COMMAND is empty"
            exit 1
        fi
        apply active
        # shellcheck disable=SC2086
        $COMMAND | while IFS= read -r line; do
            case "$line" in
                idle) apply idle ;;
                active) apply active ;;
            esac
        done
        log "the command source exited; stopping so the supervisor restarts it"
        exit 1
        ;;
    *)
        log "unknown SOURCE $SOURCE"
        exit 1
        ;;
esac
