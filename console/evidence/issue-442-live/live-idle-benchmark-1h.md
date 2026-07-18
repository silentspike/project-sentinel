# Issue #442 Gaia Console Readiness Idle Benchmark

Date: 2026-07-18
Node: `ubuntu@10.0.0.241`

The benchmark ran directly on the test VM, never through `cargo remote`. `.240`
was not contacted. The readiness service remained active and no explicit Gaia
session was submitted during the measurement.

Command:

```bash
ssh ubuntu@10.0.0.241 'bash -s' <<'EOF'
out=/tmp/issue442-idle-1h.tsv
printf 'sample\tepoch_s\tpid\tcpu_ticks\trss_kb\tclaude_count\n' > "$out"
for sample in $(seq 0 60); do
  pid=$(systemctl show sentinel-gaia-loop -p MainPID --value)
  cpu_ticks=$(awk '{print $14 + $15}' "/proc/$pid/stat")
  rss_kb=$(awk '/^VmRSS:/ {print $2}' "/proc/$pid/status")
  claude_count=$(pgrep -xc claude || true)
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$sample" "$(date +%s)" "$pid" "$cpu_ticks" "$rss_kb" "$claude_count" >> "$out"
  if [ "$sample" -lt 60 ]; then sleep 60; fi
done
awk 'NR==2 {start=$2; first_cpu=$4; min_rss=$5; max_rss=$5}
     NR>1 {samples++; end=$2; last_cpu=$4; rss_sum+=$5;
           if ($5<min_rss) min_rss=$5; if ($5>max_rss) max_rss=$5;
           claude_sum+=$6}
     END {elapsed=end-start; ticks=last_cpu-first_cpu;
       printf "samples=%d\nelapsed_seconds=%d\ncpu_ticks_delta=%d\ncpu_average_percent=%.6f\nrss_kb_min=%d\nrss_kb_average=%.2f\nrss_kb_max=%d\nclaude_process_sample_sum=%d\n",
       samples, elapsed, ticks, ticks/100/elapsed*100, min_rss,
       rss_sum/samples, max_rss, claude_sum}' "$out"
EOF
```

Output:

```text
samples=61
elapsed_seconds=3602
cpu_ticks_delta=3
cpu_average_percent=0.000833
rss_kb_min=9368
rss_kb_average=9368.00
rss_kb_max=9368
claude_process_sample_sum=0
```

The raw 61-row sample set is committed as `idle-1h-samples.tsv` beside this
file. The sampled binary was:

```text
6f1fd201e5c3f538c6a9f79b73459acc5a717bfca64617320803ef1b02030365  /opt/sentinel/bin/sentinel-gaia-loop
service=active
panic_fatal_count=0
claude_processes=0
```

The final release hash is
`569089bc92c851182d9784d329af9b41295e4b3e2e6507f3581f4445a649d410`.
The post-benchmark source change clears inherited variables only in
`ClaudeSessionRunner::run`; the readiness scan and scheduled-loop bodies are
unchanged. The final artifact is separately deployed and read back on both VMs.

System readback immediately after the run:

```text
vmstat live samples: 99-100% idle, swap=0
iostat live samples: 98.51-99.00% idle, iowait=0.50-1.00%, sda util=0.99-1.40%
ss -s: Total 178, TCP 9, UDP 9
/opt/sentinel/data filesystem: 19G total, 6.8G used, 12G available, 37%
```

Other issue benchmarks:

```text
event_to_alert_latency_seconds=22
deep_resume_token_accounting_units=31496
deep_resume_cost_usd=0.0206505
setup_token_accounting_units=12267
setup_cost_usd=0.0197928
dashboard_stream_token_accounting_units=4119
dashboard_stream_cost_usd=0.004407
native_install_smokes_cost_usd=0.027488
final_environment_hardening_smoke_cost_usd=0.004439
accepted_native_verification_cost_usd=0.0767773
failed_pre_fix_diagnostic_cost_usd=0.0642571
```

The failed pre-fix diagnostic is reported for cost transparency and excluded
from accepted-session benchmark metrics.
