#!/usr/bin/env python3
"""Install/reconcile native Codex without Node/npm. Run as root on a fleet node.

Uses the official npm package's SHA-512 integrity, checks the CLI contract,
and atomically switches a symlink. Running processes retain their executable.
Set CODEX_NODE_VERSION to a tested version to pin; default is stable latest.
"""
import base64
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request


def fetch(url):
    if not url.startswith("https://registry.npmjs.org/"):
        raise ValueError("Unexpected package origin")
    with urllib.request.urlopen(url, timeout=120) as response:
        return response.read()


def prune_versions(root, current):
    versions = sorted(
        (p for p in root.iterdir() if p.is_dir() and not p.is_symlink()
         and re.fullmatch(r"\d+\.\d+\.\d+", p.name)),
        key=lambda p: tuple(map(int, p.name.split("."))), reverse=True,
    )
    keep = {current, *versions[:2]}
    # Keep any version still mapped into a running process, including helpers.
    for process in Path("/proc").glob("[0-9]*"):
        try:
            paths = [os.readlink(process / "exe"), os.readlink(process / "cwd")]
            paths += [line.split()[-1] for line in (process / "maps").read_text().splitlines() if line.split()]
        except FileNotFoundError:
            continue
        except (OSError, PermissionError):
            return  # An incomplete process inventory never authorizes removal.
        for version in versions:
            if any(path.startswith(str(version) + "/") for path in paths):
                keep.add(version)
    for version in versions:
        if version not in keep:
            shutil.rmtree(version)
            print(f"Removed inactive Codex version {version.name}")


def main():
    root = Path("/opt/sandboxed-tools/codex")
    root.mkdir(parents=True, exist_ok=True)
    with (root / ".update.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        version = os.environ.get("CODEX_NODE_VERSION", "latest")
        if version == "latest":
            version = json.loads(fetch("https://registry.npmjs.org/@openai/codex/latest"))["version"]
        if not re.fullmatch(r"\d+\.\d+\.\d+", version):
            raise ValueError("Only stable semantic versions are accepted")
        arch = {"x86_64": "x64", "aarch64": "arm64"}[platform.machine()]
        triple = {"x64": "x86_64-unknown-linux-musl", "arm64": "aarch64-unknown-linux-musl"}[arch]
        binary = root / version / "bin" / "codex"
        if not binary.exists():
            metadata = json.loads(fetch(f"https://registry.npmjs.org/@openai/codex/{version}-linux-{arch}"))
            blob = fetch(metadata["dist"]["tarball"])
            integrity = "sha512-" + base64.b64encode(hashlib.sha512(blob).digest()).decode()
            if integrity not in metadata["dist"]["integrity"].split():
                raise ValueError("Package integrity mismatch")
            with tempfile.TemporaryDirectory(dir=root) as temporary_dir:
                staging = Path(temporary_dir)
                staging.chmod(0o755)
                prefix = f"package/vendor/{triple}/"
                with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as archive:
                    for member in archive.getmembers():
                        if not member.name.startswith(prefix):
                            continue
                        relative = Path(member.name.removeprefix(prefix))
                        if relative.is_absolute() or ".." in relative.parts or not member.isfile():
                            raise ValueError("Unexpected archive member")
                        destination = staging / relative
                        destination.parent.mkdir(parents=True, exist_ok=True)
                        with destination.open("wb") as output:
                            shutil.copyfileobj(archive.extractfile(member), output)
                        destination.chmod(0o755 if member.mode & 0o111 else 0o644)
                staged = staging / "bin" / "codex"
                observed = subprocess.check_output([staged, "--version"], text=True, timeout=20).strip()
                if observed != f"codex-cli {version}":
                    raise ValueError(f"Unexpected version: {observed}")
                help_text = subprocess.check_output([staged, "exec", "--help"], text=True, timeout=20)
                if "--json" not in help_text or "resume" not in help_text:
                    raise ValueError("Codex exec contract changed; retaining installed version")
                staging.rename(binary.parent.parent)
        binary.parent.parent.chmod(0o755)
        link = Path("/usr/local/bin/codex")
        if link.is_symlink() and link.resolve() == binary:
            prune_versions(root, binary.parent.parent)
            print(f"Codex {version} already current")
            return
        temporary = link.with_name(".codex.next")
        temporary.unlink(missing_ok=True)
        temporary.symlink_to(binary)
        temporary.replace(link)
        prune_versions(root, binary.parent.parent)
        print(f"Installed Codex {version} ({arch}) at {binary}")


if __name__ == "__main__":
    main()
