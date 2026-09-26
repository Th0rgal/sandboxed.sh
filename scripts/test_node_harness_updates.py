import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('updater', Path(__file__).with_name('update-node-harnesses.py'))
updater = importlib.util.module_from_spec(spec)
spec.loader.exec_module(updater)

class UpdateSafety(unittest.TestCase):
    def test_version_requires_component(self):
        with patch('sys.argv', ['updater', '--version', '1.2.3']):
            with self.assertRaises(SystemExit) as error: updater.main()
        self.assertEqual(error.exception.code, 2)

    def test_explicit_version_is_pinned(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(updater, 'ROOT', Path(directory)), patch.object(updater.os, 'geteuid', return_value=0), patch.object(updater, 'resolve', side_effect=AssertionError('must not resolve latest')), patch.object(updater, 'reconcile', return_value={'version': '1.2.3'}) as reconcile, patch('sys.argv', ['updater', '--only', 'codex', '--version', '1.2.3']):
            updater.main()
            reconcile.assert_called_once_with('codex', '1.2.3', 'sandboxed-node')

    def test_version_rejects_paths_and_options(self):
        for version in ['../file', '--help', '1.2.3;id', 'latest']:
            with self.assertRaises(ValueError): updater.stable(version)

    def test_archive_symlink_is_rejected(self):
        blob = io.BytesIO()
        with tarfile.open(fileobj=blob, mode='w:gz') as archive:
            entry = tarfile.TarInfo('opencode'); entry.type = tarfile.SYMTYPE; entry.linkname = '/etc/passwd'; archive.addfile(entry)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'binary'
            with self.assertRaises(ValueError): updater.extract_member(blob.getvalue(), 'opencode', output)
            self.assertFalse(output.exists())

    def test_npm_corruption_fails_integrity(self):
        with patch.object(updater, 'metadata', return_value={'dist': {'tarball': 'https://registry.npmjs.org/test', 'integrity': 'sha512-invalid'}}), patch.object(updater, 'fetch', return_value=b'corrupt'):
            with self.assertRaisesRegex(ValueError, 'integrity'): updater.npm_blob('test', '1.0.0')

    def test_contract_rejects_missing_resume(self):
        with patch.object(updater, 'run_probe', side_effect=['1.0.0', '--format --model']):
            with self.assertRaisesRegex(ValueError, 'contract'): updater.probe('/unused', 'opencode', '1.0.0', 'sandboxed-node')

    def test_incomplete_process_inventory_preserves_versions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for version in ['1.0.0','1.0.1','1.0.2']: (root/version).mkdir()
            with patch.object(updater.os, 'readlink', side_effect=PermissionError('not readable')):
                updater.prune_versions(root, root/'1.0.2')
            self.assertTrue((root/'1.0.0').exists())

if __name__ == '__main__': unittest.main()
