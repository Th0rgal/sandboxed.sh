#!/usr/bin/env python3
"""Measure pinned recursive fixtures and a conservative union without proof edits."""
import argparse
import base64
import gzip
import hashlib
import json
import subprocess
from pathlib import Path
from test_remote_lean_build_source import SourceTransportTest
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("repo", type=Path, help="pristine authorized Lido clone (checkout will change)")
parser.add_argument("output", type=Path, help="new directory for captures and combined fixture")
args = parser.parse_args()
repo = args.repo.resolve()
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=False)
union = output / 'combined-fixture'
union.mkdir()
seen = {}
rows = []
def git(*args):
    return subprocess.check_output(['git', '-C', str(repo), *args]).decode().strip()
def measure(checkout, label):
    fixture = SourceTransportTest()
    try:
        fixture.setUp()
        # Test capture uses dummy capabilities and fake curl: no remote jobs.
        fixture.repo = checkout
        fixture.command = ["lake", "build"]
        request = fixture.request()
        bundle = request['source_bundle']
        paths = subprocess.check_output(['git','-C',str(checkout),'ls-files','--recurse-submodules','-z']).split(b'\0')[:-1]
        assert sorted(p.decode() for p in paths) == [f['path'] for f in bundle['files']]
        manifest = hashlib.sha256(b'sandboxed-source-bundle-v2-complete\n')
        operations = hashlib.sha256(b'sandboxed-source-bundle-ops-v1\n')
        entries = []
        for f in bundle['files']:
            p = checkout / f['path']
            data = base64.b64decode(f['data_base64'], validate=True)
            assert data == p.read_bytes()
            assert f['executable'] == bool(p.stat().st_mode & 0o111)
            digest = hashlib.sha256(data).hexdigest()
            assert digest == f['sha256']
            manifest.update(f"{f['path']}\0{digest}\n".encode())
            operations.update(f"file\0{f['path']}\0{'x' if f['executable'] else '-'}\n".encode())
            entries.append({'path':f['path'], 'bytes':len(data), 'sha256':digest, 'executable':f['executable']})
            if checkout != union:
                key = (f['path'],digest,f['executable'])
                if key not in seen:
                    target = union / f['path']
                    if target.exists():
                        target = union / 'fixture-variants' / label / f['path']
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(data)
                    target.chmod(0o755 if f['executable'] else 0o644)
                    seen[key] = str(target.relative_to(union))
        if checkout == union:
            assert set(seen.values()) == {e['path'] for e in entries}, 'all source/receipt versions must be included'
        assert manifest.hexdigest() == bundle['manifest_sha256']
        assert operations.hexdigest() == bundle['operations_sha256']
        wire = fixture.capture.read_bytes()
        (output / (label + '.request.gz')).write_bytes(wire)
        (output / (label + '.files.json')).write_text(json.dumps(entries,indent=2)+'\n')
        row = dict(label=label,head=request['commit'],files=len(entries),decoded_bytes=sum(e['bytes'] for e in entries),json_bytes=len(gzip.decompress(wire)),gzip_bytes=len(wire),manifest_sha256=manifest.hexdigest(),operations_sha256=operations.hexdigest(),request_gzip_sha256=hashlib.sha256(wire).hexdigest())
        if checkout != union:
            row['submodules'] = git('submodule','status','--recursive')
            assert not git('status','--porcelain'), 'fixture must remain pristine'
        rows.append(row)
        (output / 'sizes.json').write_text(json.dumps(rows,indent=2)+'\n')
        print(json.dumps(row), flush=True)
    finally:
        fixture.doCleanups()
assert not git('status', '--porcelain'), 'input checkout must be pristine'
for label, ref in [('reserve-current','f27b39d8f12b1d8b5947c7f664ebfc33e0183019'),('alloc2','2bb0a7dc7bf009333ba925b2a3a2e02503297b76'),('alloc1','8691c7881a863715ab5ec9b39631ab41243e7c91')]:
    git('checkout','--detach',ref)
    git('submodule','update','--init','--recursive')
    measure(repo,label)
# All shared path/byte/mode triples appear once. Different versions at the same
# path are retained under fixture-variants; this is a capacity fixture, not a merge.
for args in [('init',),('add','--force','-A'),('-c','user.name=Transport Fixture','-c','user.email=fixture@example.invalid','commit','-m','Conservative current trio transport fixture'),('remote','add','origin','https://example.invalid/trio.git')]:
    subprocess.run(['git','-C',str(union),*args],check=True,stdout=subprocess.DEVNULL)
measure(union,'combined')
(output / 'union-provenance.json').write_text(json.dumps([{'source_path':k[0],'sha256':k[1],'executable':k[2],'fixture_path':v} for k,v in seen.items()],indent=2)+'\n')
