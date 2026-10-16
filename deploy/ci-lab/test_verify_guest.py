#!/usr/bin/env python3
"""Semantic parity negatives; no guest or system commands are executed."""
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("verify_guest", Path(__file__).with_name("verify-guest.py"))
v = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(v)


def output(text):
    return {"exit_code": 0, "stdout": text, "stderr": ""}


def probes():
    return {"sysctls": output("\n".join(f"{k} = {x}" for k, x in v.EXPECTED_SYSCTLS.items())),
            "unit_hardening": output("\n".join(f"{k}={x}" for k, x in v.EXPECTED_UNIT.items())),
            "guest_agent": output("active"), "cgroup_v2": output("cgroup2fs"),
            "cgroup_controllers": output("cpu io memory pids"),
            "hot_tier_mount": output("tmpfs rw,nosuid,nodev,noexec,size=4194304k")}


def packages():
    return output("\n".join(f"{k}\t{x}" for k, x in v.EXPECTED_PACKAGES.items()))


class SemanticParityTests(unittest.TestCase):
    def test_exact_reference_passes(self):
        v.validate_probes(probes(), packages())

    def test_successful_command_with_bad_hardening_fails(self):
        for key in v.EXPECTED_UNIT:
            data = probes()
            data["unit_hardening"]["stdout"] = data["unit_hardening"]["stdout"].replace(
                f"{key}={v.EXPECTED_UNIT[key]}", f"{key}=no")
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                v.validate_probes(data, packages())

    def test_successful_command_with_bad_sysctl_fails(self):
        for key in v.EXPECTED_SYSCTLS:
            data = probes()
            data["sysctls"]["stdout"] = data["sysctls"]["stdout"].replace(
                f"{key} = {v.EXPECTED_SYSCTLS[key]}", f"{key} = 99999")
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                v.validate_probes(data, packages())

    def test_duplicate_measured_keys_fail(self):
        data = probes()
        data["sysctls"]["stdout"] += "\nvm.swappiness = 1"
        with self.assertRaises(RuntimeError):
            v.validate_probes(data, packages())

    def test_failed_package_query_fails(self):
        data = packages()
        data["exit_code"] = 1
        with self.assertRaises(RuntimeError):
            v.validate_probes(probes(), data)

    def test_successful_query_with_package_drift_fails(self):
        data = packages()
        data["stdout"] = data["stdout"].replace("2.39-0ubuntu8.9", "unexpected-version")
        with self.assertRaises(RuntimeError):
            v.validate_probes(probes(), data)

    def test_mount_substring_is_not_a_security_flag(self):
        data = probes()
        data["hot_tier_mount"] = output("tmpfs rw,nosuid,nodev,noexec-missing,size=4194304k")
        with self.assertRaises(RuntimeError):
            v.validate_probes(data, packages())


if __name__ == "__main__":
    unittest.main()
