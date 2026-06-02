#!/usr/bin/env bash
# sentinel-health-monitor.sh — Active health monitoring for all Sentinel services.
#
# Runs via systemd timer every 60s. Checks:
#   - systemd service status (is-active)
#   - HTTP /health endpoints (where available)
#   - NATS monitoring endpoint (:8222/healthz)
#   - Projection lag (events.db vs projection_offsets)
#
# Alerts via ntfy on state transitions only (no spam).
# Attempts auto-restart for failed services (max 3 per episode).
#
# State file: /opt/sentinel/data/health-monitor.state
# Config:     /opt/sentinel/config/health-monitor.env (optional overrides)
set -uo pipefail

readonly SCRIPT_NAME="sentinel-health-monitor"
readonly STATE_FILE="/opt/sentinel/data/health-monitor.state"
readonly EVENT_STORE_DB="/opt/sentinel/data/events.db"

# --- Defaults (overridable via env file) ---
NTFY_SERVER="${NTFY_SERVER:-https://<ntfy-server>}"
NTFY_TOPIC="${NTFY_TOPIC:-sentinel-health}"
LAG_WARN="${LAG_WARN:-500}"
LAG_CRIT="${LAG_CRIT:-5000}"
MAX_RESTARTS="${MAX_RESTARTS:-3}"
CURL_TIMEOUT="${CURL_TIMEOUT:-5}"

# Load env overrides if available
if [ -f /opt/sentinel/config/health-monitor.env ]; then
    # shellcheck source=/dev/null
    source /opt/sentinel/config/health-monitor.env
fi

# --- Associative arrays for state tracking ---
declare -A PREV_STATUS PREV_SINCE PREV_RESTARTS
declare -A CURR_STATUS CURR_SINCE CURR_RESTARTS

# --- Logging ---
log_info()  { logger -t "$SCRIPT_NAME" "$*"; }
log_error() { logger -t "$SCRIPT_NAME" -p user.err "$*"; }
now_epoch() { date +%s; }

# --- State File I/O ---

load_state() {
    if [ ! -f "$STATE_FILE" ]; then
        return
    fi
    while IFS=: read -r name status since restarts; do
        [ -z "$name" ] && continue
        [[ "$name" == \#* ]] && continue
        PREV_STATUS[$name]="$status"
        PREV_SINCE[$name]="$since"
        PREV_RESTARTS[$name]="$restarts"
    done < "$STATE_FILE"
}

save_state() {
    local now
    now=$(now_epoch)
    {
        echo "# sentinel-health-monitor state — $(date -Iseconds)"
        for name in "${!CURR_STATUS[@]}"; do
            local status="${CURR_STATUS[$name]}"
            local since="${CURR_SINCE[$name]:-$now}"
            local restarts="${CURR_RESTARTS[$name]:-0}"
            echo "$name:$status:$since:$restarts"
        done
    } > "${STATE_FILE}.tmp" && mv "${STATE_FILE}.tmp" "$STATE_FILE"
}

# --- Alert Functions ---

send_ntfy() {
    local title="$1" message="$2" priority="${3:-3}" tags="${4:-warning}"
    curl -sk \
        -H "Title: $title" \
        -H "Priority: $priority" \
        -H "Tags: $tags" \
        -d "$message" \
        "$NTFY_SERVER/$NTFY_TOPIC" 2>/dev/null || true
}

alert_down() {
    local name="$1" detail="$2" priority="$3"
    send_ntfy \
        "[SENTINEL] $name DOWN" \
        "$detail. Auto-restart attempted." \
        "$priority" \
        "rotating_light,sentinel"
    log_info "ALERT DOWN: $name — $detail"
}

alert_recovered() {
    local name="$1" downtime_human="$2"
    send_ntfy \
        "[SENTINEL] $name RECOVERED" \
        "Back online after $downtime_human downtime." \
        "3" \
        "white_check_mark,sentinel"
    log_info "ALERT RECOVERED: $name after $downtime_human"
}

alert_lag() {
    local lag="$1" severity="$2"
    local priority=3
    [ "$severity" = "critical" ] && priority=5
    send_ntfy \
        "[SENTINEL] Projection Lag ${severity^^}" \
        "Lag: $lag events (warn: $LAG_WARN, crit: $LAG_CRIT)." \
        "$priority" \
        "snail,sentinel"
    log_info "ALERT LAG $severity: $lag events"
}

human_duration() {
    local secs="$1"
    if [ "$secs" -lt 60 ]; then
        echo "${secs}s"
    elif [ "$secs" -lt 3600 ]; then
        echo "$((secs / 60))m $((secs % 60))s"
    elif [ "$secs" -lt 86400 ]; then
        echo "$((secs / 3600))h $((secs % 3600 / 60))m"
    else
        echo "$((secs / 86400))d $((secs % 86400 / 3600))h"
    fi
}

# --- Check Functions ---

check_systemd_unit() {
    local unit="$1"
    systemctl is-active --quiet "$unit" 2>/dev/null
}

check_http_health() {
    local port="$1" path="$2"
    curl -sf --connect-timeout 3 --max-time "$CURL_TIMEOUT" \
        "http://localhost:${port}${path}" >/dev/null 2>&1
}

check_https_health() {
    local port="$1" path="$2"
    curl -skf --connect-timeout 3 --max-time "$CURL_TIMEOUT" \
        "https://localhost:${port}${path}" >/dev/null 2>&1
}

check_nats_health() {
    curl -sf --connect-timeout 3 --max-time "$CURL_TIMEOUT" \
        "http://localhost:8222/healthz" >/dev/null 2>&1
}

check_projection_lag() {
    if [ ! -f "$EVENT_STORE_DB" ]; then
        echo "0"
        return
    fi
    local max_id offset lag
    max_id=$(sqlite3 "$EVENT_STORE_DB" \
        "SELECT COALESCE(MAX(id), 0) FROM events" 2>/dev/null) || max_id=0
    offset=$(sqlite3 "$EVENT_STORE_DB" \
        "SELECT COALESCE(last_event_id, 0) FROM projection_offsets WHERE projection_name='sentinel-projection'" \
        2>/dev/null) || offset=0
    lag=$((max_id - offset))
    [ "$lag" -lt 0 ] && lag=0
    echo "$lag"
}

try_restart() {
    local unit="$1" name="$2"
    local count="${CURR_RESTARTS[$name]:-0}"

    if [ "$count" -ge "$MAX_RESTARTS" ]; then
        log_info "$name: restart limit ($MAX_RESTARTS) reached — manual intervention required"
        return 1
    fi

    CURR_RESTARTS[$name]=$((count + 1))
    log_info "$name: restarting (${CURR_RESTARTS[$name]}/$MAX_RESTARTS)"
    systemctl restart "$unit" 2>/dev/null || true
    sleep 3

    if check_systemd_unit "$unit"; then
        log_info "$name: restart successful"
        return 0
    else
        log_info "$name: restart failed"
        return 1
    fi
}

# --- Service Check Logic ---

check_service() {
    local name="$1" unit="$2" check_type="$3" port="$4" path="$5" priority="$6"
    local now
    now=$(now_epoch)
    local is_ok=false
    local prev="${PREV_STATUS[$name]:-ok}"
    local prev_since="${PREV_SINCE[$name]:-$now}"
    local prev_restarts="${PREV_RESTARTS[$name]:-0}"

    # Determine health
    case "$check_type" in
        systemd)
            check_systemd_unit "$unit" && is_ok=true
            ;;
        http)
            if check_systemd_unit "$unit"; then
                check_http_health "$port" "$path" && is_ok=true
            fi
            ;;
        https)
            if check_systemd_unit "$unit"; then
                check_https_health "$port" "$path" && is_ok=true
            fi
            ;;
        nats)
            if check_systemd_unit "$unit"; then
                check_nats_health && is_ok=true
            fi
            ;;
    esac

    if $is_ok; then
        CURR_STATUS[$name]="ok"
        CURR_SINCE[$name]="$now"
        CURR_RESTARTS[$name]=0

        # Recovery detection
        if [ "$prev" != "ok" ]; then
            local downtime=$((now - prev_since))
            alert_recovered "$name" "$(human_duration $downtime)"
        fi
    else
        CURR_STATUS[$name]="failed"
        CURR_RESTARTS[$name]="$prev_restarts"

        if [ "$prev" = "ok" ]; then
            # New failure — alert + restart
            CURR_SINCE[$name]="$now"
            alert_down "$name" "$unit is inactive/unhealthy" "$priority"
            try_restart "$unit" "$name"
        else
            # Ongoing failure — keep since timestamp, try restart if under limit
            CURR_SINCE[$name]="$prev_since"
            try_restart "$unit" "$name" 2>/dev/null || true
        fi
    fi
}

# --- Main ---

main() {
    load_state
    local now
    now=$(now_epoch)

    # Service definitions: name:unit:check_type:port:path:priority
    local services=(
        "daemon:sentinel-daemon.service:systemd:::5"
        "projection:sentinel-projection.service:systemd:::5"
        "nats:nats-server.service:nats:::5"
        "cortex:sentinel-gateway.service:http:8080:/health:5"
        "dashboard-backend:sentinel-dashboard-backend.service:https:8001:/api/health:3"
        "judge:sentinel-judge.service:http:8082:/health:3"
        "nats-bridge:sentinel-nats-bridge.service:http:8083:/health:3"
    )

    for entry in "${services[@]}"; do
        IFS=: read -r name unit check_type port path priority <<< "$entry"
        check_service "$name" "$unit" "$check_type" "$port" "$path" "$priority"
    done

    # --- Projection Lag Check (independent of service status) ---
    local lag
    lag=$(check_projection_lag)
    local prev_lag_status="${PREV_STATUS[projection-lag]:-ok}"

    if [ "$lag" -ge "$LAG_CRIT" ]; then
        CURR_STATUS[projection-lag]="critical"
        CURR_RESTARTS[projection-lag]="${PREV_RESTARTS[projection-lag]:-0}"
        if [ "$prev_lag_status" != "critical" ]; then
            CURR_SINCE[projection-lag]="$now"
            alert_lag "$lag" "critical"
            try_restart "sentinel-projection.service" "projection" 2>/dev/null || true
        else
            CURR_SINCE[projection-lag]="${PREV_SINCE[projection-lag]:-$now}"
        fi
    elif [ "$lag" -ge "$LAG_WARN" ]; then
        CURR_STATUS[projection-lag]="warning"
        CURR_RESTARTS[projection-lag]=0
        if [ "$prev_lag_status" = "ok" ]; then
            CURR_SINCE[projection-lag]="$now"
            alert_lag "$lag" "warning"
        else
            CURR_SINCE[projection-lag]="${PREV_SINCE[projection-lag]:-$now}"
        fi
    else
        CURR_STATUS[projection-lag]="ok"
        CURR_SINCE[projection-lag]="$now"
        CURR_RESTARTS[projection-lag]=0
        if [ "$prev_lag_status" != "ok" ]; then
            log_info "Projection lag recovered: $lag events"
        fi
    fi

    save_state

    # Summary log line
    local summary=""
    for k in $(echo "${!CURR_STATUS[@]}" | tr ' ' '\n' | sort); do
        summary+="$k=${CURR_STATUS[$k]} "
    done
    log_info "Check complete: ${summary}lag=$lag"
}

main "$@"
