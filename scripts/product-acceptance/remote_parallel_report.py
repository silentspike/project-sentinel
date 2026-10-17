#!/usr/bin/env python3
"""Fail-closed offline analysis of private remote pilot/performance evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import re

from remote_parallel_probe import distribution, require


DIGEST = re.compile(r"[0-9a-f]{64}")
MATCH_FIELDS = ("runtime_target_class", "host_fingerprint", "hardware_digest", "dataset_digest",
                "workload_digest", "cache_state", "balloon_bytes", "concurrency",
                "background_load_id", "build_profile", "sample_kind")
PHASES = ("enqueue_wait", "permit_wait", "unreserved_authority", "reservation",
          "predispatch_authority", "http", "response_decode", "persistence", "cleanup", "total")
CATEGORIES = ("wait", "integration", "build", "test", "deploy", "rework")
STAGES = ("developer", "qa_negative", "correction", "qa_positive", "release", "delivery")


def number(value):
    return type(value) in (float, int) and math.isfinite(value) and value >= 0


def evidence(root, reference):
    require(type(reference) is dict and DIGEST.fullmatch(reference.get("sha256", "")), "invalid evidence digest")
    relative = Path(reference["file"])
    require(not relative.is_absolute() and ".." not in relative.parts, "evidence must be relative")
    path = (root / relative).resolve()
    require(path.is_relative_to(root.resolve()) and path.is_file(), "evidence missing or outside bundle")
    require(path.stat().st_size <= 16 * 1024 * 1024, "evidence exceeds bound")
    require(hashlib.sha256(path.read_bytes()).hexdigest() == reference["sha256"], "evidence changed")


def benchmark_pair(baseline, candidate):
    for run in (baseline, candidate):
        require(run.get("kind") == "release_benchmark", "observation/fixture is not release benchmark")
        require(run.get("release_stable") is True, "unfrozen release")
        require(DIGEST.fullmatch(run.get("manifest_sha256", "")), "missing artifact identity")
        for field in MATCH_FIELDS:
            require(field in run and run[field] not in (None, "", "unknown"), "missing matched field: " + field)
        require(run["runtime_target_class"] in ("LAB", "SINGLE_NODE"), "wrong benchmark target")
        require(run["cache_state"] in ("warm", "process_cold", "os_cold"), "cache state not controlled")
        require(type(run["balloon_bytes"]) is int and run["balloon_bytes"] > 0, "missing balloon assignment")
        require(type(run["concurrency"]) is int and run["concurrency"] > 0, "invalid concurrency")
        sidecars = run.get("sidecars", {})
        for key in ("cpu", "ram", "io", "cache", "balloon", "concurrent_load"):
            require(type(sidecars.get(key)) is list and len(sidecars[key]) >= 2, "missing sidecar: " + key)
        require(all(value == run["balloon_bytes"] for value in sidecars["balloon"]), "balloon changed")
        require(len(run.get("samples", [])) >= 100, "fewer than 100 samples; tail not qualified")
        require(all(s.get("ok") is True and number(s.get("internal_ms")) for s in run["samples"]),
                "failed or unmeasured internal samples")
    require(all(baseline[key] == candidate[key] for key in MATCH_FIELDS), "unmatched environment/workload")
    reports = {}
    for metric in ("internal_ms", "provider_ms", "end_to_end_ms"):
        pair = []
        for run in (baseline, candidate):
            values = [s[metric] for s in run["samples"] if number(s.get(metric))]
            pair.append(distribution(values))
        reports[metric] = {"baseline": pair[0], "candidate": pair[1]}
    return {"status": "MATCHED_MEASUREMENTS", "metrics": reports,
            "provider_note": "Provider spans are independent observed fields, never total minus guessed backend time.",
            "scope": "Descriptive distributions, not statistical significance, SLO acceptance, or 5x product throughput."}


def provider_phases(rows):
    """Accept sanitized structured phase records, not raw provider/customer logs."""
    grouped = {}
    for row in rows:
        phase = row.get("phase")
        require(phase in PHASES, "unknown phase")
        require(type(row.get("request_id")) is str and 0 < len(row["request_id"]) <= 128, "missing correlation")
        require(row.get("unit") in ("us", "ms") and number(row.get("elapsed")), "unknown latency unit")
        phases = grouped.setdefault(row["request_id"], {})
        require(phase not in phases, "duplicate phase for same request; reconcile retry/attempt first")
        require(type(row.get("accepted")) is bool, "missing outcome")
        phases[phase] = {"ms": row["elapsed"] / (1000 if row["unit"] == "us" else 1),
                         "accepted": row["accepted"]}
    result = {}
    for phase in PHASES:
        values = [phases[phase]["ms"] for phases in grouped.values() if phase in phases]
        result[phase] = {**distribution(values),
                         "rejected": sum(not p[phase]["accepted"] for p in grouped.values() if phase in p)}
    return {"status": "OBSERVED_PHASES", "requests": len(grouped), "phases": result,
            "missing_phase_counts": {p: sum(p not in row for row in grouped.values()) for p in PHASES},
            "scope": "HTTP is provider-facing time; persistence is separate. Phases may nest; never sum with total."}


def phase_observations(observation):
    capture = observation["phase_observation"]
    require(observation.get("release_stable") is True and observation.get("status") == "OBSERVED",
            "changed or incomplete phase observation")
    require(capture.get("possibly_truncated") is False, "journal window truncated")
    grouped = {}
    rejected = {}
    correlated = process_only = 0
    for row in capture["records"]:
        require(row.get("unit") in ("us", "ms") and number(row.get("elapsed")), "invalid phase duration")
        key = row["family"] + ":" + row["phase"]
        grouped.setdefault(key, []).append(row["elapsed"] / (1000 if row["unit"] == "us" else 1))
        rejected[key] = rejected.get(key, 0) + int(row.get("accepted") is False)
        if row.get("attribution") == "request_id":
            correlated += 1
        else:
            process_only += 1
    return {"status": "OBSERVED_PHASES" if grouped else "NOT_MEASURED",
            "metrics": {key: distribution(values) for key, values in grouped.items()},
            "request_correlated_spans": correlated, "process_only_spans": process_only,
            "rejected_spans": rejected,
            "sampling_scope": "Threshold-censored existing log spans; conditional descriptive ranks, not workload quantiles.",
            "scope": "Spans may nest and use different requests. No sum, inferred join, original-task attribution, or provider subtraction."}


def pilot(bundle, root):
    require(bundle.get("schema") == 1, "unsupported pilot schema")
    require(bundle.get("mode") in ("fixture", "genuine_internal", "external_customer"), "missing pilot mode")
    rows = bundle.get("records", [])
    require([r.get("stage") for r in rows] == list(STAGES), "incomplete or out-of-order pilot")
    require(type(bundle.get("project_id")) is str and bundle["project_id"], "missing original project")
    require(type(bundle.get("developer_id")) is int and type(bundle.get("qa_id")) is int,
            "actor identities must be resolved")
    require(bundle["developer_id"] != bundle["qa_id"], "developer cannot review own work")
    evidence(root, bundle["qa_registration"])
    seen, sequence = set(), -1
    requests = set()
    for row in rows:
        require(row.get("project_id") == bundle["project_id"], "project lineage changed")
        require(type(row.get("sequence")) is int and row["sequence"] > sequence, "invalid durable ordering")
        sequence = row["sequence"]
        require(DIGEST.fullmatch(row.get("artifact_sha256", "")), "missing artifact digest")
        require(type(row.get("revision")) is int and row["revision"] >= 0, "missing revision")
        evidence(root, row["evidence"])
        ref = row["evidence"]["sha256"]
        require(ref not in seen, "same evidence reused for different decisions")
        seen.add(ref)
        if row["stage"] in STAGES[:4]:
            expected = bundle["qa_id"] if row["stage"].startswith("qa_") else bundle["developer_id"]
            require(row.get("actor_id") == expected, "wrong decision owner")
            if bundle["mode"] != "fixture":
                require(row.get("decision_origin") == "provider_completion", "fixture/operator decision is not genuine")
                evidence(root, row["provider_completion"])
                require(type(row.get("request_id")) is str and row["request_id"], "missing provider correlation")
                require(row["request_id"] not in requests, "one provider request reused for distinct decisions")
                requests.add(row["request_id"])
    original, negative, corrected, positive, release, delivered = rows
    require(negative["artifact_sha256"] == original["artifact_sha256"]
            and negative["revision"] == original["revision"], "QA did not inspect original artifact")
    require(corrected["revision"] > original["revision"]
            and corrected["artifact_sha256"] != original["artifact_sha256"], "no genuine corrected artifact")
    require(DIGEST.fullmatch(negative.get("report_digest", "")), "missing negative QA report")
    require(corrected.get("qa_report_digest") == negative["report_digest"], "correction lacks negative-report binding")
    require(all(r["artifact_sha256"] == corrected["artifact_sha256"]
                and r["revision"] == corrected["revision"] for r in (positive, release, delivered)),
            "QA/release/delivery not bound to corrected artifact")
    require(release.get("authority") == "release_manager", "missing release authority")
    require(delivered.get("release_digest") == release["evidence"]["sha256"], "delivery lacks release binding")
    if bundle["mode"] == "external_customer":
        evidence(root, bundle["customer_agreement"])
        evidence(root, bundle["customer_acceptance"])
    return {"status": "FIXTURE_ONLY" if bundle["mode"] == "fixture" else "EVIDENCE_CHAIN_CHECKED",
            "mode": bundle["mode"], "stages": list(STAGES),
            "product_acceptance": "REQUIRES_INDEPENDENT_RUNTIME_REVIEW",
            "scope": "Checks supplied bindings; does not authenticate the exporter or manufacture employee decisions."}


def union_ms(intervals):
    result, end = 0, None
    for start, stop in sorted(intervals):
        result += max(0, stop - max(start, end if end is not None else start))
        end = max(stop, end if end is not None else stop)
    return result


def delivery_cycles(bundle, root):
    blocks = bundle.get("blocks", [])
    require(len(blocks) == 3, "exactly three actually accepted product blocks required")
    require(len({b["id"] for b in blocks}) == 3, "duplicate product blocks")
    results = []
    for block in blocks:
        evidence(root, block["acceptance"])
        require(block.get("mode") == "genuine_product" and block.get("accepted") is True,
                "infrastructure/fixture/unaccepted block")
        require(block.get("comparison_key") and block.get("baseline_comparison_key") == block["comparison_key"],
                "unmatched historical block")
        start, stop = block["started_ms"], block["accepted_ms"]
        require(number(start) and number(stop) and stop > start, "invalid end-to-end clock")
        baseline = block["baseline_elapsed_ms"]
        require(number(baseline) and baseline > 0, "missing actual comparable baseline")
        evidence(root, block["baseline_evidence"])
        spans = {category: [] for category in CATEGORIES}
        for span in block["spans"]:
            require(span["category"] in CATEGORIES, "unknown delivery span")
            a, b = span["start_ms"], span["end_ms"]
            require(number(a) and number(b) and start <= a <= b <= stop, "span outside delivery block")
            spans[span["category"]].append((a, b))
        require(set(block.get("not_applicable", [])) <= set(CATEGORIES), "unknown N/A category")
        require(all(spans[c] or c in block.get("not_applicable", []) for c in CATEGORIES), "missing delivery category")
        measured = stop - start
        results.append({"elapsed_ms": measured, "baseline_elapsed_ms": baseline,
                        "speedup": baseline / measured,
                        "category_union_ms": {c: union_ms(spans[c]) for c in CATEGORIES},
                        "accounted_union_ms": union_ms([s for rows in spans.values() for s in rows])})
    overall = sum(r["baseline_elapsed_ms"] for r in results) / sum(r["elapsed_ms"] for r in results)
    return {"status": "SUPPLIED_ACCEPTED_BLOCKS_ANALYZED", "blocks": results,
            "elapsed_speedup": overall, "fivefold_target_met_in_supplied_data": overall >= 5,
            "scope": "Requires independently verified acceptance and comparable baselines; overlapping categories are not additive."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("benchmark", "pilot", "phases", "cycles"))
    parser.add_argument("input", type=Path)
    parser.add_argument("--candidate", type=Path)
    args = parser.parse_args()
    try:
        require(args.input.stat().st_size <= 16 * 1024 * 1024, "input exceeds bound")
        data = json.loads(args.input.read_text())
        if args.command == "benchmark":
            require(args.candidate is not None and args.candidate.stat().st_size <= 16 * 1024 * 1024,
                    "candidate missing or exceeds bound")
            result = benchmark_pair(data, json.loads(args.candidate.read_text()))
        elif args.command == "pilot":
            result = pilot(data, args.input.parent)
        elif args.command == "phases":
            result = phase_observations(data) if type(data) is dict else provider_phases(data)
        else:
            result = delivery_cycles(data, args.input.parent)
        print(json.dumps(result, allow_nan=False))
        return 0
    except (ValueError, KeyError, TypeError, OSError):
        print(json.dumps({"status": "BLOCKED", "reason": "missing, invalid, changed, or unmatched evidence"}))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
