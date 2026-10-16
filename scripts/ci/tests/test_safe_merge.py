import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
HEAD = "a" * 40

FAKE_GH = '''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
if args[:2] == ['auth', 'status']: sys.exit(0)
if args[:2] == ['repo', 'view']: print('silentspike/project-sentinel')
elif args[:2] == ['pr', 'view']:
 print(json.dumps({'number': 123, 'state': 'OPEN', 'isDraft': False,
 'headRefOid': os.environ.get('FAKE_HEAD', 'a'*40), 'url': 'https://example.invalid/pr',
 'body': 'Partially addresses #856', 'isCrossRepository': os.environ.get('FAKE_FORK') == '1', 'baseRefName': 'main'}))
elif args[:2] == ['issue', 'view']: print(json.dumps({'labels': [{'name':'quality:ready'}]}))
elif args[:2] == ['run', 'list']:
 print(json.dumps([{'name':n,'status':'completed','conclusion':os.environ.get('FAKE_CONCLUSION','success'),
 'event':'pull_request','databaseId':i} for i,n in enumerate(['CI','PR Lint','PR Quality Gate'])]))
elif args[0] == 'api':
 print(json.dumps({'id':100,'name':'ci-pass','app':{'id':int(os.environ.get('FAKE_APP','15368'))},
 'status':'completed','conclusion':os.environ.get('FAKE_CHECK','success')}))
elif args[:2] == ['pr', 'merge']:
 with open(os.environ['MERGE_ARGS'], 'w') as f: json.dump(args,f)
 if os.environ.get('FAKE_RACE') == '1': sys.exit(1)
else: sys.exit(2)
'''


class SafeMergeTests(unittest.TestCase):
    def run_merge(self, method="merge", reviewed=HEAD, **overrides):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gh = root / "gh"
            gh.write_text(FAKE_GH)
            gh.chmod(0o700)
            merged = root / "merge-args.json"
            result = subprocess.run(["bash", str(ROOT / "scripts/safe-merge.sh"), "123", method, reviewed],
                                    cwd=ROOT, env={**os.environ, "PATH": f'{root}:{os.environ["PATH"]}',
                                                   "MERGE_ARGS": str(merged), **overrides}, capture_output=True, text=True)
            return result, json.loads(merged.read_text()) if merged.exists() else None

    def test_all_methods_bind_reviewed_head(self):
        for method in ("merge", "squash", "rebase"):
            result, args = self.run_merge(method)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn(f"--{method}", args)
            self.assertEqual(args[args.index("--match-head-commit") + 1], HEAD)
            self.assertNotIn("--admin", args)

    def test_missing_review_or_changed_head_fails_before_merge(self):
        for options in ({"reviewed": ""}, {"FAKE_HEAD": "b" * 40}, {"FAKE_FORK": "1"}):
            result, args = self.run_merge(**options)
            self.assertNotEqual(result.returncode, 0)
            self.assertIsNone(args)

    def test_failed_checks_foreign_app_or_workflow_cannot_merge(self):
        for options in ({"FAKE_CHECK": "failure"}, {"FAKE_APP": "999"}, {"FAKE_CONCLUSION": "failure"}):
            result, args = self.run_merge(**options)
            self.assertNotEqual(result.returncode, 0)
            self.assertIsNone(args)

    def test_server_rejects_head_race(self):
        result, args = self.run_merge(FAKE_RACE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--match-head-commit", args)


if __name__ == "__main__":
    unittest.main()
