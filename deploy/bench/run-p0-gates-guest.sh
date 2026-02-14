#!/usr/bin/env bash
set -euo pipefail

# P0 performance gates for stack + VM validation.
# Measurement rule: no path distortion. This script benchmarks the real
# execution path and does not add proxy layers in the hot path.

if [[ -x "${HOME}/.cargo/bin/cargo" ]]; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
fi

REPO_DIR="${REPO_DIR:-$HOME/project-sentinel}"
OUT_ROOT="${OUT_ROOT:-/tmp/sentinel-p0-gates-$(date +%Y%m%d-%H%M%S)}"

SHM_RUNS="${SHM_RUNS:-5}"
PERSIST_RUNS="${PERSIST_RUNS:-5}"
PERSIST_TICKS="${PERSIST_TICKS:-5000}"
PERSIST_AGENTS="${PERSIST_AGENTS:-54}"

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing command: $1" >&2
    exit 1
  }
}

metric_from_log() {
  local name="$1"
  local file="$2"
  awk -F'\t' -v n="${name}" '$1=="METRIC" && $2==n {print $3}' "${file}" | tail -1
}

summarize_numeric_tsv() {
  local input_tsv="$1"
  local output_tsv="$2"
  awk -F'\t' '
    NR==1 { next }
    {
      key=$2
      n[key]++
      vals[key, n[key]]=$3 + 0.0
      unit[key]=$4
    }
    END {
      print "metric\tunit\tn\tmean\tmedian\tp95\tmin\tmax"
      for (k in n) {
        count=n[k]
        delete arr
        sum=0
        for (i=1; i<=count; i++) {
          arr[i]=vals[k, i]
          sum += arr[i]
        }
        asort(arr)
        mean = sum / count
        median_idx = int((count + 1) / 2)
        p95_idx = int((95 * count + 99) / 100)
        if (p95_idx < 1) p95_idx = 1
        if (p95_idx > count) p95_idx = count
        printf "%s\t%s\t%d\t%.4f\t%.4f\t%.4f\t%.4f\t%.4f\n", k, unit[k], count, mean, arr[median_idx], arr[p95_idx], arr[1], arr[count]
      }
    }
  ' "${input_tsv}" > "${output_tsv}"
}

require_cmd cargo
require_cmd awk
require_cmd sed
require_cmd rg

mkdir -p "${OUT_ROOT}/raw/shm_off" "${OUT_ROOT}/raw/shm_on" "${OUT_ROOT}/raw/persist20" "${OUT_ROOT}/summary"

cd "${REPO_DIR}"

cargo build --release --manifest-path deploy/bench/stack-harness/Cargo.toml --bin stack-harness --bin persist-probe >/dev/null
HARNESS="deploy/bench/stack-harness/target/release/stack-harness"
PROBE="deploy/bench/stack-harness/target/release/persist-probe"

{
  echo "repo_dir=${REPO_DIR}"
  echo "out_root=${OUT_ROOT}"
  echo "kernel=$(uname -r)"
  echo "cmdline=$(cat /proc/cmdline)"
  echo "shm_runs=${SHM_RUNS}"
  echo "persist_runs=${PERSIST_RUNS}"
  echo "persist_ticks=${PERSIST_TICKS}"
  echo "persist_agents=${PERSIST_AGENTS}"
} > "${OUT_ROOT}/summary/run_config.env"

# 1) Zenoh SHM gate: SHM off vs on
echo -e "profile\trun\tmetric\tvalue\tunit" > "${OUT_ROOT}/summary/zenoh_shm_metrics.tsv"
for profile in off on; do
  shm_flag=0
  raw_dir="${OUT_ROOT}/raw/shm_off"
  if [[ "${profile}" == "on" ]]; then
    shm_flag=1
    raw_dir="${OUT_ROOT}/raw/shm_on"
  fi

  for run in $(seq 1 "${SHM_RUNS}"); do
    log="${raw_dir}/run${run}.log"
    SENTINEL_ZENOH_SHM="${shm_flag}" \
      ECS_ENABLE_PERSIST=1 \
      ECS_PERSIST_EVERY_N_TICKS=20 \
      STACK_HARNESS_TMPDIR=/tmp \
      "${HARNESS}" > "${log}"

    for m in zenoh.roundtrip_mean_us zenoh.roundtrip_p95_us; do
      v="$(metric_from_log "${m}" "${log}")"
      printf "%s\trun%s\t%s\t%s\tus\n" "${profile}" "${run}" "${m}" "${v}" >> "${OUT_ROOT}/summary/zenoh_shm_metrics.tsv"
    done
  done
done

for profile in off on; do
  awk -F'\t' -v p="${profile}" 'NR==1 || $1==p {print $2 "\t" $3 "\t" $4 "\t" $5}' \
    "${OUT_ROOT}/summary/zenoh_shm_metrics.tsv" > "${OUT_ROOT}/summary/zenoh_shm_${profile}.tmp.tsv"
  summarize_numeric_tsv "${OUT_ROOT}/summary/zenoh_shm_${profile}.tmp.tsv" "${OUT_ROOT}/summary/zenoh_shm_${profile}_stats.tsv"
done

shm_on_p95_median="$(awk -F'\t' '$1=="zenoh.roundtrip_p95_us" {print $5}' "${OUT_ROOT}/summary/zenoh_shm_on_stats.tsv" | tail -1)"
shm_off_p95_median="$(awk -F'\t' '$1=="zenoh.roundtrip_p95_us" {print $5}' "${OUT_ROOT}/summary/zenoh_shm_off_stats.tsv" | tail -1)"
shm_gate="FAIL"
if awk -v v="${shm_on_p95_median:-999999}" 'BEGIN { exit !(v < 200.0) }'; then
  shm_gate="PASS"
fi

# 2) Persist gate @ interval 20 (real path burst profile)
echo -e "run\tmetric\tvalue\tunit" > "${OUT_ROOT}/summary/persist20_metrics.tsv"
for run in $(seq 1 "${PERSIST_RUNS}"); do
  log="${OUT_ROOT}/raw/persist20/run${run}.log"
  MODE=simulate \
    DB_PATH="${OUT_ROOT}/raw/persist20/run${run}.redb" \
    SIM_TICKS="${PERSIST_TICKS}" \
    SIM_AGENTS="${PERSIST_AGENTS}" \
    PERSIST_EVERY=20 \
    SIM_COLLECT_TICK_HIST=1 \
    "${PROBE}" > "${log}"

  for m in \
    ecs.us_per_tick ecs.ticks_per_s ecs.tick_us_p95 ecs.tick_us_p99 ecs.tick_us_max \
    persist.flush_attempts persist.flush_failures \
    persist.batch_size_avg persist.flush_latency_us_avg persist.flush_latency_us_max \
    persist.queue_depth_max persist.drop_count persist.coalesce_count; do
    v="$(metric_from_log "${m}" "${log}")"
    u="us"
    case "${m}" in
      ecs.ticks_per_s) u="ticks/s" ;;
      persist.flush_attempts|persist.flush_failures|persist.queue_depth_max|persist.drop_count|persist.coalesce_count) u="count" ;;
      persist.batch_size_avg) u="agents" ;;
      *) u="us" ;;
    esac
    printf "run%s\t%s\t%s\t%s\n" "${run}" "${m}" "${v}" "${u}" >> "${OUT_ROOT}/summary/persist20_metrics.tsv"
  done
done
summarize_numeric_tsv "${OUT_ROOT}/summary/persist20_metrics.tsv" "${OUT_ROOT}/summary/persist20_stats.tsv"

persist_p95_median="$(awk -F'\t' '$1=="ecs.tick_us_p95" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"
persist_us_median="$(awk -F'\t' '$1=="ecs.us_per_tick" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"
persist_gate="FAIL"
if awk -v p95="${persist_p95_median:-999999}" -v us="${persist_us_median:-999999}" \
  'BEGIN { exit !(p95 <= 3500.0 && us <= 1000.0) }'; then
  persist_gate="PASS"
fi

persist_flush_attempts_median="$(awk -F'\t' '$1=="persist.flush_attempts" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"
persist_flush_failures_median="$(awk -F'\t' '$1=="persist.flush_failures" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"
persist_flush_avg_us_median="$(awk -F'\t' '$1=="persist.flush_latency_us_avg" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"
persist_flush_max_us_median="$(awk -F'\t' '$1=="persist.flush_latency_us_max" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"
persist_queue_depth_median="$(awk -F'\t' '$1=="persist.queue_depth_max" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"
persist_drop_median="$(awk -F'\t' '$1=="persist.drop_count" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"
persist_coalesce_median="$(awk -F'\t' '$1=="persist.coalesce_count" {print $5}' "${OUT_ROOT}/summary/persist20_stats.tsv" | tail -1)"

persist_path_gate="FAIL"
if awk \
  -v attempts="${persist_flush_attempts_median:-0}" \
  -v failures="${persist_flush_failures_median:-999999}" \
  -v queue_depth="${persist_queue_depth_median:-999999}" \
  -v drops="${persist_drop_median:-999999}" \
  -v coalesce="${persist_coalesce_median:-999999}" \
  'BEGIN { exit !(attempts > 0 && failures <= 0.0001 && queue_depth <= 0.0001 && drops <= 0.0001 && coalesce <= 0.0001) }'; then
  persist_path_gate="PASS"
fi

# 3) Feature-check gates for query-cancel and circuit-breaker
query_cancel_gate="BLOCKED"
query_cancel_reason="No runtime evidence for query deadline/cancel implementation in cortex path."
if rg -q "context\\.WithTimeout|deadline|cancel\\(|max_inflight|inflight" cmd/cortex-gateway/internal; then
  query_cancel_gate="PARTIAL"
  query_cancel_reason="Code keywords found; dedicated runtime chaos test still missing."
fi

cb_gate="BLOCKED"
cb_reason="No runtime evidence for provider circuit breaker state machine in cortex path."
if rg -q "circuit.?breaker|half-open|half_open|breaker" cmd/cortex-gateway/internal; then
  cb_gate="PARTIAL"
  cb_reason="Code keywords found; dedicated fault-injection test still missing."
fi

{
  echo "# P0 Gates Summary"
  echo
  echo "- Output root: ${OUT_ROOT}"
  echo
  echo "## Gate status"
  echo "- zenoh_shm_gate: ${shm_gate}"
  echo "  - shm_on_p95_median_us: ${shm_on_p95_median:-n/a}"
  echo "  - shm_off_p95_median_us: ${shm_off_p95_median:-n/a}"
  echo "- persist20_gate: ${persist_gate}"
  echo "  - persist20_tick_p95_median_us: ${persist_p95_median:-n/a}"
  echo "  - persist20_us_per_tick_median_us: ${persist_us_median:-n/a}"
  echo "- persist_path_gate: ${persist_path_gate}"
  echo "  - persist_flush_attempts_median: ${persist_flush_attempts_median:-n/a}"
  echo "  - persist_flush_failures_median: ${persist_flush_failures_median:-n/a}"
  echo "  - persist_flush_latency_us_avg_median: ${persist_flush_avg_us_median:-n/a}"
  echo "  - persist_flush_latency_us_max_median: ${persist_flush_max_us_median:-n/a}"
  echo "  - persist_queue_depth_max_median: ${persist_queue_depth_median:-n/a}"
  echo "  - persist_drop_count_median: ${persist_drop_median:-n/a}"
  echo "  - persist_coalesce_count_median: ${persist_coalesce_median:-n/a}"
  echo "- query_cancel_gate: ${query_cancel_gate}"
  echo "  - reason: ${query_cancel_reason}"
  echo "- circuit_breaker_gate: ${cb_gate}"
  echo "  - reason: ${cb_reason}"
  echo
  echo "## Artifacts"
  echo "- summary/run_config.env"
  echo "- summary/zenoh_shm_metrics.tsv"
  echo "- summary/zenoh_shm_off_stats.tsv"
  echo "- summary/zenoh_shm_on_stats.tsv"
  echo "- summary/persist20_metrics.tsv"
  echo "- summary/persist20_stats.tsv"
} > "${OUT_ROOT}/summary/overall.md"

echo "done: ${OUT_ROOT}"
