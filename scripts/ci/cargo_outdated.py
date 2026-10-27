"""Keep virtual-workspace path patches intact during cargo-outdated inspection.

cargo-outdated 0.19 copies the virtual root outside its path-rewrite loop.
Relocate the one registered patch in a verified disposable copy, never in source.
"""

import argparse
import copy
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
VENDOR = "vendor/wasmtime-internal-cache"
PATCH = "wasmtime-internal-cache"
ENTRY = 'wasmtime-internal-cache = { path = "vendor/wasmtime-internal-cache" }'


class InspectionError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise InspectionError(message)


def git(root, *arguments):
    return subprocess.check_output(["git", "-C", str(root), *arguments], stderr=subprocess.PIPE)


def clean_head(root):
    require(root.is_absolute() and root.resolve(strict=True) == root, "source must be canonical")
    require(Path(os.fsdecode(git(root, "rev-parse", "--show-toplevel")).strip()) == root,
            "source must be the repository root")
    require(not git(root, "status", "--porcelain=v1", "--untracked-files=all"), "source must be clean")
    require(git(root, "rev-parse", "--show-object-format").strip() == b"sha1", "unsupported Git object format")
    return git(root, "rev-parse", "HEAD").decode().strip()


def scratch_parent(path, source):
    require(path.is_absolute() and path.resolve(strict=True) == path and path.is_dir(),
            "scratch must be an existing canonical directory")
    require(not path.is_relative_to(source) and not path.is_relative_to(Path("/tmp")),
            "scratch must be outside source and /tmp")
    for parent in (path, *path.parents):
        for name in ("Cargo.toml", "go.work", ".cargo/config", ".cargo/config.toml"):
            require(not os.path.lexists(parent / name), "scratch has an inherited workspace or Cargo config")


def snapshot(source, head, destination):
    # Export committed objects, not ignored private files or a racing working tree.
    entries = {}
    for row in git(source, "ls-tree", "-rz", head).split(b"\0"):
        if not row:
            continue
        metadata, name = row.split(b"\t", 1)
        mode, kind, digest = metadata.split()
        require(kind == b"blob" and mode in (b"100644", b"100755"), "unsupported tracked link or object")
        entries[os.fsdecode(name)] = (mode, digest.decode())
    raw = git(source, "archive", "--format=tar", head)
    observed = set()
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as archive:
        for member in archive:
            path = PurePosixPath(member.name)
            require(not path.is_absolute() and ".." not in path.parts, "unsafe export path")
            target = destination.joinpath(*path.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            require(member.isfile() and member.name in entries and member.name not in observed,
                    "unexpected exported file")
            stream = archive.extractfile(member)
            require(stream is not None, "missing exported bytes")
            data = stream.read()
            mode, expected = entries[member.name]
            digest = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
            require(digest == expected, "export changed committed bytes")
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open("xb") as output:
                output.write(data)
            target.chmod(0o755 if mode == b"100755" else 0o644)
            observed.add(member.name)
    require(observed == set(entries), "export omitted tracked files")
    return entries


def relocate_patch(root):
    manifest = root / "Cargo.toml"
    raw = manifest.read_bytes()
    before = tomllib.loads(raw.decode())
    require("workspace" in before and "package" not in before, "expected virtual workspace")
    expected_patch = {"crates-io": {PATCH: {"path": VENDOR}}}
    require(before.get("patch") == expected_patch, "unrecognized patch configuration")
    vendor = root / VENDOR
    require(vendor.resolve(strict=True) == vendor and vendor.is_dir(), "patch target is missing or linked")
    package = tomllib.loads((vendor / "Cargo.toml").read_text())["package"]
    require(package.get("name") == PATCH and package.get("version") == "49.0.4", "patch package identity changed")
    require(VENDOR in before["workspace"].get("exclude", []), "patch workspace membership changed")
    require(raw.count(ENTRY.encode()) == 1, "ambiguous patch serialization")
    replacement = f"{PATCH} = {{ path = {json.dumps(str(vendor))} }}".encode()
    relocated = raw.replace(ENTRY.encode(), replacement)
    expected = copy.deepcopy(before)
    expected["patch"]["crates-io"][PATCH]["path"] = str(vendor)
    require(tomllib.loads(relocated.decode()) == expected, "relocation changed manifest semantics")
    manifest.write_bytes(relocated)
    return relocated


def inspect(source, temporary):
    head = clean_head(source)
    scratch_parent(temporary, source)
    with tempfile.TemporaryDirectory(prefix="sentinel-freshness-", dir=temporary) as directory:
        operation = Path(directory)
        root = operation / "workspace"
        root.mkdir()
        entries = snapshot(source, head, root)
        relocated = relocate_patch(root)
        lock = (root / "Cargo.lock").read_bytes()
        require(clean_head(source) == head, "source changed during export")
        environment = dict(os.environ, TMPDIR=str(operation), GOWORK="off",
                           CARGO_TARGET_DIR=str(operation / "target"))
        # Both cwd and manifest are explicit. Upstream's own temporary roots are
        # siblings, so they cannot inherit source/parent workspace configuration.
        result = subprocess.run(["cargo", "outdated", "--manifest-path", str(root / "Cargo.toml"),
                                 "--root-deps-only", "--depth", "1"], cwd=root, env=environment)
        require(clean_head(source) == head, "source changed during inspection")
        require((root / "Cargo.toml").read_bytes() == relocated and (root / "Cargo.lock").read_bytes() == lock,
                "inspection changed baseline manifest or lock")
        for name, (_, digest) in entries.items():
            if name.startswith(VENDOR + "/"):
                data = (root / name).read_bytes()
                actual = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
                require(actual == digest, "inspection changed patched package bytes")
        return result.returncode if result.returncode >= 0 else 128 - result.returncode


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT)
    parser.add_argument("--scratch-dir", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        return inspect(args.source, args.scratch_dir)
    except InspectionError as error:
        print(f"cargo-outdated inspection rejected: {error}", file=sys.stderr)
    except (OSError, subprocess.CalledProcessError, tarfile.TarError, ValueError, KeyError):
        print("cargo-outdated inspection failed before a valid freshness result", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
