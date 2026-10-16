#!/usr/bin/env python3
"""Pure/static lifecycle tests; these do not mutate a hypervisor."""
import importlib.util
import copy
import json
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("control", Path(__file__).with_name("control.py"))
c = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(c)


def config():
    return {"owner": c.OWNER, "runtime_target_class": "LAB", "reserve_mib": 6144,
            "max_active": 3, "template": 8600, "slots": [8601, 8602],
            "managed_growth": {"builder": {"vmid": 9101, "allowance_mib": 1024},
                               "agent": {"vmid": 9102, "allowance_mib": 512}},
            "network": "192.0.2.0/24", "gateway": "192.0.2.1",
            "addresses": {"8600": "192.0.2.10", "8601": "192.0.2.11", "8602": "192.0.2.12"},
            "bridge": "labbr0", "pool": "sentinel-ci", "storage": "sentinel-ci-zfs", "node": "lab-host"}


class AdmissionTests(unittest.TestCase):
    def test_exact_reserve_boundary(self):
        self.assertTrue(c.admission(9216, 1, 2048, 1024, config())["allowed"])
        self.assertFalse(c.admission(9215, 1, 2048, 1024, config())["allowed"])

    def test_three_active_is_limit(self):
        self.assertFalse(c.admission(50000, 3, 2048, 1024, config())["allowed"])

    def test_full_request_not_balloon_minimum(self):
        self.assertFalse(c.admission(12000, 0, 8192, 1024, config())["allowed"])

    def test_bad_memory(self):
        for value in (0, 2047, 8193):
            with self.assertRaises(ValueError):
                c.admission(50000, 0, value, 0, config())

    def test_invalid_configuration(self):
        for change in ({"owner": "foreign"}, {"runtime_target_class": "SINGLE_NODE"},
                       {"reserve_mib": 6000}, {"max_active": 6}, {"template": 8601},
                       {"slots": [8601, 8601]}, {"bridge": "br0;reboot"},
                       {"gateway": "198.51.100.1"}, {"gateway": "192.0.2.255"},
                       {"addresses": {"8601": "192.0.2.11"}}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                c.validate_config({**config(), **change})


class ArpTests(unittest.TestCase):
    own = bytes.fromhex("020000000001")
    other = bytes.fromhex("020000000002")

    def frame(self, sender="192.0.2.11", target="192.0.2.1", mac=None):
        mac = mac or self.other
        return (b"\xff" * 6 + mac + b"\x08\x06" + struct.pack("!HHBBH", 1, 0x800, 6, 4, 1)
                + mac + socket.inet_aton(sender) + b"\0" * 6 + socket.inet_aton(target))

    def test_existing_owner(self):
        self.assertTrue(c.arp_conflict(self.frame(), "192.0.2.11", self.own))

    def test_simultaneous_probe(self):
        self.assertTrue(c.arp_conflict(self.frame("0.0.0.0", "192.0.2.11"), "192.0.2.11", self.own))

    def test_own_probe_ignored(self):
        self.assertFalse(c.arp_conflict(self.frame("0.0.0.0", "192.0.2.11", self.own), "192.0.2.11", self.own))

    def test_unrelated_or_truncated_ignored(self):
        self.assertFalse(c.arp_conflict(self.frame("192.0.2.12"), "192.0.2.11", self.own))
        self.assertFalse(c.arp_conflict(b"bad", "192.0.2.11", self.own))


class ProcessTelemetryTests(unittest.TestCase):
    def test_actual_residency_and_process_identity(self):
        root = unittest.mock.MagicMock()
        stat = "1 (qemu with spaces) S " + "0 " * 18 + "345 0"
        root.__truediv__.return_value.read_text.side_effect = [stat, "Rss: 2048 kB\nPss: 1900 kB\nSwap: 0 kB\n", stat]
        with patch.object(c, "Path", return_value=root):
            result = c.process_memory(123)
        self.assertEqual(result, {"pid": 123, "start_ticks": "345", "rss_bytes": 2048 * 1024,
                                  "pss_bytes": 1900 * 1024, "swap_bytes": 0})

    def test_pid_reuse_or_incomplete_residency_fails_closed(self):
        stat = "1 (qemu) S " + "0 " * 18 + "345 0"
        for rollup, after in (("Rss: 2048 kB\n", stat),
                              ("Rss: 2048 kB\nPss: 1900 kB\nSwap: 0 kB\n", stat.replace("345", "346"))):
            root = unittest.mock.MagicMock()
            root.__truediv__.return_value.read_text.side_effect = [stat, rollup, after]
            with patch.object(c, "Path", return_value=root), self.assertRaises(RuntimeError):
                c.process_memory(123)


class AddressInventoryTests(unittest.TestCase):
    def test_pve_comment_owner_uses_structured_exact_reservation(self):
        with tempfile.TemporaryDirectory(dir="/work/tmp/project-sentinel") as directory:
            root = Path(directory)
            path = root / "etc/pve/nodes/lab-host/qemu-server/8601.conf"
            path.parent.mkdir(parents=True)
            path.write_text("#sentinel-ci-v1\nipconfig0: ip=192.0.2.11/24,gw=192.0.2.1\n")
            reservation = {"description": c.OWNER, "ipconfig0": "ip=192.0.2.11/24,gw=192.0.2.1"}
            for change, valid in (({}, True), ({"description": "foreign"}, False),
                                  ({"ipconfig0": "ip=192.0.2.12/24,gw=192.0.2.1"}, False)):
                with patch.object(c, "Path", side_effect=lambda value: root / str(value).lstrip("/")), \
                        patch.object(c, "api", return_value={**reservation, **change}), \
                        patch.object(c, "run", return_value="[]"), patch.object(c, "arp_dad", return_value={"conflict": False}) as dad:
                    if valid:
                        c.address_inventory(config(), "192.0.2.11", 8601)
                        dad.assert_called_once_with("labbr0", "192.0.2.11")
                    else:
                        with self.assertRaises(RuntimeError):
                            c.address_inventory(config(), "192.0.2.11", 8601)
                        dad.assert_not_called()

    def test_same_basename_other_node_and_other_reservations_still_conflict(self):
        for relative in ("etc/pve/nodes/other-node/qemu-server/8601.conf", "etc/network/other-reservation"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory(dir="/work/tmp/project-sentinel") as directory:
                root = Path(directory)
                path = root / relative
                path.parent.mkdir(parents=True)
                path.write_text("#sentinel-ci-v1\nipconfig0: ip=192.0.2.11/24,gw=192.0.2.1\n")
                with patch.object(c, "Path", side_effect=lambda value: root / str(value).lstrip("/")), \
                        patch.object(c, "api") as query, patch.object(c, "run", return_value="[]"), \
                        patch.object(c, "arp_dad") as dad:
                    with self.assertRaises(RuntimeError):
                        c.address_inventory(config(), "192.0.2.11", 8601)
                    query.assert_not_called()
                    dad.assert_not_called()


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir="/work/tmp/project-sentinel")
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "state.json"
        self.lab = c.Lab(config(), self.path)
        self.sample = {"timestamp": time.time(), "available_mib": 30000,
                       "memory_psi": {"some": {"avg10": 0.0}, "full": {"avg10": 0.0}},
                       "managed": {name: {"vmid": value["vmid"], "pid": value["vmid"], "start_ticks": "1",
                                          "rss_bytes": 4096 * c.MIB, "pss_bytes": 4000 * c.MIB}
                                   for name, value in config()["managed_growth"].items()}}
        self.lab.state["capacity_sample"] = json.loads(json.dumps(self.sample))
        sample_patch = patch.object(self.lab, "capacity_sample", side_effect=lambda: self.sample)
        sample_patch.start()
        self.addCleanup(sample_patch.stop)
        process_patch = patch.object(c, "process_memory", return_value={"rss_bytes": 1024 * c.MIB})
        process_patch.start()
        self.addCleanup(process_patch.stop)

    def test_foreign_state_rejected(self):
        self.path.write_text('{"owner":"foreign"}')
        with self.assertRaises(RuntimeError):
            c.Lab(config(), self.path)

    def test_lease_survives_restart_and_blocks_second_holder(self):
        with patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "status", return_value={"status": "stopped"}):
            self.lab.lease(8601, "archive-job-1", 1800)
            with self.assertRaises(RuntimeError):
                self.lab.lease(8601, "archive-job-2", 1800)
        reloaded = c.Lab(config(), self.path)
        reloaded.require_lease(8601, "archive-job-1")
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)

    def test_start_requires_lease_before_effect(self):
        with patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.start(8601, "unknown", 2048)
            command.assert_not_called()

    def test_capacity_denial_never_starts_vm(self):
        self.lab.state["leases"]["8601"] = {"generation": "gen", "holder": "job"}
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "resources", return_value={"allowed": False}), \
                patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.start(8601, "job", 2048)
            command.assert_not_called()

    def test_running_start_is_idempotent(self):
        self.lab.state["leases"]["8601"] = {"generation": "gen", "holder": "job"}
        self.lab.state["runs"] = {"8601": {"generation": "gen", "holder": "job", "vmgenid": "vm-id", "phase": "starting"}}
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "status", return_value={"status": "running"}), \
                patch.object(self.lab, "owned", return_value={"vmgenid": "vm-id"}), \
                patch.object(c, "run") as command:
            self.assertEqual(self.lab.start(8601, "job", 2048), {"already_running": True})
            command.assert_not_called()

    def test_admitted_start_persists_window_baseline_before_effect(self):
        self.lab.config["network_check"] = ["check-network"]
        self.lab.state["leases"]["8601"] = {"holder": "job", "generation": "gen", "expires": 9999999999}
        decision = {"allowed": True, "capacity_sample": self.sample}
        def command(*args):
            if args[:2] == ("qm", "start"):
                saved = json.loads(self.path.read_text())
                self.assertEqual(saved["runs"]["8601"]["phase"], "starting")
                self.assertEqual(saved["capacity_baseline"], self.sample)
                self.assertEqual(saved["capacity_sample"], self.sample)
                self.assertEqual(saved["balloon_requests"]["8601"], 2048)
        with patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(self.lab, "owned", return_value={"vmgenid": "own-id"}), \
                patch.object(self.lab, "resources", return_value=decision) as capacity, \
                patch.object(c, "address_inventory"), patch.object(c, "run", side_effect=command):
            self.lab.start(8601, "job", 2048)
        self.assertEqual(capacity.call_count, 2)
        self.assertEqual(self.lab.state["runs"]["8601"]["phase"], "running")

    def test_capacity_loss_during_dad_cannot_boot(self):
        self.lab.config["network_check"] = ["check-network"]
        self.lab.state["leases"]["8601"] = {"holder": "job", "generation": "gen", "expires": 9999999999}
        with patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(self.lab, "owned", return_value={}), patch.object(c, "address_inventory"), \
                patch.object(self.lab, "resources", side_effect=[{"allowed": True}, {"allowed": False}]), \
                patch.object(c, "run") as command:
            with self.assertRaises(c.CapacityUnavailable):
                self.lab.start(8601, "job", 2048)
            command.assert_called_once_with("check-network")

    def test_accepted_start_refreshes_guard_clock_not_window_baseline(self):
        self.lab.config["network_check"] = ["check-network"]
        self.lab.state["leases"]["8601"] = {"holder": "job", "generation": "gen", "expires": 9999999999}
        original = json.loads(json.dumps(self.sample))
        original["timestamp"] = 0
        self.lab.state["capacity_sample"] = original
        self.lab.state["capacity_baseline"] = original
        accepted = {**self.sample, "timestamp": 40}
        with patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(self.lab, "owned", return_value={}), patch.object(c, "address_inventory"), \
                patch.object(self.lab, "resources", return_value={"allowed": True, "capacity_sample": accepted}), \
                patch.object(c, "run"):
            self.lab.start(8601, "job", 2048)
        self.assertEqual(self.lab.state["capacity_baseline"], original)
        self.assertEqual(self.lab.capacity_safety({**accepted, "timestamp": 60})["reasons"], [])

    def test_rejected_start_or_resize_cannot_refresh_guard_clock(self):
        original = json.loads(json.dumps(self.lab.state["capacity_sample"]))
        self.lab.config["network_check"] = ["check-network"]
        self.lab.state["leases"]["8601"] = {"holder": "job", "generation": "gen", "expires": 9999999999}
        denied = {"allowed": False, "capacity_sample": {**self.sample, "timestamp": 999999},
                  "available_mib": 8000, "reserved_growth_mib": 1536}
        with patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "resources", return_value=denied):
            with self.assertRaises(c.CapacityUnavailable):
                self.lab.start(8601, "job", 2048)
        with patch.object(self.lab, "status", return_value={"status": "running", "balloon": 2048 * c.MIB}), \
                patch.object(self.lab, "resources", return_value=denied):
            with self.assertRaises(c.CapacityUnavailable):
                self.lab.resize(8601, "job", 8192)
        self.assertEqual(self.lab.state["capacity_sample"], original)

    def test_accepted_resize_refreshes_clock_but_pressure_shrink_does_not(self):
        self.lab.state["leases"]["8601"] = {"holder": "job", "expires": 9999999999}
        original = json.loads(json.dumps(self.sample))
        accepted = {**self.sample, "timestamp": self.sample["timestamp"] + 10}
        with patch.object(self.lab, "status", return_value={"status": "running", "balloon": 2048 * c.MIB}), \
                patch.object(self.lab, "resources", return_value={"available_mib": 30000, "reserved_growth_mib": 1536,
                                                                 "capacity_sample": accepted}), patch.object(c, "run"):
            self.lab.resize(8601, "job", 3072)
        self.assertEqual(self.lab.state["capacity_sample"], accepted)
        with patch.object(self.lab, "status", return_value={"status": "running", "balloon": 3072 * c.MIB}), \
                patch.object(self.lab, "resources", return_value={"available_mib": 5000, "reserved_growth_mib": 1536,
                                                                 "capacity_sample": original}), patch.object(c, "run"):
            self.lab.resize(8601, "job", 2048)
        self.assertEqual(self.lab.state["capacity_sample"], accepted)

    def test_telemetry_is_read_only_and_carries_actual_process_and_lease(self):
        guest = {"vmid": 8601, "node": "lab-host", "type": "qemu", "status": "running"}
        self.lab.state["leases"]["8601"] = {"holder": "measurement", "generation": "g", "expires": 9999999999}
        self.lab.state["balloon_requests"] = {"8601": 3072}
        data = {"pid": 123, "balloon": 2048 * c.MIB, "balloon_min": 3072 * c.MIB, "cpu": 0.5}
        with patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else data), \
                patch.object(self.lab, "save") as save:
            result = self.lab.telemetry()
            save.assert_not_called()
        self.assertEqual(result["schema"], 2)
        self.assertEqual(result["guests"][0]["process"]["rss_bytes"], 1024 * c.MIB)
        self.assertEqual(result["guests"][0]["pending_target_mib"], 3072)
        self.assertEqual(result["guests"][0]["lease"]["generation"], "g")

    def test_unknown_snapshot_cannot_rollback(self):
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "owned"), patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.rollback(8601, "job", "ci-foreign")
            command.assert_not_called()

    def test_stop_is_graceful_and_cannot_force_kill(self):
        with patch.object(self.lab, "status", side_effect=[{"status": "running"}, {"status": "stopped"}]), patch.object(c, "run") as command:
            self.lab.stop(8601)
            command.assert_called_once_with("qm", "shutdown", "8601", "--timeout", "90", "--forceStop", "0")

    def test_live_lease_and_preserved_snapshot_survive_reap(self):
        self.lab.state["leases"]["8601"] = {"holder": "job", "expires": 2000}
        self.lab.state["last_used"] = {"8601": 0, "8602": 0}
        self.lab.state["snapshots"]["8602:ci-rollback"] = {
            "vmid": 8602, "name": "ci-rollback", "preserve": True, "expires": 0, "state": "ready"}
        with patch.object(c.time, "time", return_value=1000), patch.object(self.lab, "stop") as stop, \
                patch.object(c, "api", return_value=[{"vmid": 8602, "node": "lab-host", "status": "running"}]), \
                patch.object(self.lab, "status", return_value={"status": "running"}), \
                patch.object(c, "run") as command:
            result = self.lab.reap()
            stop.assert_called_once_with(8602)
            self.assertEqual(result["expired_snapshots_removed"], [])
            command.assert_not_called()

    def test_snapshot_record_is_durable_before_effect(self):
        external = []
        def command(*args):
            state = json.loads(self.path.read_text())
            self.assertEqual(state["snapshots"]["8601:ci-test"]["state"], "creating")
            external.append({"name": "ci-test", "description": state["snapshots"]["8601:ci-test"]["effect_owner"]})
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "owned", return_value={}), \
                patch.object(self.lab, "stop"), patch.object(c, "run", side_effect=command), \
                patch.object(c, "api", side_effect=lambda path: external if path.endswith("/snapshot") else []):
            result = self.lab.snapshot(8601, "job", "ci-test", False)
        self.assertEqual(result["state"], "ready")

    def test_resize_denies_unavailable_growth(self):
        with patch.object(self.lab, "require_lease"), \
                patch.object(self.lab, "status", return_value={"status": "running", "balloon": 2048 * c.MIB}), \
                patch.object(self.lab, "resources", return_value={"available_mib": 8000, "reserved_growth_mib": 1536}), \
                patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.resize(8601, "job", 8192)
            command.assert_not_called()

    def test_resize_reduces_memory_even_under_pressure(self):
        with patch.object(self.lab, "require_lease"), \
                patch.object(self.lab, "status", return_value={"status": "running", "balloon": 4096 * c.MIB}), \
                patch.object(self.lab, "resources", return_value={"available_mib": 5000, "reserved_growth_mib": 1536}), \
                patch.object(c, "run") as command:
            self.lab.resize(8601, "job", 2048)
            command.assert_called_once_with("qm", "set", "8601", "--balloon", "2048", "--shares", "0")

    def test_unknown_balloon_cannot_be_resized(self):
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "status", return_value={"status": "running"}), \
                patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.resize(8601, "job", 2048)
            command.assert_not_called()

    def test_untracked_and_preserved_snapshots_cannot_be_discarded(self):
        self.lab.state["snapshots"]["8601:ci-keep"] = {"preserve": True}
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "owned"), patch.object(c, "run") as command:
            for name in ("ci-unknown", "ci-keep"):
                with self.assertRaises(RuntimeError):
                    self.lab.discard_snapshot(8601, "job", name)
            command.assert_not_called()

    def test_guard_never_stops_foreign_guests(self):
        self.sample["available_mib"] = 6000
        with patch.object(c, "available_mib", side_effect=[6000, 6000, 8000, 8000]), \
                patch.object(c, "api", return_value=[{"vmid": 9999, "status": "running"},
                                                   {"vmid": 8601, "status": "running"}]), \
                patch.object(self.lab, "status", return_value={"status": "running"}), \
                patch.object(self.lab, "owned") as owned, patch.object(self.lab, "stop") as stop:
            result = self.lab.guard()
            owned.assert_called_once_with(8601)
            stop.assert_called_once_with(8601)
            self.assertEqual(result["pressure_stopped"], [8601])

    def test_invalidated_lease_cannot_resume_as_same_job(self):
        self.lab.state["leases"]["8601"] = {"holder": "job", "expires": 9999999999}
        self.lab.state["invalidated_leases"] = [{"vmid": 8601, "lease": {"holder": "job"}}]
        with self.assertRaises(RuntimeError):
            self.lab.require_lease(8601, "job")

    def test_invalidated_job_can_release_only_after_stopped_and_new_job_gets_new_generation(self):
        self.lab.state["leases"]["8601"] = {"holder": "job", "generation": "old", "expires": 9999999999}
        self.lab.state["invalidated_leases"] = [{"vmid": 8601, "lease": {"holder": "job"}}]
        with patch.object(self.lab, "status", return_value={"status": "running"}):
            with self.assertRaises(RuntimeError):
                self.lab.release(8601, "job")
        with patch.object(self.lab, "status", return_value={"status": "stopped"}), patch.object(self.lab, "owned"):
            with self.assertRaises(RuntimeError):
                self.lab.release(8601, "foreign-job")
            self.lab.release(8601, "job")
            lease = self.lab.lease(8601, "new-job", 1800)
        self.assertNotEqual(lease["generation"], "old")
        self.lab.require_lease(8601, "new-job")
        self.assertEqual(len(self.lab.state["invalidated_leases"]), 1)

    def test_stale_cluster_running_view_uses_fresh_own_stopped_status(self):
        guest = {"vmid": 8601, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 8192 * c.MIB}
        self.lab.state["runs"] = {"8601": {"phase": "quiesced"}}
        with patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else {"status": "stopped"}), \
                patch.object(c, "available_mib", return_value=20000):
            result = self.lab.resources(2048)
        self.assertTrue(result["allowed"])
        self.assertEqual(result["active"], 0)
        self.assertEqual(result["outstanding_commitments_mib"], 0)

    def test_transient_missing_own_pid_is_bounded_capacity_wait_not_semantic_block(self):
        guest = {"vmid": 8601, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 8192 * c.MIB}
        status = {"status": "running", "balloon": 2048 * c.MIB, "memhost": 1024 * c.MIB}
        with patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else status), \
                patch.object(c, "process_memory", side_effect=RuntimeError("missing identity")):
            with self.assertRaises(c.CapacityUnavailable):
                self.lab.resources(2048)

    def test_inverse_stale_stopped_view_reserves_newly_running_owned_target(self):
        guest = {"vmid": 8601, "node": "lab-host", "type": "qemu", "status": "stopped", "maxmem": 8192 * c.MIB}
        self.lab.state["runs"] = {"8601": {"phase": "running"}}
        self.lab.state["balloon_requests"] = {"8601": 8192}
        status = {"status": "running", "balloon": 8192 * c.MIB, "memhost": 1024 * c.MIB}
        with patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else status), \
                patch.object(c, "available_mib", return_value=12000):
            result = self.lab.resources(2048)
        self.assertFalse(result["allowed"])
        self.assertEqual(result["active"], 1)
        self.assertEqual(result["outstanding_commitments_mib"], 7168)

    def test_missing_cluster_entry_retains_accepted_running_promise(self):
        self.lab.state["runs"] = {"8601": {"phase": "running"}}
        self.lab.state["balloon_requests"] = {"8601": 8192}
        with patch.object(c, "api", return_value=[]), patch.object(c, "available_mib", return_value=12000):
            result = self.lab.resources(2048)
        self.assertFalse(result["allowed"])
        self.assertEqual(result["outstanding_commitments_mib"], 8192)

    def test_nine_cell_lifecycle_transition_table(self):
        self.lab.config["network_check"] = ["check-network"]
        for phase in ("stopped", "starting", "running"):
            for lease_state in ("active", "expired", "invalidated"):
                with self.subTest(phase=phase, lease=lease_state):
                    base = {"owner": c.OWNER, "leases": {"8601": {
                        "holder": "old-job", "generation": "old-gen", "expires": 0 if lease_state == "expired" else 9999999999}},
                        "runs": {"8601": {"holder": "old-job", "generation": "old-gen", "vmgenid": "vm", "phase": "quiesced" if phase == "stopped" else phase}},
                        "balloon_requests": {} if phase == "stopped" else {"8601": 4096}, "last_used": {},
                        "snapshots": {"investigation": {"preserve": True}},
                        "invalidated_leases": [{"vmid": 8601, "lease": {"holder": "old-job"}}] if lease_state == "invalidated" else [],
                        "capacity_sample": self.sample}
                    actual = "running" if phase == "running" else "stopped"
                    tasks = [{"id": "8601", "type": "qmstart"}] if phase == "starting" else []
                    with patch.object(self.lab, "status", return_value={"status": actual}), \
                            patch.object(self.lab, "owned", return_value={"vmgenid": "vm"}), \
                            patch.object(c, "api", return_value=tasks), patch.object(c, "address_inventory"), \
                            patch.object(self.lab, "resources", return_value={"allowed": True, "capacity_sample": self.sample}), \
                            patch.object(c, "run") as command:
                        self.lab.state = copy.deepcopy(base)
                        if lease_state == "active" and phase != "starting":
                            self.lab.start(8601, "old-job", 2048)
                        else:
                            with self.assertRaises(RuntimeError):
                                self.lab.start(8601, "old-job", 2048)
                            command.assert_not_called()
                        self.lab.state = copy.deepcopy(base)
                        if phase == "stopped" and lease_state != "expired":
                            self.lab.release(8601, "old-job")
                        else:
                            with self.assertRaises(RuntimeError):
                                self.lab.release(8601, "old-job")
                            self.assertEqual(self.lab.state["balloon_requests"], base["balloon_requests"])
                        self.assertEqual(self.lab.state["snapshots"], base["snapshots"])
                        self.lab.state = copy.deepcopy(base)
                        if phase == "stopped" and lease_state == "expired":
                            self.assertNotEqual(self.lab.lease(8601, "new-job", 1800)["generation"], "old-gen")
                            with self.assertRaises(RuntimeError):
                                self.lab.release(8601, "old-job")
                        else:
                            with self.assertRaises(RuntimeError):
                                self.lab.lease(8601, "new-job", 1800)
                            self.assertEqual(self.lab.state["runs"], base["runs"])
                    self.lab.state = copy.deepcopy(base)
                    unsafe = {**self.sample, "available_mib": 6143}
                    with patch.object(self.lab, "capacity_sample", return_value=unsafe), \
                            patch.object(c, "available_mib", return_value=6143), \
                            patch.object(c, "api", return_value=[{"vmid": 8601, "status": actual}]), \
                            patch.object(self.lab, "status", return_value={"status": actual}), \
                            patch.object(self.lab, "owned"), \
                            patch.object(self.lab, "stop", side_effect=c.CapacityUnavailable("in flight") if phase == "starting" else None) as stop:
                        self.lab.guard()
                        self.assertEqual(stop.call_count, int(phase != "stopped"))
                    self.assertEqual(self.lab.state["balloon_requests"], base["balloon_requests"])

    def test_interrupted_start_requires_unlocked_task_free_stopped_readback(self):
        self.lab.state["leases"]["8601"] = {"holder": "old", "generation": "g", "expires": 0}
        self.lab.state["runs"] = {"8601": {"phase": "starting"}}
        self.lab.state["balloon_requests"] = {"8601": 4096}
        with patch.object(self.lab, "owned", return_value={"lock": "create"}), \
                patch.object(self.lab, "status", return_value={"status": "stopped"}), patch.object(c, "api", return_value=[]):
            with self.assertRaises(c.CapacityUnavailable):
                self.lab.lease(8601, "new", 1800)
        self.assertEqual(self.lab.state["balloon_requests"]["8601"], 4096)
        with patch.object(self.lab, "owned", return_value={}), \
                patch.object(self.lab, "status", side_effect=[{"status": "stopped"}, {"status": "running"}]), \
                patch.object(c, "api", return_value=[]):
            with self.assertRaises(c.CapacityUnavailable):
                self.lab.lease(8601, "new", 1800)
        with patch.object(self.lab, "owned", return_value={}), \
                patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(c, "api", return_value=[{"id": "8601", "type": "qmstart", "endtime": 1}]):
            self.lab.lease(8601, "new", 1800)
        self.assertNotIn("8601", self.lab.state["balloon_requests"])

    def test_guard_and_reap_find_running_owned_work_despite_stopped_or_missing_cluster_entry(self):
        for hint in ([], [{"vmid": 8601, "status": "stopped"}]):
            for operation in ("guard", "reap"):
                with self.subTest(hint=hint, operation=operation):
                    self.lab.state["runs"] = {"8601": {"phase": "running"}}
                    self.lab.state["balloon_requests"] = {"8601": 4096}
                    self.lab.state["last_used"]["8601"] = 0
                    self.lab.state["capacity_baseline"] = self.sample
                    self.lab.state["leases"]["8601"] = {"holder": "job", "expires": 9999999999 if operation == "guard" else 0}
                    unsafe = {**self.sample, "available_mib": 6143}
                    with patch.object(c, "api", return_value=hint), \
                            patch.object(self.lab, "status", return_value={"status": "running"}) as status, \
                            patch.object(self.lab, "capacity_sample", return_value=unsafe), \
                            patch.object(c, "available_mib", return_value=6143), \
                            patch.object(self.lab, "owned"), patch.object(self.lab, "stop") as stop:
                        getattr(self.lab, operation)()
                        status.assert_called_once_with(8601)
                        stop.assert_called_once_with(8601)

    def test_guard_cluster_running_actual_quiesced_does_not_stop_or_invalidate_idle_job(self):
        self.lab.state["runs"] = {"8601": {"phase": "quiesced"}}
        self.lab.state["capacity_baseline"] = self.sample
        with patch.object(c, "api", return_value=[{"vmid": 8601, "status": "running"}]), \
                patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(self.lab, "stop") as stop:
            self.lab.guard()
            stop.assert_not_called()
        self.assertNotIn("capacity_baseline", self.lab.state)
        self.assertEqual(self.lab.state.get("invalidated_leases", []), [])

    def test_unsafe_pending_task_invalidates_without_releasing_baseline_or_promise(self):
        self.lab.state["runs"] = {"8601": {"phase": "starting"}}
        self.lab.state["balloon_requests"] = {"8601": 4096}
        self.lab.state["capacity_baseline"] = copy.deepcopy(self.sample)
        self.lab.state["leases"]["8601"] = {"holder": "job", "generation": "g", "expires": 9999999999}
        baseline = copy.deepcopy(self.lab.state["capacity_baseline"])
        def api(path):
            return [{"id": "8601", "type": "qmstart"}] if path.endswith("/tasks") else []
        unsafe = {**self.sample, "available_mib": 6143}
        with patch.object(c, "api", side_effect=api), patch.object(self.lab, "owned", return_value={}), \
                patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(self.lab, "capacity_sample", return_value=unsafe), \
                patch.object(c, "available_mib", return_value=6143), patch.object(c, "run") as effect:
            result = self.lab.guard()
            self.assertEqual(result["pressure_stopped"], [])
            self.assertEqual(result["quiescence_pending"][0]["vmid"], 8601)
            effect.assert_not_called()
            self.lab.state["leases"]["8601"]["expires"] = 0
            self.lab.state["last_used"]["8601"] = 0
            self.assertEqual(self.lab.reap()["quiescence_pending"][0]["vmid"], 8601)
        self.assertEqual(self.lab.state["capacity_baseline"], baseline)
        self.assertEqual(self.lab.state["balloon_requests"]["8601"], 4096)
        self.assertEqual(self.lab.state["runs"]["8601"]["phase"], "starting")
        self.lab.state["leases"]["8601"]["expires"] = 9999999999
        with self.assertRaises(RuntimeError):
            self.lab.require_lease(8601, "job")
        # A late effect cannot become valid just because the host recovered
        # or the same holder renewed its lease expiration.
        self.lab.state["leases"]["8601"]["expires"] += 60
        with patch.object(c, "api", return_value=[]), patch.object(self.lab, "owned", return_value={}), \
                patch.object(self.lab, "status", return_value={"status": "running"}), \
                patch.object(self.lab, "stop") as stop:
            recovered = self.lab.guard()
            stop.assert_called_once_with(8601)
        self.assertEqual(recovered["pressure_stopped"], [8601])
        self.assertEqual(len(self.lab.state["invalidated_leases"]), 1)

    def test_unknown_owned_activity_preserves_baseline_and_pending_commitment(self):
        self.lab.state["runs"] = {"8601": {"phase": "starting"}}
        self.lab.state["balloon_requests"] = {"8601": 4096}
        self.lab.state["capacity_baseline"] = copy.deepcopy(self.sample)
        with patch.object(c, "api", return_value=[]), patch.object(self.lab, "status", return_value={"status": "unknown"}), \
                patch.object(self.lab, "stop") as stop:
            with self.assertRaises(c.CapacityUnavailable):
                self.lab.guard()
            stop.assert_not_called()
        self.assertEqual(self.lab.state["capacity_baseline"], self.sample)
        self.assertEqual(self.lab.state["balloon_requests"]["8601"], 4096)

    def test_expired_or_released_running_guest_cannot_transfer(self):
        for holder in ("old-job", "new-job"):
            self.lab.state["leases"]["8601"] = {"holder": "old-job", "expires": 0, "generation": "old"}
            with patch.object(self.lab, "owned", return_value={}), \
                    patch.object(self.lab, "status", return_value={"status": "running"}), patch.object(c, "run") as command:
                with self.assertRaises(RuntimeError):
                    self.lab.lease(8601, holder, 1800)
                command.assert_not_called()
                self.assertEqual(self.lab.state["leases"]["8601"]["generation"], "old")

    def test_unbound_running_guest_is_not_idempotent_success(self):
        self.lab.state["leases"]["8601"] = {"holder": "job", "generation": "new", "expires": 9999999999}
        self.lab.state["runs"] = {"8601": {"holder": "old-job", "generation": "old", "phase": "running"}}
        with patch.object(self.lab, "owned", return_value={}), \
                patch.object(self.lab, "status", return_value={"status": "running"}), patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.start(8601, "job", 2048)
            command.assert_not_called()

    def test_running_guest_cannot_release_lease(self):
        self.lab.state["leases"]["8601"] = {"holder": "job", "expires": 9999999999}
        with patch.object(self.lab, "status", return_value={"status": "running"}):
            with self.assertRaises(RuntimeError):
                self.lab.release(8601, "job")

    def test_nonresident_current_targets_are_reserved(self):
        guest = {"vmid": 8601, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 8192 * c.MIB}
        status = {"balloon": 8192 * c.MIB, "memhost": 1024 * c.MIB, "maxmem": 8192 * c.MIB}
        with patch.object(c, "available_mib", return_value=10000), \
                patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else status):
            result = self.lab.resources(2048)
        self.assertFalse(result["allowed"])
        self.assertEqual(result["outstanding_commitments_mib"], 7168)
        self.assertEqual(result["projected_available_mib"], 10000 - 7168 - 1536 - 2048)

    def test_delayed_balloon_and_multiple_guests_keep_commitments(self):
        guests = [{"vmid": v, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 8192 * c.MIB}
                  for v in (8601, 8602)]
        self.lab.state["balloon_requests"] = {"8601": 4096, "8602": 2048}
        statuses = {"8601": {"balloon": 2048 * c.MIB, "memhost": 2048 * c.MIB},
                    "8602": {"balloon": 4096 * c.MIB, "memhost": 3072 * c.MIB}}
        def api(path):
            return guests if path == "/cluster/resources" else statuses[path.split("/")[-3]]
        with patch.object(c, "available_mib", return_value=12000), patch.object(c, "api", side_effect=api), \
                patch.object(c, "process_memory", side_effect=[{"rss_bytes": 2048 * c.MIB}, {"rss_bytes": 3072 * c.MIB}]):
            result = self.lab.resources(2048)
        self.assertEqual(result["outstanding_commitments_mib"], 3072)
        self.assertFalse(result["allowed"])

    def test_missing_resident_telemetry_fails_closed(self):
        guest = {"vmid": 8601, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 8192 * c.MIB}
        with patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else {"balloon": 2048 * c.MIB}):
            with self.assertRaises(RuntimeError):
                self.lab.resources(2048)

    def test_foreign_potential_is_risk_not_blanket_reservation(self):
        guest = {"vmid": 9999, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 65536 * c.MIB,
                 "memhost": 1024 * c.MIB}
        status = {"balloon": 32768 * c.MIB, "memhost": 1024 * c.MIB}
        with patch.object(c, "available_mib", return_value=12000), \
                patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else status):
            result = self.lab.resources(2048)
        self.assertTrue(result["allowed"])
        self.assertEqual(result["external_potential_mib"], 64512)
        self.assertIsNone(result["balloons"][0]["actual_mib"])
        self.assertEqual(result["balloons"][0]["potential_basis"], "cluster-maxmem-not-current-target")
        self.assertEqual(result["reserved_growth_mib"], 1536)
        self.assertEqual(result["outstanding_commitments_mib"], 0)

    def test_idle_reaper_never_queries_already_stopped_slots(self):
        self.lab.state["last_used"] = {"8601": 0, "8602": 0}
        with patch.object(c, "api", return_value=[{"vmid": 8601, "node": "lab-host", "status": "stopped"}]) as query, \
                patch.object(self.lab, "stop") as stop:
            self.lab.reap()
            stop.assert_not_called()
            query.assert_called_once_with("/cluster/resources")

    def test_unmanaged_unknown_potential_is_not_an_admission_prerequisite(self):
        guest = {"vmid": 9999, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 65536 * c.MIB}
        with patch.object(c, "available_mib", return_value=12000), \
                patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else {}):
            result = self.lab.resources(2048)
        self.assertTrue(result["allowed"])
        self.assertIsNone(result["balloons"][0]["outstanding_mib"])

    def test_stopped_starting_intent_remains_reserved_except_its_own_retry(self):
        self.lab.state["runs"] = {"8601": {"phase": "starting"}}
        self.lab.state["balloon_requests"] = {"8601": 4096}
        with patch.object(c, "api", return_value=[]), patch.object(c, "available_mib", return_value=14000):
            self.assertEqual(self.lab.resources(2048)["outstanding_commitments_mib"], 4096)
            self.assertEqual(self.lab.resources(4096, 8601)["outstanding_commitments_mib"], 0)

    def test_managed_growth_and_psi_invalidate_only_owned_work(self):
        for cause in ("builder", "agent", "psi", "stale", "pid", "reserve", "missing"):
            with self.subTest(cause=cause):
                sample = json.loads(json.dumps(self.sample))
                if cause in {"builder", "agent"}:
                    sample["managed"][cause]["rss_bytes"] += (config()["managed_growth"][cause]["allowance_mib"] + 1) * c.MIB
                elif cause == "psi":
                    sample["memory_psi"]["full"]["avg10"] = 0.1
                elif cause == "stale":
                    sample["timestamp"] += 46
                elif cause == "pid":
                    sample["managed"]["agent"]["start_ticks"] = "changed"
                elif cause == "reserve":
                    sample["available_mib"] = 6143
                self.lab.state["leases"]["8601"] = {"holder": cause, "expires": 9999999999}
                with patch.object(self.lab, "capacity_sample", side_effect=RuntimeError("unavailable") if cause == "missing" else lambda: sample), \
                        patch.object(c, "available_mib", return_value=12000), \
                        patch.object(c, "api", return_value=[{"vmid": 9999, "status": "running"}, {"vmid": 8601, "status": "running"}]), \
                        patch.object(self.lab, "status", return_value={"status": "running"}), \
                        patch.object(self.lab, "owned") as owned, patch.object(self.lab, "stop") as stop:
                    result = self.lab.guard()
                    self.assertTrue(result["capacity_safety"]["reasons"])
                    stop.assert_called_once_with(8601)
                    owned.assert_called_once_with(8601)
                with self.assertRaises(RuntimeError):
                    self.lab.require_lease(8601, cause)

    def test_growth_uses_window_baseline_not_last_sample_or_pss(self):
        self.lab.state["capacity_baseline"] = json.loads(json.dumps(self.sample))
        self.lab.state["capacity_sample"]["managed"]["builder"]["rss_bytes"] += 1000 * c.MIB
        sample = json.loads(json.dumps(self.sample))
        sample["managed"]["builder"]["rss_bytes"] += 1025 * c.MIB
        self.assertIn("builder-growth-envelope-exceeded", self.lab.capacity_safety(sample)["reasons"])

    def test_fresh_healthy_growth_within_named_allowance_is_admissible(self):
        sample = json.loads(json.dumps(self.sample))
        sample["managed"]["builder"]["rss_bytes"] += 1024 * c.MIB
        sample["managed"]["agent"]["rss_bytes"] += 512 * c.MIB
        self.assertEqual(self.lab.capacity_safety(sample)["reasons"], [])

    def test_admission_refuses_stale_sample_even_with_free_memory(self):
        self.lab.state["capacity_sample"]["timestamp"] -= 46
        with patch.object(c, "api", return_value=[]), patch.object(c, "available_mib", return_value=30000):
            self.assertFalse(self.lab.resources(2048)["allowed"])

    def test_created_snapshot_recovers_without_duplicate_effect(self):
        self.lab.state["snapshots"]["8601:ci-crash"] = {
            "vmid": 8601, "name": "ci-crash", "state": "creating", "preserve": False,
            "expires": 9999999999, "effect_owner": "own-operation"}
        external = [{"name": "ci-crash", "description": "own-operation"}]
        with patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "require_lease"), \
                patch.object(c, "api", side_effect=lambda path: external if path.endswith("/snapshot") else []), \
                patch.object(c, "run") as command, patch.object(self.lab, "stop"):
            self.assertEqual(self.lab.snapshot(8601, "job", "ci-crash", False)["state"], "ready")
            command.assert_not_called()
            self.lab.rollback(8601, "job", "ci-crash")
            command.assert_called_once_with("qm", "rollback", "8601", "ci-crash", "--start", "0")

    def test_snapshot_delete_after_effect_crash_reconciles_absence(self):
        self.lab.state["snapshots"]["8601:ci-crash"] = {
            "vmid": 8601, "name": "ci-crash", "state": "deleting", "preserve": False, "expires": 0}
        with patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "require_lease"), \
                patch.object(c, "api", return_value=[]), patch.object(c, "run") as command:
            self.lab.discard_snapshot(8601, "job", "ci-crash")
            command.assert_not_called()
        self.assertNotIn("8601:ci-crash", c.Lab(config(), self.path).state["snapshots"])

    def test_missing_or_colliding_snapshot_refuses_restore(self):
        for external in ([], [{"name": "ci-test", "description": "foreign-operation"}]):
            self.lab.state["snapshots"]["8601:ci-test"] = {
                "vmid": 8601, "name": "ci-test", "state": "ready", "preserve": False, "effect_owner": "expected"}
            with patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "require_lease"), \
                    patch.object(c, "api", side_effect=lambda path: external if path.endswith("/snapshot") else []), \
                    patch.object(c, "run") as command:
                with self.assertRaises(RuntimeError):
                    self.lab.rollback(8601, "job", "ci-test")
                command.assert_not_called()

    def test_inflight_snapshot_cannot_be_duplicated(self):
        self.lab.state["snapshots"]["8601:ci-test"] = {"state": "creating"}
        with patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "require_lease"), \
                patch.object(c, "api", return_value=[{"type": "qmsnapshot", "id": "8601"}]), \
                patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.snapshot(8601, "job", "ci-test", False)
            command.assert_not_called()

    def test_decommission_fence_survives_restart(self):
        self.lab.begin_decommission()
        with self.assertRaises(RuntimeError):
            c.Lab(config(), self.path)
        recovered = c.Lab(config(), self.path, allow_decommission=True)
        self.assertEqual(recovered.state["phase"], "decommissioning")

    def test_shared_lock_serializes_lease_and_decommission(self):
        script = '''import importlib.util,json,sys
from pathlib import Path
s=importlib.util.spec_from_file_location("control",sys.argv[1])
c=importlib.util.module_from_spec(s);s.loader.exec_module(c)
print("attempt",flush=True)
with c.state_lock(Path(sys.argv[2])):
    print(json.loads(Path(sys.argv[2]).read_text())["phase"],flush=True)
'''
        with c.state_lock(self.path):
            process = subprocess.Popen([sys.executable, "-c", script, str(Path(c.__file__)), str(self.path)],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            self.addCleanup(lambda: process.poll() is None and process.kill())
            self.assertEqual(process.stdout.readline().strip(), "attempt")
            self.assertIsNone(process.poll())
            self.lab.begin_decommission()
        stdout, stderr = process.communicate(timeout=3)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertEqual(stdout.strip(), "decommissioning")

    def test_deferred_capacity_retry_is_durable_and_deduplicated(self):
        with patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "status", return_value={"status": "stopped"}):
            self.lab.defer_start(8601, "pilot", 1800, 2048)
            self.lab.defer_start(8601, "pilot", 1800, 2048)
        with patch.object(self.lab, "resources", return_value={"allowed": False}), patch.object(self.lab, "start") as start:
            self.lab.process_deferred()
            start.assert_not_called()
        recovered = c.Lab(config(), self.path)
        with patch.object(recovered, "resources", return_value={"allowed": True}), \
                patch.object(recovered, "lease"), patch.object(recovered, "start", return_value={"allowed": True}) as start:
            recovered.process_deferred()
            recovered.process_deferred()
            start.assert_called_once_with(8601, "pilot", 2048)

    def test_deferred_reconnect_reconciles_accepted_boot_before_capacity(self):
        self.lab.state["leases"]["8601"] = {"holder": "pilot", "generation": "gen", "expires": 9999999999}
        self.lab.state["runs"] = {"8601": {"holder": "pilot", "generation": "gen", "phase": "starting"}}
        self.lab.state["deferred"] = {"8601": {"holder": "pilot", "memory_mib": 2048, "state": "pending", "expires": 9999999999}}
        with patch.object(self.lab, "resources") as capacity, patch.object(self.lab, "start", return_value={"already_running": True}) as start:
            self.lab.process_deferred()
            capacity.assert_not_called()
            start.assert_called_once_with(8601, "pilot", 2048)
        self.assertEqual(self.lab.state["deferred"]["8601"]["state"], "started")


if __name__ == "__main__":
    unittest.main()
