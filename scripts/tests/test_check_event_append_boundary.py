import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check-event-append-boundary.py"
SPEC = importlib.util.spec_from_file_location("check_event_append_boundary", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class EventAppendBoundaryTest(unittest.TestCase):
    def fixture(self) -> Path:
        root = Path(tempfile.mkdtemp())
        store = root / "crates/sentinel-limbo/src/event_store.rs"
        store.parent.mkdir(parents=True)
        store.write_text(
            "pub(crate) fn append_event() {}\n"
            "pub(crate) fn append_with_outbox() {}\n"
            "pub(crate) fn append_with_outbox_batch() {}\n",
            encoding="utf-8",
        )
        go_store = root / "pkg/sentinel-go/eventstore/store.go"
        go_store.parent.mkdir(parents=True)
        go_store.write_text("func (s *Store) appendWithOutbox() {}\n", encoding="utf-8")
        return root

    def test_accepts_classified_gateways(self):
        root = self.fixture()
        rust = root / "services/example/main.rs"
        rust.parent.mkdir(parents=True)
        rust.write_text(
            "store.legacy_append_gateway(LegacyEventProducer::TestHarness)\n"
            "    .append_event(&event);\n",
            encoding="utf-8",
        )
        go = root / "cmd/example/main.go"
        go.parent.mkdir(parents=True)
        go.write_text(
            "store.LegacyAppendGateway(eventstore.LegacyProducerCortexAudit)."
            "AppendWithOutbox(event, topic)\n",
            encoding="utf-8",
        )
        self.assertEqual(MODULE.check(root), [])

    def test_rejects_public_and_unclassified_writers(self):
        root = self.fixture()
        store = root / "crates/sentinel-limbo/src/event_store.rs"
        store.write_text(store.read_text().replace("pub(crate) fn append_event", "pub fn append_event"))
        rust = root / "services/example/main.rs"
        rust.parent.mkdir(parents=True)
        rust.write_text("store.append_event(&event);\n", encoding="utf-8")
        errors = MODULE.check(root)
        self.assertTrue(any("raw Rust writer append_event is public" in error for error in errors))
        self.assertTrue(any("unclassified Rust append_event" in error for error in errors))

    def test_rejects_second_go_schema_owner_and_raw_sql(self):
        root = self.fixture()
        main = root / "cmd/example/main.go"
        main.parent.mkdir(parents=True)
        main.write_text('eventstore.Open("events.db")\n', encoding="utf-8")
        rogue = root / "services/example/rogue.rs"
        rogue.parent.mkdir(parents=True)
        rogue.write_text('sql!("INSERT INTO events (event_id) VALUES (?)");\n', encoding="utf-8")
        rust_owner = root / "services/example/main.rs"
        rust_owner.write_text('let store = EventStore::open("events.db");\n', encoding="utf-8")
        errors = MODULE.check(root)
        self.assertTrue(any("may not own event DDL" in error for error in errors))
        self.assertTrue(any("raw events-table insert" in error for error in errors))

    def test_ignores_rust_schema_creation_inside_test_module(self):
        root = self.fixture()
        rust = root / "services/example/lib.rs"
        rust.parent.mkdir(parents=True)
        rust.write_text(
            "#[cfg(test)]\nmod tests {\n"
            "    fn fixture() { let _ = EventStore::open(\":memory:\"); }\n"
            "}\n",
            encoding="utf-8",
        )
        self.assertEqual(MODULE.check(root), [])


if __name__ == "__main__":
    unittest.main()
