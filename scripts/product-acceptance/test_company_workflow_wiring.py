#!/usr/bin/env python3
"""Product wiring tests for the bounded M0 company workflow."""

from __future__ import annotations

import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid
import tomllib


REPO_ROOT = Path(__file__).resolve().parents[2]
INIT = REPO_ROOT / "deploy/scripts/init-company-workflow-auth.sh"
AUTH_UNIT = REPO_ROOT / "deploy/systemd/sentinel-auth-init.service"
DAEMON_UNIT = REPO_ROOT / "deploy/systemd/sentinel-daemon.service"
PRINCIPALS = REPO_ROOT / "config/company-principals.json"


class CompanyWorkflowWiringTests(unittest.TestCase):
    def setUp(self) -> None:
        root = Path(
            os.environ.get(
                "RUNNER_TEMP",
                "/work/tmp/project-sentinel/company-workflow-wiring",
            )
        )
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        root.chmod(0o700)
        self.case = root / str(uuid.uuid4())
        self.case.mkdir(mode=0o700)
        self.credentials = self.case / "credentials"
        self.workflow_data = self.case / "company-delivery"

    def tearDown(self) -> None:
        shutil.rmtree(self.case, ignore_errors=True)

    def run_init(self) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.update(
            {
                "SENTINEL_WORKFLOW_AUTH_TEST_ROOT": str(self.case),
                "SENTINEL_WORKFLOW_CREDENTIAL_DIR": str(self.credentials),
                "SENTINEL_WORKFLOW_DATA_DIR": str(self.workflow_data),
            }
        )
        return subprocess.run(
            ["bash", str(INIT)],
            cwd=REPO_ROOT,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=15,
            check=False,
        )

    def test_initializer_is_idempotent_and_never_prints_credentials(self) -> None:
        first = self.run_init()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, "workflow_credentials=24 existing=0 permissions=0400\n")
        self.assertEqual(first.stderr, "")
        values = {}
        for path in self.credentials.iterdir():
            self.assertTrue(path.is_file())
            self.assertEqual(path.stat().st_mode & 0o777, 0o400)
            values[path.name] = path.read_text(encoding="ascii")
            self.assertEqual(len(values[path.name]), 64)
            self.assertNotIn(values[path.name], first.stdout + first.stderr)
        self.assertEqual(len(values), 24)
        self.assertEqual(len(set(values.values())), 24)
        self.assertNotEqual(values["workflow-internal-customer"], values["workflow-customer"])
        expected = {binding["credential_name"] for binding in json.loads(PRINCIPALS.read_text())["bindings"]}
        self.assertEqual(set(values), expected)
        self.assertTrue(self.workflow_data.is_dir())
        self.assertEqual(self.workflow_data.stat().st_mode & 0o777, 0o700)

        second = self.run_init()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(second.stdout, "workflow_credentials=0 existing=24 permissions=0400\n")
        self.assertEqual(second.stderr, "")
        for value in values.values():
            self.assertNotIn(value, second.stdout + second.stderr)
        self.assertEqual(
            values,
            {
                path.name: path.read_text(encoding="ascii")
                for path in self.credentials.iterdir()
            },
        )

    def test_config_and_systemd_credentials_are_exactly_bijective(self) -> None:
        config = json.loads(PRINCIPALS.read_text(encoding="ascii"))
        names = {binding["credential_name"] for binding in config["bindings"]}
        self.assertEqual(config["schema_version"], 1)
        self.assertEqual(len(names), len(config["bindings"]))
        self.assertEqual(len(names), 24)
        operators = [binding for binding in config["bindings"] if binding["kind"] == "operator"]
        self.assertEqual(len(operators), 1)
        self.assertEqual(operators[0]["credential_name"], "workflow-operator")
        self.assertIsNone(operators[0]["agent_id"])
        self.assertIsNone(operators[0]["customer_id"])

        daemon = DAEMON_UNIT.read_text(encoding="utf-8")
        self.assertEqual(set(re.findall(r"^LoadCredential=(workflow-[^:]+):", daemon, re.M)), names)
        for name in names:
            self.assertEqual(
                daemon.count(
                    f"LoadCredential={name}:/etc/sentinel/credentials/{name}\n"
                ),
                1,
            )
        self.assertIn("Environment=SENTINEL_COMPANY_WORKFLOW_ENABLED=true\n", daemon)
        auth = AUTH_UNIT.read_text(encoding="utf-8")
        self.assertEqual(
            auth.count(
                "ExecStart=/opt/sentinel/scripts/init-company-workflow-auth.sh\n"
            ),
            1,
        )
        self.assertIn("ReadWritePaths=/opt/sentinel/config /opt/sentinel/data /etc/sentinel", auth)

    def test_internal_and_actual_customers_have_independent_identities(self) -> None:
        bindings = json.loads(PRINCIPALS.read_text(encoding="ascii"))["bindings"]
        customers = [binding for binding in bindings if binding["kind"] == "customer"]
        self.assertEqual(len(customers), 2)
        for name, customer_id in (
            ("workflow-customer", "customer-m0"),
            ("workflow-internal-customer", "customer-internal"),
        ):
            self.assertEqual(
                [binding for binding in customers if binding["credential_name"] == name],
                [{
                    "credential_name": name,
                    "tenant_id": "m0-company",
                    "principal_id": customer_id,
                    "kind": "customer",
                    "role": "customer",
                    "customer_id": customer_id,
                    "agent_id": None,
                    "authority_generation": 1,
                }],
            )
            self.assertEqual(sum(binding["principal_id"] == customer_id for binding in bindings), 1)
            self.assertEqual(sum(binding["customer_id"] == customer_id for binding in bindings), 1)

    def test_existing_day_credentials_survive_all_shift_migration(self) -> None:
        self.credentials.mkdir(mode=0o700)
        bindings = json.loads(PRINCIPALS.read_text())["bindings"]
        old = {}
        for index, binding in enumerate(bindings[:9]):
            name = binding["credential_name"]
            old[name] = f"{index + 1:064x}"
            path = self.credentials / name
            path.write_text(old[name], encoding="ascii")
            path.chmod(0o400)
        result = self.run_init()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(list(self.credentials.iterdir())), 24)
        for name, value in old.items():
            self.assertEqual((self.credentials / name).read_text(encoding="ascii"), value)
            self.assertNotIn(value, result.stdout + result.stderr)

    def test_existing_23_credentials_survive_internal_customer_migration(self) -> None:
        self.credentials.mkdir(mode=0o700)
        bindings = json.loads(PRINCIPALS.read_text(encoding="ascii"))["bindings"]
        old = {}
        for index, binding in enumerate(bindings):
            name = binding["credential_name"]
            if name == "workflow-internal-customer":
                continue
            old[name] = f"{index + 1:064x}".encode("ascii")
            path = self.credentials / name
            path.write_bytes(old[name])
            path.chmod(0o400)
        self.assertEqual(len(old), 23)

        first = self.run_init()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, "workflow_credentials=1 existing=23 permissions=0400\n")
        self.assertEqual(first.stderr, "")
        migrated = {path.name: path.read_bytes() for path in self.credentials.iterdir()}
        self.assertEqual(set(migrated), set(old) | {"workflow-internal-customer"})
        internal = migrated["workflow-internal-customer"]
        self.assertRegex(internal, rb"^[0-9a-f]{64}$")
        self.assertNotIn(internal, old.values())
        for name, value in old.items():
            self.assertEqual(migrated[name], value)

        second = self.run_init()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(second.stdout, "workflow_credentials=0 existing=24 permissions=0400\n")
        self.assertEqual(second.stderr, "")
        self.assertEqual(
            {path.name: path.read_bytes() for path in self.credentials.iterdir()},
            migrated,
        )
        self.assertEqual(self.credentials.stat().st_mode & 0o777, 0o700)
        for name, value in migrated.items():
            path = self.credentials / name
            self.assertFalse(path.is_symlink())
            self.assertTrue(path.is_file())
            self.assertEqual(path.stat().st_mode & 0o777, 0o400)
            self.assertEqual(path.stat().st_nlink, 1)
            for result in (first, second):
                self.assertNotIn(value.decode("ascii"), result.stdout + result.stderr)

    def test_all_shift_employees_keep_required_tools_and_distinct_role_identity(self) -> None:
        agents = {}
        for path in (REPO_ROOT / "config/agents").glob("AGENT-*.toml"):
            value = tomllib.loads(path.read_text())
            agents[value["identity"]["id"]] = value
        bindings = json.loads(PRINCIPALS.read_text())["bindings"]
        roles = {"sales", "project_manager", "technical_lead", "designer", "developer", "qa", "release_manager"}
        roster = {shift: {} for shift in (1, 2, 3)}
        for binding in bindings:
            if binding["kind"] != "agent":
                continue
            agent = agents[binding["agent_id"]]
            shift = agent["identity"]["shift_set"]
            self.assertNotIn(binding["role"], roster[shift])
            roster[shift][binding["role"]] = agent
            self.assertEqual(binding["authority_generation"], 1)
        for shift, members in roster.items():
            self.assertEqual(set(members), roles, shift)
            for role in ("designer", "developer", "qa"):
                tools = {tool for tool in members[role]["capabilities"]["tools"] if "." in tool}
                day_tools = {tool for tool in roster[1][role]["capabilities"]["tools"] if "." in tool}
                self.assertEqual(tools, day_tools)

    def test_initializer_rejects_symlinked_credential(self) -> None:
        self.credentials.mkdir(mode=0o700)
        foreign = self.case / "foreign"
        foreign.write_text("x" * 64, encoding="ascii")
        for name in ("workflow-customer", "workflow-internal-customer"):
            with self.subTest(credential=name):
                credential = self.credentials / name
                credential.symlink_to(foreign)
                result = self.run_init()
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(credential.is_symlink())
                self.assertEqual(foreign.read_bytes(), b"x" * 64)
                self.assertNotIn("x" * 64, result.stdout + result.stderr)
                credential.unlink()

    def test_initializer_rejects_symlinked_workflow_data_root(self) -> None:
        foreign = self.case / "foreign-data"
        foreign.mkdir(mode=0o700)
        self.workflow_data.symlink_to(foreign, target_is_directory=True)
        result = self.run_init()
        self.assertNotEqual(result.returncode, 0)

    def test_initializer_rejects_malformed_existing_credential(self) -> None:
        self.credentials.mkdir(mode=0o700)
        for name in ("workflow-customer", "workflow-internal-customer"):
            with self.subTest(credential=name):
                credential = self.credentials / name
                credential.write_text("z" * 64, encoding="ascii")
                credential.chmod(0o400)
                result = self.run_init()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(credential.read_bytes(), b"z" * 64)
                self.assertNotIn("z" * 64, result.stdout + result.stderr)
                credential.unlink()

    def test_initializer_rejects_unprotected_internal_customer_credential(self) -> None:
        self.credentials.mkdir(mode=0o700)
        credential = self.credentials / "workflow-internal-customer"
        value = b"a" * 64
        for mode in (0o600, 0o440, 0o444):
            with self.subTest(mode=oct(mode)):
                credential.write_bytes(value)
                credential.chmod(mode)
                result = self.run_init()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(credential.read_bytes(), value)
                self.assertEqual(credential.stat().st_mode & 0o777, mode)
                self.assertNotIn(value.decode("ascii"), result.stdout + result.stderr)
                credential.unlink()

        foreign = self.case / "foreign-credential"
        foreign.write_bytes(value)
        foreign.chmod(0o400)
        credential.hardlink_to(foreign)
        result = self.run_init()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(foreign.read_bytes(), value)
        self.assertEqual(credential.stat().st_nlink, 2)
        self.assertNotIn(value.decode("ascii"), result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
