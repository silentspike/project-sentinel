"""Synthetic mechanism tests, never real employee or customer acceptance."""

import copy
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import remote_parallel_probe as probe
import remote_parallel_report as report
import remote_parallel_native as native


class DistributionTests(unittest.TestCase):
    def test_empty_and_low_n_never_manufacture_p99(self):
        self.assertIsNone(probe.distribution([])["p99_ms"])
        self.assertIsNone(probe.distribution(list(range(99)))["p99_ms"])

    def test_nearest_rank_and_sample_count(self):
        value = probe.distribution(list(range(1, 101)))
        self.assertEqual([value[k] for k in ("n", "p50_ms", "p95_ms", "p99_ms")], [100, 50, 95, 99])

    def test_nonfinite_negative_bool_rejected(self):
        for value in (float("nan"), float("inf"), -1, True, "1"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                probe.distribution([value])

    def test_observer_refuses_wrong_host_before_read_or_http(self):
        args = mock.Mock(expected_hostname="other")
        with mock.patch.object(probe.socket, "gethostname", return_value="test"), \
                mock.patch.object(probe.Path, "read_bytes") as read, \
                mock.patch.object(probe, "sample") as sample:
            with self.assertRaises(ValueError):
                probe.observe(args)
            read.assert_not_called()
            sample.assert_not_called()

    def test_http_redirects_not_followed(self):
        self.assertIsNone(probe.NoRedirect().redirect_request(None, None, 302, "", {}, "https://elsewhere.invalid"))

    def test_phase_parser_exports_only_static_fields(self):
        message = ('Owned provider phase finished agent=AGENT-1 request_id="test-request" '
                   'phase="persistence" elapsed_us=1234 elapsed_ms=1 accepted=true private=secret')
        value = probe.parse_phase({"MESSAGE": message, "__REALTIME_TIMESTAMP": "1234"})
        self.assertEqual(value["elapsed"], 1234)
        self.assertEqual(value["unit"], "us")
        self.assertNotIn("secret", json.dumps(value))
        self.assertEqual(probe.parse_phase({"MESSAGE": list(message.encode())}),
                         {key: v for key, v in value.items() if key != "timestamp_us"})

    def test_unknown_phases_and_private_messages_rejected(self):
        self.assertIsNone(probe.parse_phase({"MESSAGE": "private customer body"}))
        self.assertIsNone(probe.parse_phase({"MESSAGE": [256]}))
        self.assertIsNone(probe.parse_phase({"MESSAGE": 'Adaptive context evidence read phase="private" elapsed_ms=7'}))

    def test_context_funding_and_adoption_are_not_silently_attributed(self):
        for label, field, phase in (("Adaptive context evidence read", "phase", "preparation_snapshot"),
                                    ("Work funding preparation phase", "phase", "receipt_transaction"),
                                    ("Owned completion stage finished", "stage", "admission")):
            row = probe.parse_phase({"MESSAGE": f'{label} {field}="{phase}" elapsed_ms=12'})
            self.assertEqual(row["attribution"], "process_only_not_original_task")

    def test_inconsistent_phase_units_rejected(self):
        with self.assertRaises(ValueError):
            probe.parse_phase({"MESSAGE": 'Owned provider phase finished phase="http" elapsed_us=1000 elapsed_ms=2'})


class NativeTests(unittest.TestCase):
    def text(self):
        return "\n".join([
            "test result: ok. 1 passed; 0 failed; 0 ignored; 1 filtered out",
            *[f"READ_ONLY_HEALTH_PASS pass={i} repeat=3 elapsed_us={i * 1000} read_cache_records=1 total_changes=0"
              for i in (1, 2, 3)],
            *["READ_ONLY_FUNDING_DIAGNOSTIC mode=adaptive_health_inventory selected=Some(true) elapsed_ms=1 error_code=None failures=[] writes=0"] * 3])

    def test_exact_native_test_and_three_passes(self):
        passes = native.parse_native(self.text(), 0)
        self.assertEqual([p["process_state"] for p in passes],
                         ["process_cold", "same_process_warm", "same_process_warm"])

    def test_zero_filtered_tests_failed_selection_or_writes_rejected(self):
        for text in (self.text().replace("1 passed", "0 passed"),
                     self.text().replace("writes=0", "writes=1"),
                     self.text().replace("Some(true)", "None"),
                     self.text().replace("pass=2", "pass=1"),
                     self.text().replace("error_code=None", "error_code=CorruptStore")):
            with self.assertRaises(ValueError):
                native.parse_native(text, 0)

    def test_failure_exit_and_output_bounds_rejected(self):
        for text, code in ((self.text(), 1), ("x" * (native.MAX_OUTPUT + 1), 0)):
            with self.assertRaises(ValueError):
                native.parse_native(text, code)

    def test_wrong_target_rejected_before_file_access(self):
        with mock.patch.object(native.socket, "gethostname", return_value="test"):
            with self.assertRaises(ValueError):
                native.compare({"runtime_target_class": "LAB", "expected_hostname": "other"})

    @staticmethod
    def anchor(value):
        return {"wall_unix_ns": 100000 + value, "monotonic_before_ns": value,
                "monotonic_after_ns": value + 2}

    def test_clock_anchor_brackets_actual_wall_read(self):
        with mock.patch.object(native.time, "monotonic_ns", side_effect=[100, 120]), \
                mock.patch.object(native.time, "time_ns", return_value=1000):
            self.assertEqual(native.clock_anchor(),
                             {"wall_unix_ns": 1000, "monotonic_before_ns": 100, "monotonic_after_ns": 120})
        with mock.patch.object(native.time, "monotonic_ns", side_effect=[120, 100]):
            with self.assertRaises(ValueError):
                native.clock_anchor()

    def test_resource_sample_retains_counter_and_collection_brackets(self):
        before, after = self.anchor(100), self.anchor(200)
        with mock.patch.object(native, "clock_anchor", side_effect=[before, after]), \
                mock.patch.object(native, "counters", return_value={"monotonic_ns": 150, "memory": {}}):
            value = native.resource_sample({})
        self.assertEqual(value["monotonic_ns"], 150)
        self.assertEqual(value["collection_timing"], {"before": before, "after": after})
        with mock.patch.object(native, "clock_anchor", side_effect=[after, before]), \
                mock.patch.object(native, "counters", return_value={"monotonic_ns": 150}):
            with self.assertRaises(ValueError):
                native.resource_sample({})

    def child_fixture(self, anchors, wait_results, *, timeout_ns=0, expect_error=False):
        process = mock.Mock(pid=4321, returncode=None)
        usage = mock.Mock(ru_utime=.001, ru_stime=.002, ru_maxrss=128, ru_inblock=2, ru_oublock=3)
        results = [(pid, status, usage if pid else None) for pid, status in wait_results]
        def launch(argv, **kwargs):
            self.assertEqual(argv[1], native.TEST)
            kwargs["stdout"].write(self.text().encode())
            kwargs["stdout"].flush()
            return process
        with tempfile.TemporaryDirectory(dir=os.environ.get("RUNNER_TEMP", "/work/tmp/project-sentinel"),
                                         prefix="native-anchor-test-") as root, \
                mock.patch.object(native, "clock_anchor", side_effect=anchors), \
                mock.patch.object(native.subprocess, "Popen", side_effect=launch), \
                mock.patch.object(native.os, "wait4", side_effect=results) as wait, \
                mock.patch.object(native.os, "killpg") as kill, \
                mock.patch.object(native.time, "monotonic_ns", return_value=timeout_ns), \
                mock.patch.object(native.time, "sleep"):
            if expect_error:
                with self.assertRaises(ValueError):
                    native.child(Path("synthetic-not-executed"), {}, Path(root), 1)
                value = None
            else:
                value = native.child(Path("synthetic-not-executed"), {}, Path(root), 1)
        return value, process, wait, kill

    def test_child_records_launch_and_polled_reap_not_fabricated_exact_exit(self):
        anchors = [self.anchor(n) for n in (100, 200, 300, 400, 500)]
        value, process, wait, kill = self.child_fixture(anchors, [(0, 0), (4321, 0)])
        self.assertEqual(value["pid"], 4321)
        self.assertEqual(value["timing"]["launch_before"], anchors[0])
        self.assertEqual(value["timing"]["launch_returned"], anchors[1])
        self.assertEqual(value["timing"]["last_nonterminal_wait_before"], anchors[2])
        self.assertEqual(value["timing"]["terminal_wait_before"], anchors[3])
        self.assertEqual(value["timing"]["terminal_wait_after"], anchors[4])
        self.assertEqual(value["wall_ms"], 402 / 1e6)
        self.assertEqual(value["cpu_ms"], 3)
        self.assertEqual(process.returncode, 0)
        self.assertEqual(wait.call_count, 2)
        kill.assert_not_called()

    def test_immediate_reap_has_no_invented_prior_running_observation(self):
        value, _, _, _ = self.child_fixture([self.anchor(n) for n in (100, 200, 300, 400)], [(4321, 0)])
        self.assertIsNone(value["timing"]["last_nonterminal_wait_before"])

    def test_timeout_still_kills_and_reaps_only_owned_child(self):
        _, process, wait, kill = self.child_fixture([self.anchor(n) for n in (100, 200, 300)],
                                                   [(0, 0), (4321, 9)], timeout_ns=2_000_000_000,
                                                   expect_error=True)
        kill.assert_called_once_with(4321, native.signal.SIGKILL)
        self.assertEqual(wait.call_count, 2)
        self.assertEqual(process.returncode, -9)

    def test_clock_failure_after_launch_does_not_leak_child(self):
        _, _, _, kill = self.child_fixture([self.anchor(100), ValueError("synthetic clock failure")],
                                           [(4321, 9)], expect_error=True)
        kill.assert_called_once_with(4321, native.signal.SIGKILL)

    def test_clock_failure_after_reap_never_signals_a_recycled_pid(self):
        _, process, wait, kill = self.child_fixture(
            [self.anchor(n) for n in (100, 200, 300)] + [ValueError("synthetic clock failure")],
            [(4321, 0)], expect_error=True)
        self.assertEqual(process.returncode, 0)
        wait.assert_called_once()
        kill.assert_not_called()

    def test_missing_or_unsafe_run_id_rejected_before_readback(self):
        for run_id in (None, False, 1, "", "../foreign", "x" * 65):
            with mock.patch.object(native.socket, "gethostname", return_value="test"), \
                    mock.patch.object(native, "boot_id") as read:
                with self.assertRaises(ValueError):
                    native.compare({"runtime_target_class": "LAB", "expected_hostname": "test",
                                    **({"run_id": run_id} if run_id is not None else {})})
                read.assert_not_called()

    def test_comparison_binds_run_legs_and_unknown_host_clock_without_execution(self):
        manifest = b"synthetic installed manifest"
        config = {"runtime_target_class": "SINGLE_NODE", "expected_hostname": "synthetic",
                  "run_id": "synthetic-window", "root": "/work/tmp/project-sentinel/synthetic-not-read",
                  "manifest_sha256": hashlib.sha256(manifest).hexdigest()}
        for name in ("baseline", "candidate", "image", "metadata"):
            config[name] = {"file": name, "sha256": "a" * 64}
        original_read = Path.read_bytes
        def read(path):
            return manifest if str(path) == "/opt/sentinel/release-manifest.json" else original_read(path)
        counter = {"monotonic_ns": 150, "collection_timing":
                   {"before": self.anchor(100), "after": self.anchor(900)}}
        leg = {"passes": [{"elapsed_ms": 1}, {"elapsed_ms": 2}, {"elapsed_ms": 3}],
               "timing": {"launch_before": self.anchor(200), "terminal_wait_after": self.anchor(800)}}
        with mock.patch.object(native.socket, "gethostname", return_value="synthetic"), \
                mock.patch.object(native, "boot_id", return_value="00000000-0000-0000-0000-000000000001"), \
                mock.patch.object(native, "service_identity", return_value={}), \
                mock.patch.object(Path, "read_bytes", read), \
                mock.patch.object(native, "checked_file", side_effect=lambda root, ref, limit: root / ref["file"]) as checked, \
                mock.patch.object(native, "resource_sample", return_value=counter), \
                mock.patch.object(native, "child", return_value=leg) as child:
            result = native.compare(config)
        self.assertEqual(result["run_id"], "synthetic-window")
        self.assertEqual([r["leg_index"] for r in result["trials"]], [0, 1, 2, 3])
        self.assertEqual([r["block_index"] for r in result["trials"]], [0, 0, 0, 0])
        self.assertEqual(result["order"], ["baseline", "candidate", "candidate", "baseline"])
        self.assertEqual(result["clock_alignment"]["status"], "UNVERIFIED_WITHOUT_EXTERNAL_CLOCK_BOUND")
        self.assertIsNone(result["clock_alignment"]["cross_host_offset_bound_ns"])
        self.assertEqual(result["clock_alignment"]["balloon_observation"], "HYPERVISOR_ENDPOINTS_ONLY_NOT_CONTINUOUS")
        self.assertEqual(set(result["collector_sha256"]), {"remote_parallel_native.py", "remote_parallel_probe.py"})
        self.assertGreaterEqual(len(result["sidecars"]), 2)
        self.assertEqual(child.call_count, 4)
        self.assertEqual(sum(call.args[1]["file"] == "metadata" for call in checked.call_args_list), 9)


def measurement():
    return {"kind": "release_benchmark", "release_stable": True, "manifest_sha256": "a" * 64,
            "runtime_target_class": "LAB", "host_fingerprint": "test-slot", "hardware_digest": "b" * 64,
            "dataset_digest": "c" * 64, "workload_digest": "d" * 64, "cache_state": "process_cold",
            "balloon_bytes": 2 * 1024 ** 3, "concurrency": 1, "background_load_id": "test-idle",
            "build_profile": "release", "sample_kind": "validator",
            "sidecars": {"cpu": [1, 2], "ram": [1, 2], "io": [1, 2], "cache": [1, 2],
                         "balloon": [2 * 1024 ** 3] * 2, "concurrent_load": [1, 2]},
            "samples": [{"ok": True, "internal_ms": n} for n in range(100)]}


class BenchmarkTests(unittest.TestCase):
    def test_same_environment_and_distinct_artifact_allowed(self):
        base, candidate = measurement(), measurement()
        candidate["manifest_sha256"] = "e" * 64
        result = report.benchmark_pair(base, candidate)
        self.assertEqual(result["status"], "MATCHED_MEASUREMENTS")
        self.assertEqual(result["metrics"]["provider_ms"]["candidate"]["n"], 0)

    def test_each_environment_mismatch_rejected(self):
        for key in report.MATCH_FIELDS:
            base, candidate = measurement(), measurement()
            candidate[key] = "different"
            with self.subTest(key=key), self.assertRaises(ValueError):
                report.benchmark_pair(base, candidate)

    def test_missing_unknown_sidecar_balloon_and_unfrozen_rejected(self):
        for change in (lambda r: r.update(kind="read_only_observation"),
                       lambda r: r.update(release_stable=False),
                       lambda r: r.update(samples=r["samples"][:99]),
                       lambda r: r["samples"][0].update(ok=False),
                       lambda r: r["sidecars"].pop("io"),
                       lambda r: r["sidecars"].update(balloon=[1, 2]),
                       lambda r: r.update(cache_state="uncontrolled_observed_only")):
            candidate = measurement()
            change(candidate)
            with self.assertRaises(ValueError):
                report.benchmark_pair(measurement(), candidate)

    def test_provider_units_kept_and_total_not_summed(self):
        rows = [{"request_id": "test-request", "phase": p, "unit": u, "elapsed": t, "accepted": True}
                for p, u, t in (("http", "us", 1500), ("persistence", "ms", 2), ("total", "ms", 9))]
        value = report.provider_phases(rows)
        self.assertEqual(value["phases"]["http"]["p50_ms"], 1.5)
        self.assertEqual(value["phases"]["total"]["p50_ms"], 9)
        self.assertEqual(value["missing_phase_counts"]["cleanup"], 1)

    def test_duplicate_or_unitless_provider_phase_rejected(self):
        row = {"request_id": "test", "phase": "http", "elapsed": 1, "unit": "ms", "accepted": True}
        with self.assertRaises(ValueError):
            report.provider_phases([row, row])
        row.pop("unit")
        with self.assertRaises(ValueError):
            report.provider_phases([row])

    def test_empty_phase_window_is_not_measured(self):
        value = report.phase_observations({"status": "OBSERVED", "release_stable": True,
                                          "phase_observation": {"records": [], "possibly_truncated": False}})
        self.assertEqual(value["status"], "NOT_MEASURED")

    def test_truncated_phase_window_rejected(self):
        with self.assertRaises(ValueError):
            report.phase_observations({"status": "OBSERVED", "release_stable": True,
                                       "phase_observation": {"records": [], "possibly_truncated": True}})


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get("RUNNER_TEMP", "/work/tmp/project-sentinel"),
                                                prefix="remote-acceptance-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def reference(self, name):
        path = self.root / name
        data = json.dumps({"synthetic_fixture": name}).encode()
        path.write_bytes(data)
        return {"file": name, "sha256": hashlib.sha256(data).hexdigest()}

    def bundle(self):
        bundle = {"schema": 1, "mode": "fixture", "project_id": "synthetic-project",
                  "developer_id": 1, "qa_id": 2, "qa_registration": self.reference("qa-registration"), "records": []}
        for index, stage in enumerate(report.STAGES):
            bundle["records"].append({"stage": stage, "sequence": index, "revision": 0 if index < 2 else 1,
                                      "project_id": bundle["project_id"], "artifact_sha256": ("a" if index < 2 else "b") * 64,
                                      "actor_id": 2 if stage.startswith("qa_") else 1,
                                      "evidence": self.reference(stage)})
        bundle["records"][1]["report_digest"] = "c" * 64
        bundle["records"][2]["qa_report_digest"] = "c" * 64
        bundle["records"][4]["authority"] = "release_manager"
        bundle["records"][5]["release_digest"] = bundle["records"][4]["evidence"]["sha256"]
        return bundle

    def test_fixture_chain_stays_fixture(self):
        value = report.pilot(self.bundle(), self.root)
        self.assertEqual(value["status"], "FIXTURE_ONLY")
        self.assertEqual(value["product_acceptance"], "REQUIRES_INDEPENDENT_RUNTIME_REVIEW")

    def test_fixture_cannot_be_relabeled_genuine(self):
        bundle = self.bundle()
        bundle["mode"] = "genuine_internal"
        with self.assertRaises(ValueError):
            report.pilot(bundle, self.root)

    def test_complete_chain_checks_bindings_but_never_authenticates_exporter(self):
        bundle = self.bundle()
        bundle["mode"] = "genuine_internal"
        for row in bundle["records"][:4]:
            row.update(decision_origin="provider_completion", request_id="test-" + row["stage"],
                       provider_completion=self.reference("provider-" + row["stage"]))
        value = report.pilot(bundle, self.root)
        self.assertEqual(value["status"], "EVIDENCE_CHAIN_CHECKED")
        self.assertNotEqual(value["product_acceptance"], "PASS")

    def test_one_provider_request_cannot_be_used_for_two_decisions(self):
        bundle = self.bundle()
        bundle["mode"] = "genuine_internal"
        for row in bundle["records"][:4]:
            row.update(decision_origin="provider_completion", request_id="same-test-request",
                       provider_completion=self.reference("provider-" + row["stage"]))
        with self.assertRaises(ValueError):
            report.pilot(bundle, self.root)

    def test_adversarial_pilot_bindings(self):
        bundle = self.bundle()
        changes = (lambda b: b.update(qa_id=1),
                   lambda b: b["records"].pop(),
                   lambda b: b["records"][2].update(revision=0),
                   lambda b: b["records"][2].update(qa_report_digest="d" * 64),
                   lambda b: b["records"][3].update(artifact_sha256="d" * 64),
                   lambda b: b["records"][1].update(actor_id=1),
                   lambda b: b["records"][1].update(sequence=0),
                   lambda b: b["records"][1].update(project_id="other"),
                   lambda b: b["records"][4].update(authority="developer"),
                   lambda b: b["records"][5].update(release_digest="f" * 64))
        for change in changes:
            changed = copy.deepcopy(bundle)
            change(changed)
            with self.assertRaises(ValueError):
                report.pilot(changed, self.root)

    def test_external_customer_requires_separate_agreement_acceptance(self):
        bundle = self.bundle()
        bundle["mode"] = "external_customer"
        for row in bundle["records"][:4]:
            row.update(decision_origin="provider_completion", request_id="test-" + row["stage"],
                       provider_completion=self.reference("provider-" + row["stage"]))
        with self.assertRaises(KeyError):
            report.pilot(bundle, self.root)

    def test_changed_or_escaping_evidence_rejected(self):
        ref = self.reference("evidence")
        (self.root / "evidence").write_text("changed")
        for bad in (ref, {"file": "../outside", "sha256": "a" * 64},
                    {"file": "/etc/passwd", "sha256": "a" * 64}):
            with self.assertRaises(ValueError):
                report.evidence(self.root, bad)

    def blocks(self):
        return {"blocks": [{"id": str(n), "accepted": True, "mode": "genuine_product",
                            "acceptance": self.reference("accepted-" + str(n)),
                            "baseline_evidence": self.reference("baseline-" + str(n)),
                            "comparison_key": "same-workload", "baseline_comparison_key": "same-workload",
                            "started_ms": 100, "accepted_ms": 200, "baseline_elapsed_ms": 200,
                            "spans": [{"category": "build", "start_ms": 100, "end_ms": 150},
                                      {"category": "test", "start_ms": 125, "end_ms": 175}],
                            "not_applicable": ["wait", "integration", "deploy", "rework"]} for n in range(3)]}

    def test_three_blocks_count_elapsed_without_double_counting(self):
        result = report.delivery_cycles(self.blocks(), self.root)
        self.assertEqual(result["elapsed_speedup"], 2)
        self.assertFalse(result["fivefold_target_met_in_supplied_data"])
        self.assertEqual(result["blocks"][0]["accounted_union_ms"], 75)

    def test_missing_unaccepted_duplicate_blocks_rejected(self):
        for change in (lambda b: b["blocks"].pop(),
                       lambda b: b["blocks"][0].update(accepted=False),
                       lambda b: b["blocks"][0].update(mode="infrastructure"),
                       lambda b: b["blocks"][0].update(id="1"),
                       lambda b: b["blocks"][0].update(baseline_comparison_key="other"),
                       lambda b: b["blocks"][0]["spans"][0].update(end_ms=300)):
            blocks = self.blocks()
            change(blocks)
            with self.assertRaises(ValueError):
                report.delivery_cycles(blocks, self.root)


if __name__ == "__main__":
    unittest.main()
