#!/usr/bin/env python3
"""Calculate the public-safe cortex-catalog-v1 semantic catalog digest."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tomllib
from pathlib import Path
from typing import Any

ALGORITHM = "cortex-catalog-v1"
PROVIDER_FIELDS = {
    "type",
    "base_url",
    "binary",
    "default_model",
    "allowed_models",
    "max_tokens",
    "priority",
    "hierarchy_models",
}
HIERARCHY_FIELDS = {"tier_1", "tier_2", "tier_3"}


def normalize(document: dict[str, Any]) -> bytes:
    providers = document.get("providers")
    if not isinstance(providers, dict) or not providers:
        raise ValueError("providers must be a non-empty table")

    normalized = []
    for provider_id in sorted(providers):
        if not isinstance(provider_id, str) or not provider_id or provider_id.strip() != provider_id:
            raise ValueError("provider IDs must be non-empty strings without surrounding whitespace")
        provider = providers[provider_id]
        if not isinstance(provider, dict):
            raise ValueError(f"provider {provider_id!r} must be a table")
        unknown = set(provider) - PROVIDER_FIELDS
        if unknown:
            raise ValueError(f"provider {provider_id!r} has unknown fields: {sorted(unknown)}")
        hierarchy = provider.get("hierarchy_models")
        if not isinstance(hierarchy, dict) or set(hierarchy) != HIERARCHY_FIELDS:
            raise ValueError(f"provider {provider_id!r} must define exactly tier_1, tier_2, tier_3")
        provider_type = provider.get("type")
        default_model = provider.get("default_model")
        allowed_models = provider.get("allowed_models")
        if not isinstance(provider_type, str) or not provider_type or provider_type.strip() != provider_type:
            raise ValueError(f"provider {provider_id!r} has no type")
        if not isinstance(default_model, str) or not default_model or default_model.strip() != default_model:
            raise ValueError(f"provider {provider_id!r} has no default_model")
        if not isinstance(allowed_models, list) or not allowed_models or not all(
            isinstance(model, str) and model and model.strip() == model for model in allowed_models
        ):
            raise ValueError(f"provider {provider_id!r} has invalid allowed_models")
        if len(set(allowed_models)) != len(allowed_models):
            raise ValueError(f"provider {provider_id!r} repeats an allowed model")
        if not all(
            isinstance(model, str) and model and model.strip() == model
            for model in hierarchy.values()
        ):
            raise ValueError(f"provider {provider_id!r} has invalid hierarchy model IDs")
        if default_model not in allowed_models or any(model not in allowed_models for model in hierarchy.values()):
            raise ValueError(f"provider {provider_id!r} maps a model outside its allowlist")
        normalized.append(
            {
                "id": provider_id,
                "type": provider_type,
                "default_model": default_model,
                "allowed_models": sorted(allowed_models),
                "hierarchy_models": {
                    "tier_1": hierarchy["tier_1"],
                    "tier_2": hierarchy["tier_2"],
                    "tier_3": hierarchy["tier_3"],
                },
            }
        )

    return (
        json.dumps(
            {"algorithm": ALGORITHM, "providers": normalized},
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode("utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("path", nargs="?", type=Path)
    parser.add_argument("--stdin", action="store_true")
    parser.add_argument("--digest-only", action="store_true")
    args = parser.parse_args()
    if args.stdin == (args.path is not None):
        parser.error("provide exactly one of a path or --stdin")
    raw = sys.stdin.buffer.read() if args.stdin else args.path.read_bytes()
    try:
        canonical = normalize(tomllib.loads(raw.decode("utf-8")))
    except (ValueError, tomllib.TOMLDecodeError, UnicodeDecodeError) as exc:
        print(f"catalog validation failed: {exc}", file=sys.stderr)
        return 1
    digest = hashlib.sha256(canonical).hexdigest()
    if args.digest_only:
        print(digest)
    else:
        sys.stdout.buffer.write(canonical)
        print(digest, file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
