#!/usr/bin/env python3
"""Fetch the pinned upstream HEVC-alpha pack without transcoding."""
from pathlib import Path
import hashlib
import shutil
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parent.parent
URL = "https://github.com/PC2005-cloud/dsh-pet/releases/download/assets-mov/dsh-pet-mov-assets-mov.zip"
SHA256 = "b3fe34fa42f28d297da8525cd08d0ac2706c96c14d3cf1eb386137a59b79a3c0"
CACHE = ROOT / ".tools/macos-assets"


def prepare() -> Path:
    CACHE.mkdir(parents=True, exist_ok=True)
    archive = CACHE / "assets.zip"
    if not archive.exists():
        with tempfile.NamedTemporaryFile(dir=CACHE, delete=False) as temp:
            path = Path(temp.name)
            try:
                with urllib.request.urlopen(URL) as response:
                    shutil.copyfileobj(response, temp)
            except BaseException:
                path.unlink(missing_ok=True)
                raise
        if hashlib.sha256(path.read_bytes()).hexdigest() != SHA256:
            path.unlink()
            raise RuntimeError("Upstream MOV archive changed: SHA-256 mismatch")
        path.replace(archive)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
        raise RuntimeError(f"SHA-256 mismatch: {archive}")
    expected = {p.with_suffix(".mov").name for p in (ROOT / "dsh-pet/assets/webm").glob("*.webm")}
    with zipfile.ZipFile(archive) as source:
        names = source.namelist()
        if len(names) != 106 or len(set(names)) != 106 or set(names) != expected:
            raise RuntimeError("MOV pack must match all 106 WebM clip names exactly")
        # Reading through ZipFile also verifies each CRC; compare cached content,
        # so interrupted extraction or local changes cannot silently pass.
        output = CACHE / "mov"
        output.mkdir(exist_ok=True)
        for name in names:
            data = source.read(name)
            dest = output / name
            if not dest.exists() or dest.read_bytes() != data:
                dest.write_bytes(data)
    print(f"Verified {len(names)} HEVC-alpha clips: {output}")
    return output


if __name__ == "__main__":
    prepare()
