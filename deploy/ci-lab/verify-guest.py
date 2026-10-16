#!/usr/bin/env python3
"""Read-only runtime-underlay fingerprint plus bounded real kernel probes."""
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import time


def run(*args):
    result = subprocess.run(args, capture_output=True, text=True, timeout=30)
    return {"exit_code": result.returncode, "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}


def main():
    expected = {"kernel": "7.0.0-34-generic", "bubblewrap_version": "0.9.0-1ubuntu0.1",
                "apparmor_version": "4.0.1really4.0.1-0ubuntu0.24.04.6",
                "qemu_guest_agent_version": "1:8.2.2+ds-0ubuntu1.16",
                "bubblewrap_sha256": "52231e1caf55bcbc667b269f49c63599a6f7db4767ae6a039580d0ff853db712"}
    packages = run("dpkg-query", "-W", "-f=${binary:Package}\t${Version}\n",
                   "linux-image-7.0.0-34-generic", "bubblewrap", "apparmor", "libc6",
                   "libseccomp2", "libssl3t64", "libsqlite3-0", "qemu-guest-agent", "fuse3")
    probes = {"unprivileged_userns": run("sudo", "-u", "ubuntu", "unshare", "--user", "--map-root-user", "true"),
              "bubblewrap_isolation": run("sudo", "-u", "ubuntu", "bwrap", "--unshare-all", "--die-with-parent",
                                           "--ro-bind", "/usr", "/usr", "--symlink", "usr/lib", "/lib",
                                           "--symlink", "usr/lib64", "/lib64", "--proc", "/proc", "--dev", "/dev",
                                           "/usr/bin/sh", "-c", "test ! -e /etc/machine-id && test ! -e /opt/sentinel/data && test ! -e /home/ubuntu/.ssh/authorized_keys"),
              "cgroup_v2": run("stat", "-fc", "%T", "/sys/fs/cgroup"),
              "cgroup_controllers": run("cat", "/sys/fs/cgroup/sentinel/cgroup.subtree_control"),
              "sysctls": run("sysctl", "kernel.unprivileged_userns_clone", "kernel.apparmor_restrict_unprivileged_userns",
                              "vm.swappiness", "vm.max_map_count", "vm.dirty_bytes", "vm.dirty_background_bytes",
                              "net.core.rmem_max", "net.core.wmem_max"),
              "guest_agent": run("systemctl", "is-active", "qemu-guest-agent"),
              "hot_tier_mount": run("findmnt", "-no", "FSTYPE,OPTIONS", "-T", "/ram/sentinel"),
              "unit_hardening": run("systemctl", "show", "sentinel-daemon", "-p", "User", "-p", "Group",
                                    "-p", "NoNewPrivileges", "-p", "ProtectSystem", "-p", "PrivateTmp")}
    actual = {"kernel": platform.release(),
              "apparmor_version": run("dpkg-query", "-W", "-f=${Version}", "apparmor")["stdout"],
              "qemu_guest_agent_version": run("dpkg-query", "-W", "-f=${Version}", "qemu-guest-agent")["stdout"],
              "bubblewrap_version": run("dpkg-query", "-W", "-f=${Version}", "bubblewrap")["stdout"],
              "bubblewrap_sha256": hashlib.sha256(Path("/usr/bin/bwrap").read_bytes()).hexdigest()}
    for key in expected:
        if actual[key] != expected[key]:
            raise RuntimeError(f"underlay mismatch: {key}: {actual[key]}")
    if Path("/sys/module/apparmor/parameters/enabled").read_text().strip() != "Y":
        raise RuntimeError("AppArmor must stay enabled")
    if not Path("/dev/fuse").exists() or not Path("/sys/kernel/btf/vmlinux").exists():
        raise RuntimeError("FUSE device and BTF required")
    for name, probe in probes.items():
        if probe["exit_code"]:
            raise RuntimeError("kernel/runtime probe failed: " + name + ": " + json.dumps(probe))
    if probes["cgroup_v2"]["stdout"] != "cgroup2fs":
        raise RuntimeError("cgroup v2 required")
    if not {"cpu", "io", "memory", "pids"} <= set(probes["cgroup_controllers"]["stdout"].split()):
        raise RuntimeError("missing delegated controller")
    mount = probes["hot_tier_mount"]["stdout"]
    if not mount.startswith("tmpfs") or not all(flag in mount for flag in ("nosuid", "nodev", "noexec", "size=4194304k")):
        raise RuntimeError("hot-tier tmpfs contract mismatch")
    identity = {"machine_id_sha256": hashlib.sha256(Path("/etc/machine-id").read_bytes()).hexdigest(),
                "ssh_host_key_sha256": hashlib.sha256(Path("/etc/ssh/ssh_host_ed25519_key.pub").read_bytes()).hexdigest()}
    fingerprint = hashlib.sha256(json.dumps({"actual": actual, "packages": packages["stdout"],
                                            "sysctls": probes["sysctls"]["stdout"]}, sort_keys=True).encode()).hexdigest()
    print(json.dumps({"schema": 1, "timestamp": time.time(), "expected": expected, "actual": actual,
                      "packages": packages, "probes": probes, "identity": identity,
                      "underlay_fingerprint": fingerprint,
                      "not_tested": ["product artifacts", "real customer acceptance", "performance"]}, indent=2))


if __name__ == "__main__":
    main()
