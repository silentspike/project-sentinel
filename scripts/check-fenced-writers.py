#!/usr/bin/env python3
"""check-fenced-writers.py — #496 owner-write fence enforcement (R2 split-brain guard).

Every persistent mutation of a fenced store must route through that store's single
`begin_fenced_write` choke point — the owner-write fence (V3/V19). Today the fence is a
behavior-preserving no-op (PR1a/PR1b/PR1c); once PR2 activates the real owner-epoch
check, a single writer that bypasses the choke point becomes a split-brain hole (a stale
owner persisting after losing ownership). The type system already hides the raw handle
(the `conn`/`db` field is private to each crate), but this gate is the cheap, permanent
backstop that fails CI the moment a *new* in-module writer is added that acquires a raw
write handle without the fence — which a code review can miss.

Two store shapes, two precise rules:

  * redb-backed stores (`StateStore`, `MetadataStore`) — the write primitive is
    `Database::begin_write()` (reads use `begin_read()`), so a raw `.begin_write()`
    outside the fence is the violation.
  * the SQLite EventStore (limbo) — reads and writes share `conn.lock()`, so the write
    primitive is a write SQL statement (INSERT/UPDATE/DELETE/REPLACE) or a call to the
    `set_sim_metadata_conn` write helper; a method that performs one of those without
    `begin_fenced_write` is the violation.

Not runtime writers (whitelisted): the `begin_fenced_write` method itself; constructors
and schema bootstrap (`open*`, `new`, `ensure_*_migrations`); free helpers that operate
on a caller-supplied `&Connection` (the *caller* holds the fence); and `#[cfg(test)]`
code.

Usage: python3 scripts/check-fenced-writers.py
Exits non-zero (and prints each offending writer) on any violation.
"""

import os
import re
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

FENCE = "begin_fenced_write"
# Constructors / schema bootstrap run once before the store serves writes; they legitimately
# take a raw write handle because no ownership exists yet.
CONSTRUCTORS = {"open", "open_with_durability", "new", "ensure_outbox_migrations"}

# Each fenced store file and the regex identifying a *raw write* in that store's shape.
STORES = [
    {
        "label": "EventStore (sentinel-limbo)",
        "file": "crates/sentinel-limbo/src/event_store.rs",
        # SQLite: reads and writes share conn.lock(), so the write primitive is the
        # mutation itself (or the set_sim_metadata_conn write helper).
        "write_re": re.compile(
            r"\b(INSERT\s+INTO|UPDATE\s+\w|DELETE\s+FROM|REPLACE\s+INTO)\b"
            r"|\bset_sim_metadata_conn\s*\("
        ),
    },
    {
        "label": "StateStore (sentinel-redb)",
        "file": "crates/sentinel-redb/src/lib.rs",
        "write_re": re.compile(r"\.begin_write\s*\(\s*\)"),
    },
    {
        "label": "MetadataStore (sentinel-fs)",
        "file": "crates/sentinel-fs/src/metadata.rs",
        "write_re": re.compile(r"\.begin_write\s*\(\s*\)"),
    },
]

FN_HEADER = re.compile(
    r"^(\s*)(?:pub\s*(?:\([^)]*\)\s*)?)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_]\w*)"
)
# A free helper that receives an already-locked connection — the caller owns the fence.
CONN_PARAM = re.compile(r"&\s*(?:rusqlite::)?(?:mut\s+)?Connection\b")
_STRIP = re.compile(r'"(?:\\.|[^"\\])*"' r"|'(?:\\.|[^'\\])'" r"|//.*$")


def _brace_delta(line):
    """Net brace count of a line, ignoring string/char literals and line comments —
    so format strings (`{e}`), SQL containing braces, and comments cannot skew depth."""
    cleaned = _STRIP.sub("", line)
    return cleaned.count("{") - cleaned.count("}")


def _test_region_start(lines):
    """1-based line at which `#[cfg(test)] mod tests` begins (or len+1 if none). Computed
    by a standalone scan so test detection never depends on brace bookkeeping."""
    for idx, line in enumerate(lines):
        if re.match(r"^\s*(?:pub\s+)?mod\s+tests\b", line):
            return idx + 1
    return len(lines) + 1


def fn_blocks(text):
    """Yield (name, start_line, signature, body, is_test) for every fn in the file."""
    lines = text.splitlines()
    n = len(lines)
    test_start = _test_region_start(lines)
    i = 0
    while i < n:
        m = FN_HEADER.match(lines[i])
        if not m:
            i += 1
            continue
        name = m.group(2)
        start = i
        # A fn is test code if it lives in the `mod tests` region or carries a #[cfg(test)]
        # attribute directly above it (skipping other attribute / blank lines).
        is_test = (start + 1) >= test_start
        j = start - 1
        while j >= 0 and (lines[j].strip().startswith("#[") or lines[j].strip() == ""):
            if lines[j].strip().startswith("#[cfg(test)]"):
                is_test = True
                break
            j -= 1
        # Accumulate signature (until first '{') and body via literal-aware brace depth.
        depth = 0
        seen_open = False
        got_sig = False
        sig_parts = []
        body_parts = []
        while i < n:
            cur = lines[i]
            body_parts.append(cur)
            if not got_sig:
                brace = cur.find("{")
                sig_parts.append(cur if brace < 0 else cur[:brace])
                if brace >= 0:
                    got_sig = True
            depth += _brace_delta(cur)
            if "{" in cur:
                seen_open = True
            i += 1
            if seen_open and depth <= 0:
                break
        yield (name, start + 1, "\n".join(sig_parts), "\n".join(body_parts), is_test)


def check_store(store):
    path = os.path.join(REPO_ROOT, store["file"])
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    violations = []
    for name, start_line, signature, body, is_test in fn_blocks(text):
        if is_test or name == FENCE or name in CONSTRUCTORS:
            continue
        if CONN_PARAM.search(signature):
            continue  # free helper on a caller-supplied connection; caller holds the fence
        if store["write_re"].search(body) and FENCE not in body:
            violations.append((start_line, name))
    return violations


def main():
    total = 0
    print("== #496 fenced-writer enforcement ==")
    for store in STORES:
        violations = check_store(store)
        rel = store["file"]
        if violations:
            total += len(violations)
            for line, name in violations:
                print(
                    f"::error file={rel},line={line}::{store['label']}: writer "
                    f"`{name}` performs a raw write without `{FENCE}` "
                    f"(split-brain risk, #496)"
                )
        else:
            print(f"  OK  {store['label']}: all writers fenced ({rel})")
    if total:
        print(
            f"\nFAILED: {total} unfenced writer(s). Route every persistent mutation "
            f"through `{FENCE}` (see crates/sentinel-common/src/fencing.rs)."
        )
        return 1
    print("\nAll fenced stores route every writer through begin_fenced_write.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
