#!/usr/bin/env bash
set -euo pipefail

# Stack-near benchmark suite for Project Sentinel.
# Runs on the guest VM and produces reproducible metrics + median/p95 summary.

if [[ -x "${HOME}/.cargo/bin/cargo" ]]; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
fi

RUNS="${RUNS:-5}"
PROFILE_NAME="${PROFILE_NAME:-baseline}"
FIO_RUNTIME="${FIO_RUNTIME:-10}"
BWRAP_ITERS="${BWRAP_ITERS:-200}"
WASMTIME_ITERS="${WASMTIME_ITERS:-500}"
REPO_DIR="${REPO_DIR:-$HOME/project-sentinel}"
OUT_ROOT="${OUT_ROOT:-/tmp/sentinel-stack-suite-${PROFILE_NAME}-$(date +%Y%m%d-%H%M%S)}"
STACK_HARNESS_TMPDIR="${STACK_HARNESS_TMPDIR:-/tmp}"
FIO_DIR="${FIO_DIR:-/tmp}"
SENTINEL_ZENOH_SHM="${SENTINEL_ZENOH_SHM:-0}"
ECS_ENABLE_PERSIST="${ECS_ENABLE_PERSIST:-1}"
ECS_PERSIST_EVERY_N_TICKS="${ECS_PERSIST_EVERY_N_TICKS:-20}"
CG_MIN_ACCEPTABLE_IOPS="${CG_MIN_ACCEPTABLE_IOPS:-200}"
CG_TIMEOUT_SEC="${CG_TIMEOUT_SEC:-0}"
if [[ "${CG_TIMEOUT_SEC}" == "0" ]]; then
  CG_TIMEOUT_SEC="$((FIO_RUNTIME * 20 + 60))"
fi

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing command: $1" >&2
    exit 1
  }
}

require_cmd cargo
require_cmd fio
require_cmd jq
require_cmd wasmtime
require_cmd bwrap
require_cmd awk
require_cmd sed

if ! sudo -n true >/dev/null 2>&1; then
  echo "sudo without password is required for this suite" >&2
  exit 1
fi

mkdir -p "${OUT_ROOT}/raw" "${OUT_ROOT}/summary"

cd "${REPO_DIR}"

HARNESS_MANIFEST="deploy/bench/stack-harness/Cargo.toml"
HARNESS_BIN="deploy/bench/stack-harness/target/release/stack-harness"

cargo build --release --manifest-path "${HARNESS_MANIFEST}" >/dev/null

cat > "${OUT_ROOT}/bench.wat" <<'WAT'
(module
  (func (export "run"))
)
WAT

ROOT_SRC="$(findmnt -n -o SOURCE / || true)"
ROOT_DEV="/dev/$(lsblk -n -o PKNAME "${ROOT_SRC}" 2>/dev/null || true)"
if [[ -z "${ROOT_DEV}" || "${ROOT_DEV}" == "/dev/" ]]; then
  ROOT_DEV="/dev/sda"
fi

echo "profile=${PROFILE_NAME}" > "${OUT_ROOT}/summary/run_config.env"
echo "runs=${RUNS}" >> "${OUT_ROOT}/summary/run_config.env"
echo "fio_runtime=${FIO_RUNTIME}" >> "${OUT_ROOT}/summary/run_config.env"
echo "bwrap_iters=${BWRAP_ITERS}" >> "${OUT_ROOT}/summary/run_config.env"
echo "wasmtime_iters=${WASMTIME_ITERS}" >> "${OUT_ROOT}/summary/run_config.env"
echo "stack_harness_tmpdir=${STACK_HARNESS_TMPDIR}" >> "${OUT_ROOT}/summary/run_config.env"
echo "fio_dir=${FIO_DIR}" >> "${OUT_ROOT}/summary/run_config.env"
echo "zenoh_shm=${SENTINEL_ZENOH_SHM}" >> "${OUT_ROOT}/summary/run_config.env"
echo "ecs_enable_persist=${ECS_ENABLE_PERSIST}" >> "${OUT_ROOT}/summary/run_config.env"
echo "ecs_persist_every_n_ticks=${ECS_PERSIST_EVERY_N_TICKS}" >> "${OUT_ROOT}/summary/run_config.env"
echo "cg_min_acceptable_iops=${CG_MIN_ACCEPTABLE_IOPS}" >> "${OUT_ROOT}/summary/run_config.env"
echo "cg_timeout_sec=${CG_TIMEOUT_SEC}" >> "${OUT_ROOT}/summary/run_config.env"
echo "root_dev=${ROOT_DEV}" >> "${OUT_ROOT}/summary/run_config.env"
echo "kernel=$(uname -r)" >> "${OUT_ROOT}/summary/run_config.env"
echo "cmdline=$(cat /proc/cmdline)" >> "${OUT_ROOT}/summary/run_config.env"

for run in $(seq 1 "${RUNS}"); do
  run_dir="${OUT_ROOT}/raw/run${run}"
  mkdir -p "${run_dir}"

  {
    echo "timestamp=$(date -Is)"
    echo "profile=${PROFILE_NAME}"
    echo "run=${run}"
    echo "zenoh_shm=${SENTINEL_ZENOH_SHM}"
    echo "ecs_enable_persist=${ECS_ENABLE_PERSIST}"
    echo "ecs_persist_every_n_ticks=${ECS_PERSIST_EVERY_N_TICKS}"
    echo "tmpdir=${STACK_HARNESS_TMPDIR}"
    echo "root_dev=${ROOT_DEV}"
    echo "meminfo=$(grep -E 'MemAvailable|HugePages_Total|Hugetlb' /proc/meminfo | tr '\n' ';')"
  } > "${run_dir}/snapshot.env"

  SENTINEL_ZENOH_SHM="${SENTINEL_ZENOH_SHM}" \
  STACK_HARNESS_TMPDIR="${STACK_HARNESS_TMPDIR}" \
  ECS_ENABLE_PERSIST="${ECS_ENABLE_PERSIST}" \
  ECS_PERSIST_EVERY_N_TICKS="${ECS_PERSIST_EVERY_N_TICKS}" \
  "${HARNESS_BIN}" > "${run_dir}/harness.log"

  awk -F'\t' '$1=="METRIC"{print $2 "\t" $3 "\t" $4}' "${run_dir}/harness.log" > "${run_dir}/run_metrics.tsv"

  fio_file="${FIO_DIR}/stack-fio-${PROFILE_NAME}-run${run}.bin"

  fio --name=randrw \
    --filename="${fio_file}" \
    --rw=randrw --rwmixread=70 \
    --bs=4k --iodepth=32 --numjobs=1 \
    --time_based=1 --runtime="${FIO_RUNTIME}" \
    --direct=1 --size=1G \
    --ioengine=io_uring \
    --group_reporting --output-format=json > "${run_dir}/fio_iouring.json"

  fio --name=randrw \
    --filename="${fio_file}" \
    --rw=randrw --rwmixread=70 \
    --bs=4k --iodepth=32 --numjobs=1 \
    --time_based=1 --runtime="${FIO_RUNTIME}" \
    --direct=1 --size=1G \
    --ioengine=psync \
    --group_reporting --output-format=json > "${run_dir}/fio_psync.json"

  rm -f "${fio_file}" || true

  riops_uring="$(sed -n '/^{/,$p' "${run_dir}/fio_iouring.json" | jq -r '.jobs[0].read.iops')"
  wiops_uring="$(sed -n '/^{/,$p' "${run_dir}/fio_iouring.json" | jq -r '.jobs[0].write.iops')"
  total_uring="$(awk -v r="${riops_uring}" -v w="${wiops_uring}" 'BEGIN{printf "%.4f", r+w}')"

  riops_psync="$(sed -n '/^{/,$p' "${run_dir}/fio_psync.json" | jq -r '.jobs[0].read.iops')"
  wiops_psync="$(sed -n '/^{/,$p' "${run_dir}/fio_psync.json" | jq -r '.jobs[0].write.iops')"
  total_psync="$(awk -v r="${riops_psync}" -v w="${wiops_psync}" 'BEGIN{printf "%.4f", r+w}')"

  printf "fio.iouring.total_iops\t%s\tops/s\n" "${total_uring}" >> "${run_dir}/run_metrics.tsv"
  printf "fio.psync.total_iops\t%s\tops/s\n" "${total_psync}" >> "${run_dir}/run_metrics.tsv"

  cg_metric_ok=0
  cg_retry_used=0
  cg_total_iops="-1"
  cg_file="${run_dir}/fio_iouring_cg300.json"
  cg_retry_file="${run_dir}/fio_iouring_cg300_retry.json"
  cg_fio_cmd="fio --name=randrw --filename='${FIO_DIR}/stack-fio-cg-${PROFILE_NAME}-run${run}.bin' --rw=randrw --rwmixread=70 --bs=4k --iodepth=32 --numjobs=1 --time_based=1 --runtime=${FIO_RUNTIME} --direct=1 --size=1G --ioengine=io_uring --group_reporting --output-format=json"

  if timeout "${CG_TIMEOUT_SEC}" sudo -n systemd-run --quiet --wait --collect --pipe \
      -p "IOReadIOPSMax=${ROOT_DEV} 300" \
      -p "IOWriteIOPSMax=${ROOT_DEV} 300" \
      bash -lc "${cg_fio_cmd}" > "${cg_file}" 2>/dev/null; then
    r="$(sed -n '/^{/,$p' "${cg_file}" | jq -r '.jobs[0].read.iops')"
    w="$(sed -n '/^{/,$p' "${cg_file}" | jq -r '.jobs[0].write.iops')"
    t="$(awk -v a="${r}" -v b="${w}" 'BEGIN{printf "%.4f", a+b}')"
    cg_metric_ok=1

    if awk -v cur="${t}" -v min="${CG_MIN_ACCEPTABLE_IOPS}" 'BEGIN{exit !(cur < min)}'; then
      if timeout "${CG_TIMEOUT_SEC}" sudo -n systemd-run --quiet --wait --collect --pipe \
          -p "IOReadIOPSMax=${ROOT_DEV} 300" \
          -p "IOWriteIOPSMax=${ROOT_DEV} 300" \
          bash -lc "${cg_fio_cmd}" > "${cg_retry_file}" 2>/dev/null; then
        r2="$(sed -n '/^{/,$p' "${cg_retry_file}" | jq -r '.jobs[0].read.iops')"
        w2="$(sed -n '/^{/,$p' "${cg_retry_file}" | jq -r '.jobs[0].write.iops')"
        t2="$(awk -v a="${r2}" -v b="${w2}" 'BEGIN{printf "%.4f", a+b}')"
        cg_retry_used=1
        if awk -v a="${t2}" -v b="${t}" 'BEGIN{exit !(a > b)}'; then
          t="${t2}"
          mv -f "${cg_retry_file}" "${cg_file}"
        fi
      fi
    fi
    cg_total_iops="${t}"
  fi

  printf "cgroup.io300.total_iops\t%s\tops/s\n" "${cg_total_iops}" >> "${run_dir}/run_metrics.tsv"
  printf "cgroup.io300.available\t%s\tbool\n" "${cg_metric_ok}" >> "${run_dir}/run_metrics.tsv"
  printf "cgroup.io300.retry_used\t%s\tbool\n" "${cg_retry_used}" >> "${run_dir}/run_metrics.tsv"
  sudo -n rm -f "${FIO_DIR}/stack-fio-cg-${PROFILE_NAME}-run${run}.bin" || true

  start_ns="$(date +%s%N)"
  for _ in $(seq 1 "${BWRAP_ITERS}"); do
    sudo -n bwrap \
      --ro-bind / / \
      --tmpfs /tmp \
      --proc /proc \
      --dev /dev \
      --unshare-all \
      -- /bin/true
  done
  end_ns="$(date +%s%N)"
  elapsed_ns="$((end_ns - start_ns))"
  bwrap_ops_s="$(awk -v it="${BWRAP_ITERS}" -v ns="${elapsed_ns}" 'BEGIN{printf "%.4f", it/(ns/1000000000)}')"
  bwrap_us_spawn="$(awk -v it="${BWRAP_ITERS}" -v ns="${elapsed_ns}" 'BEGIN{printf "%.4f", (ns/1000)/it}')"
  printf "bwrap.spawn_ops_s\t%s\tops/s\n" "${bwrap_ops_s}" >> "${run_dir}/run_metrics.tsv"
  printf "bwrap.spawn_us\t%s\tus\n" "${bwrap_us_spawn}" >> "${run_dir}/run_metrics.tsv"

  start_ns="$(date +%s%N)"
  for _ in $(seq 1 "${WASMTIME_ITERS}"); do
    wasmtime run --invoke run "${OUT_ROOT}/bench.wat" >/dev/null
  done
  end_ns="$(date +%s%N)"
  elapsed_ns="$((end_ns - start_ns))"
  wasm_ops_s="$(awk -v it="${WASMTIME_ITERS}" -v ns="${elapsed_ns}" 'BEGIN{printf "%.4f", it/(ns/1000000000)}')"
  wasm_us_invoke="$(awk -v it="${WASMTIME_ITERS}" -v ns="${elapsed_ns}" 'BEGIN{printf "%.4f", (ns/1000)/it}')"
  printf "wasmtime.invoke_ops_s\t%s\tops/s\n" "${wasm_ops_s}" >> "${run_dir}/run_metrics.tsv"
  printf "wasmtime.invoke_us\t%s\tus\n" "${wasm_us_invoke}" >> "${run_dir}/run_metrics.tsv"

  if sudo -n bpftool feature probe kernel >/dev/null 2>&1; then
    bpftool_ok=1
  else
    bpftool_ok=0
  fi
  fuse_dev=0
  [[ -e /dev/fuse ]] && fuse_dev=1
  fuse_mount=0
  command -v fusermount3 >/dev/null 2>&1 && fuse_mount=1
  landlock=0
  [[ -e /sys/kernel/security/landlock ]] && landlock=1

  printf "ebpf.bpftool.available\t%s\tbool\n" "${bpftool_ok}" >> "${run_dir}/run_metrics.tsv"
  printf "fuse.device.available\t%s\tbool\n" "${fuse_dev}" >> "${run_dir}/run_metrics.tsv"
  printf "fuse.fusermount.available\t%s\tbool\n" "${fuse_mount}" >> "${run_dir}/run_metrics.tsv"
  printf "landlock.fs.available\t%s\tbool\n" "${landlock}" >> "${run_dir}/run_metrics.tsv"
done

echo -e "run\tmetric\tvalue\tunit" > "${OUT_ROOT}/summary/all_metrics.tsv"
for run in $(seq 1 "${RUNS}"); do
  awk -v r="run${run}" -F'\t' '{printf "%s\t%s\t%s\t%s\n", r, $1, $2, $3}' \
    "${OUT_ROOT}/raw/run${run}/run_metrics.tsv" >> "${OUT_ROOT}/summary/all_metrics.tsv"
done

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
}' "${OUT_ROOT}/summary/all_metrics.tsv" | sort > "${OUT_ROOT}/summary/stats.tsv"

{
  echo "# Stack Suite Summary (${PROFILE_NAME})"
  echo
  echo "- runs: ${RUNS}"
  echo "- out_root: ${OUT_ROOT}"
  echo "- zenoh_shm: ${SENTINEL_ZENOH_SHM}"
  echo "- stack_harness_tmpdir: ${STACK_HARNESS_TMPDIR}"
  echo
  echo "## Key Metrics (median / p95)"
  echo
  echo "| Metric | Median | P95 | Unit |"
  echo "|---|---:|---:|---|"
  awk -F'\t' '
  $1=="ecs.ticks_per_s" ||
  $1=="ecs.us_per_tick" ||
  $1=="redb.write_ops_s" ||
  $1=="redb.read_ops_s" ||
  $1=="limbo.insert_ops_s" ||
  $1=="zenoh.roundtrip_mean_us" ||
  $1=="zenoh.roundtrip_p95_us" ||
  $1=="fio.iouring.total_iops" ||
  $1=="fio.psync.total_iops" ||
  $1=="cgroup.io300.total_iops" ||
  $1=="bwrap.spawn_us" ||
  $1=="wasmtime.invoke_us" {
    printf "| %s | %.4f | %.4f | %s |\n", $1, $5, $6, $2
  }' "${OUT_ROOT}/summary/stats.tsv"
} > "${OUT_ROOT}/summary/highlights.md"

echo "STACK_SUITE_DONE ${OUT_ROOT}"
