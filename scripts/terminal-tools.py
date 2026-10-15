#!/usr/bin/env python3
"""Cache pinned terminal build inputs. No system packages/settings are changed."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import urllib.request
import zipfile

ZIG_VERSION = "0.16.0"
GHOSTTY_COMMIT = "b40acce58dcf77df52231c3798ea58e924647c89"
SOURCE_HASH = "a623aa0b1267594d9a1bb76a5fef535f198004e78730894f494a5bdc522a4850"
SOURCE_ROOT = "libghostty-vt-1.3.2-main+b40acce"


def download(url, target, digest):
    if target.exists() and hashlib.sha256(target.read_bytes()).hexdigest() == digest:
        return
    temporary = target.with_suffix(".download")
    subprocess.run(["curl", "--fail", "--location", "--silent", "--show-error", "--retry", "2", url, "-o", str(temporary)], check=True)
    if hashlib.sha256(temporary.read_bytes()).hexdigest() != digest:
        temporary.unlink()
        raise RuntimeError("checksum mismatch: " + url)
    temporary.replace(target)


def setup(cache):
    cache.mkdir(parents=True, exist_ok=True)
    executable = "zig.exe" if os.name == "nt" else "zig"
    zig = cache / ("zig-python-" + ZIG_VERSION) / "ziglang" / executable
    if not zig.exists() and not os.environ.get("ZIG"):
        machine = platform.machine().lower()
        architecture = "aarch64" if machine in ("arm64", "aarch64") else "x86_64"
        with urllib.request.urlopen("https://pypi.org/pypi/ziglang/" + ZIG_VERSION + "/json") as response:
            releases = json.load(response)["urls"]
        def matches(item):
            name = item["filename"]
            if platform.system() == "Darwin":
                return "macosx" in name and ("arm64" if architecture == "aarch64" else "x86_64") in name
            if platform.system() == "Linux":
                return "manylinux" in name and architecture in name
            return "win_amd64" in name and architecture == "x86_64"
        release = next(item for item in releases if matches(item))
        wheel = cache / ("zig-" + ZIG_VERSION + ".whl")
        download(release["url"], wheel, release["digests"]["sha256"])
        with zipfile.ZipFile(wheel) as archive:
            archive.extractall(zig.parent.parent)
        zig.chmod(0o755)
    source = cache / "ghostty-b40acce" / SOURCE_ROOT
    if not (source / "build.zig").exists():
        archive_path = cache / "ghostty-b40acce.tar.gz"
        download("https://tip.files.ghostty.org/" + GHOSTTY_COMMIT + "/libghostty-vt-source.tar.gz", archive_path, SOURCE_HASH)
        source.parent.mkdir(exist_ok=True)
        with tarfile.open(archive_path) as archive:
            for member in archive.getmembers():
                path = Path(member.name)
                if path.is_absolute() or ".." in path.parts or member.issym() or member.islnk():
                    raise RuntimeError("unexpected path in terminal source archive")
            archive.extractall(source.parent)
    print("Terminal build inputs ready: " + str(cache))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, required=True)
    setup(parser.parse_args().cache)
