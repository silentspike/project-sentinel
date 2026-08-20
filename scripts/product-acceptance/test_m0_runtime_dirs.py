#!/usr/bin/env python3
"""Regression tests for the pre-service M0 runtime-directory authority."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "deploy/scripts/init-m0-runtime-dirs.py"
AUTH_UNIT = REPO_ROOT / "deploy/systemd/sentinel-auth-init.service"
GAIA_UNIT = REPO_ROOT / "deploy/systemd/sentinel-gaia-loop.service"
DASHBOARD_UNIT = REPO_ROOT / "deploy/systemd/sentinel-dashboard-backend.service"
MANIFEST_GENERATOR = REPO_ROOT / "deploy/generate-manifest.sh"
PROVISIONER = REPO_ROOT / "deploy/provision-m0-single-node.sh"
PREFLIGHT = REPO_ROOT / "scripts/product-acceptance/run_m0_preflight.py"


class M0RuntimeDirectoryTests(unittest.TestCase):
    def setUp(self) -> None:
        base = Path(
            os.environ.get(
                "RUNNER_TEMP",
                "/work/tmp/project-sentinel/orc-650-runtime-dir-tests",
            )
        )
        base.mkdir(mode=0o700, parents=True, exist_ok=True)
        base.chmod(0o700)
        self.root = base / str(uuid.uuid4())
        self.root.mkdir(mode=0o700)
        current = self.root
        for component, mode in (("opt", 0o755), ("sentinel", 0o755), ("data", 0o750)):
            current /= component
            current.mkdir(mode=mode)
        self.gaia = current / "gaia-console"

    def tearDown(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)

    def run_initializer(self) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["SENTINEL_M0_RUNTIME_DIRS_TEST_ROOT"] = str(self.root)
        return subprocess.run(
            ["python3", str(SCRIPT)],
            cwd=REPO_ROOT,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
            check=False,
        )

    def test_initializer_is_idempotent_and_private(self) -> None:
        for _ in range(2):
            result = self.run_initializer()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                result.stdout,
                "m0_runtime_dirs=verified directories=2 mode=0700\n",
            )
        for path in (self.gaia, self.gaia / "sessions"):
            self.assertTrue(path.is_dir())
            self.assertFalse(path.is_symlink())
            self.assertEqual(path.stat().st_uid, os.geteuid())
            self.assertEqual(path.stat().st_gid, os.getegid())
            self.assertEqual(path.stat().st_mode & 0o777, 0o700)

    def test_legacy_data_root_mode_is_narrowed_without_touching_content(self) -> None:
        data = self.root / "opt/sentinel/data"
        retained = data / "retained"
        retained.write_text("preserve", encoding="ascii")
        retained.chmod(0o600)
        data.chmod(0o775)
        result = self.run_initializer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(data.stat().st_mode & 0o777, 0o750)
        self.assertEqual(retained.read_text(encoding="ascii"), "preserve")
        self.assertEqual(retained.stat().st_mode & 0o777, 0o600)

    def test_symlink_file_and_unsafe_parent_fail_closed(self) -> None:
        cases = ("symlink", "file", "unsafe-parent")
        for case in cases:
            with self.subTest(case=case):
                shutil.rmtree(self.root)
                self.setUp()
                if case == "symlink":
                    foreign = self.root / "foreign"
                    foreign.mkdir(mode=0o700)
                    self.gaia.symlink_to(foreign, target_is_directory=True)
                elif case == "file":
                    self.gaia.write_text("not-a-directory", encoding="ascii")
                else:
                    (self.root / "opt/sentinel/data").chmod(0o777)
                result = self.run_initializer()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("ERROR: runtime_directory_", result.stderr)

    def test_systemd_orders_initializer_before_every_gaia_consumer(self) -> None:
        auth = AUTH_UNIT.read_text(encoding="utf-8")
        gaia = GAIA_UNIT.read_text(encoding="utf-8")
        dashboard = DASHBOARD_UNIT.read_text(encoding="utf-8")
        self.assertEqual(
            auth.count(
                "ExecStart=/usr/bin/python3 /opt/sentinel/scripts/init-m0-runtime-dirs.py\n"
            ),
            1,
        )
        for consumer in (gaia, dashboard):
            after = next(line for line in consumer.splitlines() if line.startswith("After="))
            requires = next(
                line for line in consumer.splitlines() if line.startswith("Requires=")
            )
            self.assertIn(
                "sentinel-auth-init.service", after.removeprefix("After=").split()
            )
            self.assertIn(
                "sentinel-auth-init.service",
                requires.removeprefix("Requires=").split(),
            )
            self.assertIn("/opt/sentinel/data/gaia-console", consumer)

    def test_release_authorities_include_the_initializer_exactly_once(self) -> None:
        source = "deploy/scripts/init-m0-runtime-dirs.py"
        destination = "/opt/sentinel/scripts/init-m0-runtime-dirs.py"
        for path in (MANIFEST_GENERATOR, PROVISIONER, PREFLIGHT):
            text = path.read_text(encoding="utf-8")
            self.assertEqual(text.count(source), 1, path)
            self.assertEqual(text.count(destination), 1, path)


if __name__ == "__main__":
    unittest.main()
