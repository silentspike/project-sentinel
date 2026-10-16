#!/usr/bin/env python3
"""Pure/static lifecycle tests; these do not mutate a hypervisor."""
import importlib.util
import json
from pathlib import Path
import socket
import struct
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("control", Path(__file__).with_name("control.py"))
c = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(c)


def config():
    return {"owner": c.OWNER, "runtime_target_class": "LAB", "reserve_mib": 6144,
            "max_active": 3, "template": 8600, "slots": [8601, 8602],
            "network": "172.30.86.0/24", "gateway": "172.30.86.1",
            "addresses": {"8600": "172.30.86.10", "8601": "172.30.86.11", "8602": "172.30.86.12"},
            "bridge": "vmbr86", "pool": "sentinel-ci", "storage": "sentinel-ci-zfs", "node": "lab-host"}


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
                       {"gateway": "192.0.2.1"}, {"gateway": "172.30.86.255"},
                       {"addresses": {"8601": "172.30.86.11"}}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                c.validate_config({**config(), **change})


class ArpTests(unittest.TestCase):
    own = bytes.fromhex("020000000001")
    other = bytes.fromhex("020000000002")

    def frame(self, sender="172.30.86.11", target="172.30.86.1", mac=None):
        mac = mac or self.other
        return (b"\xff" * 6 + mac + b"\x08\x06" + struct.pack("!HHBBH", 1, 0x800, 6, 4, 1)
                + mac + socket.inet_aton(sender) + b"\0" * 6 + socket.inet_aton(target))

    def test_existing_owner(self):
        self.assertTrue(c.arp_conflict(self.frame(), "172.30.86.11", self.own))

    def test_simultaneous_probe(self):
        self.assertTrue(c.arp_conflict(self.frame("0.0.0.0", "172.30.86.11"), "172.30.86.11", self.own))

    def test_own_probe_ignored(self):
        self.assertFalse(c.arp_conflict(self.frame("0.0.0.0", "172.30.86.11", self.own), "172.30.86.11", self.own))

    def test_unrelated_or_truncated_ignored(self):
        self.assertFalse(c.arp_conflict(self.frame("172.30.86.12"), "172.30.86.11", self.own))
        self.assertFalse(c.arp_conflict(b"bad", "172.30.86.11", self.own))


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir="/work/tmp/project-sentinel")
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "state.json"
        self.lab = c.Lab(config(), self.path)

    def test_foreign_state_rejected(self):
        self.path.write_text('{"owner":"foreign"}')
        with self.assertRaises(RuntimeError):
            c.Lab(config(), self.path)

    def test_lease_survives_restart_and_blocks_second_holder(self):
        with patch.object(self.lab, "owned", return_value={}):
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
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "status", return_value={"status": "stopped"}), \
                patch.object(self.lab, "owned", return_value={}), patch.object(self.lab, "resources", return_value={"allowed": False}), \
                patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.start(8601, "job", 2048)
            command.assert_not_called()

    def test_running_start_is_idempotent(self):
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "status", return_value={"status": "running"}), \
                patch.object(c, "run") as command:
            self.assertEqual(self.lab.start(8601, "job", 2048), {"already_running": True})
            command.assert_not_called()

    def test_unknown_snapshot_cannot_rollback(self):
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "owned"), patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.rollback(8601, "job", "ci-foreign")
            command.assert_not_called()

    def test_stop_is_graceful_and_cannot_force_kill(self):
        with patch.object(self.lab, "status", return_value={"status": "running"}), patch.object(c, "run") as command:
            self.lab.stop(8601)
            command.assert_called_once_with("qm", "shutdown", "8601", "--timeout", "90", "--forceStop", "0")

    def test_live_lease_and_preserved_snapshot_survive_reap(self):
        self.lab.state["leases"]["8601"] = {"holder": "job", "expires": 2000}
        self.lab.state["last_used"] = {"8601": 0, "8602": 0}
        self.lab.state["snapshots"]["8602:ci-rollback"] = {
            "vmid": 8602, "name": "ci-rollback", "preserve": True, "expires": 0, "state": "ready"}
        with patch.object(c.time, "time", return_value=1000), patch.object(self.lab, "stop") as stop, \
                patch.object(c, "run") as command:
            result = self.lab.reap()
            stop.assert_called_once_with(8602)
            self.assertEqual(result["expired_snapshots_removed"], [])
            command.assert_not_called()

    def test_snapshot_record_is_durable_before_effect(self):
        def command(*args):
            state = json.loads(self.path.read_text())
            self.assertEqual(state["snapshots"]["8601:ci-test"]["state"], "creating")
        with patch.object(self.lab, "require_lease"), patch.object(self.lab, "owned"), \
                patch.object(self.lab, "stop"), patch.object(c, "run", side_effect=command):
            result = self.lab.snapshot(8601, "job", "ci-test", False)
        self.assertEqual(result["state"], "ready")

    def test_resize_denies_unavailable_growth(self):
        with patch.object(self.lab, "require_lease"), \
                patch.object(self.lab, "status", return_value={"status": "running", "balloon": 2048 * c.MIB}), \
                patch.object(self.lab, "resources", return_value={"available_mib": 8000, "foreign_growth_mib": 1536}), \
                patch.object(c, "run") as command:
            with self.assertRaises(RuntimeError):
                self.lab.resize(8601, "job", 8192)
            command.assert_not_called()

    def test_resize_reduces_memory_even_under_pressure(self):
        with patch.object(self.lab, "require_lease"), \
                patch.object(self.lab, "status", return_value={"status": "running", "balloon": 4096 * c.MIB}), \
                patch.object(self.lab, "resources", return_value={"available_mib": 5000, "foreign_growth_mib": 1536}), \
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
        with patch.object(c, "available_mib", side_effect=[6000, 6000, 8000, 8000]), \
                patch.object(c, "api", return_value=[{"vmid": 9999, "status": "running"},
                                                   {"vmid": 8601, "status": "running"}]), \
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


if __name__ == "__main__":
    unittest.main()
