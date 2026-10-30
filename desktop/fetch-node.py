#!/usr/bin/env python3
"""Fetch the pinned, standalone Node runtime used by the macOS PTC mode."""
import hashlib
import os
from pathlib import Path
import tarfile
import urllib.request


VERSION = "26.13.0"
ARCHIVE = f"node-v{VERSION}-darwin-arm64.tar.gz"
SHA256 = "d38bd5e5a2ddce6bbb5ce45e46e208337da4d51fffe611e567fd32073d6439f6"


def file_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    root = Path(__file__).resolve().parent / "build" / "runtime"
    root.mkdir(parents=True, exist_ok=True)
    archive = root / ARCHIVE
    if not archive.exists() or file_digest(archive) != SHA256:
        temporary = archive.with_suffix(".download")
        try:
            with urllib.request.urlopen(f"https://nodejs.org/dist/v{VERSION}/{ARCHIVE}", timeout=30) as response:
                with temporary.open("wb") as output:
                    while chunk := response.read(1024 * 1024):
                        output.write(chunk)
            if file_digest(temporary) != SHA256:
                raise RuntimeError("Node archive checksum mismatch")
            os.replace(temporary, archive)
        finally:
            temporary.unlink(missing_ok=True)
    # Read exactly these regular files, never extract archive paths or symlinks.
    prefix = ARCHIVE.removesuffix(".tar.gz")
    with tarfile.open(archive, "r:gz") as source:
        for member_name, destination in [("bin/node", "node"), ("LICENSE", "NODE-LICENSE.txt")]:
            member = source.getmember(f"{prefix}/{member_name}")
            if not member.isfile():
                raise RuntimeError(f"Unexpected archive member: {member_name}")
            data = source.extractfile(member)
            if data is None:
                raise RuntimeError(f"Missing archive member: {member_name}")
            (root / destination).write_bytes(data.read())
    (root / "node").chmod(0o755)
    print(f"Node.js {VERSION} verified and prepared in {root}")


if __name__ == "__main__":
    main()
