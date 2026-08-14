#!/usr/bin/env python3
"""Product wiring tests for the bounded M0 company workflow."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid


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
        self.assertEqual(len(values), 8)
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
        self.assertEqual(len(names), 8)

        daemon = DAEMON_UNIT.read_text(encoding="utf-8")
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
