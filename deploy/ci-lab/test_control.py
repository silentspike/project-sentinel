#!/usr/bin/env python3
"""Pure/static lifecycle tests; these do not mutate a hypervisor."""
import importlib.util
import json
from pathlib import Path
import socket
import struct
import subprocess
import sys
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
        self.assertEqual(result["projected_available_mib"], 10000 - 7168 - 1024 - 2048)

    def test_delayed_balloon_and_multiple_guests_keep_commitments(self):
        guests = [{"vmid": v, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 8192 * c.MIB}
                  for v in (8601, 8602)]
        self.lab.state["balloon_requests"] = {"8601": 4096, "8602": 2048}
        statuses = {"8601": {"balloon": 2048 * c.MIB, "memhost": 2048 * c.MIB},
                    "8602": {"balloon": 4096 * c.MIB, "memhost": 3072 * c.MIB}}
        def api(path):
            return guests if path == "/cluster/resources" else statuses[path.split("/")[-3]]
        with patch.object(c, "available_mib", return_value=12000), patch.object(c, "api", side_effect=api):
            result = self.lab.resources(2048)
        self.assertEqual(result["outstanding_commitments_mib"], 3072)
        self.assertFalse(result["allowed"])

    def test_missing_resident_telemetry_fails_closed(self):
        guest = {"vmid": 8601, "node": "lab-host", "type": "qemu", "status": "running", "maxmem": 8192 * c.MIB}
        with patch.object(c, "api", side_effect=lambda path: [guest] if path == "/cluster/resources" else {"balloon": 2048 * c.MIB}):
            with self.assertRaises(RuntimeError):
                self.lab.resources(2048)

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
