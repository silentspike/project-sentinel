#!/usr/bin/env python3
"""Owned Proxmox lab admission and lifecycle; run on the registered lab host."""
from __future__ import annotations

import argparse
import fcntl
import ipaddress
import json
import os
from pathlib import Path
import re
import socket
import struct
import subprocess
import time
from typing import Any

MIB = 1024 * 1024
OWNER = "sentinel-ci-v1"


def run(*args: str, data: str | None = None) -> str:
    return subprocess.run(args, input=data, text=True, check=True,
                          capture_output=True, timeout=300).stdout.strip()


def api(path: str) -> Any:
    return json.loads(run("pvesh", "get", path, "--output-format", "json"))


def available_mib() -> int:
    return int(re.search(r"MemAvailable:\s+(\d+)", Path("/proc/meminfo").read_text())[1]) // 1024


def atomic_json(path: Path, value: Any) -> None:
    pending = path.with_suffix(".pending")
    with pending.open("w") as stream:
        os.chmod(pending, 0o600)
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    pending.replace(path)
    descriptor = os.open(path.parent, os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def validate_config(config: dict) -> None:
    if config.get("owner") != OWNER:
        raise ValueError("unrecognized lab owner")
    if config.get("runtime_target_class") != "LAB":
        raise ValueError("runtime target must be LAB")
    if config.get("reserve_mib", 0) < 6144 or not 1 <= config.get("max_active", 0) <= 3:
        raise ValueError("reserve must be >=6 GiB and initial active limit <=3")
    if config.get("template") in config.get("slots", []) or not config.get("slots"):
        raise ValueError("template and slots must be disjoint")
    ids = [config["template"], *config["slots"]]
    if len(set(ids)) != len(ids) or any(not isinstance(v, int) or v < 100 for v in ids):
        raise ValueError("invalid or duplicate VM IDs")
    network = ipaddress.ip_network(config["network"])
    if network.version != 4 or not network.is_private:
        raise ValueError("lab network must be private IPv4")
    addresses = list(config["addresses"].values()) + [config["gateway"]]
    if set(config["addresses"]) != {str(v) for v in ids}:
        raise ValueError("address map must cover exactly the assigned VM IDs")
    if len(set(addresses)) != len(addresses) or any(ipaddress.ip_address(a) not in network
                                                   for a in addresses):
        raise ValueError("invalid or duplicate lab addresses")
    if any(ipaddress.ip_address(a) in (network.network_address, network.broadcast_address) for a in addresses):
        raise ValueError("network and broadcast addresses cannot be assigned")
    for key in ("bridge", "pool", "storage", "node"):
        if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,31}", config[key]):
            raise ValueError("invalid resource name")


def admission(available_mib: int, active: int, target_mib: int,
              growth_mib: int, config: dict) -> dict:
    if not 2048 <= target_mib <= 8192 or growth_mib < 0:
        raise ValueError("invalid memory request")
    # Reserve the complete requested target before start, not the balloon minimum.
    remaining = available_mib - target_mib - growth_mib
    allowed = active < config["max_active"] and remaining >= config["reserve_mib"]
    return {"allowed": allowed, "active": active, "max_active": config["max_active"],
            "available_mib": available_mib, "target_mib": target_mib,
            "foreign_growth_mib": growth_mib, "projected_available_mib": remaining,
            "reserve_mib": config["reserve_mib"]}


def arp_conflict(packet: bytes, address: str, own_mac: bytes) -> bool:
    if len(packet) < 42 or packet[12:14] != b"\x08\x06":
        return False
    if packet[14:20] != b"\x00\x01\x08\x00\x06\x04" or packet[22:28] == own_mac:
        return False
    target = socket.inet_aton(address)
    return packet[28:32] == target or (packet[28:32] == b"\0" * 4
                                                      and packet[38:42] == target)


def arp_dad(bridge: str, address: str) -> dict:
    """RFC 5227-style probes, including simultaneous probe detection."""
    mac = bytes.fromhex(Path(f"/sys/class/net/{bridge}/address").read_text().strip().replace(":", ""))
    frame = (b"\xff" * 6 + mac + b"\x08\x06" +
             struct.pack("!HHBBH", 1, 0x0800, 6, 4, 1) + mac + b"\0" * 4 +
             b"\0" * 6 + socket.inet_aton(address))
    with socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(0x0806)) as sock:
        sock.bind((bridge, 0))
        sock.settimeout(0.2)
        for _ in range(3):
            sock.send(frame)
            deadline = time.monotonic() + 1.1
            while time.monotonic() < deadline:
                try:
                    packet = sock.recv(2048)
                except socket.timeout:
                    continue
                if arp_conflict(packet, address, mac):
                    raise RuntimeError(f"ARP duplicate address detected: {address}")
    return {"address": address, "probes": 3, "conflict": False, "time": time.time()}


def address_inventory(config: dict, address: str, vmid: int | None = None) -> dict:
    """Check configured reservations as well as live L2; ping is never a gate."""
    conflicts = []
    checked = 0
    roots = [Path("/etc/pve/nodes"), Path("/etc/pve/sdn"),
             Path("/etc/network"), Path("/etc/dnsmasq.d"), Path("/var/lib/misc")]
    paths = [Path("/etc/hosts"), Path("/etc/dnsmasq.conf")]
    for root in roots:
        if root.exists():
            paths.extend(path for path in root.rglob("*") if path.is_file())
    for path in paths:
        if not path.exists():
            continue
        checked += 1
        content = path.read_text(errors="replace")
        if (vmid in [config["template"], *config["slots"]]
                and path.name == f"{vmid}.conf" and path.parent.name == "qemu-server"
                and f"description: {OWNER}" in content.splitlines()
                and f"ipconfig0: ip={address}/24,gw={config['gateway']}" in content.splitlines()):
            continue
        for line in content.splitlines():
            if address not in re.findall(r"(?<![\d.])(?:\d{1,3}\.){3}\d{1,3}(?![\d.])", line):
                continue
            # Only the owned bridge gateway stanza is an existing reservation.
            if path == Path("/etc/network/interfaces.d/sentinel-ci.cfg") and address == config["gateway"]:
                continue
            conflicts.append(str(path))
    for interface in json.loads(run("ip", "-j", "address")):
        for item in interface.get("addr_info", []):
            if item.get("local") == address:
                conflicts.append("interface:" + interface["ifname"])
    if conflicts:
        raise RuntimeError(f"configured address collision: {address}: {conflicts}")
    return {"configuration_files_checked": checked, "arp_dad": arp_dad(config["bridge"], address)}


class Lab:
    def __init__(self, config: dict, state: Path):
        validate_config(config)
        self.config = config
        self.path = state
        self.state = json.loads(state.read_text()) if state.exists() else {
            "owner": OWNER, "leases": {}, "snapshots": {}, "last_used": {}}
        if self.state.get("owner") != OWNER:
            raise RuntimeError("foreign lifecycle state")

    def save(self):
        atomic_json(self.path, self.state)

    def owned(self, vmid: int) -> dict:
        if vmid not in [self.config["template"], *self.config["slots"]]:
            raise ValueError("VM is not an assigned lab resource")
        resource = next((v for v in api("/cluster/resources")
                         if v.get("vmid") == vmid), None)
        if not resource or resource.get("node") != self.config["node"]:
            raise RuntimeError("VM is absent or on a foreign node")
        data = api(f"/nodes/{self.config['node']}/qemu/{vmid}/config")
        if data.get("description") != OWNER:
            raise RuntimeError("VM owner marker mismatch")
        pool = api("/pools/" + self.config["pool"])
        if not any(v.get("vmid") == vmid for v in pool.get("members", [])):
            raise RuntimeError("VM is not in the owned pool")
        nics = [value for name, value in data.items() if re.fullmatch(r"net\d+", name)]
        if len(nics) != 1 or f"bridge={self.config['bridge']}" not in nics[0].split(","):
            raise RuntimeError("lab VM network drift")
        if data.get("scsihw") != "virtio-scsi-single" or not data.get("scsi0", "").startswith(self.config["storage"] + ":"):
            raise RuntimeError("lab VM storage drift")
        expected = f"ip={self.config['addresses'][str(vmid)]}/24,gw={self.config['gateway']}"
        if data.get("ipconfig0") != expected:
            raise RuntimeError("lab VM fixed-IP drift")
        return data

    def status(self, vmid: int) -> dict:
        self.owned(vmid)
        return api(f"/nodes/{self.config['node']}/qemu/{vmid}/status/current")

    def resources(self, target_mib: int) -> dict:
        available = available_mib()
        guests = api("/cluster/resources")
        active = sum(v.get("status") == "running" for v in guests
                     if v.get("vmid") in [self.config["template"], *self.config["slots"]])
        # Actual balloon target is a commitment, even when guest-used RAM is low.
        growth = self.config.get("growth_margin_mib", 1024)
        balloons = []
        for guest in guests:
            if guest.get("node") != self.config["node"] or guest.get("status") != "running" or guest.get("type") != "qemu":
                continue
            status = api(f"/nodes/{self.config['node']}/qemu/{guest['vmid']}/status/current")
            actual = int(status.get("balloon", status.get("maxmem", 0))) // MIB
            if actual <= 0:
                raise RuntimeError("missing actual balloon telemetry")
            balloons.append({"vmid": guest["vmid"], "actual_mib": actual,
                             "max_mib": guest["maxmem"] // MIB})
        # Automatic balloon growth is not treated as free capacity. Admission
        # is a bounded current window, guarded by ongoing host telemetry.
        return {**admission(available, active, target_mib, growth, self.config), "balloons": balloons}

    def lease(self, vmid: int, holder: str, seconds: int) -> dict:
        self.owned(vmid)
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}", holder) or not 60 <= seconds <= 14400:
            raise ValueError("invalid bounded lease")
        current = self.state["leases"].get(str(vmid))
        if current and current["expires"] > time.time() and current["holder"] != holder:
            raise RuntimeError("lab slot already leased")
        lease = {"holder": holder, "expires": time.time() + seconds}
        self.state["leases"][str(vmid)] = lease
        self.state["last_used"][str(vmid)] = time.time()
        self.save()
        return lease

    def require_lease(self, vmid: int, holder: str):
        value = self.state["leases"].get(str(vmid), {})
        if value.get("holder") != holder or value.get("expires", 0) <= time.time():
            raise RuntimeError("active matching lab lease required")
        if any(fault.get("vmid") == vmid and fault.get("lease", {}).get("holder") == holder
               for fault in self.state.get("invalidated_leases", []) if fault.get("lease")):
            raise RuntimeError("lease invalidated by host pressure; use a new job identity")

    def start(self, vmid: int, holder: str, memory_mib: int) -> dict:
        self.require_lease(vmid, holder)
        status = self.status(vmid)
        if status["status"] == "running":
            return {"already_running": True}
        config = self.owned(vmid)
        if config.get("template"):
            raise RuntimeError("template cannot be started")
        decision = self.resources(memory_mib)
        if not decision["allowed"]:
            raise RuntimeError("capacity unavailable: " + json.dumps(decision))
        address_inventory(self.config, self.config["addresses"][str(vmid)], vmid)
        # Proxmox sends this explicit balloon target before resuming QEMU.
        # Disable pvestatd automatic growth; later increases require admission.
        run("qm", "set", str(vmid), "--memory", "8192", "--balloon", str(memory_mib), "--shares", "0")
        run("qm", "start", str(vmid))
        self.state["last_used"][str(vmid)] = time.time()
        self.save()
        return decision

    def resize(self, vmid: int, holder: str, memory_mib: int) -> dict:
        self.require_lease(vmid, holder)
        status = self.status(vmid)
        if status["status"] != "running" or not 2048 <= memory_mib <= 8192:
            raise RuntimeError("running guest and 2-8 GiB target required")
        actual = status.get("balloon")
        if actual is None:
            raise RuntimeError("actual balloon is unknown")
        delta = max(0, memory_mib - int(actual) // MIB)
        # resources() validates whole start requests; resize admits only growth.
        decision = self.resources(2048)
        decision.update(target_mib=memory_mib, growth_request_mib=delta)
        decision["projected_available_mib"] = (decision["available_mib"] - delta
                                               - decision["foreign_growth_mib"])
        decision["allowed"] = decision["projected_available_mib"] >= self.config["reserve_mib"]
        if delta and not decision["allowed"]:
            raise RuntimeError("balloon growth capacity unavailable: " + json.dumps(decision))
        run("qm", "set", str(vmid), "--balloon", str(memory_mib), "--shares", "0")
        self.state["last_used"][str(vmid)] = time.time()
        self.save()
        return decision

    def release(self, vmid: int, holder: str):
        self.require_lease(vmid, holder)
        self.state["leases"][str(vmid)]["expires"] = time.time()
        self.state["last_used"][str(vmid)] = time.time()
        self.save()

    def telemetry(self) -> dict:
        guests = []
        for guest in api("/cluster/resources"):
            if guest.get("node") != self.config["node"] or guest.get("status") != "running":
                continue
            fields = {key: guest.get(key) for key in ("vmid", "type", "status", "cpu", "maxmem", "mem", "memhost")}
            if guest.get("type") == "qemu":
                data = api(f"/nodes/{self.config['node']}/qemu/{guest['vmid']}/status/current")
                fields.update({key: data.get(key) for key in ("balloon", "balloon_min", "ballooninfo", "mem", "memhost", "maxmem", "cpus", "uptime")})
            guests.append(fields)
        memory = {key: int(value) * 1024 for key, value in re.findall(
            r"^(\w+):\s+(\d+) kB$", Path("/proc/meminfo").read_text(), re.M)}
        return {"schema": 1, "owner": OWNER, "timestamp": time.time(),
                "host_mem_bytes": memory, "host_reserve_bytes": self.config["reserve_mib"] * MIB,
                "host_loadavg": Path("/proc/loadavg").read_text().strip(),
                "host_psi": {name: Path(f"/proc/pressure/{name}").read_text().strip()
                             for name in ("cpu", "memory", "io")},
                "guests": guests}

    def stop(self, vmid: int) -> None:
        if self.status(vmid)["status"] == "running":
            run("qm", "shutdown", str(vmid), "--timeout", "90", "--forceStop", "0")

    def snapshot(self, vmid: int, holder: str, name: str, preserve: bool) -> dict:
        self.require_lease(vmid, holder)
        self.owned(vmid)
        if not re.fullmatch(r"ci-[a-z0-9-]{1,40}", name):
            raise ValueError("snapshot name must use ci- prefix")
        key = f"{vmid}:{name}"
        if key in self.state["snapshots"]:
            raise RuntimeError("snapshot name already tracked")
        # Stop the guest for application-consistent disks; external effects are
        # never reversible and RAM is deliberately excluded.
        self.stop(vmid)
        self.state["snapshots"][key] = {"vmid": vmid, "name": name,
                                       "expires": time.time() + 86400,
                                       "preserve": preserve, "state": "creating"}
        self.save()
        run("qm", "snapshot", str(vmid), name, "--vmstate", "0", "--description", OWNER)
        self.state["snapshots"][key]["state"] = "ready"
        self.save()
        return self.state["snapshots"][key]

    def rollback(self, vmid: int, holder: str, name: str) -> None:
        self.require_lease(vmid, holder)
        self.owned(vmid)
        snapshot = self.state["snapshots"].get(f"{vmid}:{name}", {})
        if snapshot.get("state") != "ready":
            raise RuntimeError("tracked ready snapshot required")
        self.stop(vmid)
        run("qm", "rollback", str(vmid), name, "--start", "0")

    def discard_snapshot(self, vmid: int, holder: str, name: str) -> None:
        self.require_lease(vmid, holder)
        self.owned(vmid)
        key = f"{vmid}:{name}"
        if key not in self.state["snapshots"]:
            raise RuntimeError("only tracked snapshots may be removed")
        if self.state["snapshots"][key]["preserve"]:
            raise RuntimeError("preserved investigation/rollback snapshot cannot be removed")
        run("qm", "delsnapshot", str(vmid), name)
        del self.state["snapshots"][key]
        self.save()

    def guard(self) -> dict:
        before = available_mib()
        stopped = []
        if before < self.config["reserve_mib"] + 256:
            # An unsafe host window invalidates lab work, even if a lease is
            # active. Stop only owned labs gracefully, never foreign guests.
            active = [v for v in api("/cluster/resources")
                      if v.get("vmid") in self.config["slots"] and v.get("status") == "running"]
            for guest in active:
                vmid = guest["vmid"]
                self.owned(vmid)
                fault = {"vmid": vmid, "time": time.time(), "reason": "host-reserve-pressure",
                         "available_mib": available_mib(),
                         "lease": self.state["leases"].get(str(vmid))}
                self.state.setdefault("invalidated_leases", []).append(fault)
                self.save()
                self.stop(vmid)
                stopped.append(vmid)
                if available_mib() >= self.config["reserve_mib"] + 512:
                    break
        return {"available_before_mib": before, "available_after_mib": available_mib(),
                "reserve_mib": self.config["reserve_mib"], "pressure_stopped": stopped}

    def reap(self) -> dict:
        now = time.time()
        stopped, removed = [], []
        for vmid in self.config["slots"]:
            lease = self.state["leases"].get(str(vmid), {})
            if lease.get("expires", 0) > now:
                continue
            last = self.state["last_used"].get(str(vmid), now)
            if now - last >= self.config.get("idle_seconds", 900):
                self.stop(vmid)
                stopped.append(vmid)
        for key, snapshot in list(self.state["snapshots"].items()):
            vmid = snapshot["vmid"]
            lease = self.state["leases"].get(str(vmid), {})
            if snapshot["preserve"] or snapshot["expires"] > now or lease.get("expires", 0) > now:
                continue
            self.owned(vmid)
            run("qm", "delsnapshot", str(vmid), snapshot["name"])
            del self.state["snapshots"][key]
            removed.append(key)
        self.save()
        return {"idle_stopped": stopped, "expired_snapshots_removed": removed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("action", choices=["status", "telemetry", "admit", "lease", "release", "start", "resize", "stop", "snapshot", "rollback", "discard-snapshot", "guard", "reap"])
    parser.add_argument("--vmid", type=int)
    parser.add_argument("--holder", default="")
    parser.add_argument("--seconds", type=int, default=1800)
    parser.add_argument("--memory-mib", type=int, default=2048)
    parser.add_argument("--name", default="")
    parser.add_argument("--preserve", action="store_true")
    args = parser.parse_args()
    args.state.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with args.state.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        lab = Lab(json.loads(args.config.read_text()), args.state)
        if args.action == "status":
            result = {str(v): lab.status(v) for v in lab.config["slots"]}
        elif args.action == "telemetry":
            result = lab.telemetry()
        elif args.action == "admit":
            result = lab.resources(args.memory_mib)
        elif args.action == "lease":
            result = lab.lease(args.vmid, args.holder, args.seconds)
        elif args.action == "start":
            result = lab.start(args.vmid, args.holder, args.memory_mib)
        elif args.action == "resize":
            result = lab.resize(args.vmid, args.holder, args.memory_mib)
        elif args.action == "release":
            result = lab.release(args.vmid, args.holder)
        elif args.action == "stop":
            lab.require_lease(args.vmid, args.holder)
            result = lab.stop(args.vmid)
        elif args.action == "snapshot":
            result = lab.snapshot(args.vmid, args.holder, args.name, args.preserve)
        elif args.action == "rollback":
            result = lab.rollback(args.vmid, args.holder, args.name)
        elif args.action == "discard-snapshot":
            result = lab.discard_snapshot(args.vmid, args.holder, args.name)
        elif args.action == "guard":
            result = lab.guard()
        else:
            result = lab.reap()
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
