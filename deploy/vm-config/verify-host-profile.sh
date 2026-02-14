#!/usr/bin/env bash
set -euo pipefail

EXPECTED_RUNNING_CONTAINS_1="${EXPECTED_RUNNING_CONTAINS_1:-isolcpus=0-3}"
EXPECTED_RUNNING_CONTAINS_2="${EXPECTED_RUNNING_CONTAINS_2:-irqaffinity=4-11}"
EXPECTED_NEXT_CMDLINE_CONTAINS_1="${EXPECTED_NEXT_CMDLINE_CONTAINS_1:-isolcpus=0-3}"
EXPECTED_NEXT_CMDLINE_CONTAINS_2="${EXPECTED_NEXT_CMDLINE_CONTAINS_2:-irqaffinity=4-11}"
EXPECTED_ARC_MAX="${EXPECTED_ARC_MAX:-4319084544}"
EXPECTED_KSM_RUN="${EXPECTED_KSM_RUN:-0}"
EXPECTED_HUGEPAGES_TOTAL="${EXPECTED_HUGEPAGES_TOTAL:-0}"

fail=0

check_contains() {
  local haystack="$1"
  local needle="$2"
  local name="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "ok: ${name} contains '${needle}'"
  else
    echo "fail: ${name} missing '${needle}'"
    fail=1
  fi
}

running_cmdline="$(cat /proc/cmdline)"
next_cmdline="$(cat /etc/kernel/cmdline)"
arc_max="$(cat /sys/module/zfs/parameters/zfs_arc_max)"
ksm_run="$(cat /sys/kernel/mm/ksm/run)"
hugepages_total="$(awk '/HugePages_Total/ {print $2}' /proc/meminfo)"

check_contains "${running_cmdline}" "${EXPECTED_RUNNING_CONTAINS_1}" "running_cmdline"
check_contains "${running_cmdline}" "${EXPECTED_RUNNING_CONTAINS_2}" "running_cmdline"
check_contains "${next_cmdline}" "${EXPECTED_NEXT_CMDLINE_CONTAINS_1}" "next_cmdline"
check_contains "${next_cmdline}" "${EXPECTED_NEXT_CMDLINE_CONTAINS_2}" "next_cmdline"

if [[ "${arc_max}" == "${EXPECTED_ARC_MAX}" ]]; then
  echo "ok: arc_max=${arc_max}"
else
  echo "fail: arc_max=${arc_max}, expected=${EXPECTED_ARC_MAX}"
  fail=1
fi

if [[ "${ksm_run}" == "${EXPECTED_KSM_RUN}" ]]; then
  echo "ok: ksm_run=${ksm_run}"
else
  echo "fail: ksm_run=${ksm_run}, expected=${EXPECTED_KSM_RUN}"
  fail=1
fi

if [[ "${hugepages_total}" == "${EXPECTED_HUGEPAGES_TOTAL}" ]]; then
  echo "ok: hugepages_total=${hugepages_total}"
else
  echo "fail: hugepages_total=${hugepages_total}, expected=${EXPECTED_HUGEPAGES_TOTAL}"
  fail=1
fi

echo "running_cmdline=${running_cmdline}"
echo "next_cmdline=${next_cmdline}"
echo "arc_max=${arc_max}"
echo "ksm_run=${ksm_run}"
echo "hugepages_total=${hugepages_total}"

if [[ "${fail}" -ne 0 ]]; then
  exit 1
fi
