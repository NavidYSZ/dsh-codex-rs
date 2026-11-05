#!/usr/bin/env python3
"""Fetch the pinned Pi coding agent runtime for Aether's second chat engine.

Downloads exactly three npm tarballs, checks each against its pinned SHA-512
and copies only the files Pi needs at run time into one folder:

    <out>/package.json, LICENSE, dist/bundle/**      @earendil-works/pi-coding-agent
    <out>/node_modules/jiti/**                       jiti (TypeScript extensions)
    <out>/node_modules/@silvia-odwyer/photon-node/** photon-node (image resizing)

Node itself is the runtime fetch-node.py already pins. Usage:

    python3 desktop/fetch-pi.py [OUT]   default OUT: desktop/build/pi
"""
import base64
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import sys
import tarfile
import urllib.request

PI_VERSION = "1.5.0"
REGISTRY = "https://registry.npmjs.org"
PACKAGES = [
    # (name, version, sha512 integrity, destination below OUT, kept paths)
    (
        "@earendil-works/pi-coding-agent",
        PI_VERSION,
        "xNDZ5kqowO6Z03FS2gwAkPEGk1ssxVcYKQ6qZId84mFEvwACvvyc2l0vAMOS+c5zvxLRMLBE9r4y44IGYkMJaQ==",
        "",
        ("package.json", "LICENSE", "README.md", "dist/bundle/"),
    ),
    (
        "jiti",
        "2.7.0",
        "AC/7JofJvZGrrneWNaEnJeOLUx+JlGt7tNa0wZiRPT4MY1wmfKjt2+6O2p2uz2+skll8OZZmJMNqeke7kKbNgQ==",
        "node_modules/jiti",
        ("package.json", "LICENSE", "lib/", "dist/"),
    ),
    (
        "@silvia-odwyer/photon-node",
        "0.3.4",
        "bnly4BKB3KDTFxrUIcgCLbaeVVS8lrAkri1pEzskpmxu9MdfGQTy8b8EgcD83ywD3RPMsIulY8xJH5Awa+t9fA==",
        "node_modules/@silvia-odwyer/photon-node",
        ("package.json", "LICENSE.md", "photon_rs.js", "photon_rs_bg.js", "photon_rs_bg.wasm", "photon_rs.d.ts"),
    ),
]


def tarball_url(name, version):
    base = name.rsplit("/", 1)[-1]
    return f"{REGISTRY}/{name}/-/{base}-{version}.tgz"


def integrity(data):
    return base64.b64encode(hashlib.sha512(data).digest()).decode()


def download(name, version, expected, cache):
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"{name.replace('/', '__')}-{version}.tgz"
    if archive.exists() and integrity(archive.read_bytes()) == expected:
        return archive.read_bytes()
    with urllib.request.urlopen(tarball_url(name, version), timeout=60) as response:
        data = response.read()
    if integrity(data) != expected:
        raise RuntimeError(f"{name}@{version}: tarball checksum mismatch")
    temporary = archive.with_suffix(".download")
    temporary.write_bytes(data)
    os.replace(temporary, archive)
    return data


def kept(path, keep):
    return any(path == k or (k.endswith("/") and path.startswith(k)) for k in keep)


def extract(data, destination, keep):
    """Regular files under `package/` only; never links, absolute or `..` paths."""
    count = 0
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as source:
        for member in source.getmembers():
            parts = PurePosixPath(member.name).parts
            if len(parts) < 2 or parts[0] != "package" or ".." in parts or member.name.startswith("/"):
                continue
            relative = "/".join(parts[1:])
            if not kept(relative, keep) or member.isdir():
                continue
            if not member.isfile():
                raise RuntimeError(f"unexpected archive member type: {member.name}")
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            payload = source.extractfile(member)
            if payload is None:
                raise RuntimeError(f"missing archive member: {member.name}")
            target.write_bytes(payload.read())
            target.chmod(0o755 if member.mode & 0o111 else 0o644)
            count += 1
    return count


def main():
    here = Path(__file__).resolve().parent
    out = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else here / "build" / "pi"
    cache = here / "build" / "pi-cache"
    staging = out.with_name(out.name + ".staging")
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)
    try:
        for name, version, expected, subdir, keep in PACKAGES:
            data = download(name, version, expected, cache)
            files = extract(data, staging / subdir, keep)
            if files == 0:
                raise RuntimeError(f"{name}@{version}: nothing extracted")
        manifest = json.loads((staging / "package.json").read_text())
        if manifest.get("name") != PACKAGES[0][0] or manifest.get("version") != PI_VERSION:
            raise RuntimeError("unexpected Pi package manifest")
        if not (staging / "dist" / "bundle" / "cli.js").is_file():
            raise RuntimeError("dist/bundle/cli.js missing")
        shutil.rmtree(out, ignore_errors=True)
        os.replace(staging, out)
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    print(f"Pi {PI_VERSION} verified and prepared in {out}")


if __name__ == "__main__":
    main()
