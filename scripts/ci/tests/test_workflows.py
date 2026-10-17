from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[3]


class WorkflowTests(unittest.TestCase):
    def test_every_pr_workflow_isolates_forks(self):
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            text = path.read_text()
            if not re.search(r"^  pull_request:", text, re.M):
                continue
            for line in text.splitlines():
                if "runs-on:" in line and "self-hosted" in line:
                    # Parentheses are essential: fork || hosted && label would
                    # evaluate to the boolean true instead of a runner label.
                    self.assertIn("((github.event_name == 'pull_request'", line, path.name)
                    self.assertIn("github.repository) || vars.RUNNER_MODE == 'hosted') &&", line, path.name)

    def test_ebpf_errors_are_not_swallowed(self):
        text = (ROOT / ".github/workflows/ci.yml").read_text()
        block = text.split("  rust-ebpf:\n")[1].split("\n  go:\n")[0]
        self.assertNotIn("||", "\n".join(line for line in block.splitlines() if "cargo " in line or "apt-get install" in line))
        self.assertNotIn("continue-on-error", block)

    def test_shadow_reporter_never_loads_candidate_or_self_hosted_runner(self):
        text = (ROOT / ".github/workflows/candidate-report.yml").read_text()
        self.assertNotIn("pull_request:", text)
        self.assertNotIn("self-hosted", text)
        self.assertIn("persist-credentials: false", text)
        self.assertIn("ref: ${{ github.sha }}", text)
        self.assertIn("PLAN_SHA256: ${{ vars.SENTINEL_CI_PLAN_SHA256 }}", text)

    def test_node_reporter_regressions(self):
        result = subprocess.run(["node", "--test", "scripts/ci/tests/test-report.cjs"], cwd=ROOT,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_all_parallel_delivery_mechanisms_are_mandatory_in_lint(self):
        text = (ROOT / ".github/workflows/ci.yml").read_text()
        lint = text.split("  lint:\n")[1].split("  bans:\n")[0]
        name = "      - name: Remote parallel-delivery mechanism regressions\n"
        block = lint.split(name)[1].split("      - name:")[0]
        self.assertLess(lint.index("uses: actions/setup-node@"), lint.index(name))
        for path in ("scripts/ci/test-archive-tests.py", "scripts/product-acceptance/remote_parallel_test.py",
                     "scripts/product-acceptance/remote_parallel_desktop_test.mjs", "deploy/ci-lab/test_control.py"):
            self.assertIn(f"test -f {path}", block)
        for command in ("python3 -B scripts/ci/test-archive-tests.py -v",
                        "python3 -B -m unittest discover -s scripts/product-acceptance -p 'remote_parallel_test.py' -v",
                        "node --test scripts/product-acceptance/remote_parallel_desktop_test.mjs",
                        "python3 -B -m unittest discover -s deploy/ci-lab -p 'test_*.py' -v",
                        "bash -n deploy/ci-lab/prepare-guest.sh"):
            self.assertIn(command, block)
        self.assertNotIn("if:", block)
        self.assertNotIn("if ", block)
        self.assertNotIn("||", block)
        self.assertNotIn("continue-on-error", block)


if __name__ == "__main__":
    unittest.main()
