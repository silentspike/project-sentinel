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
        values = {}
        for path in self.credentials.iterdir():
            self.assertTrue(path.is_file())
            self.assertEqual(path.stat().st_mode & 0o777, 0o400)
            values[path.name] = path.read_text(encoding="ascii")
            self.assertEqual(len(values[path.name]), 64)
            self.assertNotIn(values[path.name], first.stdout + first.stderr)
        self.assertEqual(len(values), 23)
        self.assertEqual(len(set(values.values())), 23)
        expected = {binding["credential_name"] for binding in json.loads(PRINCIPALS.read_text())["bindings"]}
        self.assertEqual(set(values), expected)
        self.assertTrue(self.workflow_data.is_dir())
        self.assertEqual(self.workflow_data.stat().st_mode & 0o777, 0o700)

        second = self.run_init()
        self.assertEqual(second.returncode, 0, second.stderr)
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
        self.assertEqual(len(names), 23)
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
        self.assertEqual(len(list(self.credentials.iterdir())), 23)
        for name, value in old.items():
            self.assertEqual((self.credentials / name).read_text(encoding="ascii"), value)
            self.assertNotIn(value, result.stdout + result.stderr)

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
        (self.credentials / "workflow-customer").symlink_to(foreign)
        result = self.run_init()
        self.assertNotEqual(result.returncode, 0)

    def test_initializer_rejects_symlinked_workflow_data_root(self) -> None:
        foreign = self.case / "foreign-data"
        foreign.mkdir(mode=0o700)
        self.workflow_data.symlink_to(foreign, target_is_directory=True)
        result = self.run_init()
        self.assertNotEqual(result.returncode, 0)

    def test_initializer_rejects_malformed_existing_credential(self) -> None:
        self.credentials.mkdir(mode=0o700)
        credential = self.credentials / "workflow-customer"
        credential.write_text("z" * 64, encoding="ascii")
        credential.chmod(0o400)
        result = self.run_init()
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
