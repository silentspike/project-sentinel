#!/usr/bin/env python3
"""Source-only regression tests for the #650 operator credential boundary."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "deploy/scripts/init-dashboard-auth.sh"
DASHBOARD_UNIT = REPO_ROOT / "deploy/systemd/sentinel-dashboard-backend.service"
DAEMON_UNIT = REPO_ROOT / "deploy/systemd/sentinel-daemon.service"
GATEWAY_UNIT = REPO_ROOT / "deploy/systemd/sentinel-gateway.service"
NIGHTRUN_UNIT = REPO_ROOT / "deploy/systemd/sentinel-nightrun.service"
DAEMON_CONFIG = REPO_ROOT / "config/daemon.toml"
GATEWAY_MAIN = REPO_ROOT / "cmd/cortex-gateway/main.go"
GATEWAY_CREDENTIAL = REPO_ROOT / "cmd/cortex-gateway/internal/proxy/auth.go"
DAEMON_MAIN = REPO_ROOT / "services/sentinel-daemon/src/main.rs"
DAEMON_CREDENTIAL = REPO_ROOT / "services/sentinel-daemon/src/config.rs"
DASHBOARD_CONFIG = REPO_ROOT / "services/sentinel-dashboard-backend/src/lib.rs"
DASHBOARD_MAIN = REPO_ROOT / "services/sentinel-dashboard-backend/src/main.rs"
DASHBOARD_PROXY = REPO_ROOT / "services/sentinel-dashboard-backend/src/control.rs"
DASHBOARD_GAIA = REPO_ROOT / "services/sentinel-dashboard-backend/src/gaia.rs"
SENTINEL_CTL = REPO_ROOT / "services/sentinel-ctl/src/main.rs"
GAIA_CONFIG = REPO_ROOT / "services/sentinel-gaia-loop/src/config.rs"
GAIA_SESSION = REPO_ROOT / "services/sentinel-gaia-loop/src/session.rs"
GAIA_SYSTEM_PROMPT = REPO_ROOT / "services/sentinel-gaia-loop/prompts/gaia-system.md"
M0_PREFLIGHT = REPO_ROOT / "scripts/product-acceptance/run_m0_preflight.py"

DASHBOARD_SECRET = "dashboard-" + "a" * 40
OPERATOR_SECRET = "operator-" + "b" * 40
OTHER_OPERATOR_SECRET = "operator-" + "c" * 40


class OperatorCredentialWiringTests(unittest.TestCase):
    def setUp(self) -> None:
        runner_root = Path(
            os.environ.get(
                "RUNNER_TEMP",
                "/work/tmp/project-sentinel/cdx3-650-operator-credential-tests",
            )
        )
        runner_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        runner_root.chmod(0o700)
        self.case = runner_root / str(uuid.uuid4())
        self.case.mkdir(mode=0o700)
        self.env_file = self.case / "config/dashboard-backend.env"
        self.credential_file = self.case / "credentials/operator-api"

    def tearDown(self) -> None:
        shutil.rmtree(self.case, ignore_errors=True)

    def run_script(self) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.update(
            {
                "SENTINEL_AUTH_TEST_ROOT": str(self.case),
                "SENTINEL_DASHBOARD_ENV_FILE": str(self.env_file),
                "SENTINEL_OPERATOR_CREDENTIAL_FILE": str(self.credential_file),
            }
        )
        return subprocess.run(
            ["bash", str(SCRIPT)],
            cwd=REPO_ROOT,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=15,
            check=False,
        )

    def write_env(self, *lines: str) -> None:
        self.env_file.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
        self.env_file.write_text("\n".join(lines) + "\n", encoding="ascii")
        self.env_file.chmod(0o600)

    def write_credential(self, value: str) -> None:
        self.credential_file.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.credential_file.write_text(value, encoding="ascii")
        self.credential_file.chmod(0o400)

    def assert_public_output(self, result: subprocess.CompletedProcess[str]) -> None:
        for secret in (DASHBOARD_SECRET, OPERATOR_SECRET, OTHER_OPERATOR_SECRET):
            self.assertNotIn(secret, result.stdout)
            self.assertNotIn(secret, result.stderr)

    def test_units_use_exact_file_only_systemd_credential_contract(self) -> None:
        dashboard_source = DASHBOARD_UNIT.read_text(encoding="utf-8")
        daemon_source = DAEMON_UNIT.read_text(encoding="utf-8")
        gateway_source = GATEWAY_UNIT.read_text(encoding="utf-8")
        load_credential = (
            "LoadCredential=operator-api:/etc/sentinel/credentials/operator-api\n"
        )

        for source in (dashboard_source, daemon_source, gateway_source):
            self.assertEqual(source.count(load_credential), 1)
            self.assertNotIn("SENTINEL_OPERATOR_API_KEY=", source)
            self.assertNotIn("SENTINEL_OPERATOR_SHARED_SECRET=", source)

        self.assertIn(
            "Environment=SENTINEL_OPERATOR_API_KEY_FILE=%d/operator-api\n",
            dashboard_source,
        )
        self.assertNotIn("SENTINEL_OPERATOR_CREDENTIAL_FILE=", dashboard_source)
        self.assertIn(
            "Environment=SENTINEL_OPERATOR_CREDENTIAL_FILE=%d/operator-api\n",
            daemon_source,
        )
        self.assertNotIn("SENTINEL_OPERATOR_API_KEY_FILE=", daemon_source)
        self.assertIn(
            "Environment=SENTINEL_OPERATOR_API_KEY_FILE=%d/operator-api\n",
            gateway_source,
        )
        self.assertNotIn("SENTINEL_OPERATOR_CREDENTIAL_FILE=", gateway_source)
        self.assertIn("independent dashboard login key", dashboard_source)
        self.assertIn("ExecStartPre=/usr/bin/test -x /usr/bin/bwrap", dashboard_source)

    def test_productive_operator_consumers_use_file_references_only(self) -> None:
        gateway_main = GATEWAY_MAIN.read_text(encoding="utf-8")
        gateway_credential = GATEWAY_CREDENTIAL.read_text(encoding="utf-8")
        daemon_main = DAEMON_MAIN.read_text(encoding="utf-8")
        daemon_credential = DAEMON_CREDENTIAL.read_text(encoding="utf-8")
        dashboard_config = DASHBOARD_CONFIG.read_text(encoding="utf-8")
        dashboard_main = DASHBOARD_MAIN.read_text(encoding="utf-8")
        dashboard_proxy = DASHBOARD_PROXY.read_text(encoding="utf-8")
        dashboard_gaia = DASHBOARD_GAIA.read_text(encoding="utf-8")
        sentinel_ctl = SENTINEL_CTL.read_text(encoding="utf-8")
        gaia_config = GAIA_CONFIG.read_text(encoding="utf-8")
        gaia_session = GAIA_SESSION.read_text(encoding="utf-8")
        gaia_system_prompt = GAIA_SYSTEM_PROMPT.read_text(encoding="utf-8")
        preflight = M0_PREFLIGHT.read_text(encoding="utf-8")

        self.assertIn("proxy.LoadAPICPOperatorCredential", gateway_main)
        self.assertIn("apicpSyncURL := \"\"", gateway_main)
        self.assertIn("SetAPICPActivationValidator", gateway_main)
        self.assertNotIn('os.Getenv("SENTINEL_OPERATOR_API_KEY")', gateway_main)
        self.assertRegex(
            gateway_credential,
            r'operatorCredentialFileEnv\s*=\s*"SENTINEL_OPERATOR_API_KEY_FILE"',
        )
        self.assertIn("syscall.O_NOFOLLOW", gateway_credential)
        self.assertIn("os.LookupEnv(directOperatorCredentialEnv)", gateway_credential)
        self.assertIn("bind_operator_credential", daemon_main)
        self.assertIn("OPERATOR_CREDENTIAL_FILE_ENV", daemon_main)
        self.assertIn("operator credential file is required", daemon_credential)
        self.assertIn("Config::from_production_env()", dashboard_main)
        self.assertIn(
            "direct operator API credentials are not allowed in production",
            dashboard_config,
        )
        self.assertIn("OPERATOR_KEY_HEADER", dashboard_proxy)
        self.assertIn("st.config.operator_key", dashboard_proxy)
        self.assertIn("read_operator_credential()?", sentinel_ctl)
        self.assertNotIn('std::env::var("SENTINEL_OPERATOR_API_KEY")', sentinel_ctl)
        self.assertIn("execute_via_broker", sentinel_ctl)
        self.assertIn("permits only read observations", sentinel_ctl)
        self.assertNotIn("SENTINEL_OPERATOR_API_KEY", gaia_config)
        self.assertIn("command.env_clear();", gaia_session)
        self.assertIn("OPERATOR_BROKER_SOCKET_ENV", gaia_session)
        self.assertIn("OPERATOR_BROKER_SESSION_ENV", gaia_session)
        self.assertIn("OPERATOR_BROKER_CAPABILITY_ENV", gaia_session)
        self.assertNotIn(
            'command.env("SENTINEL_OPERATOR_API_KEY"',
            gaia_session.split("#[cfg(test)]", 1)[0],
        )
        self.assertNotIn(
            'command.env("SENTINEL_OPERATOR_API_KEY_FILE"',
            gaia_session.split("#[cfg(test)]", 1)[0],
        )
        self.assertIn("verify_broker_peer", dashboard_gaia)
        self.assertIn("process_group_for_pid", dashboard_gaia)
        self.assertIn("expected_pgid", dashboard_gaia)
        self.assertIn("validate_broker_request", dashboard_gaia)
        self.assertIn("claim_broker_operation", dashboard_gaia)
        self.assertIn("permits only bodyless read observations", dashboard_gaia)
        self.assertNotIn('("POST", true, "/control/reload")', dashboard_gaia)
        self.assertIn("broker_process_authority_binds_once_and_revokes", gaia_session)
        self.assertIn('const BWRAP_BIN: &str = "/usr/bin/bwrap"', gaia_session)
        self.assertIn('"--tmpfs"', gaia_session)
        self.assertIn('"--unshare-pid"', gaia_session)
        self.assertIn('"/proc"', gaia_session)
        self.assertIn("`sentinel-ctl` is observation-only", gaia_system_prompt)
        self.assertIn("governed customer/operator workflow", gaia_system_prompt)
        self.assertIn("do not retry it as a mutation", gaia_system_prompt)
        self.assertIn('parser.add_argument("--operator-credential-file"', preflight)
        self.assertIn("load_operator_credential(inputs, deps)", preflight)
        self.assertNotIn("SENTINEL_OPERATOR_API_KEY=", preflight)

    def test_nightrun_current_main_authenticated_oneshot_after_integration(
        self,
    ) -> None:
        source = NIGHTRUN_UNIT.read_text(encoding="utf-8")
        if "m0-readiness.py nightrun" not in source:
            self.skipTest("PR #790 Nightrun contract awaits the required Main integration")
        self.assertEqual(
            source.count(
                "LoadCredential=operator-api:/etc/sentinel/credentials/operator-api\n"
            ),
            1,
        )
        self.assertIn(
            "m0-readiness.py nightrun --credential-file %d/operator-api",
            source,
        )
        self.assertNotIn("curl", source)
        self.assertNotIn("SENTINEL_OPERATOR_API_KEY=", source)

    def test_generated_credentials_are_independent_secure_and_idempotent(self) -> None:
        first = self.run_script()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assert_public_output(first)
        dashboard_value = self.env_file.read_text(encoding="ascii")
        operator_value = self.credential_file.read_text(encoding="ascii")
        operator_inode = self.credential_file.stat().st_ino
        self.assertRegex(dashboard_value, r"^SENTINEL_DASHBOARD_API_KEY=[0-9a-f]{64}\n$")
        self.assertRegex(operator_value, r"^[0-9a-f]{64}$")
        self.assertNotEqual(dashboard_value.removeprefix("SENTINEL_DASHBOARD_API_KEY=").strip(), operator_value)
        self.assertEqual(self.env_file.stat().st_mode & 0o7777, 0o600)
        self.assertEqual(self.credential_file.stat().st_mode & 0o7777, 0o400)
        self.assertEqual(self.credential_file.parent.stat().st_mode & 0o7777, 0o700)
        self.assertEqual(self.env_file.stat().st_uid, os.geteuid())
        self.assertEqual(self.env_file.stat().st_gid, os.getegid())
        self.assertEqual(self.credential_file.stat().st_uid, os.geteuid())
        self.assertEqual(self.credential_file.stat().st_gid, os.getegid())

        second = self.run_script()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assert_public_output(second)
        self.assertEqual(self.env_file.read_text(encoding="ascii"), dashboard_value)
        self.assertEqual(self.credential_file.read_text(encoding="ascii"), operator_value)
        self.assertEqual(self.credential_file.stat().st_ino, operator_inode)

    def test_legacy_operator_value_migrates_without_changing_dashboard_key(self) -> None:
        self.write_env(
            "SENTINEL_DASHBOARD_RATE_LIMIT=7",
            f"SENTINEL_DASHBOARD_API_KEY={DASHBOARD_SECRET}",
            f"SENTINEL_OPERATOR_API_KEY={OPERATOR_SECRET}",
        )
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_public_output(result)
        self.assertEqual(self.credential_file.read_text(encoding="ascii"), OPERATOR_SECRET)
        environment = self.env_file.read_text(encoding="ascii")
        self.assertIn("SENTINEL_DASHBOARD_RATE_LIMIT=7\n", environment)
        self.assertIn(f"SENTINEL_DASHBOARD_API_KEY={DASHBOARD_SECRET}\n", environment)
        self.assertNotIn("SENTINEL_OPERATOR_API_KEY", environment)

    def test_matching_legacy_and_file_value_converge_but_conflict_fails_closed(self) -> None:
        self.write_env(
            f"SENTINEL_DASHBOARD_API_KEY={DASHBOARD_SECRET}",
            f"SENTINEL_OPERATOR_API_KEY={OPERATOR_SECRET}",
        )
        self.write_credential(OPERATOR_SECRET)
        matching = self.run_script()
        self.assertEqual(matching.returncode, 0, matching.stderr)
        self.assert_public_output(matching)
        self.assertNotIn("SENTINEL_OPERATOR_API_KEY", self.env_file.read_text(encoding="ascii"))

        self.write_env(
            f"SENTINEL_DASHBOARD_API_KEY={DASHBOARD_SECRET}",
            f"SENTINEL_OPERATOR_API_KEY={OTHER_OPERATOR_SECRET}",
        )
        before_file = self.credential_file.read_bytes()
        conflict = self.run_script()
        self.assertNotEqual(conflict.returncode, 0)
        self.assert_public_output(conflict)
        self.assertEqual(self.credential_file.read_bytes(), before_file)
        self.assertIn("SENTINEL_OPERATOR_API_KEY", self.env_file.read_text(encoding="ascii"))

    def test_script_rejects_symlink_hardlink_and_test_root_escape(self) -> None:
        self.write_env(f"SENTINEL_DASHBOARD_API_KEY={DASHBOARD_SECRET}")
        target = self.case / "operator-target"
        target.write_text(OPERATOR_SECRET, encoding="ascii")
        target.chmod(0o400)
        self.credential_file.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.credential_file.symlink_to(target)
        symlink_result = self.run_script()
        self.assertNotEqual(symlink_result.returncode, 0)
        self.assert_public_output(symlink_result)

        self.credential_file.unlink()
        os.link(target, self.credential_file)
        hardlink_result = self.run_script()
        self.assertNotEqual(hardlink_result.returncode, 0)
        self.assert_public_output(hardlink_result)

        environment = os.environ.copy()
        environment.update(
            {
                "SENTINEL_AUTH_TEST_ROOT": str(self.case),
                "SENTINEL_DASHBOARD_ENV_FILE": str(self.case.parent / "escaped.env"),
                "SENTINEL_OPERATOR_CREDENTIAL_FILE": str(self.credential_file),
            }
        )
        escape = subprocess.run(
            ["bash", str(SCRIPT)],
            cwd=REPO_ROOT,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=15,
            check=False,
        )
        self.assertNotEqual(escape.returncode, 0)
        self.assert_public_output(escape)

    def test_invalid_existing_canonical_file_fails_without_mutation(self) -> None:
        cases = (
            ("newline", b"n" * 32 + b"\n", 0o400),
            ("control", b"c" * 16 + b"\x01" + b"c" * 16, 0o400),
            ("nul", b"z" * 16 + b"\x00" + b"z" * 16, 0o400),
            ("unsafe-mode", OPERATOR_SECRET.encode("ascii"), 0o644),
        )
        for name, content, mode in cases:
            with self.subTest(name=name):
                shutil.rmtree(self.case, ignore_errors=True)
                self.case.mkdir(mode=0o700)
                self.write_env(f"SENTINEL_DASHBOARD_API_KEY={DASHBOARD_SECRET}")
                self.credential_file.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                self.credential_file.write_bytes(content)
                self.credential_file.chmod(mode)
                env_before = self.env_file.read_bytes()
                credential_before = self.credential_file.read_bytes()
                metadata_before = self.credential_file.stat()

                result = self.run_script()
                self.assertNotEqual(result.returncode, 0)
                self.assert_public_output(result)
                self.assertEqual(self.env_file.read_bytes(), env_before)
                self.assertEqual(self.credential_file.read_bytes(), credential_before)
                metadata_after = self.credential_file.stat()
                self.assertEqual(metadata_after.st_mode, metadata_before.st_mode)
                self.assertEqual(metadata_after.st_ino, metadata_before.st_ino)

    @unittest.skipUnless(os.geteuid() == 0, "foreign-owner representation requires root")
    def test_foreign_owned_existing_file_fails_without_mutation(self) -> None:
        self.write_env(f"SENTINEL_DASHBOARD_API_KEY={DASHBOARD_SECRET}")
        self.write_credential(OPERATOR_SECRET)
        os.chown(self.credential_file, 65534, 65534)
        env_before = self.env_file.read_bytes()
        credential_before = self.credential_file.read_bytes()
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assert_public_output(result)
        self.assertEqual(self.env_file.read_bytes(), env_before)
        self.assertEqual(self.credential_file.read_bytes(), credential_before)

    def test_canonical_config_and_units_do_not_embed_operator_authority(self) -> None:
        self.assertNotIn("shared_secret", DAEMON_CONFIG.read_text(encoding="utf-8"))
        for unit in sorted((REPO_ROOT / "deploy/systemd").glob("*.service")):
            source = unit.read_text(encoding="utf-8")
            self.assertNotIn("SENTINEL_OPERATOR_API_KEY=", source, unit)


if __name__ == "__main__":
    unittest.main()
