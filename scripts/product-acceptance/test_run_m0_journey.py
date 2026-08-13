from __future__ import annotations

from collections import Counter
from contextlib import redirect_stderr
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import shutil
import stat
import sys
import threading
import time
import unittest
from urllib import parse
import uuid


sys.path.insert(0, str(Path(__file__).parent))
import run_m0_journey as runner  # noqa: E402


TEST_ROOT = Path("/work/tmp/project-sentinel/cdx1-650-runner-tests")
DIGEST_A = "a" * 64
DIGEST_B = "b" * 64


class FakeState:
    def __init__(self) -> None:
        self.calls: Counter[str] = Counter()
        self.auth_headers: list[tuple[str, str | None]] = []
        self.bodies: list[tuple[str, dict[str, object]]] = []
        self.delay_path: str | None = None
        self.effective_mutations: Counter[str] = Counter()
        self.entered = threading.Event()
        self.redirect_location: str | None = None
        self.response_overrides: dict[str, dict[str, object]] = {}
        self.seen_operations: set[tuple[str, str]] = set()
        self.targets: list[str] = []


class FakeHandler(BaseHTTPRequestHandler):
    server: "FakeServer"
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:
        self.respond()

    def do_POST(self) -> None:
        content_length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(content_length) or b"{}")
        self.respond(body)

    def respond(self, body: dict[str, object] | None = None) -> None:
        state = self.server.state
        target = parse.urlsplit(self.path)
        path = target.path
        state.calls[path] += 1
        state.targets.append(self.path)
        state.entered.set()
        authorization = self.headers.get("Authorization")
        state.auth_headers.append((path, authorization))
        if body is not None:
            state.bodies.append((path, body))
        if state.delay_path == path:
            time.sleep(0.2)

        if path == "/operator/redirect":
            self.send_response(302)
            self.send_header("Location", state.redirect_location or "/operator/readiness")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path == "/operator/wrong-content":
            self.write_raw(200, b'{"ready":true}', "text/plain")
            return
        if path == "/operator/duplicate-json":
            self.write_raw(200, b'{"ready":true,"ready":true}')
            return
        if path == "/operator/oversized-length":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(runner.MAX_RESPONSE_BYTES + 1))
            self.end_headers()
            return
        if path == "/operator/oversized-chunked":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            try:
                chunk = b"x" * 65536
                for _ in range(17):
                    self.wfile.write(f"{len(chunk):x}\r\n".encode("ascii"))
                    self.wfile.write(chunk + b"\r\n")
                    self.wfile.flush()
                self.wfile.write(b"0\r\n\r\n")
            except (BrokenPipeError, ConnectionResetError):
                pass
            return
        if path == "/operator/slow-body":
            encoded = b'{"ready":true}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            try:
                self.wfile.write(encoded[:1])
                self.wfile.flush()
                time.sleep(0.2)
                self.wfile.write(encoded[1:])
            except (BrokenPipeError, ConnectionResetError):
                pass
            return

        expected_tokens = {
            "/operator/readiness": "operator-secret-value",
            "/customer/workflow/commands": "customer-secret-value",
            "/operator/workflow/commands": "operator-secret-value",
            "/agent/workflow/commands": "agent-secret-value",
            "/operator/delivery/commands": "operator-secret-value",
            "/customer/delivery/commands": "customer-secret-value",
        }
        expected = expected_tokens.get(path)
        if expected is not None and authorization != f"Bearer {expected}":
            self.write_json(403, {"code": "authority_denied"})
            return
        if path == "/operator/forbidden" and authorization == "Bearer customer-secret-value":
            self.write_json(403, {"code": "authority_denied"})
            return
        if path == "/operator/unavailable":
            self.write_json(503, {"code": "execution_unavailable"})
            return

        if body is not None and isinstance(body.get("operation_id"), str):
            operation_key = (path, body["operation_id"])
            if operation_key not in state.seen_operations:
                state.seen_operations.add(operation_key)
                state.effective_mutations[path] += 1

        responses = {
            "/readiness": {"ready": True, "status": "ready"},
            "/operator/readiness": {"ready": True, "status": "ready"},
            "/customer/workflow/commands": {
                "request_id": "request-1",
                "request_digest": DIGEST_A,
                "private_token": "response-secret-value",
            },
            "/operator/workflow/commands": {
                "project_id": "project-1",
                "request_digest": DIGEST_A,
            },
            "/agent/workflow/commands": {
                "artifact_id": "artifact-1",
                "artifact_digest": DIGEST_B,
            },
            "/operator/delivery/commands": {
                "release_id": "release-1",
                "artifact_digest": DIGEST_B,
                "delivery_id": "delivery-1",
            },
            "/customer/delivery/commands": {
                "acceptance_id": "acceptance-1",
                "delivery_id": "delivery-1",
                "state": "accepted",
            },
        }
        response = self.server.state.response_overrides.get(
            path, responses.get(path, {"ok": True})
        )
        if body is not None:
            for key in ("request_id", "request_digest", "artifact_digest", "delivery_id"):
                if key in body:
                    response = {**response, key: body[key]}
        self.write_json(200, response)

    def write_raw(
        self, status: int, encoded: bytes, content_type: str = "application/json"
    ) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def write_json(self, status: int, payload: dict[str, object]) -> None:
        encoded = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        try:
            self.wfile.write(encoded)
        except BrokenPipeError:
            pass

    def log_message(self, format: str, *args: object) -> None:
        del format, args


class FakeServer(ThreadingHTTPServer):
    def __init__(self, state: FakeState) -> None:
        super().__init__(("127.0.0.1", 0), FakeHandler)
        self.state = state


def canonical_plan() -> dict[str, object]:
    return {
        "schema_version": 1,
        "journey_id": "journey-fixture-1",
        "provider_mode": "token_free",
        "steps": [
            {
                "id": "readiness",
                "phase": "readiness",
                "kind": "readiness",
                "method": "GET",
                "path": "/operator/readiness",
                "role": "operator",
                "route_role": "operator",
                "expected_status": [200],
                "assertions": [{"pointer": "/ready", "equals": True}],
            },
            {
                "id": "submit",
                "phase": "customer_request",
                "kind": "positive",
                "method": "POST",
                "path": "/customer/workflow/commands",
                "role": "customer",
                "route_role": "customer",
                "body": {
                    "operation_id": {"$operation_id": True},
                    "summary_ref": "public-brief-ref",
                },
                "expected_status": [200],
                "capture": {
                    "request_id": {"pointer": "/request_id", "type": "id"},
                    "request_digest": {"pointer": "/request_digest", "type": "digest"},
                },
                "checkpoint": "after_customer_request",
            },
            {
                "id": "plan_project",
                "phase": "governed_project",
                "kind": "positive",
                "method": "POST",
                "path": "/operator/workflow/commands",
                "role": "operator",
                "route_role": "operator",
                "body": {
                    "operation_id": {"$operation_id": True},
                    "request_id": {"$ref": "submit.request_id"},
                    "request_digest": {"$ref": "submit.request_digest"},
                },
                "expected_status": [200],
                "assertions": [
                    {
                        "pointer": "/request_digest",
                        "equals": {"$ref": "submit.request_digest"},
                    }
                ],
                "capture": {
                    "project_id": {"pointer": "/project_id", "type": "id"}
                },
                "checkpoint": "after_governed_project",
            },
            {
                "id": "authority_negative",
                "phase": "governed_project",
                "kind": "negative",
                "method": "POST",
                "path": "/operator/forbidden",
                "role": "customer",
                "route_role": "operator",
                "allow_route_mismatch": True,
                "body": {"operation_id": {"$operation_id": True}},
                "expected_status": [403],
                "assertions": [
                    {"pointer": "/code", "equals": "authority_denied"}
                ],
            },
            {
                "id": "execute",
                "phase": "workbench_execution",
                "kind": "positive",
                "method": "POST",
                "path": "/agent/workflow/commands",
                "role": "agent",
                "route_role": "agent",
                "body": {
                    "operation_id": {"$operation_id": True},
                    "project_id": {"$ref": "plan_project.project_id"},
                },
                "expected_status": [200],
                "capture": {
                    "artifact_id": {"pointer": "/artifact_id", "type": "id"},
                    "artifact_digest": {"pointer": "/artifact_digest", "type": "digest"},
                },
                "checkpoint": "after_workbench_execution",
            },
            {
                "id": "qa_release",
                "phase": "qa_release",
                "kind": "positive",
                "method": "POST",
                "path": "/operator/delivery/commands",
                "role": "operator",
                "route_role": "operator",
                "body": {
                    "operation_id": {"$operation_id": True},
                    "artifact_digest": {"$ref": "execute.artifact_digest"},
                },
                "expected_status": [200],
                "assertions": [
                    {
                        "pointer": "/artifact_digest",
                        "equals": {"$ref": "execute.artifact_digest"},
                    }
                ],
                "capture": {
                    "release_id": {"pointer": "/release_id", "type": "id"}
                },
                "checkpoint": "after_qa_release",
            },
            {
                "id": "delivery",
                "phase": "delivery",
                "kind": "positive",
                "method": "POST",
                "path": "/operator/delivery/commands",
                "role": "operator",
                "route_role": "operator",
                "body": {
                    "operation_id": {"$operation_id": True},
                    "release_id": {"$ref": "qa_release.release_id"},
                },
                "expected_status": [200],
                "capture": {
                    "delivery_id": {"pointer": "/delivery_id", "type": "id"}
                },
                "checkpoint": "after_delivery",
            },
            {
                "id": "acceptance",
                "phase": "acceptance",
                "kind": "positive",
                "method": "POST",
                "path": "/customer/delivery/commands",
                "role": "customer",
                "route_role": "customer",
                "query": {
                    "delivery_id": {"$ref": "delivery.delivery_id"},
                    "receipt": {"$ref": "execute.artifact_digest"},
                },
                "body": {
                    "operation_id": {"$operation_id": True},
                    "delivery_id": {"$ref": "delivery.delivery_id"},
                },
                "expected_status": [200],
                "assertions": [
                    {
                        "pointer": "/delivery_id",
                        "equals": {"$ref": "delivery.delivery_id"},
                    },
                    {"pointer": "/state", "equals": "accepted"},
                ],
                "capture": {
                    "acceptance_id": {"pointer": "/acceptance_id", "type": "id"},
                    "state": {"pointer": "/state", "type": "state"},
                },
                "checkpoint": "after_acceptance",
            },
        ],
    }


class JourneyRunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        TEST_ROOT.mkdir(parents=True, exist_ok=True)
        TEST_ROOT.chmod(0o700)
        self.directory = TEST_ROOT / f"case-{uuid.uuid4().hex}"
        self.directory.mkdir(mode=0o700)
        self.ledger = self.directory / "ledger.json"
        self.evidence = self.directory / "evidence.json"
        self.state = FakeState()
        self.server = FakeServer(self.state)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"
        self.credentials = {
            "operator": "M0_TEST_OPERATOR_CREDENTIAL",
            "customer": "M0_TEST_CUSTOMER_CREDENTIAL",
            "agent": "M0_TEST_AGENT_CREDENTIAL",
        }
        self.old_environment = {
            name: os.environ.get(name) for name in self.credentials.values()
        }
        os.environ["M0_TEST_OPERATOR_CREDENTIAL"] = "operator-secret-value"
        os.environ["M0_TEST_CUSTOMER_CREDENTIAL"] = "customer-secret-value"
        os.environ["M0_TEST_AGENT_CREDENTIAL"] = "agent-secret-value"

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=1)
        for name, value in self.old_environment.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        shutil.rmtree(self.directory)
        runner.lock_path_for("journey-fixture-1").unlink(missing_ok=True)

    def run_plan(
        self, plan: dict[str, object] | None = None, checkpoint: str | None = None
    ) -> dict[str, object]:
        return runner.run_journey(
            canonical_plan() if plan is None else plan,
            self.base_url,
            self.credentials,
            self.ledger,
            self.evidence,
            1.0,
            checkpoint,
        )

    def test_checkpoint_resume_chains_digests_without_duplicate_mutation(self) -> None:
        first = self.run_plan(checkpoint="after_governed_project")
        self.assertEqual(first["result"], "checkpoint_reached")
        first_counts = self.state.calls.copy()
        completed = self.run_plan()
        self.assertEqual(completed["result"], "complete")
        self.assertEqual(self.state.calls["/customer/workflow/commands"], 2)
        self.assertEqual(self.state.calls["/operator/workflow/commands"], 2)
        self.assertEqual(self.state.effective_mutations["/customer/workflow/commands"], 1)
        self.assertEqual(self.state.effective_mutations["/operator/workflow/commands"], 1)
        self.assertEqual(first_counts["/customer/workflow/commands"], 1)
        self.assertEqual(first_counts["/operator/workflow/commands"], 1)
        self.assertEqual(self.state.calls["/operator/readiness"], 2)
        first_records = {item["id"]: item for item in first["steps"]}
        evidence = json.loads(self.evidence.read_text(encoding="utf-8"))
        records = {item["id"]: item for item in evidence["steps"]}
        expected_operation = runner.stable_operation_id("journey-fixture-1", "submit")
        self.assertEqual(first_records["submit"]["operation_id"], expected_operation)
        self.assertEqual(records["submit"]["operation_id"], expected_operation)
        self.assertEqual(records["acceptance"]["captures"]["state"], "accepted")
        self.assertEqual(records["execute"]["captures"]["artifact_digest"], DIGEST_B)
        self.assertEqual(len(records["submit"]["request_digest"]), 64)
        self.assertIn("submit", evidence["replay_verified_steps"])
        self.assertEqual(
            records["acceptance"]["query"],
            f"delivery_id=delivery-1&receipt={DIGEST_B}",
        )
        self.assertEqual(evidence["target_origin"], self.base_url)
        self.assertEqual(evidence["record_chain_tip"], records["acceptance"]["record_digest"])

    def test_role_credentials_are_separated_and_never_persisted(self) -> None:
        self.run_plan()
        observed = dict(self.state.auth_headers)
        self.assertEqual(
            observed["/customer/workflow/commands"], "Bearer customer-secret-value"
        )
        self.assertEqual(
            observed["/agent/workflow/commands"], "Bearer agent-secret-value"
        )
        self.assertEqual(
            observed["/operator/workflow/commands"], "Bearer operator-secret-value"
        )
        persisted = self.ledger.read_text(encoding="utf-8") + self.evidence.read_text(
            encoding="utf-8"
        )
        for secret in (
            "operator-secret-value",
            "customer-secret-value",
            "agent-secret-value",
            "response-secret-value",
        ):
            self.assertNotIn(secret, persisted)
        self.assertNotIn("private_token", persisted)
        observed_material = json.dumps(self.state.bodies) + json.dumps(self.state.targets)
        self.assertNotIn("secret-value", observed_material)

    def test_numeric_loopback_origin_is_required(self) -> None:
        with self.assertRaisesRegex(runner.JourneyError, "loopback HTTP origin"):
            runner.run_journey(
                canonical_plan(),
                f"http://localhost:{self.server.server_port}",
                self.credentials,
                self.ledger,
                self.evidence,
                1.0,
            )

    def test_explicit_no_auth_readiness_route_is_allowed(self) -> None:
        plan = canonical_plan()
        plan["steps"][0].update(path="/readiness", role="none", route_role="none")
        self.run_plan(plan, checkpoint="after_customer_request")
        observed = dict(self.state.auth_headers)
        self.assertIsNone(observed["/readiness"])

    def test_same_origin_redirect_is_denied_without_followup(self) -> None:
        plan = canonical_plan()
        plan["steps"][0]["path"] = "/operator/redirect"
        with self.assertRaisesRegex(runner.JourneyError, "redirect was denied"):
            self.run_plan(plan)
        self.assertEqual(self.state.calls["/operator/redirect"], 1)
        self.assertEqual(self.state.calls["/operator/readiness"], 0)

    def test_cross_origin_redirect_does_not_disclose_credentials(self) -> None:
        destination_state = FakeState()
        destination = FakeServer(destination_state)
        thread = threading.Thread(target=destination.serve_forever, daemon=True)
        thread.start()
        try:
            self.state.redirect_location = (
                f"http://127.0.0.1:{destination.server_port}/operator/readiness"
            )
            plan = canonical_plan()
            plan["steps"][0]["path"] = "/operator/redirect"
            with self.assertRaisesRegex(runner.JourneyError, "redirect was denied"):
                self.run_plan(plan)
            self.assertEqual(sum(destination_state.calls.values()), 0)
            self.assertFalse(destination_state.auth_headers)
        finally:
            destination.shutdown()
            destination.server_close()
            thread.join(timeout=1)

    def test_proxy_environment_is_ignored(self) -> None:
        proxy_state = FakeState()
        proxy = FakeServer(proxy_state)
        thread = threading.Thread(target=proxy.serve_forever, daemon=True)
        thread.start()
        previous = os.environ.get("HTTP_PROXY")
        os.environ["HTTP_PROXY"] = f"http://127.0.0.1:{proxy.server_port}"
        try:
            self.run_plan(checkpoint="after_customer_request")
            self.assertEqual(sum(proxy_state.calls.values()), 0)
            self.assertFalse(proxy_state.auth_headers)
        finally:
            if previous is None:
                os.environ.pop("HTTP_PROXY", None)
            else:
                os.environ["HTTP_PROXY"] = previous
            proxy.shutdown()
            proxy.server_close()
            thread.join(timeout=1)

    def test_positive_cross_role_route_is_rejected_before_http(self) -> None:
        plan = canonical_plan()
        plan["steps"][1]["route_role"] = "operator"
        with self.assertRaisesRegex(runner.JourneyError, "derived route authority"):
            self.run_plan(plan)
        self.assertEqual(sum(self.state.calls.values()), 0)

    def test_route_label_and_path_ambiguity_are_rejected_before_http(self) -> None:
        for path in (
            "/operator/%2e%2e/customer/workflow",
            "/operator/../customer/workflow",
            "/operator\\customer/workflow",
            "/operator//workflow",
            "/unknown/workflow",
        ):
            with self.subTest(path=path):
                plan = canonical_plan()
                plan["steps"][2]["path"] = path
                with self.assertRaises(runner.JourneyError):
                    self.run_plan(plan)
        self.assertEqual(sum(self.state.calls.values()), 0)

    def test_structured_query_chains_ids_and_rejects_injection(self) -> None:
        self.run_plan()
        self.assertIn(
            f"/customer/delivery/commands?delivery_id=delivery-1&receipt={DIGEST_B}",
            self.state.targets,
        )
        for query in (
            {"credential": "public-value"},
            {"safe": "value&injected=true"},
            {"safe": "prefix-customer-secret-value-suffix"},
        ):
            with self.subTest(query=query):
                plan = canonical_plan()
                plan["steps"][1]["query"] = query
                other = self.directory / uuid.uuid4().hex
                other.mkdir(mode=0o700)
                with self.assertRaises(runner.JourneyError) as caught:
                    runner.run_journey(
                        plan,
                        self.base_url,
                        self.credentials,
                        other / "ledger.json",
                        other / "evidence.json",
                        1.0,
                    )
                self.assertNotIn("customer-secret-value", str(caught.exception))

    def test_raw_query_and_spoofed_route_role_are_rejected(self) -> None:
        plan = canonical_plan()
        plan["steps"][2]["path"] = "/operator/workflow/commands?role=customer"
        with self.assertRaises(runner.JourneyError):
            self.run_plan(plan)
        plan = canonical_plan()
        plan["steps"][2]["route_role"] = "customer"
        with self.assertRaisesRegex(runner.JourneyError, "spoofs"):
            self.run_plan(plan)

    def test_negative_cross_route_requires_explicit_hook_and_denial_status(self) -> None:
        plan = canonical_plan()
        negative = plan["steps"][3]
        negative.pop("allow_route_mismatch")
        with self.assertRaisesRegex(runner.JourneyError, "authenticated route"):
            self.run_plan(plan)
        negative["allow_route_mismatch"] = True
        negative["expected_status"] = [200]
        with self.assertRaisesRegex(runner.JourneyError, "only 401, 403, or 405"):
            self.run_plan(plan)

    def test_plan_truth_rejects_noop_or_false_success_commands(self) -> None:
        mutations = (
            lambda step: step.update(method="GET", body=None),
            lambda step: step.update(expected_status=[500]),
            lambda step: step["body"].pop("operation_id"),
            lambda step: step.update(capture={}),
            lambda step: step.pop("checkpoint"),
        )
        for mutate in mutations:
            plan = canonical_plan()
            mutate(plan["steps"][1])
            with self.assertRaises(runner.JourneyError):
                self.run_plan(plan)
        plan = canonical_plan()
        plan["steps"][0]["assertions"] = [{"pointer": "/status", "equals": "ready"}]
        with self.assertRaisesRegex(runner.JourneyError, "explicit ready assertion"):
            self.run_plan(plan)
        plan = canonical_plan()
        plan["steps"][3]["checkpoint"] = "negative_checkpoint"
        with self.assertRaisesRegex(runner.JourneyError, "only positive"):
            self.run_plan(plan)
        self.assertEqual(sum(self.state.calls.values()), 0)

    def test_unavailable_adapter_fails_closed_without_completing_step(self) -> None:
        plan = canonical_plan()
        step = plan["steps"][2]
        step["path"] = "/operator/unavailable"
        with self.assertRaisesRegex(runner.JourneyError, "adapter is unavailable"):
            self.run_plan(plan)
        ledger = json.loads(self.ledger.read_text(encoding="utf-8"))
        self.assertNotIn("plan_project", ledger["completed"])

    def test_resume_rejects_changed_or_tampered_request_digest(self) -> None:
        self.run_plan(checkpoint="after_customer_request")
        ledger = json.loads(self.ledger.read_text(encoding="utf-8"))
        ledger["completed"]["submit"]["request_digest"] = "0" * 64
        runner.atomic_json_write(self.ledger, ledger)
        with self.assertRaisesRegex(runner.JourneyError, "ledger semantics"):
            self.run_plan()
        self.assertEqual(self.state.calls["/customer/workflow/commands"], 1)

    def test_resume_replays_server_authority_and_rejects_changed_outcome(self) -> None:
        self.run_plan(checkpoint="after_customer_request")
        self.state.response_overrides["/customer/workflow/commands"] = {
            "request_id": "changed-request",
            "request_digest": DIGEST_A,
        }
        with self.assertRaisesRegex(runner.JourneyError, "replay changed the outcome"):
            self.run_plan()
        self.assertEqual(self.state.calls["/customer/workflow/commands"], 2)
        self.assertEqual(self.state.effective_mutations["/customer/workflow/commands"], 1)

    def test_resume_rejects_missing_server_outcome(self) -> None:
        self.run_plan(checkpoint="after_customer_request")
        self.state.response_overrides["/customer/workflow/commands"] = {
            "request_digest": DIGEST_A
        }
        with self.assertRaisesRegex(runner.JourneyError, "missing a required JSON pointer"):
            self.run_plan()
        self.assertEqual(self.state.effective_mutations["/customer/workflow/commands"], 1)

    def test_resume_ledger_is_bound_to_exact_target_origin(self) -> None:
        self.run_plan(checkpoint="after_customer_request")
        other_state = FakeState()
        other_server = FakeServer(other_state)
        thread = threading.Thread(target=other_server.serve_forever, daemon=True)
        thread.start()
        try:
            with self.assertRaisesRegex(runner.JourneyError, "does not match"):
                runner.run_journey(
                    canonical_plan(),
                    f"http://127.0.0.1:{other_server.server_port}",
                    self.credentials,
                    self.ledger,
                    self.evidence,
                    1.0,
                )
            self.assertEqual(sum(other_state.calls.values()), 0)
        finally:
            other_server.shutdown()
            other_server.server_close()
            thread.join(timeout=1)

    def test_record_chain_and_evidence_tampering_fail_before_http(self) -> None:
        self.run_plan(checkpoint="after_customer_request")
        prior_calls = sum(self.state.calls.values())
        ledger = json.loads(self.ledger.read_text(encoding="utf-8"))
        ledger["completed"]["submit"]["prior_record_digest"] = "f" * 64
        runner.atomic_json_write(self.ledger, ledger)
        with self.assertRaisesRegex(runner.JourneyError, "ledger semantics"):
            self.run_plan()
        self.assertEqual(sum(self.state.calls.values()), prior_calls)

        ledger["completed"]["submit"]["prior_record_digest"] = ledger["completed"][
            "readiness"
        ]["record_digest"]
        ledger["completed"]["submit"]["record_digest"] = runner.record_digest(
            ledger["completed"]["submit"]
        )
        ledger["chain_tip"] = ledger["completed"]["submit"]["record_digest"]
        runner.atomic_json_write(self.ledger, ledger)
        evidence = json.loads(self.evidence.read_text(encoding="utf-8"))
        evidence["record_chain_tip"] = "f" * 64
        runner.atomic_json_write(self.evidence, evidence)
        with self.assertRaisesRegex(runner.JourneyError, "evidence does not match"):
            self.run_plan()
        self.assertEqual(sum(self.state.calls.values()), prior_calls)

    def test_record_chain_truncation_fails_before_http(self) -> None:
        self.run_plan(checkpoint="after_customer_request")
        prior_calls = sum(self.state.calls.values())
        ledger = json.loads(self.ledger.read_text(encoding="utf-8"))
        del ledger["completed"]["submit"]
        ledger["chain_tip"] = ledger["completed"]["readiness"]["record_digest"]
        runner.atomic_json_write(self.ledger, ledger)
        with self.assertRaisesRegex(runner.JourneyError, "evidence does not match"):
            self.run_plan()
        self.assertEqual(sum(self.state.calls.values()), prior_calls)

    def test_record_chain_reordering_fails_before_http(self) -> None:
        self.run_plan(checkpoint="after_customer_request")
        prior_calls = sum(self.state.calls.values())
        ledger = json.loads(self.ledger.read_text(encoding="utf-8"))
        readiness = ledger["completed"]["readiness"]
        submit = ledger["completed"]["submit"]
        ledger["completed"]["readiness"] = submit
        ledger["completed"]["submit"] = readiness
        runner.atomic_json_write(self.ledger, ledger)
        with self.assertRaisesRegex(runner.JourneyError, "ledger"):
            self.run_plan()
        self.assertEqual(sum(self.state.calls.values()), prior_calls)

    def test_plan_rejects_future_reference_before_any_http_mutation(self) -> None:
        plan = canonical_plan()
        plan["steps"][1]["body"]["future"] = {"$ref": "execute.artifact_id"}
        with self.assertRaisesRegex(runner.JourneyError, "unavailable capture"):
            self.run_plan(plan)
        self.assertEqual(sum(self.state.calls.values()), 0)

    def test_resume_rejects_non_prefix_completed_steps(self) -> None:
        plan = canonical_plan()
        self.run_plan(plan, checkpoint="after_customer_request")
        ledger = json.loads(self.ledger.read_text(encoding="utf-8"))
        del ledger["completed"]["readiness"]
        runner.atomic_json_write(self.ledger, ledger)
        prior_calls = sum(self.state.calls.values())
        with self.assertRaisesRegex(runner.JourneyError, "canonical completed-step prefix"):
            self.run_plan(plan)
        self.assertEqual(sum(self.state.calls.values()), prior_calls)

    def test_provider_call_and_non_token_free_plan_are_rejected(self) -> None:
        plan = canonical_plan()
        plan["steps"][4]["provider_call"] = True
        with self.assertRaisesRegex(runner.JourneyError, "provider calls are forbidden"):
            self.run_plan(plan)
        plan = canonical_plan()
        plan["provider_mode"] = "real"
        with self.assertRaisesRegex(runner.JourneyError, "token_free"):
            self.run_plan(plan)

    def test_unsafe_output_paths_are_rejected(self) -> None:
        for value in (
            "/tmp/issue-650.json",
            "relative.json",
            "/work/tmp/project-sentinel",
            "/work/tmp/project-sentinel/not-json.txt",
        ):
            with self.subTest(value=value):
                with self.assertRaises(runner.JourneyError):
                    runner.safe_output_path(value, "evidence")

    def test_symlink_output_path_is_rejected(self) -> None:
        target = self.directory / "target.json"
        link = self.directory / "link.json"
        link.symlink_to(target)
        with self.assertRaisesRegex(runner.JourneyError, "symlink"):
            runner.safe_output_path(str(link), "evidence")

    def test_symlink_output_parent_is_rejected_even_within_safe_root(self) -> None:
        actual = self.directory / "actual"
        actual.mkdir()
        linked = self.directory / "linked"
        linked.symlink_to(actual, target_is_directory=True)
        with self.assertRaisesRegex(runner.JourneyError, "symlink"):
            runner.safe_output_path(str(linked / "evidence.json"), "evidence")

    def test_concurrent_runner_fails_closed_before_second_http_request(self) -> None:
        self.state.delay_path = "/operator/readiness"
        outcome: list[object] = []

        def first_runner() -> None:
            try:
                outcome.append(self.run_plan(checkpoint="after_customer_request"))
            except Exception as exc:  # pragma: no cover - asserted below
                outcome.append(exc)

        thread = threading.Thread(target=first_runner)
        thread.start()
        self.assertTrue(self.state.entered.wait(timeout=1))
        second = self.directory / "second-ledger"
        second.mkdir(mode=0o700)
        with self.assertRaisesRegex(runner.JourneyError, "exclusive lock"):
            runner.run_journey(
                canonical_plan(),
                self.base_url,
                self.credentials,
                second / "ledger.json",
                second / "evidence.json",
                1.0,
                "after_customer_request",
            )
        thread.join(timeout=3)
        self.assertFalse(thread.is_alive())
        self.assertEqual(len(outcome), 1)
        self.assertIsInstance(outcome[0], dict)
        self.assertEqual(self.state.calls["/operator/readiness"], 1)
        lock = runner.lock_path_for("journey-fixture-1")
        self.assertEqual(stat.S_IMODE(lock.stat().st_mode), 0o600)

    def test_permissive_ledger_evidence_and_parent_are_rejected(self) -> None:
        self.run_plan(checkpoint="after_customer_request")
        prior_calls = sum(self.state.calls.values())
        self.ledger.chmod(0o644)
        with self.assertRaisesRegex(runner.JourneyError, "owner-only"):
            self.run_plan()
        self.assertEqual(sum(self.state.calls.values()), prior_calls)
        self.ledger.chmod(0o600)
        self.evidence.chmod(0o644)
        with self.assertRaisesRegex(runner.JourneyError, "owner-only"):
            self.run_plan()
        self.assertEqual(sum(self.state.calls.values()), prior_calls)
        self.evidence.chmod(0o600)
        lock = runner.lock_path_for("journey-fixture-1")
        lock.chmod(0o644)
        with self.assertRaisesRegex(runner.JourneyError, "owner-only"):
            self.run_plan()
        self.assertEqual(sum(self.state.calls.values()), prior_calls)
        lock.chmod(0o600)

        unsafe = self.directory / "unsafe-parent"
        unsafe.mkdir(mode=0o755)
        with self.assertRaisesRegex(runner.JourneyError, "owner-only"):
            runner.run_journey(
                canonical_plan(),
                self.base_url,
                self.credentials,
                unsafe / "ledger.json",
                unsafe / "evidence.json",
                1.0,
            )

    def test_bounded_timeout_is_fail_closed_and_public_safe(self) -> None:
        plan = canonical_plan()
        self.state.delay_path = "/operator/readiness"
        with self.assertRaisesRegex(runner.JourneyError, "HTTP transport failed") as caught:
            runner.run_journey(
                plan,
                self.base_url,
                self.credentials,
                self.ledger,
                self.evidence,
                0.05,
            )
        message = str(caught.exception)
        self.assertNotIn("secret", message)
        self.assertFalse(self.ledger.exists())

    def test_slow_body_is_bounded_by_complete_request_deadline(self) -> None:
        plan = canonical_plan()
        plan["steps"][0]["path"] = "/operator/slow-body"
        with self.assertRaisesRegex(runner.JourneyError, "deadline expired"):
            runner.run_journey(
                plan,
                self.base_url,
                self.credentials,
                self.ledger,
                self.evidence,
                0.05,
            )

    def test_strict_http_json_response_bounds(self) -> None:
        cases = {
            "/operator/wrong-content": "was not JSON",
            "/operator/duplicate-json": "strict JSON",
            "/operator/oversized-length": "exceeded the limit",
            "/operator/oversized-chunked": "exceeded the limit",
        }
        for path, message in cases.items():
            with self.subTest(path=path):
                plan = canonical_plan()
                plan["steps"][0]["path"] = path
                case = self.directory / uuid.uuid4().hex
                case.mkdir(mode=0o700)
                with self.assertRaisesRegex(runner.JourneyError, message):
                    runner.run_journey(
                        plan,
                        self.base_url,
                        self.credentials,
                        case / "ledger.json",
                        case / "evidence.json",
                        1.0,
                    )

    def test_missing_credential_names_role_without_exposing_environment(self) -> None:
        credentials = dict(self.credentials)
        credentials.pop("customer")
        with self.assertRaisesRegex(
            runner.JourneyError, "credential reference is missing for role customer"
        ):
            runner.run_journey(
                canonical_plan(),
                self.base_url,
                credentials,
                self.ledger,
                self.evidence,
                1.0,
            )

    def test_credentials_must_use_distinct_references_and_values(self) -> None:
        references = dict(self.credentials)
        references["agent"] = references["customer"]
        with self.assertRaisesRegex(runner.JourneyError, "references must be role-separated"):
            runner.run_journey(
                canonical_plan(),
                self.base_url,
                references,
                self.ledger,
                self.evidence,
                1.0,
            )
        os.environ["M0_TEST_AGENT_CREDENTIAL"] = "customer-secret-value"
        with self.assertRaisesRegex(runner.JourneyError, "values must be role-separated"):
            self.run_plan()

    def test_echoed_credential_cannot_enter_typed_evidence(self) -> None:
        self.state.response_overrides["/customer/workflow/commands"] = {
            "request_id": "customer-secret-value",
            "request_digest": DIGEST_A,
        }
        with self.assertRaisesRegex(runner.JourneyError, "capture a credential"):
            self.run_plan()
        persisted = self.ledger.read_text(encoding="utf-8")
        self.assertNotIn("customer-secret-value", persisted)

    def test_secret_substrings_and_controls_fail_without_disclosure(self) -> None:
        plan = canonical_plan()
        plan["steps"][1]["body"]["summary_ref"] = (
            "prefix-customer-secret-value-suffix"
        )
        with self.assertRaises(runner.JourneyError) as caught:
            self.run_plan(plan)
        self.assertNotIn("customer-secret-value", str(caught.exception))
        self.assertFalse(self.state.bodies)

        os.environ["M0_TEST_CUSTOMER_CREDENTIAL"] = "unsafe\ncredential-value"
        stderr = io.StringIO()
        plan_path = self.directory / "plan.json"
        plan_path.write_text(json.dumps(canonical_plan()), encoding="utf-8")
        with redirect_stderr(stderr):
            exit_code = runner.main(
                [
                    "--plan",
                    str(plan_path),
                    "--base-url",
                    self.base_url,
                    "--credential",
                    "operator=M0_TEST_OPERATOR_CREDENTIAL",
                    "--credential",
                    "customer=M0_TEST_CUSTOMER_CREDENTIAL",
                    "--credential",
                    "agent=M0_TEST_AGENT_CREDENTIAL",
                    "--ledger",
                    str(self.ledger),
                    "--evidence",
                    str(self.evidence),
                ]
            )
        self.assertEqual(exit_code, 1)
        self.assertNotIn("unsafe\ncredential-value", stderr.getvalue())

    def test_sensitive_body_keys_and_short_credentials_fail_pre_http(self) -> None:
        plan = canonical_plan()
        plan["steps"][1]["body"]["api_token"] = "public-value"
        with self.assertRaisesRegex(runner.JourneyError, "sensitive"):
            self.run_plan(plan)
        os.environ["M0_TEST_CUSTOMER_CREDENTIAL"] = "too-short"
        with self.assertRaisesRegex(runner.JourneyError, "unsafe for role customer"):
            self.run_plan()
        self.assertEqual(sum(self.state.calls.values()), 0)

    def test_capture_rejects_sensitive_or_unbounded_values(self) -> None:
        plan = canonical_plan()
        plan["steps"][1]["capture"]["private"] = {
            "pointer": "/private_token",
            "type": "id",
        }
        with self.assertRaisesRegex(runner.JourneyError, "targets sensitive data"):
            self.run_plan(plan)

    def test_capture_rejects_wrong_response_type(self) -> None:
        self.state.response_overrides["/customer/workflow/commands"] = {
            "request_id": 42,
            "request_digest": DIGEST_A,
        }
        with self.assertRaisesRegex(runner.JourneyError, "captured id"):
            self.run_plan()


if __name__ == "__main__":
    unittest.main()
