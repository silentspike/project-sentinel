#!/usr/bin/env python3
"""Deterministic, network-free validation for a staged static web candidate."""

from __future__ import annotations

import hashlib
import html.parser
import json
import os
from pathlib import Path, PurePosixPath
import sys
from urllib.parse import unquote, urlsplit


MAX_FILES = 1024
MAX_FILE_BYTES = 8 * 1024 * 1024
HTML_SUFFIXES = {".html", ".htm"}
FORBIDDEN_SCHEMES = {"data", "file", "ftp", "http", "https", "javascript"}


class CandidateParser(html.parser.HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.has_html = False
        self.has_head = False
        self.has_body = False
        self.has_title = False
        self.references: list[str] = []
        self.inline_scripts = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        lowered = tag.casefold()
        self.has_html |= lowered == "html"
        self.has_head |= lowered == "head"
        self.has_body |= lowered == "body"
        self.has_title |= lowered == "title"
        if lowered == "script":
            source = dict(attrs).get("src")
            if not source:
                self.inline_scripts += 1
        for key, value in attrs:
            if key.casefold() in {"href", "src"} and value:
                self.references.append(value)


def canonical_relative(value: str) -> PurePosixPath | None:
    parsed = urlsplit(value)
    if parsed.scheme.casefold() in FORBIDDEN_SCHEMES or parsed.netloc:
        return None
    decoded = unquote(parsed.path)
    if not decoded or decoded.startswith("/"):
        return PurePosixPath(".") if not decoded else None
    path = PurePosixPath(decoded)
    if any(part in {"", ".", ".."} for part in path.parts):
        return None
    return path


def fail(code: str, detail: str) -> None:
    print(json.dumps({"schema_version": 1, "outcome": "fail", "code": code,
                      "detail_sha256": hashlib.sha256(detail.encode()).hexdigest()},
                     sort_keys=True, separators=(",", ":")))
    raise SystemExit(1)


def main() -> None:
    if not 2 <= len(sys.argv) <= 65:
        fail("arguments_denied", "runner requires one to sixty-four declared input paths")
    requested = [Path(value) for value in sys.argv[1:]]
    entrypoints = [path for path in requested if path.name == "index.html"]
    if len(entrypoints) != 1:
        fail("entrypoint_missing", "index.html")
    root = entrypoints[0].resolve(strict=True).parent
    if not root.is_dir() or root.is_symlink():
        fail("input_root_invalid", str(root))
    files = sorted(path.resolve(strict=True) for path in requested)
    if len(set(files)) != len(files):
        fail("input_inventory_invalid", "duplicate input")
    if not files or len(files) > MAX_FILES:
        fail("input_inventory_invalid", str(len(files)))
    relative_files: set[PurePosixPath] = set()
    html_files: list[Path] = []
    total_bytes = 0
    for path in files:
        resolved = path.resolve(strict=True)
        if not resolved.is_relative_to(root) or path.is_symlink():
            fail("input_escape", str(path))
        stat = resolved.stat()
        if not os.path.isfile(resolved) or stat.st_size > MAX_FILE_BYTES:
            fail("input_file_invalid", str(path))
        total_bytes += stat.st_size
        relative = PurePosixPath(resolved.relative_to(root).as_posix())
        relative_files.add(relative)
        if resolved.suffix.casefold() in HTML_SUFFIXES:
            html_files.append(resolved)
    if not html_files or PurePosixPath("index.html") not in relative_files:
        fail("entrypoint_missing", "index.html")

    references = 0
    for path in html_files:
        parser = CandidateParser()
        try:
            parser.feed(path.read_text(encoding="utf-8"))
            parser.close()
        except (UnicodeDecodeError, html.parser.HTMLParseError) as error:
            fail("html_invalid", f"{path}:{error.__class__.__name__}")
        if not all((parser.has_html, parser.has_head, parser.has_title, parser.has_body)):
            fail("html_structure", str(path))
        if parser.inline_scripts:
            fail("inline_script_denied", str(path))
        base = PurePosixPath(path.relative_to(root).parent.as_posix())
        for reference in parser.references:
            target = canonical_relative(reference)
            if target is None:
                fail("external_or_unsafe_reference", reference)
            if target == PurePosixPath("."):
                continue
            normalized = base / target
            if any(part == ".." for part in normalized.parts) or normalized not in relative_files:
                fail("local_reference_missing", f"{path}:{reference}")
            references += 1

    inventory = "\n".join(f"{path}:{(root / path).stat().st_size}" for path in sorted(relative_files))
    print(json.dumps({
        "schema_version": 1,
        "outcome": "pass",
        "suite_id": "web-qa-v1",
        "files": len(files),
        "html_files": len(html_files),
        "references": references,
        "bytes": total_bytes,
        "inventory_sha256": hashlib.sha256(inventory.encode()).hexdigest(),
    }, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
