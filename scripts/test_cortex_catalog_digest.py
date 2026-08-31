#!/usr/bin/env python3
"""Token-free regression tests for cortex-catalog-v1 normalization."""

from __future__ import annotations

import copy
import hashlib
import tomllib
import unittest
from pathlib import Path

from scripts.cortex_catalog_digest import normalize


def catalog() -> dict:
    return {
        "providers": {
            "beta": {
                "type": "mock",
                "default_model": "b2",
                "allowed_models": ["b3", "b1", "b2"],
                "hierarchy_models": {"tier_1": "b1", "tier_2": "b2", "tier_3": "b3"},
            },
            "alpha": {
                "type": "local-loop",
                "default_model": "a2",
                "allowed_models": ["a2", "a1", "a3"],
                "hierarchy_models": {"tier_1": "a1", "tier_2": "a2", "tier_3": "a3"},
            },
        }
    }


class CatalogDigestTests(unittest.TestCase):
    def test_provider_and_allowlist_order_do_not_change_normalized_bytes(self) -> None:
        first = catalog()
        second = copy.deepcopy(first)
        second["providers"] = {
            "alpha": second["providers"]["alpha"],
            "beta": second["providers"]["beta"],
        }
        second["providers"]["alpha"]["allowed_models"].reverse()
        second["providers"]["beta"]["allowed_models"].reverse()
        self.assertEqual(normalize(first), normalize(second))

    def test_semantic_content_change_changes_normalized_bytes(self) -> None:
        first = catalog()
        second = copy.deepcopy(first)
        second["providers"]["alpha"]["default_model"] = "a1"
        self.assertNotEqual(normalize(first), normalize(second))

    def test_hierarchy_mapping_changes_semantic_bytes(self) -> None:
        first = catalog()
        second = copy.deepcopy(first)
        second["providers"]["alpha"]["hierarchy_models"] = {
            "tier_1": "a3",
            "tier_2": "a2",
            "tier_3": "a1",
        }
        self.assertNotEqual(normalize(first), normalize(second))

    def test_normalized_bytes_match_v1_golden(self) -> None:
        expected = (
            b'{"algorithm":"cortex-catalog-v1","providers":['
            b'{"allowed_models":["a1","a2","a3"],"default_model":"a2",'
            b'"hierarchy_models":{"tier_1":"a1","tier_2":"a2","tier_3":"a3"},'
            b'"id":"alpha","type":"local-loop"},'
            b'{"allowed_models":["b1","b2","b3"],"default_model":"b2",'
            b'"hierarchy_models":{"tier_1":"b1","tier_2":"b2","tier_3":"b3"},'
            b'"id":"beta","type":"mock"}]}'
            b"\n"
        )
        self.assertEqual(normalize(catalog()), expected)

    def test_repository_catalog_matches_gate_c_semantic_pin(self) -> None:
        path = Path(__file__).resolve().parents[1] / "config" / "cortex-gateway.toml"
        document = tomllib.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(
            hashlib.sha256(normalize(document)).hexdigest(),
            "50eb02d1dec87cdeee8dda8252862128d45f488e780323477ae824b4f96a6647",
        )

    def test_incomplete_hierarchy_map_fails_closed(self) -> None:
        document = catalog()
        del document["providers"]["alpha"]["hierarchy_models"]["tier_3"]
        with self.assertRaisesRegex(ValueError, "define exactly"):
            normalize(document)

    def test_extended_provider_schema_fails_closed(self) -> None:
        document = catalog()
        document["providers"]["alpha"]["credential"] = "must-not-enter-catalog"
        with self.assertRaisesRegex(ValueError, "unknown fields"):
            normalize(document)

    def test_surrounding_model_whitespace_fails_closed(self) -> None:
        document = catalog()
        document["providers"]["alpha"]["allowed_models"][0] = " a2"
        with self.assertRaisesRegex(ValueError, "invalid allowed_models"):
            normalize(document)


if __name__ == "__main__":
    unittest.main()
