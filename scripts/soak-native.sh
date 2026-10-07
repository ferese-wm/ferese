#!/usr/bin/env bash

set -euo pipefail

soak_seconds="${FERESE_SOAK_SECONDS:-86400}"
iteration_delay="${FERESE_SOAK_DELAY:-0.15}"
client_program="${FERESE_SOAK_CLIENT:-foot}"
feresectl_program="${FERESECTL:-target/debug/feresectl}"
malformed_client_program="${FERESE_MALFORMED_CLIENT:-target/debug/ferese-malformed-client}"
compositor_pid="${FERESE_PID:-}"
log_path="${FERESE_SOAK_LOG:-/tmp/ferese-soak-$$.log}"
max_rss_growth_kib="${FERESE_SOAK_MAX_RSS_GROWTH_KIB:-131072}"
max_fd_growth="${FERESE_SOAK_MAX_FD_GROWTH:-32}"
max_thread_growth="${FERESE_SOAK_MAX_THREAD_GROWTH:-8}"

if [[ -z "${WAYLAND_DISPLAY:-}" ]]; then
    echo "WAYLAND_DISPLAY must name the Ferese socket" >&2
    exit 2
fi

if [[ ! "$soak_seconds" =~ ^[1-9][0-9]*$ ]]; then
    echo "FERESE_SOAK_SECONDS must be a positive integer" >&2
    exit 2
fi

for value in "$max_rss_growth_kib" "$max_fd_growth" "$max_thread_growth"; do
    if [[ ! "$value" =~ ^[0-9]+$ ]]; then
        echo "soak resource-growth limits must be non-negative integers" >&2
        exit 2
    fi
done

if [[ ! -x "$feresectl_program" ]]; then
    cargo build -p feresectl
fi

if [[ ! -x "$malformed_client_program" ]]; then
    if [[ -n "${FERESE_MALFORMED_CLIENT:-}" ]]; then
        echo "configured malformed-client probe is not executable: $malformed_client_program" >&2
        exit 2
    fi
    echo "Local malformed-client probe unavailable; skipping protocol fuzz checks" >&2
fi

if ! command -v "$client_program" >/dev/null 2>&1; then
    echo "soak client not found: $client_program" >&2
    exit 2
fi

if [[ -z "$compositor_pid" ]]; then
    compositor_pid="$(pgrep -n -x ferese || true)"
fi

if [[ -z "$compositor_pid" || ! -r "/proc/$compositor_pid/status" ]]; then
    echo "set FERESE_PID to the running compositor process" >&2
    exit 2
fi

client_pids=()

cleanup() {
    local pid

    for pid in "${client_pids[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
        fi
    done
}

trap cleanup EXIT INT TERM

rss_kib() {
    awk '/^VmRSS:/ { print $2; exit }' "/proc/$compositor_pid/status"
}

thread_count() {
    awk '/^Threads:/ { print $2; exit }' "/proc/$compositor_pid/status"
}

fd_count() {
    find "/proc/$compositor_pid/fd" -mindepth 1 -maxdepth 1 -printf . | wc -c
}

check_resource_budget() {
    local rss_growth=$((current_rss - initial_rss))
    local fd_growth=$((current_fds - initial_fds))
    local thread_growth=$((current_threads - initial_threads))

    if ((rss_growth > max_rss_growth_kib)); then
        echo "FAIL RSS growth ${rss_growth} KiB exceeds ${max_rss_growth_kib} KiB" \
            | tee -a "$log_path"
        exit 1
    fi
    if ((fd_growth > max_fd_growth)); then
        echo "FAIL file-descriptor growth $fd_growth exceeds $max_fd_growth" \
            | tee -a "$log_path"
        exit 1
    fi
    if ((thread_growth > max_thread_growth)); then
        echo "FAIL thread growth $thread_growth exceeds $max_thread_growth" \
            | tee -a "$log_path"
        exit 1
    fi
}

control() {
    "$feresectl_program" "$@" >/dev/null
}

launch_client() {
    "$client_program" >/dev/null 2>&1 &
    client_pids+=("$!")
}

reap_clients() {
    local pid

    for pid in "${client_pids[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
        fi
        wait "$pid" 2>/dev/null || true
    done
    client_pids=()
}

wait_for_focus() {
    local attempt
    local focused

    for ((attempt = 0; attempt < 40; attempt += 1)); do
        focused="$("$feresectl_program" -j focused-window 2>/dev/null || true)"
        if [[ "$focused" != "null" && -n "$focused" ]]; then
            return 0
        fi
        sleep 0.05
    done

    echo "a launched client did not receive focus" >&2
    return 1
}

start_time="$SECONDS"
deadline=$((start_time + soak_seconds))
iteration=0
initial_rss="$(rss_kib)"
initial_fds="$(fd_count)"
initial_threads="$(thread_count)"
maximum_rss="$initial_rss"
maximum_fds="$initial_fds"
maximum_threads="$initial_threads"

{
    echo "ferese native soak"
    echo "pid=$compositor_pid display=$WAYLAND_DISPLAY client=$client_program"
    echo "duration_seconds=$soak_seconds initial_rss_kib=$initial_rss initial_fds=$initial_fds initial_threads=$initial_threads"
    echo "limits rss_growth_kib=$max_rss_growth_kib fd_growth=$max_fd_growth thread_growth=$max_thread_growth"
} | tee "$log_path"

while ((SECONDS < deadline)); do
    if ! kill -0 "$compositor_pid" 2>/dev/null; then
        echo "FAIL compositor exited at iteration $iteration" | tee -a "$log_path"
        exit 1
    fi

    launch_client
    launch_client
    wait_for_focus

    control focus left
    control focus right
    control resize left
    control resize right
    control cycle-column-width
    control cycle-column-width
    control toggle-fullscreen
    control toggle-fullscreen
    control toggle-floating
    control toggle-floating
    control workspace 2
    control workspace 1
    control close
    sleep "$iteration_delay"
    control close
    sleep "$iteration_delay"

    if ((iteration % 10 == 0)); then
        launch_client
        wait_for_focus
        kill -TERM "${client_pids[-1]}" 2>/dev/null || true
        if [[ -x "$malformed_client_program" ]]; then
            "$malformed_client_program" >>"$log_path" 2>&1
        fi
        control get-outputs
    fi
    reap_clients

    current_rss="$(rss_kib)"
    current_fds="$(fd_count)"
    current_threads="$(thread_count)"
    if ((current_rss > maximum_rss)); then
        maximum_rss="$current_rss"
    fi
    if ((current_fds > maximum_fds)); then
        maximum_fds="$current_fds"
    fi
    if ((current_threads > maximum_threads)); then
        maximum_threads="$current_threads"
    fi
    check_resource_budget

    if ((iteration % 100 == 0)); then
        printf 'iteration=%d elapsed=%d rss_kib=%d max_rss_kib=%d fds=%d max_fds=%d threads=%d max_threads=%d\n' \
            "$iteration" "$((SECONDS - start_time))" "$current_rss" "$maximum_rss" \
            "$current_fds" "$maximum_fds" "$current_threads" "$maximum_threads" \
            | tee -a "$log_path"
    fi

    iteration=$((iteration + 1))
    sleep "$iteration_delay"
done

control get-outputs
control get-workspaces
final_rss="$(rss_kib)"
final_fds="$(fd_count)"
final_threads="$(thread_count)"
current_rss="$final_rss"
current_fds="$final_fds"
current_threads="$final_threads"
check_resource_budget

printf 'PASS iterations=%d final_rss_kib=%d max_rss_kib=%d rss_growth_kib=%d final_fds=%d max_fds=%d fd_growth=%d final_threads=%d max_threads=%d thread_growth=%d\n' \
    "$iteration" "$final_rss" "$maximum_rss" "$((final_rss - initial_rss))" \
    "$final_fds" "$maximum_fds" "$((final_fds - initial_fds))" \
    "$final_threads" "$maximum_threads" "$((final_threads - initial_threads))" \
    | tee -a "$log_path"
