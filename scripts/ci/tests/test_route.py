import copy
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("route", ROOT / "scripts/ci/route.py")
route = importlib.util.module_from_spec(spec)
spec.loader.exec_module(route)


class RouteTests(unittest.TestCase):
    def needs(self, paths):
        outputs = route.classify(paths)
        return {name: {"result": "success", "outputs": outputs if name == "changes" else {}}
                for name in route.ALWAYS | route.JOBS}

    def test_go_modules_and_shared_schema(self):
        for path in ("cmd/cortex-gateway/go.mod", "pkg/sentinel-go/go.sum", "services/sentinel-judge/main.go"):
            self.assertEqual(route.classify([path])["go"], "true")
        self.assertEqual(route.classify(["schemas/event/foo.fbs"])["rust"], "true")

    def test_unknown_build_and_fixture_inputs_invalidate(self):
        for path in ("scripts/test.sh", ".github/workflows/ci.yml", ".cargo/config.toml", "rust-toolchain.toml", "docs/test.py"):
            self.assertTrue(all(value == "true" for value in route.classify([path]).values()))
        self.assertEqual(route.classify(["crates/example/fixtures/test.md"])["rust"], "true")

    def test_docs_only_skips(self):
        needs = self.needs(["README.md", "docs/design.md"])
        for name in route.JOBS:
            needs[name]["result"] = "skipped"
        route.evaluate(needs)

    def test_classifier_failure_or_missing_outputs_fails(self):
        for result in ("failure", "cancelled", "skipped", "", None):
            needs = self.needs(["README.md"])
            needs["changes"]["result"] = result
            with self.assertRaises(ValueError):
                route.evaluate(needs)
        needs = self.needs(["README.md"])
        del needs["changes"]["outputs"]["rust"]
        with self.assertRaises(ValueError):
            route.evaluate(needs)

    def test_required_skip_and_any_failure_fail(self):
        for name in route.ALWAYS | route.JOBS:
            for result in ("failure", "cancelled", "skipped", "unknown"):
                needs = self.needs(["scripts/ci/route.py"])
                needs[name]["result"] = result
                with self.assertRaises(ValueError, msg=(name, result)):
                    route.evaluate(needs)

    def test_empty_inventory_and_extra_jobs_fail(self):
        with self.assertRaises(ValueError):
            route.classify([])
        needs = self.needs(["README.md"])
        for name in ("changes", "lint", "rust-test"):
            missing = copy.deepcopy(needs)
            del missing[name]
            with self.assertRaises(ValueError):
                route.evaluate(missing)


if __name__ == "__main__":
    unittest.main()
