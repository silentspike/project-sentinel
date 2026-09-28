#!/usr/bin/env python3
"""Exercise native Python/Node file operations on an explicitly supplied workspace.

This is a filesystem mechanism probe, not evidence of model-driven employee work.
Run on the deployment mount or inside its actual workbench, never on the builder.
"""

import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


NODE_PROBE = r"""
const fs = require('node:fs');
const path = require('node:path');
const root = process.argv[1];
const source = path.join(root, 'node-source.js');
fs.writeFileSync(source, 'module.exports = n => n + 1;\n');
const fd = fs.openSync(source, 'r+');
fs.fsyncSync(fd);
fs.closeSync(fd);
const target = path.join(root, 'node-module.js');
fs.renameSync(source, target);
if (require(target)(41) !== 42) throw new Error('module execution mismatch');
fs.copyFileSync(target, path.join(root, 'node-copy.js'), fs.constants.COPYFILE_EXCL);
if (fs.readFileSync(target, 'utf8') !== fs.readFileSync(path.join(root, 'node-copy.js'), 'utf8'))
  throw new Error('copy mismatch');
const stats = fs.statfsSync(root);
if (stats.bsize <= 0 || stats.blocks <= 0) throw new Error('invalid statfs');
console.log(JSON.stringify({node_file_operations: 'pass'}));
"""


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def append_lines(path, worker):
    fd = os.open(path, os.O_WRONLY | os.O_APPEND)
    try:
        for index in range(20):
            line = f"{worker}:{index}\n".encode()
            require(os.write(fd, line) == len(line), "short append write")
        os.fsync(fd)
    finally:
        os.close(fd)


def probe(root):
    directory = root / "nested"
    directory.mkdir()
    source = directory / "source.bin"
    with source.open("w+b", buffering=0) as stream:
        stream.write(b"abcdef")
        stream.seek(2)
        stream.write(b"XY")
        stream.seek(0)
        require(stream.read() == b"abXYef", "read-your-writes mismatch")
        stream.truncate(10)
        stream.seek(0)
        require(stream.read() == b"abXYef\0\0\0\0", "extension is not zero-filled")
        stream.truncate(4)
        os.fsync(stream.fileno())
    require(source.read_bytes() == b"abXY", "durable truncate mismatch")

    descriptor = os.open(source, os.O_RDWR)
    try:
        original_inode = os.fstat(descriptor).st_ino
        replacement = directory / "renamed.bin"
        source.rename(replacement)
        require(replacement.stat().st_ino == original_inode, "rename changed inode identity")
        replacement.unlink()
        require(os.pread(descriptor, 4, 0) == b"abXY", "open-unlink lost content")
        require(os.pwrite(descriptor, b"ok", 0) == 2, "open-unlink write failed")
        os.fsync(descriptor)
        require(os.pread(descriptor, 4, 0) == b"okXY", "open-unlink write was not visible")
    finally:
        os.close(descriptor)

    source.write_bytes(b"shared inode")
    hardlink = directory / "hardlink.bin"
    os.link(source, hardlink)
    require(source.stat().st_ino == hardlink.stat().st_ino, "hardlink did not share inode")
    hardlink.write_bytes(b"same object")
    require(source.read_bytes() == b"same object", "hardlink write diverged")
    symlink = directory / "relative-link"
    symlink.symlink_to(source.name)
    require(symlink.read_bytes() == b"same object", "relative symlink resolution failed")
    require(os.readlink(symlink) == source.name, "readlink changed target")
    source.chmod(0o640)
    require(source.stat().st_mode & 0o777 == 0o640, "chmod was not applied")

    destination = directory / "replace.bin"
    destination.write_bytes(b"old")
    staging = directory / "staged.bin"
    staging.write_bytes(b"new")
    os.replace(staging, destination)
    require(destination.read_bytes() == b"new" and not staging.exists(), "atomic replacement failed")
    try:
        with source.open("xb"):
            pass
    except FileExistsError:
        require(source.read_bytes() == b"same object", "exclusive create changed content")
    else:
        raise RuntimeError("exclusive create unexpectedly succeeded")

    sparse = directory / "sparse.bin"
    with sparse.open("w+b", buffering=0) as stream:
        stream.seek(1_000_000)
        stream.write(b"x")
        os.fsync(stream.fileno())
        stream.seek(999_996)
        require(stream.read() == b"\0\0\0\0x", "sparse hole was not zero-filled")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--node", default="node")
    arguments = parser.parse_args()
    workspace = arguments.workspace.resolve(strict=True)
    require(workspace.is_dir(), "workspace is not a directory")
    root = Path(tempfile.mkdtemp(prefix=".sentinel-posix-probe-", dir=workspace))
    try:
        probe(root)
        append = root / "append.log"
        append.touch()
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
            list(executor.map(lambda worker: append_lines(append, worker), range(4)))
        expected = {f"{worker}:{index}" for worker in range(4) for index in range(20)}
        lines = append.read_text().splitlines()
        require(len(lines) == 80 and set(lines) == expected, "concurrent append lost or duplicated writes")
        directory_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
        subprocess.run([arguments.node, "-e", NODE_PROBE, str(root)], check=True, timeout=30,
                       stdin=subprocess.DEVNULL, capture_output=True, text=True)
        print(json.dumps({"python_file_operations": "pass", "node_file_operations": "pass",
                          "scope": "native_posix_mechanism_only"}, sort_keys=True))
    finally:
        shutil.rmtree(root)


if __name__ == "__main__":
    main()
