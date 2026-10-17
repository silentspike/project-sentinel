#!/usr/bin/env python3
"""Owned Proxmox lab admission and lifecycle; run on the registered lab host."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from copy import deepcopy
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
import uuid
from typing import Any

MIB = 1024 * 1024
BOOT_MEMORY_MIB = 8192
OWNER = "sentinel-ci-v1"


class CapacityUnavailable(RuntimeError):
    pass


@contextmanager
def state_lock(state: Path):
    state.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with state.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def run(*args: str, data: str | None = None) -> str:
    return subprocess.run(args, input=data, text=True, check=True,
                          capture_output=True, timeout=300).stdout.strip()


def api(path: str) -> Any:
    return json.loads(run("pvesh", "get", path, "--output-format", "json"))


def available_mib() -> int:
    return int(re.search(r"MemAvailable:\s+(\d+)", Path("/proc/meminfo").read_text())[1]) // 1024


def process_memory(pid: int) -> dict:
    if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 0:
        raise RuntimeError("missing QEMU process identity")
    root = Path(f"/proc/{pid}")
    before = (root / "stat").read_text().rsplit(")", 1)[1].split()[19]
    values = {key: int(value) * 1024 for key, value in re.findall(
        r"^(Rss|Pss|Swap):\s+(\d+) kB$", (root / "smaps_rollup").read_text(), re.M)}
    after = (root / "stat").read_text().rsplit(")", 1)[1].split()[19]
    if before != after or not {"Rss", "Pss", "Swap"} <= values.keys():
        raise RuntimeError("QEMU process changed or resident telemetry incomplete")
    return {"pid": pid, "start_ticks": before, "rss_bytes": values["Rss"],
            "pss_bytes": values["Pss"], "swap_bytes": values["Swap"]}


def memory_psi() -> dict:
    lines = Path("/proc/pressure/memory").read_text().splitlines()
    result = {parts[0]: {key: float(value) for key, value in
                         (item.split("=", 1) for item in parts[1:])}
              for parts in (line.split() for line in lines)}
    if not all(kind in result and "avg10" in result[kind] for kind in ("some", "full")):
        raise RuntimeError("missing memory PSI telemetry")
    return result


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
    check = config.get("network_check")
    if check is not None and (not isinstance(check, list) or not check or any(not isinstance(x, str) or not x for x in check)):
        raise ValueError("network_check must be an explicit argument vector")
    managed = config.get("managed_growth", {})
    if set(managed) != {"builder", "agent"}:
        raise ValueError("named provisional builder and agent growth allowances required")
    for name, allowance in (("builder", 1024), ("agent", 512)):
        item = managed[name]
        if item.get("allowance_mib") != allowance or not isinstance(item.get("vmid"), int) or item["vmid"] in ids:
            raise ValueError("invalid managed growth binding")
    if managed["builder"]["vmid"] == managed["agent"]["vmid"]:
        raise ValueError("managed guests must be distinct")


def admission(available_mib: int, active: int, target_mib: int,
              growth_mib: int, config: dict) -> dict:
    if not 2048 <= target_mib <= 8192 or growth_mib < 0:
        raise ValueError("invalid memory request")
    # Reserve the complete requested target before start, not the balloon minimum.
    remaining = available_mib - target_mib - growth_mib
    allowed = active < config["max_active"] and remaining >= config["reserve_mib"]
    return {"allowed": allowed, "active": active, "max_active": config["max_active"],
            "available_mib": available_mib, "target_mib": target_mib,
            "reserved_growth_mib": growth_mib, "projected_available_mib": remaining,
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
                and path == Path(f"/etc/pve/nodes/{config['node']}/qemu-server/{vmid}.conf")):
            # PVE stores descriptions as encoded comments, not config keys.
            reservation = api(f"/nodes/{config['node']}/qemu/{vmid}/config")
            if (reservation.get("description") == OWNER
                    and reservation.get("ipconfig0") == f"ip={address}/24,gw={config['gateway']}"):
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
    def __init__(self, config: dict, state: Path, allow_decommission: bool = False):
        validate_config(config)
        self.config = config
        self.path = state
        self.state = json.loads(state.read_text()) if state.exists() else {
            "owner": OWNER, "leases": {}, "snapshots": {}, "last_used": {}}
        if self.state.get("owner") != OWNER:
            raise RuntimeError("foreign lifecycle state")
        if not allow_decommission and self.state.get("phase", "active") != "active":
            raise RuntimeError("lab decommission admission fence is active")

    def save(self):
        atomic_json(self.path, self.state)

    def begin_decommission(self):
        if any(lease.get("expires", 0) > time.time() for lease in self.state["leases"].values()):
            raise RuntimeError("active leases block decommission")
        if any(request.get("state") == "pending" for request in self.state.get("deferred", {}).values()):
            raise RuntimeError("pending starts block decommission")
        if any(snapshot.get("preserve") for snapshot in self.state["snapshots"].values()):
            raise RuntimeError("preserved snapshots block decommission")
        self.state["phase"] = "decommissioning"
        self.save()

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

    def lease_purpose(self, vmid: int) -> str:
        value = self.state["leases"].get(str(vmid), {}).get("purpose")
        return "functional" if value == "functional" else "measurement"

    def owned_commitments(self, activity: dict, starting_vmid: int | None = None) -> dict:
        active, headroom, balloons = 0, 0, []
        for vmid, current in activity.items():
            key = str(vmid)
            requested = self.state.get("balloon_requests", {}).get(key, 8192)
            run_record = self.state.get("runs", {}).get(key, {})
            startup = run_record.get("startup_memory_mib", BOOT_MEMORY_MIB if current["promise"] else 0)
            if current["status"] == "stopped":
                # An accepted effect can resume after its cluster hint vanished.
                if current["promise"] and vmid != starting_vmid:
                    active += 1
                    headroom += max(requested, startup)
                continue
            active += 1
            status = current["status_data"]
            actual = status.get("balloon")
            actual = status.get("maxmem") if actual is None else actual
            resident = status.get("memhost")
            if actual is None or resident is None or actual <= 0 or resident < 0:
                raise CapacityUnavailable("missing actual owned balloon/resident telemetry")
            try:
                measured = process_memory(status.get("pid"))
            except (OSError, RuntimeError, ValueError, IndexError) as error:
                raise CapacityUnavailable("owned QEMU process sample unavailable") from error
            pve_resident = resident
            resident = min(pve_resident, measured["rss_bytes"])
            target = self.state.get("balloon_requests", {}).get(key, 0) * MIB
            committed = max(actual, status.get("balloon_min") or 0, target,
                            startup * MIB if current["phase"] == "starting" else 0)
            outstanding = (max(0, committed - resident) + MIB - 1) // MIB
            headroom += outstanding
            balloons.append({"vmid": vmid, "actual_mib": status["balloon"] // MIB if status.get("balloon") is not None else None,
                             "commitment_basis": "reported-balloon" if status.get("balloon") is not None else "maxmem-fallback",
                             "pve_resident_bytes": pve_resident, "process": measured,
                             "requested_mib": target // MIB, "startup_commitment_mib": startup,
                             "owned": True,
                             "potential_basis": "current-owned-target", "committed_mib": committed // MIB,
                             "resident_mib": resident // MIB, "outstanding_mib": outstanding,
                             "max_mib": status.get("maxmem", 8192 * MIB) // MIB})
        return {"active": active, "outstanding_commitments_mib": headroom, "balloons": balloons}

    def resources(self, target_mib: int, starting_vmid: int | None = None,
                  purpose: str = "measurement", cold_start: bool = False) -> dict:
        if not 2048 <= target_mib <= BOOT_MEMORY_MIB:
            raise ValueError("invalid memory request")
        guests = api("/cluster/resources")
        sample = self.capacity_sample()
        safety = self.capacity_safety(sample, purpose=purpose)
        own = self.owned_commitments(self.owned_activity(guests=guests, check_ownership=False), starting_vmid)
        margin = sum(v["allowance_mib"] for v in self.config["managed_growth"].values())
        external, balloons = 0, list(own["balloons"])
        for guest in guests:
            if (guest.get("node") != self.config["node"] or guest.get("type") != "qemu"
                    or guest.get("status") != "running"
                    or guest.get("vmid") in [self.config["template"], *self.config["slots"]]):
                continue
            actual, resident = guest.get("maxmem"), guest.get("memhost")
            if actual is None or resident is None or actual <= 0 or resident < 0:
                balloons.append({"vmid": guest["vmid"], "owned": False, "outstanding_mib": None,
                                 "error": "external potential unknown"})
                continue
            outstanding = (max(0, actual - resident) + MIB - 1) // MIB
            external += outstanding
            balloons.append({"vmid": guest["vmid"], "actual_mib": None, "owned": False,
                             "potential_basis": "cluster-maxmem-not-current-target",
                             "committed_mib": actual // MIB, "resident_mib": resident // MIB,
                             "outstanding_mib": outstanding, "max_mib": actual // MIB})
        # QEMU exposes the full launch RAM before the guest balloon driver converges.
        launch_mib = BOOT_MEMORY_MIB if cold_start else target_mib
        decision = admission(min(sample["available_mib"], available_mib()), own["active"], launch_mib,
                             margin + own["outstanding_commitments_mib"], self.config)
        decision["allowed"] = decision["allowed"] and not safety["reasons"]
        decision["required_available_mib"] = self.config["reserve_mib"] + 256
        decision["allowed"] = (decision["allowed"]
                               and decision["projected_available_mib"] >= decision["required_available_mib"])
        return {**decision, "requested_target_mib": target_mib, "cold_start": cold_start,
                "startup_commitment_mib": launch_mib if cold_start else 0,
                "growth_margin_mib": margin,
                "outstanding_commitments_mib": own["outstanding_commitments_mib"],
                "external_potential_mib": external, "external_risk": "uncontrolled foreign bursts are not reserved or guaranteed",
                "capacity_safety": safety, "capacity_sample": sample, "purpose": purpose, "balloons": balloons}

    def capacity_sample(self) -> dict:
        managed = {}
        for name, item in self.config["managed_growth"].items():
            status = api(f"/nodes/{self.config['node']}/qemu/{item['vmid']}/status/current")
            if status.get("status") != "running":
                raise RuntimeError("managed guest is not running; growth baseline requires reconciliation")
            managed[name] = {"vmid": item["vmid"], **process_memory(status.get("pid"))}
        return {"timestamp": time.time(), "available_mib": available_mib(),
                "memory_psi": memory_psi(), "managed": managed}

    def capacity_safety(self, sample: dict, include_hold: bool = True,
                        purpose: str = "measurement") -> dict:
        reasons, growth, growth_reasons = [], {}, []
        if include_hold and sample["timestamp"] < self.state.get("capacity_hold_until", 0):
            reasons.append("previous-unsafe-sample-window")
        previous = self.state.get("capacity_sample", {})
        baseline = self.state.get("capacity_baseline") or previous
        if not previous or not 0 <= sample["timestamp"] - previous.get("timestamp", 0) <= 45:
            reasons.append("managed-sample-missing-or-stale")
        for name, item in self.config["managed_growth"].items():
            old = baseline.get("managed", {}).get(name, {})
            current = sample["managed"][name]
            if not old or any(current.get(k) != old.get(k) for k in ("pid", "start_ticks", "vmid")):
                reasons.append(name + "-process-baseline-changed")
                continue
            delta = max(0, current["rss_bytes"] - old["rss_bytes"])
            growth[name] = {"rss_growth_mib": (delta + MIB - 1) // MIB,
                            "allowance_mib": item["allowance_mib"], "provisional": True}
            if delta > item["allowance_mib"] * MIB:
                growth_reasons.append(name + "-growth-envelope-exceeded")
        # Conservative provisional pressure trip points, not a performance SLA.
        if sample["memory_psi"]["some"]["avg10"] >= 1.0 or sample["memory_psi"]["full"]["avg10"] >= 0.1:
            reasons.append("host-memory-psi")
        if sample["available_mib"] < self.config["reserve_mib"] + 256:
            reasons.append("host-reserve-pressure")
        if purpose != "functional":
            reasons.extend(growth_reasons)
        return {"reasons": reasons, "warnings": growth_reasons if purpose == "functional" else [],
                "growth_reasons": growth_reasons, "managed_growth": growth,
                "sample_max_age_seconds": 45, "psi_some_avg10_limit": 1.0, "psi_full_avg10_limit": 0.1}

    def fence_starting(self, vmid: int):
        if self.state.get("runs", {}).get(str(vmid), {}).get("phase") != "starting":
            return
        config = self.owned(vmid)
        tasks = api(f"/nodes/{self.config['node']}/tasks")
        if config.get("lock") or any(str(v.get("id")) == str(vmid) and not v.get("endtime")
                                      and v.get("type") in {"qmstart", "qmstop", "qmshutdown", "qmresume", "qmreboot", "qmreset", "qmrollback"}
                                      for v in tasks):
            raise CapacityUnavailable("accepted start effect is still in flight; retain its generation and commitment")
        if self.status(vmid)["status"] != "stopped":
            raise CapacityUnavailable("start effect changed during quiescence readback")

    def lease(self, vmid: int, holder: str, seconds: int, purpose: str | None = None) -> dict:
        if purpose not in {"functional", "measurement"}:
            raise ValueError("explicit functional or measurement purpose required")
        return self._grant_lease(vmid, holder, seconds, purpose)

    def _grant_lease(self, vmid: int, holder: str, seconds: int, purpose: str | None) -> dict:
        # None is only the persisted legacy-request recovery path, never a new
        # API request. Keep its stored purpose absent and its semantics strict.
        if purpose is not None and purpose not in {"functional", "measurement"}:
            raise ValueError("invalid persisted lease purpose")
        self.owned(vmid)
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}", holder) or not 60 <= seconds <= 14400:
            raise ValueError("invalid bounded lease")
        current = self.state["leases"].get(str(vmid))
        if current and current["expires"] > time.time() and current["holder"] != holder:
            raise RuntimeError("lab slot already leased")
        renewal = bool(current and current["expires"] > time.time() and current["holder"] == holder)
        if renewal and (purpose or "measurement") != self.lease_purpose(vmid):
            raise RuntimeError("lease purpose is immutable within a generation")
        if not renewal:
            if self.status(vmid)["status"] != "stopped":
                raise RuntimeError("lease takeover requires the prior guest to be stopped")
            self.fence_starting(vmid)
            self.state.setdefault("runs", {}).pop(str(vmid), None)
            self.state.setdefault("balloon_requests", {}).pop(str(vmid), None)
        lease = {"holder": holder, "expires": time.time() + seconds,
                 "generation": current.get("generation", uuid.uuid4().hex) if renewal else uuid.uuid4().hex}
        if (renewal and "purpose" in current) or (not renewal and purpose is not None):
            lease["purpose"] = current["purpose"] if renewal else purpose
        self.state["leases"][str(vmid)] = lease
        self.state["last_used"][str(vmid)] = time.time()
        self.save()
        return lease

    def require_lease(self, vmid: int, holder: str, allow_invalidated: bool = False):
        value = self.state["leases"].get(str(vmid), {})
        if value.get("holder") != holder or value.get("expires", 0) <= time.time():
            raise RuntimeError("active matching lab lease required")
        if not allow_invalidated and any(fault.get("vmid") == vmid and fault.get("lease", {}).get("holder") == holder
               for fault in self.state.get("invalidated_leases", []) if fault.get("lease")):
            raise RuntimeError("lease invalidated by host pressure; use a new job identity")

    def start(self, vmid: int, holder: str, memory_mib: int) -> dict:
        self.require_lease(vmid, holder)
        status = self.status(vmid)
        config = self.owned(vmid)
        lease = self.state["leases"][str(vmid)]
        if status["status"] == "running":
            previous = self.state.get("runs", {}).get(str(vmid), {})
            if (previous.get("generation") != lease.get("generation")
                    or previous.get("holder") != holder or previous.get("vmgenid") != config.get("vmgenid")
                    or previous.get("phase") not in {"starting", "running"}):
                raise RuntimeError("running guest is not bound to this lease generation")
            previous["phase"] = "running"
            self.save()
            return {"already_running": True}
        if config.get("template"):
            raise RuntimeError("template cannot be started")
        self.fence_starting(vmid)
        decision = self.resources(memory_mib, vmid, self.lease_purpose(vmid), cold_start=True)
        if not decision["allowed"]:
            raise CapacityUnavailable("capacity unavailable: " + json.dumps(decision))
        if not self.config.get("network_check"):
            raise RuntimeError("live network policy check is required before start")
        run(*self.config["network_check"])
        address_inventory(self.config, self.config["addresses"][str(vmid)], vmid)
        decision = self.resources(memory_mib, vmid, self.lease_purpose(vmid), cold_start=True)
        if not decision["allowed"]:
            raise CapacityUnavailable("capacity changed during network preflight: " + json.dumps(decision))
        self.state.setdefault("runs", {})[str(vmid)] = {
            "holder": holder, "generation": lease["generation"], "vmgenid": config.get("vmgenid"),
            "phase": "starting", "startup_memory_mib": BOOT_MEMORY_MIB}
        self.state.setdefault("balloon_requests", {})[str(vmid)] = memory_mib
        self.state.setdefault("capacity_baseline", decision["capacity_sample"])
        self.state["capacity_sample"] = decision["capacity_sample"]
        self.save()
        # Proxmox sends this explicit balloon target before resuming QEMU.
        # Disable pvestatd automatic growth; later increases require admission.
        run("qm", "set", str(vmid), "--memory", str(BOOT_MEMORY_MIB), "--balloon", str(memory_mib), "--shares", "0")
        run("qm", "start", str(vmid))
        self.state["runs"][str(vmid)]["phase"] = "running"
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
        current_target = max(int(actual) // MIB, int(status.get("balloon_min") or 0) // MIB,
                             self.state.get("balloon_requests", {}).get(str(vmid), 0))
        delta = max(0, memory_mib - current_target)
        actual_growth = max(0, memory_mib - int(actual) // MIB)
        # resources() validates whole start requests; resize admits only growth.
        decision = self.resources(2048, purpose=self.lease_purpose(vmid))
        decision.update(target_mib=memory_mib, growth_request_mib=delta, actual_growth_mib=actual_growth)
        decision["projected_available_mib"] = (decision["available_mib"] - delta
                                               - decision["reserved_growth_mib"])
        decision["allowed"] = (decision["projected_available_mib"] >= self.config["reserve_mib"] + 256
                               and not decision.get("capacity_safety", {}).get("reasons")
                               and decision.get("active", 0) <= self.config["max_active"])
        if (delta or actual_growth) and not decision["allowed"]:
            raise CapacityUnavailable("balloon growth capacity unavailable: " + json.dumps(decision))
        if decision["allowed"]:
            self.state["capacity_sample"] = decision["capacity_sample"]
        self.state.setdefault("balloon_requests", {})[str(vmid)] = memory_mib
        self.save()
        run("qm", "set", str(vmid), "--balloon", str(memory_mib), "--shares", "0")
        self.state["last_used"][str(vmid)] = time.time()
        self.save()
        return decision

    def release(self, vmid: int, holder: str):
        self.require_lease(vmid, holder, allow_invalidated=True)
        if self.status(vmid)["status"] != "stopped":
            raise RuntimeError("stop the guest before releasing its lease")
        self.fence_starting(vmid)
        self.state["leases"][str(vmid)]["expires"] = time.time()
        self.state["last_used"][str(vmid)] = time.time()
        self.save()

    def defer_start(self, vmid: int, holder: str, seconds: int, memory_mib: int,
                    purpose: str | None = None) -> dict:
        if purpose not in {"functional", "measurement"}:
            raise ValueError("explicit functional or measurement purpose required")
        self.owned(vmid)
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}", holder) or not 60 <= seconds <= 14400 or not 2048 <= memory_mib <= 8192:
            raise ValueError("invalid bounded deferred request")
        lease = self.state["leases"].get(str(vmid), {})
        if lease.get("expires", 0) > time.time() and lease.get("holder") != holder:
            raise RuntimeError("slot is leased by another job")
        if lease.get("expires", 0) > time.time() and self.lease_purpose(vmid) != purpose:
            raise RuntimeError("deferred request cannot change live lease purpose")
        current = self.state.setdefault("deferred", {}).get(str(vmid))
        if current and current["holder"] == holder:
            if current["memory_mib"] != memory_mib or current.get("purpose", "measurement") != purpose:
                raise RuntimeError("same request identity cannot change memory or purpose")
            return current
        if current and current["state"] == "pending" and current["expires"] > time.time():
            raise RuntimeError("slot already has a deferred request")
        if self.status(vmid)["status"] != "stopped":
            raise RuntimeError("new deferred requests require a stopped slot")
        request = {"holder": holder, "memory_mib": memory_mib, "expires": time.time() + seconds,
                   "state": "pending", "created": time.time(), "purpose": purpose}
        self.state["deferred"][str(vmid)] = request
        self.save()
        return request

    def process_deferred(self) -> dict:
        results = {}
        for key, request in self.state.get("deferred", {}).items():
            if request["state"] != "pending":
                continue
            if request["expires"] <= time.time():
                request["state"] = "expired"
                continue
            vmid = int(key)
            try:
                current = self.state["leases"].get(key, {})
                if current.get("expires", 0) > time.time() and current.get("holder") != request["holder"]:
                    request["last_wait"] = "slot leased by another job"
                    continue
                if (current.get("expires", 0) > time.time()
                        and self.lease_purpose(vmid) != request.get("purpose", "measurement")):
                    raise RuntimeError("deferred purpose does not match the live generation")
                previous = self.state.get("runs", {}).get(key, {})
                if (current.get("expires", 0) > time.time() and current.get("holder") == request["holder"]
                        and previous.get("generation") == current.get("generation")
                        and previous.get("phase") in {"starting", "running"}):
                    results[key] = self.start(vmid, request["holder"], request["memory_mib"])
                    request["state"] = "started"
                    continue
                decision = self.resources(request["memory_mib"], purpose=request.get("purpose", "measurement"),
                                          cold_start=True)
                if not decision["allowed"]:
                    request["last_wait"] = decision
                    continue
                remaining = int(request["expires"] - time.time())
                if remaining < 60:
                    request["state"] = "expired"
                    continue
                if request.get("purpose") is None:
                    self._grant_lease(vmid, request["holder"], remaining, None)
                else:
                    self.lease(vmid, request["holder"], remaining, request["purpose"])
                results[key] = self.start(vmid, request["holder"], request["memory_mib"])
                request["state"] = "started"
            except CapacityUnavailable as error:
                request["last_wait"] = str(error)
            except (subprocess.SubprocessError, OSError) as error:
                # A retained starting intent binds a reconnect to this exact
                # generation; transport failures never justify a second boot.
                request["last_wait"] = str(error)
            except (RuntimeError, ValueError) as error:
                request["state"] = "blocked"
                request["error"] = str(error)
            finally:
                self.save()
        self.save()
        return results

    def telemetry(self) -> dict:
        guests = []
        for guest in api("/cluster/resources"):
            if guest.get("node") != self.config["node"] or guest.get("status") != "running":
                continue
            fields = {key: guest.get(key) for key in ("vmid", "type", "status", "cpu", "maxmem", "mem", "memhost")}
            if guest.get("type") == "qemu":
                data = api(f"/nodes/{self.config['node']}/qemu/{guest['vmid']}/status/current")
                fields.update({key: data.get(key) for key in ("balloon", "balloon_min", "ballooninfo", "mem", "memhost", "maxmem", "cpus", "uptime", "cpu", "diskread", "diskwrite", "netin", "netout")})
                try:
                    fields["process"] = process_memory(data.get("pid"))
                except (OSError, RuntimeError, ValueError, IndexError) as error:
                    fields["process"] = None
                    fields["process_error"] = str(error)
            fields["owned"] = guest.get("vmid") in self.config["slots"]
            if fields["owned"]:
                key = str(guest["vmid"])
                fields["lease"] = self.state["leases"].get(key)
                fields["effective_purpose"] = self.lease_purpose(guest["vmid"])
                fields["legacy_purpose"] = (fields["lease"] or {}).get("purpose") not in {"functional", "measurement"}
                fields["run"] = self.state.get("runs", {}).get(key)
                fields["pending_target_mib"] = self.state.get("balloon_requests", {}).get(key)
            guests.append(fields)
        memory = {key: int(value) * 1024 for key, value in re.findall(
            r"^(\w+):\s+(\d+) kB$", Path("/proc/meminfo").read_text(), re.M)}
        return {"schema": 2, "lease_contract_version": 2, "owner": OWNER, "timestamp": time.time(),
                "host_mem_bytes": memory, "host_reserve_bytes": self.config["reserve_mib"] * MIB,
                "host_loadavg": Path("/proc/loadavg").read_text().strip(),
                "host_cpu_stat": Path("/proc/stat").read_text().splitlines()[0],
                "host_diskstats": Path("/proc/diskstats").read_text(),
                "host_psi": {name: Path(f"/proc/pressure/{name}").read_text().strip()
                             for name in ("cpu", "memory", "io")},
                "managed_growth_allowances": self.config["managed_growth"],
                "capacity_baseline": self.state.get("capacity_baseline"),
                "invalidated_leases": self.state.get("invalidated_leases", []),
                "external_risk": "uncontrolled foreign bursts are not reserved or guaranteed",
                "guests": guests}

    def stop(self, vmid: int) -> None:
        if self.status(vmid)["status"] == "running":
            run("qm", "shutdown", str(vmid), "--timeout", "90", "--forceStop", "0")
        if self.status(vmid)["status"] != "stopped":
            raise RuntimeError("guest has not quiesced")
        self.fence_starting(vmid)
        if str(vmid) in self.state.get("runs", {}):
            self.state["runs"][str(vmid)]["phase"] = "quiesced"
        self.state.setdefault("balloon_requests", {}).pop(str(vmid), None)
        self.save()

    def reconcile_snapshot(self, vmid: int, name: str) -> dict | None:
        key = f"{vmid}:{name}"
        record = self.state["snapshots"].get(key)
        if record is None:
            return None
        config = self.owned(vmid)
        if config.get("lock"):
            raise RuntimeError("VM effect is still locked; retain snapshot intent")
        tasks = api(f"/nodes/{self.config['node']}/tasks")
        if any(str(task.get("id")) == str(vmid) and not task.get("endtime")
               and task.get("type") in {"qmsnapshot", "qmdelsnapshot", "qmrollback"} for task in tasks):
            raise RuntimeError("snapshot effect is still active")
        external = next((item for item in api(f"/nodes/{self.config['node']}/qemu/{vmid}/snapshot")
                         if item.get("name") == name), None)
        if external:
            if external.get("description") != record.get("effect_owner", OWNER):
                raise RuntimeError("external snapshot owner collision")
            if record["state"] == "creating":
                record["state"] = "ready"
                self.save()
        elif record["state"] == "deleting":
            del self.state["snapshots"][key]
            self.save()
            return None
        elif record["state"] == "ready":
            raise RuntimeError("tracked ready snapshot is missing externally")
        return record

    def snapshot(self, vmid: int, holder: str, name: str, preserve: bool) -> dict:
        self.require_lease(vmid, holder)
        self.owned(vmid)
        if not re.fullmatch(r"ci-[a-z0-9-]{1,40}", name):
            raise ValueError("snapshot name must use ci- prefix")
        key = f"{vmid}:{name}"
        if key in self.state["snapshots"]:
            record = self.reconcile_snapshot(vmid, name)
            if record is None or record["state"] != "creating":
                if record and record["state"] == "ready":
                    return record
                raise RuntimeError("snapshot deletion is in progress")
        else:
            external = api(f"/nodes/{self.config['node']}/qemu/{vmid}/snapshot")
            if any(item.get("name") == name for item in external):
                raise RuntimeError("untracked snapshot name collision")
            self.state["snapshots"][key] = {"vmid": vmid, "name": name,
                                           "expires": time.time() + 86400, "preserve": preserve,
                                           "effect_owner": OWNER + ":snapshot:" + uuid.uuid4().hex,
                                           "state": "creating"}
            self.save()
        # Stop the guest for application-consistent disks; external effects are
        # never reversible and RAM is deliberately excluded.
        self.stop(vmid)
        run("qm", "snapshot", str(vmid), name, "--vmstate", "0", "--description",
            self.state["snapshots"][key]["effect_owner"])
        record = self.reconcile_snapshot(vmid, name)
        if not record or record["state"] != "ready":
            raise RuntimeError("snapshot effect not yet proven; retain intent")
        return record

    def rollback(self, vmid: int, holder: str, name: str) -> None:
        self.require_lease(vmid, holder)
        self.owned(vmid)
        snapshot = self.reconcile_snapshot(vmid, name) or {}
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
        self.reconcile_snapshot(vmid, name)
        if key not in self.state["snapshots"]:
            return
        self.state["snapshots"][key]["state"] = "deleting"
        self.save()
        if self.reconcile_snapshot(vmid, name) is None:
            return
        run("qm", "delsnapshot", str(vmid), name)
        if self.reconcile_snapshot(vmid, name) is not None:
            raise RuntimeError("snapshot deletion not yet proven")

    def owned_activity(self, expired_only: bool = False, guests: list | None = None,
                       check_ownership: bool = True) -> dict:
        guests = api("/cluster/resources") if guests is None else guests
        candidates = {v.get("vmid") for v in guests
                      if v.get("vmid") in self.config["slots"] and v.get("status") == "running"}
        for vmid in self.config["slots"]:
            key = str(vmid)
            if (self.state.get("runs", {}).get(key, {}).get("phase") in {"starting", "running"}
                    or key in self.state.get("balloon_requests", {})
                    or self.state["leases"].get(key, {}).get("expires", 0) > time.time()):
                candidates.add(vmid)
        activity = {}
        for vmid in sorted(candidates):
            if expired_only and self.state["leases"].get(str(vmid), {}).get("expires", 0) > time.time():
                continue
            status = (self.status(vmid) if check_ownership else
                      api(f"/nodes/{self.config['node']}/qemu/{vmid}/status/current"))
            if not isinstance(status, dict) or status.get("status") not in {"running", "stopped"}:
                raise CapacityUnavailable("owned activity status unavailable; retain promises and baseline")
            phase = self.state.get("runs", {}).get(str(vmid), {}).get("phase")
            activity[vmid] = {"status": status["status"], "status_data": status, "phase": phase,
                              "promise": phase in {"starting", "running"}
                              or str(vmid) in self.state.get("balloon_requests", {})}
        return activity

    def guard(self) -> dict:
        before = available_mib()
        stopped = []
        try:
            sample = self.capacity_sample()
            safety = self.capacity_safety(sample, include_hold=False, purpose="functional")
        except (OSError, RuntimeError, ValueError, IndexError, subprocess.SubprocessError) as error:
            sample = None
            safety = {"reasons": ["capacity-measurement-unavailable"], "error": str(error)}
        activity = self.owned_activity()
        retention = None
        if sample:
            try:
                own = self.owned_commitments(activity)
                margin = sum(v["allowance_mib"] for v in self.config["managed_growth"].values())
                available = min(before, sample["available_mib"], available_mib())
                projected = available - own["outstanding_commitments_mib"] - margin
                retention = {**own, "available_mib": available, "growth_margin_mib": margin,
                             "projected_available_mib": projected,
                             "required_available_mib": self.config["reserve_mib"] + 256}
                if projected < self.config["reserve_mib"] + 256:
                    safety["reasons"].append("owned-retention-headroom")
                if own["active"] > self.config["max_active"]:
                    safety["reasons"].append("owned-active-limit")
            except (OSError, RuntimeError, ValueError, IndexError, subprocess.SubprocessError) as error:
                safety["reasons"].append("owned-commitment-measurement-unavailable")
                safety["commitment_error"] = str(error)
        hard_reasons = list(safety["reasons"])
        fatal_reasons = set(hard_reasons)
        pending = []
        for vmid, current in activity.items():
            lease = self.state["leases"].get(str(vmid))
            invalidated = any(f.get("vmid") == vmid
                              and (f.get("lease") or {}).get("holder") == (lease or {}).get("holder")
                              for f in self.state.get("invalidated_leases", []))
            reasons = list(hard_reasons)
            if self.lease_purpose(vmid) != "functional":
                reasons.extend(safety.get("growth_reasons", []))
            if current["phase"] == "running" and current["status"] == "stopped":
                reasons.append("owned-run-unexpectedly-stopped")
            affected = current["status"] == "running" or current["promise"]
            if affected and (reasons or invalidated):
                fatal_reasons.update(reasons)
                # Invalidation survives a late start and later pressure recovery.
                self.owned(vmid)
                if not invalidated:
                    self.state.setdefault("invalidated_leases", []).append({
                        "vmid": vmid, "time": time.time(), "reasons": reasons,
                        "available_mib": available_mib(), "lease": dict(lease) if lease else None,
                        "capacity_evidence": deepcopy({"sample": sample, "retention": retention,
                                                       "owned_activity": activity})})
                self.save()
                try:
                    self.stop(vmid)
                    stopped.append(vmid)
                    current.update(status="stopped", phase="quiesced", promise=False)
                except CapacityUnavailable as error:
                    pending.append({"vmid": vmid, "error": str(error)})
        safety["reasons"] = sorted(fatal_reasons)
        if hard_reasons:
            self.state["capacity_hold_until"] = time.time() + 15
        if sample:
            self.state["capacity_sample"] = sample
        if not any(v["status"] == "running" or v["promise"] for v in activity.values()):
            self.state.pop("capacity_baseline", None)
        self.save()
        return {"available_before_mib": before, "available_after_mib": available_mib(),
                "reserve_mib": self.config["reserve_mib"], "pressure_stopped": stopped,
                "quiescence_pending": pending, "owned_activity": activity,
                "capacity_safety": safety, "retention": retention, "sample": sample}

    def reap(self) -> dict:
        now = time.time()
        stopped, removed = [], []
        activity = self.owned_activity(expired_only=True)
        pending = []
        for vmid in self.config["slots"]:
            lease = self.state["leases"].get(str(vmid), {})
            if lease.get("expires", 0) > now:
                continue
            last = self.state["last_used"].get(str(vmid), now)
            current = activity.get(vmid, {})
            if (current.get("status") == "running" or current.get("promise")) and now - last >= self.config.get("idle_seconds", 900):
                try:
                    self.stop(vmid)
                    stopped.append(vmid)
                except CapacityUnavailable as error:
                    pending.append({"vmid": vmid, "error": str(error)})
        for key, snapshot in list(self.state["snapshots"].items()):
            vmid = snapshot["vmid"]
            lease = self.state["leases"].get(str(vmid), {})
            if snapshot["preserve"] or snapshot["expires"] > now or lease.get("expires", 0) > now:
                continue
            if self.reconcile_snapshot(vmid, snapshot["name"]) is not None:
                self.state["snapshots"][key]["state"] = "deleting"
                self.save()
                if self.reconcile_snapshot(vmid, snapshot["name"]) is not None:
                    run("qm", "delsnapshot", str(vmid), snapshot["name"])
                    if self.reconcile_snapshot(vmid, snapshot["name"]) is not None:
                        raise RuntimeError("expired snapshot deletion not proven")
            removed.append(key)
        self.save()
        return {"idle_stopped": stopped, "expired_snapshots_removed": removed, "quiescence_pending": pending}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("action", choices=["status", "telemetry", "admit", "lease", "release", "start", "defer-start", "resize", "stop", "snapshot", "rollback", "discard-snapshot", "guard", "reap"])
    parser.add_argument("--vmid", type=int)
    parser.add_argument("--holder", default="")
    parser.add_argument("--seconds", type=int, default=1800)
    parser.add_argument("--memory-mib", type=int, default=2048)
    parser.add_argument("--purpose", choices=["functional", "measurement"])
    parser.add_argument("--name", default="")
    parser.add_argument("--preserve", action="store_true")
    args = parser.parse_args()
    if args.action in {"lease", "defer-start"} and args.purpose is None:
        parser.error("--purpose is required for new lease and deferred requests")
    with state_lock(args.state):
        lab = Lab(json.loads(args.config.read_text()), args.state)
        if args.action == "status":
            result = {str(v): lab.status(v) for v in lab.config["slots"]}
        elif args.action == "telemetry":
            result = lab.telemetry()
        elif args.action == "admit":
            result = lab.resources(args.memory_mib, purpose=args.purpose or "measurement", cold_start=True)
        elif args.action == "lease":
            result = lab.lease(args.vmid, args.holder, args.seconds, args.purpose)
        elif args.action == "start":
            result = lab.start(args.vmid, args.holder, args.memory_mib)
        elif args.action == "defer-start":
            result = lab.defer_start(args.vmid, args.holder, args.seconds, args.memory_mib, args.purpose)
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
