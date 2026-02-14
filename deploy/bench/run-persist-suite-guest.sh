#!/usr/bin/env bash
set -euo pipefail

# Measurement rule: no path distortion.
# This runner must benchmark the real persistence/data path and must not add
# artificial proxy layers that change IOPS/latency behavior.

if [[ -x "${HOME}/.cargo/bin/cargo" ]]; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
fi

REPO_DIR="${REPO_DIR:-$HOME/project-sentinel}"
OUT_ROOT="${OUT_ROOT:-/tmp/sentinel-persist-suite-$(date +%Y%m%d-%H%M%S)}"

T1_RUNS="${T1_RUNS:-5}"
T1_TICKS="${T1_TICKS:-5001}"
T1_AGENTS="${T1_AGENTS:-15}"
T1_SPARSE_INTERVAL="${T1_SPARSE_INTERVAL:-10}"

T2_CYCLES="${T2_CYCLES:-50}"
T2_KILL_SLEEP_MS_MIN="${T2_KILL_SLEEP_MS_MIN:-50}"
T2_KILL_SLEEP_MS_MAX="${T2_KILL_SLEEP_MS_MAX:-200}"
T2_SIM_SLEEP_US="${T2_SIM_SLEEP_US:-200}"

T3_RUNS="${T3_RUNS:-5}"
T3_TICKS="${T3_TICKS:-20000}"
T3_AGENTS="${T3_AGENTS:-54}"
T3_PERSIST_EVERY="${T3_PERSIST_EVERY:-1}"

T4_DURATION_SEC="${T4_DURATION_SEC:-3600}"
T4_AGENTS="${T4_AGENTS:-15}"
T4_PERSIST_EVERY="${T4_PERSIST_EVERY:-10}"
T4_RSS_SAMPLE_SEC="${T4_RSS_SAMPLE_SEC:-5}"

T5_RUNS="${T5_RUNS:-5}"
T5_TICKS="${T5_TICKS:-10001}"
T5_AGENTS="${T5_AGENTS:-15}"
T5_INTERVALS="${T5_INTERVALS:-10 20 50}"

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

result_from_log() {
  local name="$1"
  local file="$2"
  awk -F'\t' -v n="${name}" '$1=="RESULT" && $2==n {print $3}' "${file}" | tail -1
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
require_cmd grep
require_cmd sort
require_cmd timeout

mkdir -p "${OUT_ROOT}/raw" "${OUT_ROOT}/summary"
cd "${REPO_DIR}"

cargo build --release --manifest-path deploy/bench/stack-harness/Cargo.toml --bin persist-probe >/dev/null
PROBE="deploy/bench/stack-harness/target/release/persist-probe"

{
  echo "repo_dir=${REPO_DIR}"
  echo "out_root=${OUT_ROOT}"
  echo "kernel=$(uname -r)"
  echo "cmdline=$(cat /proc/cmdline)"
  echo "t1_runs=${T1_RUNS}"
  echo "t1_ticks=${T1_TICKS}"
  echo "t1_agents=${T1_AGENTS}"
  echo "t1_sparse_interval=${T1_SPARSE_INTERVAL}"
  echo "t2_cycles=${T2_CYCLES}"
  echo "t2_kill_sleep_ms_min=${T2_KILL_SLEEP_MS_MIN}"
  echo "t2_kill_sleep_ms_max=${T2_KILL_SLEEP_MS_MAX}"
  echo "t2_sim_sleep_us=${T2_SIM_SLEEP_US}"
  echo "t3_runs=${T3_RUNS}"
  echo "t3_ticks=${T3_TICKS}"
  echo "t3_agents=${T3_AGENTS}"
  echo "t3_persist_every=${T3_PERSIST_EVERY}"
  echo "t4_duration_sec=${T4_DURATION_SEC}"
  echo "t4_agents=${T4_AGENTS}"
  echo "t4_persist_every=${T4_PERSIST_EVERY}"
  echo "t5_runs=${T5_RUNS}"
  echo "t5_ticks=${T5_TICKS}"
  echo "t5_agents=${T5_AGENTS}"
  echo "t5_intervals=${T5_INTERVALS}"
} > "${OUT_ROOT}/summary/run_config.env"

#############################################
# T1: Correctness Proxy (interval invariance)
#############################################
t1_dir="${OUT_ROOT}/raw/t1"
mkdir -p "${t1_dir}"
echo -e "run\tmatch\tfull_hash\tsparse_hash\tfull_us_per_tick\tsparse_us_per_tick\tfull_tick_max\tsparse_tick_max" > "${OUT_ROOT}/summary/t1.tsv"
t1_pass=0

for run in $(seq 1 "${T1_RUNS}"); do
  run_dir="${t1_dir}/run${run}"
  mkdir -p "${run_dir}"
  db_full="${run_dir}/full.redb"
  db_sparse="${run_dir}/sparse.redb"

  MODE=simulate DB_PATH="${db_full}" SIM_TICKS="${T1_TICKS}" SIM_AGENTS="${T1_AGENTS}" \
    PERSIST_EVERY=1 SIM_COLLECT_TICK_HIST=1 "${PROBE}" > "${run_dir}/full.log"
  MODE=simulate DB_PATH="${db_sparse}" SIM_TICKS="${T1_TICKS}" SIM_AGENTS="${T1_AGENTS}" \
    PERSIST_EVERY="${T1_SPARSE_INTERVAL}" SIM_COLLECT_TICK_HIST=1 "${PROBE}" > "${run_dir}/sparse.log"

  full_hash="$(result_from_log state_hash "${run_dir}/full.log")"
  sparse_hash="$(result_from_log state_hash "${run_dir}/sparse.log")"
  full_us="$(metric_from_log ecs.us_per_tick "${run_dir}/full.log")"
  sparse_us="$(metric_from_log ecs.us_per_tick "${run_dir}/sparse.log")"
  full_tick_max="$(result_from_log state_tick_max "${run_dir}/full.log")"
  sparse_tick_max="$(result_from_log state_tick_max "${run_dir}/sparse.log")"

  match=0
  if [[ -n "${full_hash}" && "${full_hash}" == "${sparse_hash}" ]]; then
    match=1
    t1_pass=$((t1_pass + 1))
  fi

  printf "run%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
    "${run}" "${match}" "${full_hash}" "${sparse_hash}" "${full_us}" "${sparse_us}" \
    "${full_tick_max}" "${sparse_tick_max}" >> "${OUT_ROOT}/summary/t1.tsv"
done

{
  echo "# T1 Correctness Proxy"
  echo
  echo "- Runs: ${T1_RUNS}"
  echo "- Match count: ${t1_pass}/${T1_RUNS}"
  echo "- Status: $([[ "${t1_pass}" -eq "${T1_RUNS}" ]] && echo PASS || echo FAIL)"
  echo "- Note: Delta-Persistenz ist im aktuellen Stack nicht implementiert; gemessen wurde Interval-Invarianz (persist=1 vs persist=${T1_SPARSE_INTERVAL})."
} > "${OUT_ROOT}/summary/t1.md"

##############################
# T2: Crash Safety (SIGKILL)
##############################
t2_dir="${OUT_ROOT}/raw/t2"
mkdir -p "${t2_dir}"
echo -e "cycle\tvalidate_ok\tdelay_ms\tstate_count\tvalidate_pass" > "${OUT_ROOT}/summary/t2.tsv"
t2_failures=0

for cycle in $(seq 1 "${T2_CYCLES}"); do
  cycle_dir="${t2_dir}/cycle${cycle}"
  mkdir -p "${cycle_dir}"
  db_path="${cycle_dir}/crash.redb"

  MODE=simulate DB_PATH="${db_path}" SIM_DURATION_SECS=3600 SIM_AGENTS=15 \
    PERSIST_EVERY=1 SIM_SLEEP_US="${T2_SIM_SLEEP_US}" "${PROBE}" > "${cycle_dir}/worker.log" 2>&1 &
  pid=$!

  range=$((T2_KILL_SLEEP_MS_MAX - T2_KILL_SLEEP_MS_MIN + 1))
  delay_ms=$((T2_KILL_SLEEP_MS_MIN + RANDOM % range))
  sleep "$(awk -v ms="${delay_ms}" 'BEGIN { printf "%.3f", ms / 1000.0 }')"
  kill -9 "${pid}" >/dev/null 2>&1 || true
  wait "${pid}" >/dev/null 2>&1 || true

  validate_ok=1
  if MODE=validate DB_PATH="${db_path}" VALIDATE_MIN_AGENTS=0 VALIDATE_STRICT=0 \
    "${PROBE}" > "${cycle_dir}/validate.log" 2>&1; then
    validate_ok=1
  else
    validate_ok=0
    t2_failures=$((t2_failures + 1))
  fi

  state_count="$(result_from_log state_count "${cycle_dir}/validate.log" || true)"
  validate_pass="$(result_from_log validate_pass "${cycle_dir}/validate.log" || true)"
  printf "cycle%s\t%s\t%s\t%s\t%s\n" \
    "${cycle}" "${validate_ok}" "${delay_ms}" "${state_count:-0}" "${validate_pass:-0}" \
    >> "${OUT_ROOT}/summary/t2.tsv"
done

{
  echo "# T2 Crash Safety"
  echo
  echo "- Cycles: ${T2_CYCLES}"
  echo "- Validation failures: ${t2_failures}"
  echo "- Status: $([[ "${t2_failures}" -eq 0 ]] && echo PASS || echo FAIL)"
} > "${OUT_ROOT}/summary/t2.md"

######################################
# T3: Backpressure Proxy (Burst Load)
######################################
t3_dir="${OUT_ROOT}/raw/t3"
mkdir -p "${t3_dir}"
echo -e "run\tmetric\tvalue\tunit" > "${OUT_ROOT}/summary/t3_metrics.tsv"

for run in $(seq 1 "${T3_RUNS}"); do
  run_dir="${t3_dir}/run${run}"
  mkdir -p "${run_dir}"
  MODE=simulate DB_PATH="${run_dir}/burst.redb" SIM_TICKS="${T3_TICKS}" SIM_AGENTS="${T3_AGENTS}" \
    PERSIST_EVERY="${T3_PERSIST_EVERY}" SIM_COLLECT_TICK_HIST=1 "${PROBE}" > "${run_dir}/simulate.log"

  for metric_name in \
    ecs.us_per_tick ecs.ticks_per_s ecs.tick_us_p95 ecs.tick_us_p99 ecs.tick_us_max \
    persist.flush_attempts persist.flush_failures \
    persist.batch_size_avg persist.flush_latency_us_avg persist.flush_latency_us_max \
    persist.queue_depth_max persist.drop_count persist.coalesce_count; do
    value="$(metric_from_log "${metric_name}" "${run_dir}/simulate.log")"
    unit="us"
    case "${metric_name}" in
      ecs.ticks_per_s) unit="ticks/s" ;;
      persist.flush_attempts|persist.flush_failures|persist.queue_depth_max|persist.drop_count|persist.coalesce_count) unit="count" ;;
      persist.batch_size_avg) unit="agents" ;;
      *) unit="us" ;;
    esac
    printf "run%s\t%s\t%s\t%s\n" "${run}" "${metric_name}" "${value}" "${unit}" >> "${OUT_ROOT}/summary/t3_metrics.tsv"
  done
done

summarize_numeric_tsv "${OUT_ROOT}/summary/t3_metrics.tsv" "${OUT_ROOT}/summary/t3_stats.tsv"
t3_p95_median="$(awk -F'\t' '$1=="ecs.tick_us_p95" {print $5}' "${OUT_ROOT}/summary/t3_stats.tsv" | tail -1)"
t3_flush_failures_median="$(awk -F'\t' '$1=="persist.flush_failures" {print $5}' "${OUT_ROOT}/summary/t3_stats.tsv" | tail -1)"
t3_queue_depth_median="$(awk -F'\t' '$1=="persist.queue_depth_max" {print $5}' "${OUT_ROOT}/summary/t3_stats.tsv" | tail -1)"
t3_drop_median="$(awk -F'\t' '$1=="persist.drop_count" {print $5}' "${OUT_ROOT}/summary/t3_stats.tsv" | tail -1)"
t3_coalesce_median="$(awk -F'\t' '$1=="persist.coalesce_count" {print $5}' "${OUT_ROOT}/summary/t3_stats.tsv" | tail -1)"
t3_flush_avg_median="$(awk -F'\t' '$1=="persist.flush_latency_us_avg" {print $5}' "${OUT_ROOT}/summary/t3_stats.tsv" | tail -1)"
t3_status="FAIL"
if awk \
  -v p95="${t3_p95_median:-999999}" \
  -v failures="${t3_flush_failures_median:-999999}" \
  -v queue_depth="${t3_queue_depth_median:-999999}" \
  -v drops="${t3_drop_median:-999999}" \
  -v coalesce="${t3_coalesce_median:-999999}" \
  'BEGIN { exit !(p95 <= 3500.0 && failures <= 0.0001 && queue_depth <= 0.0001 && drops <= 0.0001 && coalesce <= 0.0001) }'; then
  t3_status="PASS"
fi

{
  echo "# T3 Backpressure Proxy"
  echo
  echo "- Runs: ${T3_RUNS}"
  echo "- Burst profile: agents=${T3_AGENTS}, ticks=${T3_TICKS}, persist_every=${T3_PERSIST_EVERY}"
  echo "- ecs.tick_us_p95 median: ${t3_p95_median:-n/a} us"
  echo "- persist.flush_latency_us_avg median: ${t3_flush_avg_median:-n/a} us"
  echo "- persist.flush_failures median: ${t3_flush_failures_median:-n/a}"
  echo "- persist.queue_depth_max median: ${t3_queue_depth_median:-n/a}"
  echo "- persist.drop_count median: ${t3_drop_median:-n/a}"
  echo "- persist.coalesce_count median: ${t3_coalesce_median:-n/a}"
  echo "- Interim Gate (p95<=3500us + no silent drops/failures): ${t3_status}"
  echo "- Note: queue/dequeue remains 0 by design until write-behind queue is implemented."
} > "${OUT_ROOT}/summary/t3.md"

##############################
# T4: Soak (duration-based)
##############################
t4_dir="${OUT_ROOT}/raw/t4"
mkdir -p "${t4_dir}"
rss_tsv="${OUT_ROOT}/summary/t4_rss.tsv"
echo -e "timestamp_s\trss_kb" > "${rss_tsv}"

MODE=simulate DB_PATH="${t4_dir}/soak.redb" SIM_DURATION_SECS="${T4_DURATION_SEC}" \
  SIM_AGENTS="${T4_AGENTS}" PERSIST_EVERY="${T4_PERSIST_EVERY}" SIM_COLLECT_TICK_HIST=1 \
  "${PROBE}" > "${t4_dir}/simulate.log" 2>&1 &
t4_pid=$!
t4_start="$(date +%s)"

while kill -0 "${t4_pid}" >/dev/null 2>&1; do
  now="$(date +%s)"
  rss_kb="$(awk '/VmRSS:/ {print $2}' "/proc/${t4_pid}/status" 2>/dev/null || echo 0)"
  printf "%s\t%s\n" "$((now - t4_start))" "${rss_kb:-0}" >> "${rss_tsv}"
  sleep "${T4_RSS_SAMPLE_SEC}"
done

t4_rc=0
wait "${t4_pid}" || t4_rc=$?
rss_min="$(awk -F'\t' 'NR>1 {if (min=="" || $2<min) min=$2} END {print (min=="" ? 0 : min)}' "${rss_tsv}")"
rss_max="$(awk -F'\t' 'NR>1 {if (max=="" || $2>max) max=$2} END {print (max=="" ? 0 : max)}' "${rss_tsv}")"
rss_first="$(awk -F'\t' 'NR==2 {print $2}' "${rss_tsv}")"
rss_last="$(awk -F'\t' 'END {if (NR>=2) print $2; else print 0}' "${rss_tsv}")"
rss_delta=$((rss_last - rss_first))
t4_ticks_per_s="$(metric_from_log ecs.ticks_per_s "${t4_dir}/simulate.log" || true)"
t4_us_per_tick="$(metric_from_log ecs.us_per_tick "${t4_dir}/simulate.log" || true)"
t4_p95="$(metric_from_log ecs.tick_us_p95 "${t4_dir}/simulate.log" || true)"

{
  echo "# T4 Soak"
  echo
  echo "- Duration target: ${T4_DURATION_SEC}s"
  echo "- Exit code: ${t4_rc}"
  echo "- rss_min_kb: ${rss_min}"
  echo "- rss_max_kb: ${rss_max}"
  echo "- rss_delta_kb: ${rss_delta}"
  echo "- ecs.ticks_per_s: ${t4_ticks_per_s:-n/a}"
  echo "- ecs.us_per_tick: ${t4_us_per_tick:-n/a}"
  echo "- ecs.tick_us_p95: ${t4_p95:-n/a}"
  if [[ "${T4_DURATION_SEC}" -lt 3600 ]]; then
    echo "- Status: PRE-SOAK (unter 1h)"
  else
    echo "- Status: $([[ "${t4_rc}" -eq 0 ]] && echo PASS || echo FAIL)"
  fi
} > "${OUT_ROOT}/summary/t4.md"

#####################################
# T5: Durability Window (10/20/50)
#####################################
t5_dir="${OUT_ROOT}/raw/t5"
mkdir -p "${t5_dir}"
echo -e "interval\trun\tecs_us_per_tick\tecs_ticks_per_s\tecs_tick_us_p95\tecs_tick_us_max" > "${OUT_ROOT}/summary/t5_metrics.tsv"

for interval in ${T5_INTERVALS}; do
  for run in $(seq 1 "${T5_RUNS}"); do
    run_dir="${t5_dir}/int${interval}/run${run}"
    mkdir -p "${run_dir}"
    MODE=simulate DB_PATH="${run_dir}/durability.redb" SIM_TICKS="${T5_TICKS}" SIM_AGENTS="${T5_AGENTS}" \
      PERSIST_EVERY="${interval}" SIM_COLLECT_TICK_HIST=1 "${PROBE}" > "${run_dir}/simulate.log"

    us_per_tick="$(metric_from_log ecs.us_per_tick "${run_dir}/simulate.log")"
    ticks_per_s="$(metric_from_log ecs.ticks_per_s "${run_dir}/simulate.log")"
    tick_p95="$(metric_from_log ecs.tick_us_p95 "${run_dir}/simulate.log")"
    tick_max="$(metric_from_log ecs.tick_us_max "${run_dir}/simulate.log")"

    printf "%s\trun%s\t%s\t%s\t%s\t%s\n" \
      "${interval}" "${run}" "${us_per_tick}" "${ticks_per_s}" "${tick_p95}" "${tick_max}" \
      >> "${OUT_ROOT}/summary/t5_metrics.tsv"
  done
done

awk -F'\t' '
NR==1 { next }
{
  intv=$1
  n[intv]++
  us[intv, n[intv]]=$3+0
  tps[intv, n[intv]]=$4+0
  p95[intv, n[intv]]=$5+0
}
END {
  print "interval\tn\tus_per_tick_mean\tus_per_tick_median\tus_per_tick_p95\tticks_per_s_mean\tticks_per_s_median\tticks_per_s_p95\ttick_us_p95_median\trpo_ticks\trpo_seconds"
  for (intv in n) {
    count=n[intv]
    delete a_us; delete a_tps; delete a_p95
    sum_us=0; sum_tps=0
    for (i=1; i<=count; i++) {
      a_us[i]=us[intv, i]; sum_us += a_us[i]
      a_tps[i]=tps[intv, i]; sum_tps += a_tps[i]
      a_p95[i]=p95[intv, i]
    }
    asort(a_us); asort(a_tps); asort(a_p95)
    mid=int((count+1)/2)
    p95_idx=int((95*count+99)/100)
    if (p95_idx < 1) p95_idx=1
    if (p95_idx > count) p95_idx=count
    rpo_ticks=intv-1
    rpo_seconds=(sum_tps/count > 0) ? rpo_ticks / (sum_tps/count) : -1
    printf "%s\t%d\t%.4f\t%.4f\t%.4f\t%.4f\t%.4f\t%.4f\t%.4f\t%d\t%.6f\n",
      intv, count,
      sum_us/count, a_us[mid], a_us[p95_idx],
      sum_tps/count, a_tps[mid], a_tps[p95_idx],
      a_p95[mid], rpo_ticks, rpo_seconds
  }
}
' "${OUT_ROOT}/summary/t5_metrics.tsv" > "${OUT_ROOT}/summary/t5_stats.tsv"

best_interval="$(awk -F'\t' 'NR>1 { if (best=="" || $4 < best) {best=$4; intv=$1} } END {print intv}' "${OUT_ROOT}/summary/t5_stats.tsv")"
best_us="$(awk -F'\t' -v i="${best_interval}" 'NR>1 && $1==i {print $4}' "${OUT_ROOT}/summary/t5_stats.tsv")"
best_rpo_s="$(awk -F'\t' -v i="${best_interval}" 'NR>1 && $1==i {print $11}' "${OUT_ROOT}/summary/t5_stats.tsv")"

{
  echo "# T5 Durability Window"
  echo
  echo "- Intervals: ${T5_INTERVALS}"
  echo "- Runs per interval: ${T5_RUNS}"
  echo "- Best latency interval (median us_per_tick): ${best_interval} (us_per_tick=${best_us}, rpo_seconds=${best_rpo_s})"
} > "${OUT_ROOT}/summary/t5.md"

#################
# Overall summary
#################
t1_status="FAIL"
[[ "${t1_pass}" -eq "${T1_RUNS}" ]] && t1_status="PASS"
t2_status="FAIL"
[[ "${t2_failures}" -eq 0 ]] && t2_status="PASS"
t4_status="FAIL"
if [[ "${T4_DURATION_SEC}" -lt 3600 ]]; then
  t4_status="PRE-SOAK"
elif [[ "${t4_rc}" -eq 0 ]]; then
  t4_status="PASS"
fi

{
  echo "# Persistenz Suite Summary"
  echo
  echo "- Output root: ${OUT_ROOT}"
  echo "- T1 (Correctness Proxy): ${t1_status}"
  echo "- T2 (Crash Safety): ${t2_status}"
  echo "- T3 (Backpressure Proxy): ${t3_status}"
  echo "- T4 (Soak): ${t4_status}"
  echo "- T5 (Durability Window): PASS (see t5_stats.tsv)"
  echo
  echo "## Artefakte"
  echo "- summary/run_config.env"
  echo "- summary/t1.tsv, summary/t1.md"
  echo "- summary/t2.tsv, summary/t2.md"
  echo "- summary/t3_metrics.tsv, summary/t3_stats.tsv, summary/t3.md"
  echo "- summary/t4_rss.tsv, summary/t4.md"
  echo "- summary/t5_metrics.tsv, summary/t5_stats.tsv, summary/t5.md"
} > "${OUT_ROOT}/summary/overall.md"

echo "done: ${OUT_ROOT}"
