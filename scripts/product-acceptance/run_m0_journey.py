#!/usr/bin/env python3
"""Run the token-free M0 product journey against a loopback HTTP surface."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import ipaddress
import json
import math
import os
from pathlib import Path
import re
import socket
import stat
import sys
import tempfile
import time
from typing import Any
from urllib import error, parse, request
import uuid


SCHEMA_VERSION = 1
SCHEMA_VERSION_V2 = 2
SUPPORTED_SCHEMA_VERSIONS = {SCHEMA_VERSION, SCHEMA_VERSION_V2}
SAFE_ROOT = Path("/work/tmp/project-sentinel")
MAX_RESPONSE_BYTES = 1024 * 1024
MAX_REQUEST_BYTES = 1024 * 1024
MAX_TIMEOUT_SECONDS = 30.0
MAX_QUERY_BYTES = 4096
MIN_SECRET_BYTES = 16
MAX_SECRET_BYTES = 4096
MAX_OBSERVE_ATTEMPTS = 300
MAX_OBSERVE_INTERVAL_MS = 2_000
MAX_OBSERVE_ELAPSED_MS = 300_000
OBSERVE_RETRY_STATUSES = {404, 409, 425, 429}
ALLOWED_ROLES = {"agent", "customer", "none", "operator"}
ALLOWED_ROUTE_ROLES = ALLOWED_ROLES | {"company"}
NO_AUTH_PATHS = {"/health", "/readiness"}
PHASES = (
    "readiness",
    "customer_request",
    "governed_project",
    "workbench_execution",
    "qa_release",
    "delivery",
    "acceptance",
)
MUTATING_PHASES = PHASES[1:]
SAFE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:-]{0,239}$")
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
DELIVERY_DIGEST_DOMAIN_RE = re.compile(r"^[a-z0-9-]{1,96}$")
ENV_NAME_RE = re.compile(r"^[A-Z][A-Z0-9_]{0,127}$")
SENSITIVE_KEY_RE = re.compile(
    r"(?:authorization|cookie|credential|password|prompt|secret|token)", re.IGNORECASE
)
QUERY_KEY_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_.:-]{0,63}$")
ZERO_DIGEST = "0" * 64


class JourneyError(RuntimeError):
    """A public-safe, fail-closed journey error."""


class DuplicateJsonKey(ValueError):
    """Raised when JSON contains an ambiguous duplicate object key."""


class NoRedirectHandler(request.HTTPRedirectHandler):
    """Reject every redirect before urllib constructs a follow-up request."""

    def redirect_request(
        self,
        req: request.Request,
        fp: Any,
        code: int,
        msg: str,
        headers: Any,
        newurl: str,
    ) -> None:
        del req, fp, code, msg, headers, newurl
        return None


HTTP_OPENER = request.build_opener(request.ProxyHandler({}), NoRedirectHandler())


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateJsonKey(key)
        result[key] = value
    return result


def decode_json(data: bytes, label: str) -> Any:
    try:
        return json.loads(
            data.decode("utf-8"), object_pairs_hook=reject_duplicate_keys
        )
    except (UnicodeError, json.JSONDecodeError, DuplicateJsonKey) as exc:
        raise JourneyError(f"{label} is not strict JSON") from exc


def canonical_json(value: Any) -> bytes:
    try:
        encoded = json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=True,
            separators=(",", ":"),
            sort_keys=True,
        )
    except (TypeError, ValueError) as exc:
        raise JourneyError("value is not canonical JSON") from exc
    return encoded.encode("ascii")


def digest(value: Any) -> str:
    return hashlib.sha256(canonical_json(value)).hexdigest()


def delivery_digest(record_type: str, schema_version: int, value: Any) -> str:
    if (
        not isinstance(record_type, str)
        or not DELIVERY_DIGEST_DOMAIN_RE.fullmatch(record_type)
        or not isinstance(schema_version, int)
        or isinstance(schema_version, bool)
        or not 1 <= schema_version <= 0xFFFF
    ):
        raise JourneyError("delivery digest domain is invalid")
    encoded = canonical_json(value)
    hasher = hashlib.sha256()
    hasher.update(b"sentinel.delivery.digest\0")
    hasher.update(schema_version.to_bytes(2, "big"))
    hasher.update(len(record_type).to_bytes(4, "big"))
    hasher.update(record_type.encode("ascii"))
    hasher.update(len(encoded).to_bytes(8, "big"))
    hasher.update(encoded)
    return hasher.hexdigest()


def stable_operation_id(
    journey_id: str, step_id: str, schema_version: int = SCHEMA_VERSION
) -> str:
    suffix = hashlib.sha256(f"{journey_id}\0{step_id}".encode("ascii")).hexdigest()[:24]
    if schema_version == SCHEMA_VERSION_V2:
        raw = bytearray(
            hashlib.sha256(f"{journey_id}\0{step_id}".encode("ascii")).digest()[:16]
        )
        raw[6] = (raw[6] & 0x0F) | 0x50
        raw[8] = (raw[8] & 0x3F) | 0x80
        return str(uuid.UUID(bytes=bytes(raw)))
    return f"m0-{suffix}"


def safe_output_path(raw_path: str, label: str) -> Path:
    if not isinstance(raw_path, str):
        raise JourneyError(f"{label} path must be text")
    path = Path(raw_path)
    if not path.is_absolute():
        raise JourneyError(f"{label} path must be absolute")
    if ".." in path.parts:
        raise JourneyError(f"{label} path must not contain parent traversal")
    if path.suffix != ".json":
        raise JourneyError(f"{label} path must end in .json")
    root = SAFE_ROOT.absolute()
    try:
        relative = path.relative_to(root)
    except ValueError as exc:
        raise JourneyError(f"{label} path must be below {SAFE_ROOT}") from exc
    current = root
    for part in relative.parts:
        current = current / part
        if current.is_symlink():
            raise JourneyError(f"{label} path must not traverse a symlink")

    resolved = path.resolve(strict=False)
    root = root.resolve(strict=False)
    if not resolved.is_relative_to(root) or resolved == root:
        raise JourneyError(f"{label} path must be below {SAFE_ROOT}")
    return resolved


def validate_owner_only_node(path: Path, label: str, *, directory: bool) -> None:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise JourneyError(f"{label} metadata is unavailable") from exc
    expected = stat.S_ISDIR if directory else stat.S_ISREG
    if (
        not expected(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or stat.S_IMODE(metadata.st_mode) & 0o077
        or (not directory and metadata.st_nlink != 1)
    ):
        raise JourneyError(f"{label} must be an owner-only regular node")


def ensure_output_parent(path: Path) -> None:
    missing: list[Path] = []
    current = path.parent
    while current != SAFE_ROOT and not current.exists():
        missing.append(current)
        current = current.parent
    if current != SAFE_ROOT and not current.exists():
        raise JourneyError(f"output parent must be below {SAFE_ROOT}")
    for directory in reversed(missing):
        directory.mkdir(mode=0o700)
    current = SAFE_ROOT
    for part in path.parent.relative_to(SAFE_ROOT).parts:
        current /= part
        validate_owner_only_node(current, "output parent", directory=True)


def validate_existing_output(path: Path, label: str) -> None:
    ensure_output_parent(path)
    if path.exists() or path.is_symlink():
        validate_owner_only_node(path, label, directory=False)


def lock_path_for(journey_id: str) -> Path:
    suffix = hashlib.sha256(journey_id.encode("ascii")).hexdigest()[:32]
    return SAFE_ROOT / f".m0-journey-{suffix}.lock"


@contextmanager
def exclusive_journey_lock(journey_id: str) -> Any:
    path = lock_path_for(journey_id)
    ensure_output_parent(path)
    flags = os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags, 0o600)
    except OSError as exc:
        raise JourneyError("journey lock is unavailable") from exc
    try:
        validate_owner_only_node(path, "journey lock", directory=False)
        opened = os.fstat(descriptor)
        named = path.lstat()
        if (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino):
            raise JourneyError("journey lock identity changed during acquisition")
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise JourneyError("another journey runner holds the exclusive lock") from exc
        yield
    finally:
        os.close(descriptor)


def atomic_json_write(path: Path, value: Any) -> None:
    validate_existing_output(path, "output file")
    data = canonical_json(value) + b"\n"
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, stat.S_IRUSR | stat.S_IWUSR)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if temporary.exists():
            temporary.unlink()


def load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = decode_json(path.read_bytes(), label)
    except OSError as exc:
        raise JourneyError(f"{label} is unavailable or invalid JSON") from exc
    if not isinstance(value, dict):
        raise JourneyError(f"{label} must contain a JSON object")
    return value


def validate_base_url(base_url: str) -> str:
    try:
        parsed = parse.urlsplit(base_url)
    except (TypeError, ValueError) as exc:
        raise JourneyError("base URL is invalid") from exc
    if (
        parsed.scheme != "http"
        or parsed.hostname not in {"127.0.0.1", "::1"}
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path not in {"", "/"}
    ):
        raise JourneyError("base URL must be an uncredentialed loopback HTTP origin")
    try:
        port = parsed.port
    except ValueError as exc:
        raise JourneyError("base URL port is invalid") from exc
    if port is None:
        raise JourneyError("base URL must include an explicit port")
    if not 1 <= port <= 65535:
        raise JourneyError("base URL port is outside the valid range")
    host = "[::1]" if parsed.hostname == "::1" else "127.0.0.1"
    return f"http://{host}:{port}"


def validate_timeout(timeout: float) -> float:
    if (
        not isinstance(timeout, (int, float))
        or isinstance(timeout, bool)
        or not math.isfinite(timeout)
        or not 0.05 <= timeout <= MAX_TIMEOUT_SECONDS
    ):
        raise JourneyError(
            f"timeout must be between 0.05 and {MAX_TIMEOUT_SECONDS:g} seconds"
        )
    return timeout


def validate_credentials(
    raw_values: list[str], schema_version: int = SCHEMA_VERSION
) -> dict[str, Any]:
    credentials: dict[str, Any] = {}
    for raw in raw_values:
        identity, separator, env_name = raw.partition("=")
        if schema_version == SCHEMA_VERSION:
            role = identity
            if (
                not separator
                or role not in ALLOWED_ROLES - {"none"}
                or not ENV_NAME_RE.fullmatch(env_name)
                or role in credentials
                or env_name in credentials.values()
            ):
                raise JourneyError(
                    "credential references must be unique ROLE=ENV_NAME pairs"
                )
            credentials[role] = env_name
            continue
        if schema_version != SCHEMA_VERSION_V2:
            raise JourneyError("plan schema_version is unsupported")
        alias, role_separator, role = identity.partition(":")
        if (
            not separator
            or not role_separator
            or not SAFE_ID_RE.fullmatch(alias)
            or role not in ALLOWED_ROLES - {"none"}
            or not ENV_NAME_RE.fullmatch(env_name)
            or alias in credentials
            or any(binding["env"] == env_name for binding in credentials.values())
        ):
            raise JourneyError(
                "credential references must be unique ALIAS:ROLE=ENV_NAME triples"
            )
        credentials[alias] = {"role": role, "env": env_name}
    return credentials


def validate_path(path: object, step_id: str) -> str:
    parsed = parse.urlsplit(path) if isinstance(path, str) else None
    if (
        not isinstance(path, str)
        or not path.startswith("/")
        or path.startswith("//")
        or "://" in path
        or "%" in path
        or "\\" in path
        or not path.isascii()
        or any(ord(character) < 0x21 or ord(character) == 0x7F for character in path)
        or any(character.isspace() for character in path)
        or parsed is None
        or parsed.query
        or parsed.fragment
        or any(part in {"", ".", ".."} for part in parsed.path.split("/")[1:])
    ):
        raise JourneyError(f"step {step_id} has an invalid relative HTTP path")
    return path


def derived_route_role(path: str) -> str:
    if path in NO_AUTH_PATHS:
        return "none"
    for role in ("customer", "operator", "agent"):
        if path.startswith(f"/{role}/"):
            return role
    if path.startswith("/company/"):
        return "company"
    raise JourneyError("HTTP path is outside the authority route contract")


def credential_role_allowed(route_role: str, credential_role: str) -> bool:
    if route_role == "none":
        return credential_role == "none"
    if route_role in {"customer", "agent"}:
        return credential_role == route_role
    if route_role == "operator":
        return credential_role in {"agent", "operator"}
    if route_role == "company":
        return credential_role in {"agent", "customer", "operator"}
    return False


def validate_public_structure(value: Any, label: str) -> None:
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str) or SENSITIVE_KEY_RE.search(key):
                raise JourneyError(f"{label} contains a sensitive or invalid key")
            validate_public_structure(item, label)
    elif isinstance(value, list):
        for item in value:
            validate_public_structure(item, label)
    elif isinstance(value, str):
        if not value.isascii() or any(ord(character) < 0x20 for character in value):
            raise JourneyError(f"{label} contains unsafe text")


def validate_query(
    query: object, available_references: set[str], step_id: str
) -> dict[str, Any]:
    if query is None:
        return {}
    if not isinstance(query, dict):
        raise JourneyError(f"step {step_id} query must be an object")
    for key, value in query.items():
        if (
            not isinstance(key, str)
            or not QUERY_KEY_RE.fullmatch(key)
            or SENSITIVE_KEY_RE.search(key)
        ):
            raise JourneyError(f"step {step_id} query has an invalid key")
        validate_template(value, available_references, f"step {step_id} query")
    return query


def encode_query(query: dict[str, Any]) -> str:
    pairs: list[tuple[str, str]] = []
    for key in sorted(query):
        value = query[key]
        if isinstance(value, bool):
            rendered = "true" if value else "false"
        elif isinstance(value, int) and not isinstance(value, bool) and value >= 0:
            rendered = str(value)
        elif isinstance(value, str) and SAFE_ID_RE.fullmatch(value):
            rendered = value
        else:
            raise JourneyError("resolved query contains an unsafe value")
        pairs.append((key, rendered))
    encoded = parse.urlencode(pairs, doseq=False)
    if len(encoded.encode("ascii")) > MAX_QUERY_BYTES:
        raise JourneyError("resolved query exceeds the size limit")
    return encoded


def validate_template(value: Any, available_references: set[str], label: str) -> None:
    if isinstance(value, list):
        for item in value:
            validate_template(item, available_references, label)
        return
    if not isinstance(value, dict):
        return
    if "$operation_id" in value:
        if value != {"$operation_id": True}:
            raise JourneyError(f"{label} has an invalid operation-ID template")
        return
    if "$ref" in value:
        if set(value) != {"$ref"} or value["$ref"] not in available_references:
            raise JourneyError(f"{label} references an unavailable capture")
        return
    if "$delivery_digest" in value:
        if set(value) != {"$delivery_digest"}:
            raise JourneyError(f"{label} has an invalid delivery-digest template")
        specification = value["$delivery_digest"]
        if not isinstance(specification, dict) or set(specification) != {
            "record_type",
            "schema_version",
            "value",
        }:
            raise JourneyError(f"{label} has an invalid delivery-digest template")
        delivery_digest(
            specification["record_type"], specification["schema_version"], {}
        )
        validate_template(specification["value"], available_references, label)
        validate_public_structure(specification["value"], label)
        return
    for item in value.values():
        validate_template(item, available_references, label)


def validate_assertions(
    assertions: object,
    available_references: set[str],
    step_id: str,
    field: str,
    reject_sensitive_pointer: bool,
) -> None:
    if not isinstance(assertions, list):
        raise JourneyError(f"step {step_id} {field} must be an array")
    for assertion in assertions:
        if not isinstance(assertion, dict) or "pointer" not in assertion:
            raise JourneyError(f"step {step_id} has an invalid {field} assertion")
        if set(assertion) not in ({"pointer", "present"}, {"pointer", "equals"}):
            raise JourneyError(f"step {step_id} {field} assertion shape is invalid")
        pointer = validate_pointer(
            assertion["pointer"],
            f"step {step_id} {field}",
            require_non_root=reject_sensitive_pointer,
        )
        pointer_parts = decode_pointer_parts(pointer)
        if reject_sensitive_pointer and any(
            SENSITIVE_KEY_RE.search(part) for part in pointer_parts
        ):
            raise JourneyError(f"step {step_id} {field} targets sensitive data")
        if "present" in assertion and assertion["present"] is not True:
            raise JourneyError("assertion present must be true")
        if "equals" in assertion:
            validate_template(
                assertion["equals"],
                available_references,
                f"step {step_id} {field}",
            )
            validate_public_structure(
                assertion["equals"], f"step {step_id} {field} equals"
            )
            if not public_safe(assertion["equals"]):
                raise JourneyError(
                    f"step {step_id} {field} equals is not public-safe"
                )


def validate_observe_contract(value: object, step_id: str) -> None:
    if not isinstance(value, dict) or set(value) != {
        "interval_ms",
        "max_attempts",
        "max_elapsed_ms",
        "replay",
        "retry_statuses",
    }:
        raise JourneyError(f"step {step_id} has an invalid observe contract")
    attempts = value["max_attempts"]
    interval = value["interval_ms"]
    elapsed = value["max_elapsed_ms"]
    retry_statuses = value["retry_statuses"]
    if (
        not isinstance(attempts, int)
        or isinstance(attempts, bool)
        or not 1 <= attempts <= MAX_OBSERVE_ATTEMPTS
        or not isinstance(interval, int)
        or isinstance(interval, bool)
        or not 0 <= interval <= MAX_OBSERVE_INTERVAL_MS
        or not isinstance(elapsed, int)
        or isinstance(elapsed, bool)
        or not 50 <= elapsed <= MAX_OBSERVE_ELAPSED_MS
        or value["replay"] != "exact_status_and_captures"
        or not isinstance(retry_statuses, list)
        or len(retry_statuses) != len(set(retry_statuses))
        or any(
            not isinstance(status, int)
            or isinstance(status, bool)
            or status not in OBSERVE_RETRY_STATUSES
            for status in retry_statuses
        )
    ):
        raise JourneyError(f"step {step_id} has unsafe observe bounds")


def validate_plan(plan: dict[str, Any]) -> None:
    schema_version = plan.get("schema_version")
    if schema_version not in SUPPORTED_SCHEMA_VERSIONS:
        raise JourneyError("plan schema_version is unsupported")
    journey_id = plan.get("journey_id")
    if not isinstance(journey_id, str) or not SAFE_ID_RE.fullmatch(journey_id):
        raise JourneyError("plan journey_id is invalid")
    if plan.get("provider_mode") != "token_free":
        raise JourneyError("the M0 journey runner permits token_free provider mode only")
    if set(plan) - {"schema_version", "journey_id", "provider_mode", "steps"}:
        raise JourneyError("plan contains unknown top-level fields")

    steps = plan.get("steps")
    if not isinstance(steps, list) or not steps:
        raise JourneyError("plan steps must be a non-empty array")
    step_ids: set[str] = set()
    seen_phases: list[str] = []
    positive_phases: set[str] = set()
    checkpoint_phases: set[str] = set()
    negative_steps = 0
    last_phase_index = -1
    available_references: set[str] = set()
    checkpoints: set[str] = set()
    allowed_fields = {
        "allow_route_mismatch",
        "assertions",
        "body",
        "capture",
        "checkpoint",
        "expected_status",
        "id",
        "kind",
        "method",
        "path",
        "phase",
        "provider_call",
        "query",
        "role",
        "route_role",
    }
    if schema_version == SCHEMA_VERSION_V2:
        allowed_fields = (allowed_fields - {"role"}) | {
            "credential_alias",
            "initial_assertions",
            "observe",
            "replay_assertions",
        }
    for raw_step in steps:
        if not isinstance(raw_step, dict):
            raise JourneyError("every plan step must be an object")
        if set(raw_step) - allowed_fields:
            raise JourneyError("plan step contains unknown fields")
        step_id = raw_step.get("id")
        if (
            not isinstance(step_id, str)
            or not SAFE_ID_RE.fullmatch(step_id)
            or step_id in step_ids
        ):
            raise JourneyError("plan step IDs must be unique public-safe values")
        step_ids.add(step_id)

        phase = raw_step.get("phase")
        if phase not in PHASES:
            raise JourneyError(f"step {step_id} has an unknown phase")
        phase_index = PHASES.index(phase)
        if phase_index < last_phase_index:
            raise JourneyError("plan phases must follow the canonical M0 order")
        last_phase_index = phase_index
        if phase not in seen_phases:
            seen_phases.append(phase)

        kind = raw_step.get("kind", "positive")
        if kind not in {"readiness", "positive", "negative", "observe"}:
            raise JourneyError(f"step {step_id} has an unknown kind")
        if phase == "readiness" and kind != "readiness":
            raise JourneyError("the readiness phase accepts readiness steps only")
        if phase != "readiness" and kind == "readiness":
            raise JourneyError("readiness steps must remain in the readiness phase")
        if phase == "readiness" and kind == "observe":
            raise JourneyError("readiness must retain its explicit readiness kind")
        if kind == "positive":
            positive_phases.add(phase)
        if kind == "negative":
            negative_steps += 1

        method = raw_step.get("method")
        if method not in {"GET", "POST"}:
            raise JourneyError(f"step {step_id} must use GET or POST")
        path = validate_path(raw_step.get("path"), step_id)
        route_authority = derived_route_role(path)
        route_role = raw_step.get("route_role")
        if route_role not in ALLOWED_ROUTE_ROLES:
            raise JourneyError(f"step {step_id} has an invalid route authority role")
        if route_role != route_authority:
            raise JourneyError(f"step {step_id} spoofs its derived route authority")
        allow_mismatch = raw_step.get("allow_route_mismatch") is True
        if schema_version == SCHEMA_VERSION:
            role = raw_step.get("role")
            if role not in ALLOWED_ROLES:
                raise JourneyError(f"step {step_id} has an invalid credential role")
            mismatch = role != route_role
            if mismatch and not (kind == "negative" and allow_mismatch):
                raise JourneyError(f"step {step_id} crosses its authenticated route")
            if allow_mismatch and (kind != "negative" or not mismatch):
                raise JourneyError(
                    "allow_route_mismatch is valid only for an explicit negative"
                )
        else:
            alias = raw_step.get("credential_alias")
            if route_role == "none":
                if alias is not None:
                    raise JourneyError(
                        f"step {step_id} must not select a credential for a no-auth route"
                    )
            elif not isinstance(alias, str) or not SAFE_ID_RE.fullmatch(alias):
                raise JourneyError(f"step {step_id} has an invalid credential alias")
            if allow_mismatch and kind != "negative":
                raise JourneyError(
                    "allow_route_mismatch is valid only for an explicit negative"
                )

        expected = raw_step.get("expected_status", [200])
        if (
            not isinstance(expected, list)
            or not expected
            or any(
                not isinstance(status, int)
                or isinstance(status, bool)
                or not 100 <= status <= 599
                for status in expected
            )
        ):
            raise JourneyError(f"step {step_id} has invalid expected_status values")
        if allow_mismatch and any(status not in {401, 403, 405} for status in expected):
            raise JourneyError("route-separation probes must expect only 401, 403, or 405")
        if kind in {"readiness", "positive", "observe"} and any(
            not 200 <= status <= 299 for status in expected
        ):
            raise JourneyError("readiness, positive, and observe steps must expect only 2xx")
        if kind == "negative" and any(
            not 400 <= status <= 499 for status in expected
        ):
            raise JourneyError("negative steps must expect only 4xx")
        if raw_step.get("provider_call") not in {None, False}:
            raise JourneyError("provider calls are forbidden in the token-free runner")
        if method == "GET" and raw_step.get("body") is not None:
            raise JourneyError(f"GET step {step_id} must not carry a body")
        if method == "POST" and not isinstance(raw_step.get("body"), dict):
            raise JourneyError(f"POST step {step_id} requires an object body")
        if method == "POST":
            validate_template(
                raw_step["body"], available_references, f"step {step_id} body"
            )
            validate_public_structure(raw_step["body"], f"step {step_id} body")
        if kind == "readiness":
            if (
                method != "GET"
                or not isinstance(raw_step.get("assertions"), list)
                or not any(
                assertion == {"pointer": "/ready", "equals": True}
                for assertion in raw_step.get("assertions", [])
                )
            ):
                raise JourneyError("readiness requires GET and an explicit ready assertion")
        if kind == "positive":
            if method != "POST":
                raise JourneyError("positive M0 commands must use POST")
            stable_keys = {
                key
                for key in ("operation_id", "idempotency_key")
                if raw_step["body"].get(key) == {"$operation_id": True}
            }
            if len(stable_keys) != 1:
                raise JourneyError(
                    "positive M0 commands require exactly one stable operation key"
                )
        if kind == "observe":
            if schema_version != SCHEMA_VERSION_V2:
                raise JourneyError("observe steps require plan schema_version 2")
            if method != "GET":
                raise JourneyError("observe steps must use GET")
            validate_observe_contract(raw_step.get("observe"), step_id)
        elif raw_step.get("observe") is not None:
            raise JourneyError("observe bounds are valid only for observe steps")

        validate_query(raw_step.get("query"), available_references, step_id)

        capture = raw_step.get("capture", {})
        if not isinstance(capture, dict):
            raise JourneyError(f"step {step_id} capture must be an object")
        for name, specification in capture.items():
            if (
                not isinstance(name, str)
                or not SAFE_ID_RE.fullmatch(name)
                or not isinstance(specification, dict)
            ):
                raise JourneyError(f"step {step_id} has an invalid capture")
            if set(specification) != {"pointer", "type"}:
                raise JourneyError(f"step {step_id} capture must declare pointer and type")
            pointer = validate_pointer(
                specification["pointer"],
                f"step {step_id} capture",
                require_non_root=schema_version == SCHEMA_VERSION_V2,
            )
            pointer_parts = decode_pointer_parts(pointer)
            if SENSITIVE_KEY_RE.search(name) or any(
                SENSITIVE_KEY_RE.search(part) for part in pointer_parts
            ):
                raise JourneyError(f"step {step_id} capture targets sensitive data")
            if specification["type"] not in {
                "boolean",
                "digest",
                "id",
                "integer",
                "state",
            }:
                raise JourneyError(f"step {step_id} capture type is unsupported")
        if kind == "positive" and not capture:
            raise JourneyError("positive M0 commands require at least one typed capture")

        assertion_fields = ["assertions"]
        if schema_version == SCHEMA_VERSION_V2:
            assertion_fields.extend(["initial_assertions", "replay_assertions"])
        for assertion_field in assertion_fields:
            validate_assertions(
                raw_step.get(assertion_field, []),
                available_references,
                step_id,
                assertion_field,
                schema_version == SCHEMA_VERSION_V2,
            )
        if kind == "observe" and not capture and not raw_step.get("assertions"):
            raise JourneyError("observe steps require an assertion or typed capture")
        if schema_version == SCHEMA_VERSION_V2 and kind == "positive":
            if not raw_step.get("initial_assertions") or not raw_step.get(
                "replay_assertions"
            ):
                raise JourneyError(
                    "schema v2 positive commands require initial_assertions and replay_assertions"
                )
        elif schema_version == SCHEMA_VERSION_V2 and (
            raw_step.get("initial_assertions") is not None
            or raw_step.get("replay_assertions") is not None
        ):
            raise JourneyError(
                "initial_assertions and replay_assertions are valid only for positive commands"
            )

        available_references.update(f"{step_id}.{name}" for name in capture)

        checkpoint = raw_step.get("checkpoint")
        if kind != "positive" and checkpoint is not None:
            raise JourneyError("only positive M0 commands may own checkpoints")
        if kind == "positive" and checkpoint is None:
            raise JourneyError("every positive M0 command requires a checkpoint")
        if checkpoint is not None:
            if not isinstance(checkpoint, str) or not SAFE_ID_RE.fullmatch(checkpoint):
                raise JourneyError(f"step {step_id} checkpoint is invalid")
            if checkpoint in checkpoints:
                raise JourneyError("plan checkpoints must be unique")
            checkpoints.add(checkpoint)
            checkpoint_phases.add(phase)

    if seen_phases != list(PHASES):
        raise JourneyError("plan must contain every canonical M0 phase in order")
    if positive_phases != set(MUTATING_PHASES):
        raise JourneyError("plan must contain a positive step for every M0 mutation phase")
    if not set(MUTATING_PHASES).issubset(checkpoint_phases):
        raise JourneyError("every mutating M0 phase requires a restart checkpoint")
    if negative_steps == 0:
        raise JourneyError("plan must contain at least one explicit negative probe")


def decode_pointer_parts(pointer: str) -> list[str]:
    if pointer == "":
        return []
    parts: list[str] = []
    for encoded_part in pointer[1:].split("/"):
        index = 0
        while index < len(encoded_part):
            if encoded_part[index] != "~":
                index += 1
                continue
            if index + 1 >= len(encoded_part) or encoded_part[index + 1] not in "01":
                raise JourneyError("JSON pointer contains an invalid escape")
            index += 2
        parts.append(encoded_part.replace("~1", "/").replace("~0", "~"))
    return parts


def validate_pointer(
    pointer: object, label: str, *, require_non_root: bool = False
) -> str:
    if not isinstance(pointer, str) or (pointer != "" and not pointer.startswith("/")):
        raise JourneyError(f"{label} has an invalid JSON pointer")
    try:
        parts = decode_pointer_parts(pointer)
    except JourneyError as exc:
        raise JourneyError(f"{label} has an invalid JSON pointer escape") from exc
    if require_non_root and (not pointer or not any(parts)):
        raise JourneyError(f"{label} must target a non-root JSON pointer")
    return pointer


def pointer_get(value: Any, pointer: str) -> Any:
    if pointer == "":
        return value
    current = value
    for part in decode_pointer_parts(pointer):
        if isinstance(current, dict) and part in current:
            current = current[part]
        elif isinstance(current, list) and part.isdigit() and int(part) < len(current):
            current = current[int(part)]
        else:
            raise JourneyError("response is missing a required JSON pointer")
    return current


def validate_capture(value: Any, capture_type: str) -> Any:
    if capture_type == "boolean":
        if not isinstance(value, bool):
            raise JourneyError("captured boolean has the wrong type")
        return value
    if capture_type == "integer":
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise JourneyError("captured integer has the wrong type")
        return value
    if capture_type == "digest":
        if not isinstance(value, str) or not DIGEST_RE.fullmatch(value):
            raise JourneyError("captured digest is invalid")
        return value
    if capture_type in {"id", "state"}:
        if not isinstance(value, str) or not SAFE_ID_RE.fullmatch(value):
            raise JourneyError(f"captured {capture_type} is not public-safe")
        return value
    raise JourneyError("capture type is unsupported")


def resolve_template(value: Any, references: dict[str, Any], operation_id: str) -> Any:
    if isinstance(value, list):
        return [resolve_template(item, references, operation_id) for item in value]
    if isinstance(value, dict):
        if set(value) == {"$operation_id"} and value["$operation_id"] is True:
            return operation_id
        if set(value) == {"$ref"}:
            reference = value["$ref"]
            if not isinstance(reference, str) or reference not in references:
                raise JourneyError("request references an unavailable capture")
            return references[reference]
        if set(value) == {"$delivery_digest"}:
            specification = value["$delivery_digest"]
            if not isinstance(specification, dict):
                raise JourneyError("delivery digest template is invalid")
            resolved = resolve_template(
                specification.get("value"), references, operation_id
            )
            return delivery_digest(
                specification.get("record_type"),
                specification.get("schema_version"),
                resolved,
            )
        return {
            key: resolve_template(item, references, operation_id)
            for key, item in value.items()
        }
    return value


def response_is_unavailable(status: int, payload: Any) -> bool:
    if status == 503:
        return True
    if not isinstance(payload, dict):
        return False
    code = payload.get("code")
    state = payload.get("status")
    return (
        isinstance(code, str)
        and (code.endswith("_unavailable") or code == "adapter_unavailable")
    ) or state == "unavailable"


def response_socket(response: Any) -> socket.socket | None:
    candidates = (
        getattr(getattr(getattr(response, "fp", None), "raw", None), "_sock", None),
        getattr(getattr(response, "fp", None), "_sock", None),
    )
    return next(
        (candidate for candidate in candidates if isinstance(candidate, socket.socket)),
        None,
    )


def validate_response_peer(response: Any, base_url: str) -> None:
    peer_socket = response_socket(response)
    if peer_socket is None:
        return
    try:
        peer = peer_socket.getpeername()
    except OSError as exc:
        raise JourneyError("HTTP peer identity is unavailable") from exc
    parsed = parse.urlsplit(base_url)
    try:
        peer_ip = ipaddress.ip_address(peer[0])
        origin_ip = ipaddress.ip_address(parsed.hostname or "")
    except ValueError as exc:
        raise JourneyError("HTTP peer identity is invalid") from exc
    if peer_ip != origin_ip or peer[1] != parsed.port:
        raise JourneyError("HTTP peer does not match the canonical target origin")


def http_json(
    base_url: str,
    step: dict[str, Any],
    body: dict[str, Any] | None,
    encoded_query: str,
    credential_values: dict[str, dict[str, str]],
    timeout: float,
    schema_version: int,
) -> tuple[int, Any]:
    alias, role = credential_identity(step, schema_version, credential_values)
    headers = {"Accept": "application/json"}
    if role != "none" and alias is not None:
        headers["Authorization"] = f"Bearer {credential_values[alias]['secret']}"
    data = None
    if body is not None:
        data = canonical_json(body)
        if len(data) > MAX_REQUEST_BYTES:
            raise JourneyError(f"HTTP request exceeded the limit for step {step['id']}")
        headers["Content-Type"] = "application/json"
    url = base_url + step["path"]
    if encoded_query:
        url += f"?{encoded_query}"
    http_request = request.Request(url, data=data, headers=headers, method=step["method"])
    deadline = time.monotonic() + timeout
    try:
        response = HTTP_OPENER.open(http_request, timeout=timeout)
    except error.HTTPError as exc:
        if 300 <= exc.code <= 399:
            exc.close()
            raise JourneyError(f"HTTP redirect was denied for step {step['id']}") from exc
        response = exc
    except (error.URLError, TimeoutError, OSError, socket.timeout) as exc:
        raise JourneyError(f"HTTP transport failed for step {step['id']}") from exc

    with response:
        if response.geturl() != url:
            raise JourneyError("HTTP response origin or request target changed")
        validate_response_peer(response, base_url)
        status = response.status
        content_length_header = response.headers.get("Content-Length")
        content_length: int | None = None
        if content_length_header is not None:
            try:
                content_length = int(content_length_header)
            except ValueError as exc:
                raise JourneyError("HTTP response has invalid Content-Length") from exc
            if content_length < 0 or content_length > MAX_RESPONSE_BYTES:
                raise JourneyError(f"HTTP response exceeded the limit for step {step['id']}")
        content_encoding = response.headers.get("Content-Encoding")
        if content_encoding not in {None, "identity"}:
            raise JourneyError("HTTP response content encoding is unsupported")
        content_type = response.headers.get_content_type().lower()

        chunks: list[bytes] = []
        received = 0
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise JourneyError(f"HTTP deadline expired for step {step['id']}")
            peer_socket = response_socket(response)
            if peer_socket is not None:
                peer_socket.settimeout(remaining)
            try:
                chunk = response.read(min(65536, MAX_RESPONSE_BYTES + 1 - received))
            except (TimeoutError, OSError, socket.timeout) as exc:
                raise JourneyError(f"HTTP deadline expired for step {step['id']}") from exc
            if not chunk:
                break
            chunks.append(chunk)
            received += len(chunk)
            if received > MAX_RESPONSE_BYTES:
                raise JourneyError(f"HTTP response exceeded the limit for step {step['id']}")
        raw = b"".join(chunks)
        if content_length is not None and len(raw) != content_length:
            raise JourneyError("HTTP response length does not match Content-Length")
    if len(raw) > MAX_RESPONSE_BYTES:
        raise JourneyError(f"HTTP response exceeded the limit for step {step['id']}")
    if not raw:
        return status, {}
    if content_type != "application/json":
        raise JourneyError(f"HTTP response was not JSON for step {step['id']}")
    payload = decode_json(raw, f"HTTP response for step {step['id']}")
    return status, payload


def public_safe(value: Any) -> bool:
    if isinstance(value, dict):
        return all(
            isinstance(key, str)
            and not SENSITIVE_KEY_RE.search(key)
            and public_safe(item)
            for key, item in value.items()
        )
    if isinstance(value, list):
        return all(public_safe(item) for item in value)
    if isinstance(value, str):
        if (
            not value.isascii()
            or len(value) > 512
            or any(ord(character) < 0x20 for character in value)
        ):
            return False
        if value.startswith("/"):
            try:
                validate_path(value, "public evidence")
                derived_route_role(value)
            except JourneyError:
                return False
        return True
    return value is None or isinstance(value, (bool, int))


def load_ledger(
    path: Path,
    schema_version: int,
    plan_digest: str,
    journey_id: str,
    target_origin: str,
) -> dict[str, Any]:
    if not path.exists():
        return {
            "schema_version": schema_version,
            "journey_id": journey_id,
            "plan_digest": plan_digest,
            "target_origin": target_origin,
            "chain_tip": ZERO_DIGEST,
            "completed": {},
        }
    ledger = load_json(path, "resume ledger")
    if (
        set(ledger)
        != {
            "schema_version",
            "journey_id",
            "plan_digest",
            "target_origin",
            "chain_tip",
            "completed",
        }
        or ledger.get("schema_version") != schema_version
        or ledger.get("journey_id") != journey_id
        or ledger.get("plan_digest") != plan_digest
        or ledger.get("target_origin") != target_origin
        or not isinstance(ledger.get("chain_tip"), str)
        or not DIGEST_RE.fullmatch(ledger["chain_tip"])
        or not isinstance(ledger.get("completed"), dict)
        or not public_safe(ledger)
    ):
        raise JourneyError("resume ledger does not match the current public-safe plan")
    return ledger


def record_digest(record: dict[str, Any]) -> str:
    material = {key: value for key, value in record.items() if key != "record_digest"}
    return digest(material)


def validate_completed_prefix(
    plan: dict[str, Any], completed: dict[str, Any], chain_tip: str
) -> None:
    plan_step_ids = [step["id"] for step in plan["steps"]]
    completed_ids = set(completed)
    if completed_ids != set(plan_step_ids[: len(completed_ids)]):
        raise JourneyError("resume ledger is not a canonical completed-step prefix")
    expected_fields = {
        "captures",
        "checkpoint",
        "kind",
        "method",
        "operation_id",
        "path",
        "phase",
        "prior_record_digest",
        "query",
        "record_digest",
        "replay_contract",
        "request_digest",
        "status",
    }
    if plan["schema_version"] == SCHEMA_VERSION_V2:
        expected_fields |= {
            "attempt_count",
            "auth_alias_digest",
            "auth_role",
            "response_contract_digest",
        }
    steps_by_id = {step["id"]: step for step in plan["steps"]}
    prior = ZERO_DIGEST
    for step_id in plan_step_ids[: len(completed_ids)]:
        record = completed[step_id]
        step = steps_by_id[step_id]
        if (
            not isinstance(record, dict)
            or set(record) != expected_fields
            or not isinstance(record.get("captures"), dict)
            or not isinstance(record.get("request_digest"), str)
            or not DIGEST_RE.fullmatch(record["request_digest"])
            or not isinstance(record.get("operation_id"), str)
            or not SAFE_ID_RE.fullmatch(record["operation_id"])
            or not isinstance(record.get("record_digest"), str)
            or not DIGEST_RE.fullmatch(record["record_digest"])
            or not isinstance(record.get("prior_record_digest"), str)
            or not DIGEST_RE.fullmatch(record["prior_record_digest"])
            or not isinstance(record.get("query"), str)
            or len(record["query"].encode("ascii", errors="ignore")) > MAX_QUERY_BYTES
        ):
            raise JourneyError(f"resume ledger record is invalid for step {step_id}")
        capture_specifications = step.get("capture", {})
        if set(record["captures"]) != set(capture_specifications):
            raise JourneyError(f"resume ledger captures are invalid for step {step_id}")
        for name, value in record["captures"].items():
            validate_capture(value, capture_specifications[name]["type"])
        replay_contract = replay_contract_for_step(step, plan["schema_version"])
        if (
            record["operation_id"]
            != stable_operation_id(plan["journey_id"], step_id, plan["schema_version"])
            or record["phase"] != step["phase"]
            or record["kind"] != step.get("kind", "positive")
            or record["method"] != step["method"]
            or record["path"] != step["path"]
            or record["checkpoint"] != step.get("checkpoint")
            or record["status"] not in step.get("expected_status", [200])
            or record["replay_contract"] != replay_contract
            or record["prior_record_digest"] != prior
            or record["record_digest"] != record_digest(record)
            or not public_safe(record)
        ):
            raise JourneyError(f"resume ledger semantics are invalid for step {step_id}")
        if plan["schema_version"] == SCHEMA_VERSION_V2:
            alias = step.get("credential_alias")
            is_observe = step.get("kind", "positive") == "observe"
            expected_max_attempts = (
                step["observe"]["max_attempts"] if is_observe else 1
            )
            allow_mismatch = step.get("allow_route_mismatch") is True
            if (
                record["auth_alias_digest"] != credential_alias_digest(alias)
                or record["auth_role"] not in ALLOWED_ROLES
                or (
                    not credential_role_allowed(step["route_role"], record["auth_role"])
                    and not allow_mismatch
                )
                or (
                    credential_role_allowed(step["route_role"], record["auth_role"])
                    and allow_mismatch
                )
                or not isinstance(record["attempt_count"], int)
                or isinstance(record["attempt_count"], bool)
                or not 1 <= record["attempt_count"] <= expected_max_attempts
                or not isinstance(record["response_contract_digest"], str)
                or not DIGEST_RE.fullmatch(record["response_contract_digest"])
                or record["response_contract_digest"]
                != response_contract_digest(record["status"], record["captures"])
            ):
                raise JourneyError(f"resume ledger v2 binding is invalid for step {step_id}")
        prior = record["record_digest"]
    if chain_tip != prior:
        raise JourneyError("resume ledger record chain is inconsistent")


def validate_evidence_binding(
    path: Path,
    ledger: dict[str, Any],
    plan: dict[str, Any],
) -> bool:
    if not path.exists():
        return bool(ledger["completed"])
    evidence = load_json(path, "journey evidence")
    required = {
        "schema_version",
        "journey_id",
        "plan_digest",
        "provider_mode",
        "target_origin",
        "record_chain_tip",
        "record_count",
        "result",
        "stopped_at",
        "replay_verified_steps",
        "steps",
    }
    steps = evidence.get("steps")
    replay_steps = evidence.get("replay_verified_steps")
    structurally_valid = (
        isinstance(steps, list)
        and all(isinstance(item, dict) for item in steps)
        and isinstance(replay_steps, list)
        and all(isinstance(item, str) for item in replay_steps)
    )
    record_count = evidence.get("record_count")
    if (
        set(evidence) != required
        or not structurally_valid
        or evidence.get("schema_version") != plan["schema_version"]
        or evidence.get("journey_id") != ledger["journey_id"]
        or evidence.get("plan_digest") != ledger["plan_digest"]
        or evidence.get("provider_mode") != "token_free"
        or evidence.get("target_origin") != ledger["target_origin"]
        or not isinstance(record_count, int)
        or isinstance(record_count, bool)
        or not 0 <= record_count <= len(ledger["completed"])
        or evidence.get("result") not in {"in_progress", "checkpoint_reached", "complete"}
        or (
            evidence.get("result") == "checkpoint_reached"
            and evidence.get("stopped_at")
            not in {step.get("checkpoint") for step in plan["steps"]}
        )
        or (
            evidence.get("result") != "checkpoint_reached"
            and evidence.get("stopped_at") is not None
        )
        or len(evidence["replay_verified_steps"])
        != len(set(evidence["replay_verified_steps"]))
        or not set(evidence["replay_verified_steps"]).issubset(
            {
                step["id"]
                for step in plan["steps"]
                if step["id"] in ledger["completed"]
            }
        )
        or [item.get("id") for item in evidence["steps"]]
        != [step["id"] for step in plan["steps"][:record_count]]
        or any(
            item != {"id": step_id, **ledger["completed"][step_id]}
            for item, step_id in zip(
                evidence["steps"],
                (step["id"] for step in plan["steps"][:record_count]),
            )
        )
        or not public_safe(evidence)
    ):
        raise JourneyError("journey evidence does not match the resume ledger")
    prefix_tip = (
        ZERO_DIGEST
        if record_count == 0
        else ledger["completed"][plan["steps"][record_count - 1]["id"]][
            "record_digest"
        ]
    )
    last_checkpoint = (
        None if record_count == 0 else plan["steps"][record_count - 1].get("checkpoint")
    )
    if (
        evidence["record_chain_tip"] != prefix_tip
        or len(evidence["steps"]) != record_count
        or not set(evidence["replay_verified_steps"]).issubset(
            {step["id"] for step in plan["steps"][:record_count]}
        )
        or (
            evidence["result"] == "checkpoint_reached"
            and (last_checkpoint is None or evidence["stopped_at"] != last_checkpoint)
        )
        or (
            evidence["result"] == "complete"
            and record_count != len(plan["steps"])
        )
    ):
        raise JourneyError("journey evidence is not a canonical ledger prefix")
    return record_count < len(ledger["completed"])


def completed_references(completed: dict[str, Any]) -> dict[str, Any]:
    references: dict[str, Any] = {}
    for step_id, record in completed.items():
        if not isinstance(record, dict) or not isinstance(record.get("captures"), dict):
            raise JourneyError("resume ledger contains an invalid completed record")
        for name, value in record["captures"].items():
            references[f"{step_id}.{name}"] = value
    return references


def resolve_credential_values(
    plan: dict[str, Any], credential_references: dict[str, Any]
) -> dict[str, dict[str, str]]:
    if not isinstance(credential_references, dict):
        raise JourneyError("credential references must be an object")
    schema_version = plan["schema_version"]
    if schema_version == SCHEMA_VERSION:
        required = {step["role"] for step in plan["steps"]} - {"none"}
        normalized = {
            role: {"role": role, "env": env_name}
            for role, env_name in credential_references.items()
        }
        identity_label = "role"
    else:
        required = {
            step["credential_alias"]
            for step in plan["steps"]
            if step["route_role"] != "none"
        }
        normalized = credential_references
        identity_label = "alias"
    if set(credential_references) != required:
        missing = sorted(required - set(credential_references))
        if missing:
            raise JourneyError(
                f"credential reference is missing for {identity_label} {missing[0]}"
            )
        raise JourneyError(f"credential references contain unused {identity_label}s")
    if any(
        not isinstance(binding, dict)
        or set(binding) != {"role", "env"}
        or binding["role"] not in ALLOWED_ROLES - {"none"}
        for binding in normalized.values()
    ):
        raise JourneyError("credential alias binding is invalid")
    env_names = [binding["env"] for binding in normalized.values()]
    if any(
        not isinstance(name, str) or not ENV_NAME_RE.fullmatch(name)
        for name in env_names
    ):
        raise JourneyError("credential reference contains an invalid environment name")
    if len(env_names) != len(set(env_names)):
        raise JourneyError("credential environment references must be role-separated")

    values: dict[str, dict[str, str]] = {}
    seen_secrets: set[str] = set()
    for alias, binding in normalized.items():
        role = binding["role"]
        env_name = binding["env"]
        secret = os.environ.get(env_name)
        if secret is None or not secret:
            raise JourneyError(
                f"credential environment is unavailable for {identity_label} {alias}"
            )
        secret_size = len(secret.encode("utf-8"))
        if (
            not MIN_SECRET_BYTES <= secret_size <= MAX_SECRET_BYTES
            or not secret.isascii()
            or any(
                ord(character) < 0x21 or ord(character) > 0x7E
                for character in secret
            )
        ):
            if schema_version == SCHEMA_VERSION:
                raise JourneyError(f"credential value is unsafe for role {role}")
            raise JourneyError(f"credential value is unsafe for alias {alias}")
        if secret in seen_secrets:
            raise JourneyError("credential values must be role-separated")
        seen_secrets.add(secret)
        values[alias] = {"role": role, "secret": secret}

    for step in plan["steps"]:
        route_role = step["route_role"]
        if route_role == "none":
            continue
        alias = step["role"] if schema_version == SCHEMA_VERSION else step["credential_alias"]
        credential_role = values[alias]["role"]
        mismatch = not credential_role_allowed(route_role, credential_role)
        allow_mismatch = step.get("allow_route_mismatch") is True
        if mismatch and not (step.get("kind", "positive") == "negative" and allow_mismatch):
            raise JourneyError(f"step {step['id']} crosses its authenticated route")
        if allow_mismatch and (
            step.get("kind", "positive") != "negative" or not mismatch
        ):
            raise JourneyError(
                "allow_route_mismatch is valid only for an explicit negative"
            )
    return values


def credential_identity(
    step: dict[str, Any], schema_version: int, credential_values: dict[str, dict[str, str]]
) -> tuple[str | None, str]:
    if step["route_role"] == "none":
        return None, "none"
    alias = step["role"] if schema_version == SCHEMA_VERSION else step["credential_alias"]
    return alias, credential_values[alias]["role"]


def contains_credential(value: Any, credential_values: set[str]) -> bool:
    if isinstance(value, dict):
        return any(
            contains_credential(item, credential_values) for item in value.values()
        )
    if isinstance(value, list):
        return any(contains_credential(item, credential_values) for item in value)
    return isinstance(value, str) and any(
        credential in value for credential in credential_values
    )


def evaluate_assertions(
    assertions: list[dict[str, Any]],
    payload: Any,
    references: dict[str, Any],
    operation_id: str,
) -> None:
    for assertion in assertions:
        actual = pointer_get(payload, assertion["pointer"])
        if "equals" in assertion:
            expected = resolve_template(assertion["equals"], references, operation_id)
            if actual != expected:
                raise JourneyError("response assertion did not match")


def build_evidence(
    plan: dict[str, Any],
    ledger: dict[str, Any],
    result: str,
    stopped_at: str | None,
    replay_verified_steps: set[str],
) -> dict[str, Any]:
    evidence = {
        "schema_version": plan["schema_version"],
        "journey_id": ledger["journey_id"],
        "plan_digest": ledger["plan_digest"],
        "provider_mode": "token_free",
        "target_origin": ledger["target_origin"],
        "record_chain_tip": ledger["chain_tip"],
        "record_count": len(ledger["completed"]),
        "result": result,
        "stopped_at": stopped_at,
        "replay_verified_steps": sorted(replay_verified_steps),
        "steps": [
            {"id": step["id"], **ledger["completed"][step["id"]]}
            for step in plan["steps"]
            if step["id"] in ledger["completed"]
        ],
    }
    if not public_safe(evidence):
        raise JourneyError("journey evidence is not public-safe")
    return evidence


def evidence_ledger_prefix(
    plan: dict[str, Any], ledger: dict[str, Any], record_count: int
) -> dict[str, Any]:
    if not 0 <= record_count <= len(ledger["completed"]):
        raise JourneyError("evidence prefix length is invalid")
    step_ids = [step["id"] for step in plan["steps"][:record_count]]
    completed = {step_id: ledger["completed"][step_id] for step_id in step_ids}
    chain_tip = ZERO_DIGEST if not step_ids else completed[step_ids[-1]]["record_digest"]
    return {**ledger, "completed": completed, "chain_tip": chain_tip}


def resolved_request(
    step: dict[str, Any],
    references: dict[str, Any],
    operation_id: str,
    credential_secrets: set[str],
    schema_version: int,
    credential_values: dict[str, dict[str, str]],
) -> tuple[dict[str, Any] | None, str, str]:
    body = None
    if step["method"] == "POST":
        body = resolve_template(step["body"], references, operation_id)
        validate_public_structure(body, f"step {step['id']} body")
        if contains_credential(body, credential_secrets):
            raise JourneyError(
                f"request body contains credential material at step {step['id']}"
            )
    query = resolve_template(step.get("query", {}), references, operation_id)
    validate_public_structure(query, f"step {step['id']} query")
    if contains_credential(query, credential_secrets):
        raise JourneyError(
            f"request query contains credential material at step {step['id']}"
        )
    encoded_query = encode_query(query)
    if schema_version == SCHEMA_VERSION:
        request_material = {
            "method": step["method"],
            "path": step["path"],
            "query": encoded_query,
            "role": step["role"],
            "route_role": step["route_role"],
            "body": body,
        }
    else:
        alias, credential_role = credential_identity(
            step, schema_version, credential_values
        )
        mismatch = not credential_role_allowed(step["route_role"], credential_role)
        allow_mismatch = step.get("allow_route_mismatch") is True
        if mismatch != allow_mismatch:
            raise JourneyError(f"step {step['id']} crosses its authenticated route")
        request_material = {
            "schema_version": SCHEMA_VERSION_V2,
            "method": step["method"],
            "path": step["path"],
            "query": encoded_query,
            "credential_alias": alias,
            "credential_role": credential_role,
            "route_role": step["route_role"],
            "body": body,
        }
    request_digest = digest(request_material)
    return body, encoded_query, request_digest


def response_contract_digest(status: int, captures: dict[str, Any]) -> str:
    return digest(
        {
            "domain": "m0-response-contract-v2",
            "schema_version": SCHEMA_VERSION_V2,
            "status": status,
            "captures": captures,
        }
    )


def credential_alias_digest(alias: str | None) -> str:
    return digest(
        {
            "domain": "m0-credential-alias-v2",
            "schema_version": SCHEMA_VERSION_V2,
            "alias": alias,
        }
    )


def replay_contract_for_step(step: dict[str, Any], schema_version: int) -> str:
    if schema_version == SCHEMA_VERSION:
        return "server_response_verified"
    if step.get("kind", "positive") == "observe":
        return "bounded_observe_v2"
    if step.get("kind", "positive") == "positive":
        return "idempotent_command_v2"
    return "server_response_verified_v2"


def observe_step(
    base_url: str,
    step: dict[str, Any],
    body: dict[str, Any] | None,
    encoded_query: str,
    credential_values: dict[str, dict[str, str]],
    credential_secrets: set[str],
    references: dict[str, Any],
    operation_id: str,
    timeout: float,
    schema_version: int,
    assertions: list[dict[str, Any]],
) -> tuple[int, dict[str, Any], int]:
    observe = step.get("observe")
    max_attempts = observe["max_attempts"] if observe is not None else 1
    deadline = (
        time.monotonic() + observe["max_elapsed_ms"] / 1_000
        if observe is not None
        else None
    )
    retry_statuses = set(observe["retry_statuses"]) if observe is not None else set()
    last_error: JourneyError | None = None
    for attempt in range(1, max_attempts + 1):
        attempt_timeout = timeout
        if deadline is not None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            attempt_timeout = min(timeout, remaining)
        status, payload = http_json(
            base_url,
            step,
            body,
            encoded_query,
            credential_values,
            attempt_timeout,
            schema_version,
        )
        if response_is_unavailable(status, payload):
            raise JourneyError(f"required adapter is unavailable at step {step['id']}")
        if status in retry_statuses:
            last_error = JourneyError(f"observe status is not ready at step {step['id']}")
        elif status not in step.get("expected_status", [200]):
            raise JourneyError(f"unexpected HTTP status at step {step['id']}")
        else:
            try:
                evaluate_assertions(assertions, payload, references, operation_id)
                captures: dict[str, Any] = {}
                for name, specification in step.get("capture", {}).items():
                    captures[name] = validate_capture(
                        pointer_get(payload, specification["pointer"]),
                        specification["type"],
                    )
            except JourneyError as exc:
                if observe is None:
                    raise
                last_error = exc
            else:
                if any(
                    contains_credential(value, credential_secrets)
                    for value in captures.values()
                ):
                    raise JourneyError(
                        f"step {step['id']} attempted to capture a credential"
                    )
                return status, captures, attempt
        if attempt == max_attempts:
            break
        interval = observe["interval_ms"] / 1_000
        if deadline is not None and time.monotonic() + interval >= deadline:
            break
        if interval:
            time.sleep(interval)
    raise JourneyError(f"observe bounds exhausted at step {step['id']}") from last_error


def _run_journey_locked(
    plan: dict[str, Any],
    base_url: str,
    credential_values: dict[str, dict[str, str]],
    ledger_path: Path,
    evidence_path: Path,
    timeout: float,
    stop_after_checkpoint: str | None = None,
) -> dict[str, Any]:
    plan_digest = digest(plan)
    journey_id = plan["journey_id"]
    schema_version = plan["schema_version"]
    credential_secrets = {
        binding["secret"] for binding in credential_values.values()
    }
    ledger = load_ledger(
        ledger_path, schema_version, plan_digest, journey_id, base_url
    )
    completed = ledger["completed"]
    validate_completed_prefix(plan, completed, ledger["chain_tip"])
    evidence_needs_repair = validate_evidence_binding(evidence_path, ledger, plan)
    if evidence_needs_repair:
        atomic_json_write(
            evidence_path,
            build_evidence(plan, ledger, "in_progress", None, set()),
        )
    references = completed_references(completed)
    stopped_at: str | None = None
    replay_verified_steps: set[str] = set()

    processed_count = 0
    for index, step in enumerate(plan["steps"]):
        step_id = step["id"]
        operation_id = stable_operation_id(journey_id, step_id, schema_version)
        body, encoded_query, request_digest = resolved_request(
            step,
            references,
            operation_id,
            credential_secrets,
            schema_version,
            credential_values,
        )
        existing = completed.get(step_id)
        if existing is not None:
            if (
                not isinstance(existing, dict)
                or existing.get("request_digest") != request_digest
                or existing.get("operation_id") != operation_id
                or existing.get("query") != encoded_query
            ):
                raise JourneyError(f"resume conflict for completed step {step_id}")
            replay_assertions = (
                step.get("assertions", [])
                if schema_version == SCHEMA_VERSION
                or step.get("kind", "positive") != "positive"
                else step.get("assertions", []) + step["replay_assertions"]
            )
            status, captures, _attempt_count = observe_step(
                base_url,
                step,
                body,
                encoded_query,
                credential_values,
                credential_secrets,
                references,
                operation_id,
                timeout,
                schema_version,
                replay_assertions,
            )
            if status != existing["status"] or captures != existing["captures"]:
                raise JourneyError(
                    f"authoritative replay changed the outcome for step {step_id}"
                )
            replay_verified_steps.add(step_id)
            atomic_json_write(
                evidence_path,
                build_evidence(plan, ledger, "in_progress", None, replay_verified_steps),
            )
        else:
            initial_assertions = (
                step.get("assertions", [])
                if schema_version == SCHEMA_VERSION
                or step.get("kind", "positive") != "positive"
                else step.get("assertions", []) + step["initial_assertions"]
            )
            status, captures, attempt_count = observe_step(
                base_url,
                step,
                body,
                encoded_query,
                credential_values,
                credential_secrets,
                references,
                operation_id,
                timeout,
                schema_version,
                initial_assertions,
            )
            record = {
                "captures": captures,
                "checkpoint": step.get("checkpoint"),
                "kind": step.get("kind", "positive"),
                "method": step["method"],
                "operation_id": operation_id,
                "path": step["path"],
                "phase": step["phase"],
                "prior_record_digest": ledger["chain_tip"],
                "query": encoded_query,
                "replay_contract": "server_response_verified",
                "request_digest": request_digest,
                "status": status,
            }
            if schema_version == SCHEMA_VERSION_V2:
                alias, credential_role = credential_identity(
                    step, schema_version, credential_values
                )
                record.update(
                    {
                        "attempt_count": attempt_count,
                        "auth_alias_digest": credential_alias_digest(alias),
                        "auth_role": credential_role,
                        "response_contract_digest": response_contract_digest(
                            status, captures
                        ),
                        "replay_contract": replay_contract_for_step(
                            step, schema_version
                        ),
                    }
                )
            record["record_digest"] = record_digest(record)
            if not public_safe(record):
                raise JourneyError(f"step {step_id} produced non-public evidence")
            completed[step_id] = record
            ledger["chain_tip"] = record["record_digest"]
            for name, value in captures.items():
                references[f"{step_id}.{name}"] = value
            atomic_json_write(ledger_path, ledger)
            atomic_json_write(
                evidence_path,
                build_evidence(plan, ledger, "in_progress", None, replay_verified_steps),
            )

        checkpoint = step.get("checkpoint")
        processed_count = index + 1
        if checkpoint is not None and checkpoint == stop_after_checkpoint:
            stopped_at = checkpoint
            break

    evidence_ledger = (
        evidence_ledger_prefix(plan, ledger, processed_count)
        if stopped_at is not None
        else ledger
    )
    evidence = build_evidence(
        plan,
        evidence_ledger,
        "checkpoint_reached" if stopped_at else "complete",
        stopped_at,
        replay_verified_steps,
    )
    atomic_json_write(evidence_path, evidence)
    return evidence


def run_journey(
    plan: dict[str, Any],
    base_url: str,
    credential_references: dict[str, Any],
    ledger_path: Path,
    evidence_path: Path,
    timeout: float,
    stop_after_checkpoint: str | None = None,
) -> dict[str, Any]:
    validate_plan(plan)
    base_url = validate_base_url(base_url)
    timeout = validate_timeout(timeout)
    ledger_path = safe_output_path(str(ledger_path), "ledger")
    evidence_path = safe_output_path(str(evidence_path), "evidence")
    if ledger_path == evidence_path:
        raise JourneyError("ledger and evidence paths must be distinct")
    credential_values = resolve_credential_values(plan, credential_references)
    if stop_after_checkpoint is not None and stop_after_checkpoint not in {
        step.get("checkpoint") for step in plan["steps"]
    }:
        raise JourneyError("requested restart checkpoint is not in the plan")
    validate_existing_output(ledger_path, "resume ledger")
    validate_existing_output(evidence_path, "journey evidence")
    with exclusive_journey_lock(plan["journey_id"]):
        return _run_journey_locked(
            plan,
            base_url,
            credential_values,
            ledger_path,
            evidence_path,
            timeout,
            stop_after_checkpoint,
        )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--credential", action="append", default=[], metavar="ROLE=ENV")
    parser.add_argument("--ledger", required=True)
    parser.add_argument("--evidence", required=True)
    parser.add_argument("--timeout", type=float, default=5.0)
    parser.add_argument("--stop-after-checkpoint")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        plan = load_json(args.plan, "journey plan")
        ledger = safe_output_path(args.ledger, "ledger")
        evidence = safe_output_path(args.evidence, "evidence")
        credential_references = validate_credentials(
            args.credential, plan.get("schema_version")
        )
        result = run_journey(
            plan,
            args.base_url,
            credential_references,
            ledger,
            evidence,
            args.timeout,
            args.stop_after_checkpoint,
        )
    except JourneyError as exc:
        print(f"M0 journey failed: {exc}", file=sys.stderr)
        return 1
    print(
        f"M0 journey {result['result']}: {result['journey_id']} "
        f"steps={len(result['steps'])}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
