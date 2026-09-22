#!/usr/bin/env python3
"""Reconcile native fleet harnesses from official stable releases, atomically.

Root only. No account credentials are copied. Optional HARNESS_VERSIONS_FILE
pins {"opencode":"...", "claude":"...", "grok":"...", "codex":"..."}.
Keep two recent versions, the rollback target and versions mapped by live processes.
"""
import argparse
import base64
import fcntl
import gzip
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
import time
import urllib.request

ROOT = Path('/opt/sandboxed-tools')
ORIGINS = ('https://registry.npmjs.org/', 'https://api.github.com/repos/anomalyco/opencode/',
           'https://github.com/anomalyco/opencode/releases/download/', 'https://x.ai/cli/')


def fetch(url):
    if not url.startswith(ORIGINS):
        raise ValueError('Unexpected artifact origin')
    req = urllib.request.Request(url, headers={'User-Agent': 'sandboxed-harness-maintenance'})
    with urllib.request.urlopen(req, timeout=120) as response:
        data = response.read(512 * 1024 * 1024 + 1)
    if len(data) > 512 * 1024 * 1024:
        raise ValueError('Artifact exceeds 512 MiB')
    return data


def metadata(url):
    return json.loads(fetch(url))


def stable(version):
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise ValueError('Only stable semantic versions are accepted')
    return version


def npm_blob(package, version):
    meta = metadata(f'https://registry.npmjs.org/{package}/{version}')
    blob = fetch(meta['dist']['tarball'])
    actual = 'sha512-' + base64.b64encode(hashlib.sha512(blob).digest()).decode()
    if actual not in meta['dist']['integrity'].split():
        raise ValueError('npm integrity mismatch')
    return blob


def extract_member(blob, name, destination):
    with tarfile.open(fileobj=io.BytesIO(blob), mode='r:gz') as archive:
        member = archive.getmember(name)
        if not member.isfile() or member.size > 512 * 1024 * 1024:
            raise ValueError('Unexpected archive member')
        with archive.extractfile(member) as src, destination.open('wb') as dst:
            shutil.copyfileobj(src, dst)
    destination.chmod(0o755)


def run_probe(binary, args, user=None):
    command = [str(binary), *args]
    if user:
        command = ['runuser', '-u', user, '--', *command]
    result = subprocess.run(command, capture_output=True, text=True, timeout=30,
                            env={**os.environ, 'DISABLE_AUTOUPDATER': '1', 'NO_COLOR': '1'})
    if result.returncode:
        raise ValueError(f'CLI probe failed ({args}, exit {result.returncode})')
    return result.stdout + result.stderr


def probe(binary, name, version, user):
    observed = run_probe(binary, ['--version'], user).strip()
    if not re.search(r'(?<![\d.])' + re.escape(version) + r'(?![\d.])', observed):
        raise ValueError(f'Unexpected {name} version: {observed[:150]}')
    args, required = {
        'opencode': (['run', '--help'], ['--format', '--model', '--session']),
        'claude': (['--help'], ['--output-format', '--resume', '--print']),
        'grok': (['--help'], ['--output-format', '--resume']),
    }[name]
    help_text = run_probe(binary, args, user)
    if not all(flag in help_text for flag in required):
        raise ValueError(f'{name} CLI contract changed; not activating')
    return observed


def resolve(name, pins):
    if name in pins:
        return stable(pins[name])
    if name == 'opencode':
        return stable(metadata('https://api.github.com/repos/anomalyco/opencode/releases/latest')['tag_name'].removeprefix('v'))
    if name == 'grok':
        return stable(fetch('https://x.ai/cli/stable').decode().strip())
    package = {'claude': '@anthropic-ai/claude-code', 'codex': '@openai/codex'}[name]
    return stable(metadata(f'https://registry.npmjs.org/{package}/latest')['version'])


def reconcile(name, version, user):
    if name == 'codex':
        subprocess.run(['/usr/bin/python3', '/usr/local/sbin/update-node-codex'],
                       env={**os.environ, 'CODEX_NODE_VERSION': version}, check=True, timeout=300)
        observed = run_probe('/usr/local/bin/codex', ['--version'], user).strip()
        if observed != f'codex-cli {version}':
            raise ValueError('Codex service-user probe failed')
        return {'version': version, 'observed': observed}
    arch = {'x86_64': 'x64', 'aarch64': 'arm64'}[platform.machine()]
    root = ROOT / name
    root.mkdir(parents=True, exist_ok=True)
    destination = root / version
    binary = destination / name
    if not binary.exists():
        if shutil.disk_usage(root).free < 2 * 1024**3:
            raise ValueError('Need 2 GiB free for safe staging')
        with tempfile.TemporaryDirectory(prefix='.stage-', dir=root) as temporary:
            stage = Path(temporary)
            stage.chmod(0o755)
            candidate = stage / name
            if name == 'claude':
                blob = npm_blob(f'@anthropic-ai/claude-code-linux-{arch}', version)
                extract_member(blob, 'package/claude', candidate)
            elif name == 'opencode':
                release = metadata(f'https://api.github.com/repos/anomalyco/opencode/releases/tags/v{version}')
                asset_name = f'opencode-linux-{arch}' + ('-baseline' if arch == 'x64' else '') + '.tar.gz'
                asset = next(a for a in release['assets'] if a['name'] == asset_name)
                blob = fetch(asset['browser_download_url'])
                if asset.get('digest') != 'sha256:' + hashlib.sha256(blob).hexdigest():
                    raise ValueError('GitHub release digest mismatch')
                extract_member(blob, 'opencode', candidate)
            else:
                # Official installer uses this same stable channel/artifact origin.
                # Upstream supplies HTTPS, but no separate signed digest manifest.
                native_arch = {'x64': 'x86_64', 'arm64': 'aarch64'}[arch]
                blob = fetch(f'https://x.ai/cli/grok-{version}-linux-{native_arch}.gz')
                with gzip.GzipFile(fileobj=io.BytesIO(blob)) as src, candidate.open('wb') as dst:
                    remaining = 512 * 1024 * 1024
                    while chunk := src.read(min(1024 * 1024, remaining + 1)):
                        remaining -= len(chunk)
                        if remaining < 0:
                            raise ValueError('Expanded binary exceeds cap')
                        dst.write(chunk)
                candidate.chmod(0o755)
            observed = probe(candidate, name, version, user)
            (stage / 'receipt.json').write_text(json.dumps({'name': name, 'version': version,
                'sha256': hashlib.sha256(candidate.read_bytes()).hexdigest(), 'verified_at': time.time()}))
            stage.rename(destination)
    observed = probe(binary, name, version, user)
    link = Path('/usr/local/bin') / name
    if link.exists() or link.is_symlink():
        previous = root / 'previous'
        if link.is_symlink():
            target = link.resolve()
        else:
            target = root / 'before-managed-install'
            if not target.exists():
                shutil.copy2(link, target)
        if target != binary:
            temp_previous = root / '.previous.next'
            temp_previous.unlink(missing_ok=True)
            temp_previous.symlink_to(target)
            temp_previous.replace(previous)
    temporary_link = link.with_name('.' + name + '.next')
    temporary_link.unlink(missing_ok=True)
    temporary_link.symlink_to(binary)
    temporary_link.replace(link)
    prune_versions(root, destination)
    return {'version': version, 'observed': observed, 'path': str(binary)}


def prune_versions(root, current):
    if not Path("/proc").is_dir():
        return
    versions = sorted(
        (p for p in root.iterdir() if p.is_dir() and not p.is_symlink()
         and re.fullmatch(r"\d+\.\d+\.\d+", p.name)),
        key=lambda p: tuple(map(int, p.name.split("."))), reverse=True,
    )
    keep = {current, *versions[:2]}
    previous = root / "previous"
    if previous.is_symlink():
        keep.add(previous.resolve().parent)
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
            print(f"Removed inactive harness version {version.name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--only', choices=['opencode', 'claude', 'codex', 'grok'])
    parser.add_argument('--probe-user', default='sandboxed-node')
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise SystemExit('Run as root')
    ROOT.mkdir(parents=True, exist_ok=True)
    pins_path = os.environ.get('HARNESS_VERSIONS_FILE')
    pins = json.loads(Path(pins_path).read_text()) if pins_path else {}
    with (ROOT / '.harness-update.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        results = {}
        for name in ([args.only] if args.only else ['opencode', 'claude', 'codex', 'grok']):
            try:
                version = resolve(name, pins)
                results[name] = {'ok': True, **reconcile(name, version, args.probe_user)}
            except Exception as error:
                results[name] = {'ok': False, 'error': str(error)[:250]}
            print(json.dumps({name: results[name]}), flush=True)
        report = ROOT / '.harness-status.next'
        report.write_text(json.dumps({'checked_at': time.time(), 'harnesses': results}, indent=2))
        report.replace(ROOT / 'harness-status.json')
        if not all(r['ok'] for r in results.values()):
            raise SystemExit(1)


if __name__ == '__main__':
    main()
