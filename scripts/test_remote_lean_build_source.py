#!/usr/bin/env python3
"""Offline full-source transport tests; fake curl captures the actual wire request.

Run: python3 scripts/test_remote_lean_build_source.py
--emit-fixture prints one captured recursive request for the Rust receiver test.
"""
import base64
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

WRAPPER = Path(__file__).with_name("remote-lean-build").resolve()


class SourceTransportTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.bin = self.base / "bin"
        self.bin.mkdir()
        self.capture = self.base / "request.gz"
        self.trace = self.base / "git-trace"
        self.env = {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "HOME": str(self.base / "home"),
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
            "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
            "REMOTE_BUILD_URL": "http://example.invalid/api/remote-build",
            "REMOTE_BUILD_TOKEN": "test-capability",
            "REMOTE_BUILD_MISSION_ID": "00000000-0000-0000-0000-000000000001",
            "REMOTE_BUILD_SOURCE_MODE": "full",
            "REMOTE_BUILD_STATE_DIR": str(self.base / "state"),
            "REMOTE_BUILD_TEST_CAPTURE": str(self.capture),
        }
        (self.bin / "curl").write_text('''#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) shift; output="$1" ;;
        --data-binary) shift; data="$1" ;;
    esac
    shift
done
cp "${data#@}" "$REMOTE_BUILD_TEST_CAPTURE"
printf 'test runner unavailable' > "$output"
printf '503'
''')
        (self.bin / "curl").chmod(0o755)
        nested = self.new_repo("nested-origin", {
            "Nested.lean": b"nested\n", "assets/data.bin": b"\x00\xffnested\n",
            "run.sh": b"#!/bin/sh\nexit 0\n", "Gone.lean": b"gone\n",
        })
        (nested / "run.sh").chmod(0o755)
        self.commit(nested)
        sub = self.new_repo("sub-origin", {"Sub.lean": b"sub\n"})
        self.git(sub, "-c", "protocol.file.allow=always", "submodule", "add", str(nested), "vendor/nested")
        self.commit(sub)
        self.repo = self.new_repo("repo", {
            "Root.lean": b"root\n", "Theory/Proof.lean": b"proof\n",
            "lean-toolchain": b"leanprover/lean4:v4.19.0\n",
            "lakefile.toml": b"name = 'fixture'\n",
        })
        self.git(self.repo, "-c", "protocol.file.allow=always", "submodule", "add", str(sub), "deps/sub")
        self.commit(self.repo)
        self.git(self.repo, "-c", "protocol.file.allow=always", "submodule", "update", "--init", "--recursive")
        self.sub = self.repo / "deps/sub"
        self.nested = self.sub / "vendor/nested"
        self.git(self.repo, "remote", "add", "origin", "https://user:root-secret@example.invalid/private.git?token=query-secret#fragment")
        for checkout in (self.repo, self.sub, self.nested):
            self.git(checkout, "config", "credential.helper", "!echo credential-helper-must-not-run >&2; exit 1")
            self.git(checkout, "config", "test.secret", "git-config-secret")

    def git(self, repo, *args):
        result = subprocess.run(["git", "-C", str(repo), *args], env=self.env,
                                capture_output=True, check=True)
        return result.stdout.decode().strip()

    def new_repo(self, name, files):
        repo = self.base / name
        repo.mkdir()
        self.git(repo, "init", "--quiet")
        self.git(repo, "config", "user.name", "Source Test")
        self.git(repo, "config", "user.email", "source@example.invalid")
        for name, data in files.items():
            path = repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.commit(repo)
        return repo

    def commit(self, repo):
        self.git(repo, "add", "-A")
        self.git(repo, "-c", "commit.gpgsign=false", "commit", "--quiet", "--allow-empty", "-m", "fixture")

    def run_wrapper(self, cwd=None, **env):
        self.capture.unlink(missing_ok=True)
        self.trace.unlink(missing_ok=True)
        result = subprocess.run(["bash", str(WRAPPER), "true"], cwd=cwd or self.repo,
                                env={**self.env, "GIT_TRACE": str(self.trace), **env},
                                capture_output=True, timeout=20)
        trace = self.trace.read_text()
        for command in (" fetch ", " clone ", " submodule ", "credential-helper-must-not-run"):
            self.assertNotIn(command, trace)
        return result

    def request(self, cwd=None, **env):
        result = self.run_wrapper(cwd, **env)
        self.assertEqual(result.returncode, 75, result.stderr.decode())
        self.assertTrue(self.capture.is_file())
        return json.loads(gzip.decompress(self.capture.read_bytes()))

    def rejected(self, message, **env):
        result = self.run_wrapper(**env)
        self.assertEqual(result.returncode, 2, result.stderr.decode())
        self.assertIn(message, result.stderr.decode())
        self.assertFalse(self.capture.exists(), "invalid source reached submission")
        self.assertNotIn(b"git-config-secret", result.stderr)

    def test_recursive_wire_contents_and_digests(self):
        request = self.request(self.repo / "Theory")
        self.assertEqual(request["cwd_rel"], "Theory")
        self.assertEqual(request["commit"], self.git(self.repo, "rev-parse", "HEAD"))
        self.assertEqual(request["base_tree_sha"], self.git(self.repo, "rev-parse", "HEAD^{tree}"))
        self.assertEqual(request["repo"], "https://example.invalid/private.git")
        self.assertNotIn("source_archive", request)
        bundle = request["source_bundle"]
        self.assertTrue(bundle["complete"])
        self.assertEqual(bundle["deleted_paths"], [])
        expected = [".gitmodules", "Root.lean", "Theory/Proof.lean", "deps/sub/.gitmodules",
                    "deps/sub/Sub.lean", "deps/sub/vendor/nested/Gone.lean",
                    "deps/sub/vendor/nested/Nested.lean", "deps/sub/vendor/nested/assets/data.bin",
                    "deps/sub/vendor/nested/run.sh", "lakefile.toml", "lean-toolchain"]
        self.assertEqual([f["path"] for f in bundle["files"]], expected)
        manifest = hashlib.sha256(b"sandboxed-source-bundle-v2-complete\n")
        operations = hashlib.sha256(b"sandboxed-source-bundle-ops-v1\n")
        for entry in bundle["files"]:
            path = entry["path"]
            data = base64.b64decode(entry["data_base64"], validate=True)
            self.assertEqual(data, (self.repo / path).read_bytes())
            self.assertNotIn(b"git-config-secret", data)
            digest = hashlib.sha256(data).hexdigest()
            self.assertEqual(entry["sha256"], digest)
            manifest.update(f"{path}\0{digest}\n".encode())
            operations.update(f"file\0{path}\0{'x' if entry['executable'] else '-'}\n".encode())
            self.assertEqual(entry["executable"], path.endswith("run.sh"))
        self.assertEqual(bundle["manifest_sha256"], manifest.hexdigest())
        self.assertEqual(bundle["operations_sha256"], operations.hexdigest())
        self.assertEqual(bundle, self.request()["source_bundle"])
        wire = json.dumps(request)
        for secret in ("root-secret", "query-secret", "git-config-secret"):
            self.assertNotIn(secret, wire)

    def test_local_sources_recursive_allowlist_deletions_and_modes(self):
        baseline = self.request()["source_bundle"]
        for checkout in (self.repo, self.sub, self.nested):
            (checkout / "New.lean").write_text("new\n")
            (checkout / "secret.txt").write_text("untracked-secret")
            (checkout / ".gitignore").write_text("ignored.lean\n.lake/\n")
            (checkout / "ignored.lean").write_text("ignored\n")
            (checkout / ".lake").mkdir()
            (checkout / ".lake" / "Cache.lean").write_text("cache\n")
        (self.sub / "Sub.lean").write_text("dirty\n")
        (self.nested / "Gone.lean").unlink()
        self.git(self.nested, "add", "New.lean")
        bundle = self.request()["source_bundle"]
        paths = [f["path"] for f in bundle["files"]]
        for prefix in ("", "deps/sub/", "deps/sub/vendor/nested/"):
            self.assertIn(prefix + "New.lean", paths)
            self.assertNotIn(prefix + "secret.txt", paths)
            self.assertNotIn(prefix + "ignored.lean", paths)
            self.assertNotIn(prefix + ".lake/Cache.lean", paths)
        self.assertNotIn("deps/sub/vendor/nested/Gone.lean", paths)
        self.assertNotEqual(baseline["manifest_sha256"], bundle["manifest_sha256"])
        (self.nested / "run.sh").chmod(0o644)
        mode_changed = self.request()["source_bundle"]
        self.assertEqual(bundle["manifest_sha256"], mode_changed["manifest_sha256"])
        self.assertNotEqual(bundle["operations_sha256"], mode_changed["operations_sha256"])

    def test_uninitialized_submodule(self):
        shutil.rmtree(self.sub)
        self.sub.mkdir()
        self.rejected("submodule deps/sub is not initialized")

    def test_missing_nested_submodule(self):
        shutil.rmtree(self.nested)
        self.rejected("submodule deps/sub/vendor/nested is not initialized")

    def test_wrong_submodule_revision(self):
        self.git(self.sub, "checkout", "--detach", "HEAD^")
        self.rejected("submodule deps/sub revision mismatch: expected")

    def test_wrong_nested_revision(self):
        self.git(self.nested, "checkout", "--detach", "HEAD^")
        self.rejected("submodule deps/sub/vendor/nested revision mismatch: expected")

    def test_staged_gitlink_change(self):
        self.git(self.sub, "checkout", "--detach", "HEAD^")
        self.git(self.repo, "add", "deps/sub")
        self.rejected("gitlinks differ from pinned commit")

    def test_new_nested_pin_is_bound_even_when_file_bytes_are_identical(self):
        before = self.request()
        for checkout in (self.nested, self.sub, self.repo):
            self.git(checkout, "config", "user.name", "Source Test")
            self.git(checkout, "config", "user.email", "source@example.invalid")
            self.commit(checkout)
        after = self.request()
        self.assertEqual(before["source_bundle"], after["source_bundle"])
        self.assertNotEqual(before["commit"], after["commit"])
        self.assertNotEqual(before["base_tree_sha"], after["base_tree_sha"])

    def test_replacement_refs_cannot_redefine_pinned_tree(self):
        before = self.request()
        self.git(self.repo, "replace", "HEAD", "HEAD^")
        after = self.request()
        self.assertEqual(before["base_tree_sha"], after["base_tree_sha"])
        self.assertEqual(before["source_bundle"], after["source_bundle"])

    def test_missing_promisor_tree_does_not_fetch(self):
        tree = self.git(self.repo, "rev-parse", "HEAD^{tree}")
        self.git(self.repo, "config", "remote.origin.promisor", "true")
        self.git(self.repo, "config", "remote.origin.partialclonefilter", "blob:none")
        (self.repo / ".git/objects" / tree[:2] / tree[2:]).unlink()
        self.rejected("cannot inspect pinned Git source")

    def test_staged_nested_gitlink_change(self):
        self.git(self.nested, "checkout", "--detach", "HEAD^")
        self.git(self.sub, "add", "vendor/nested")
        self.rejected("gitlinks differ from pinned commit in deps/sub/")

    def test_staged_gitlink_deletion(self):
        self.git(self.repo, "rm", "--cached", "deps/sub")
        self.rejected("gitlinks differ from pinned commit")

    def test_sparse_submodule_cannot_silently_omit_sources(self):
        self.git(self.sub, "update-index", "--skip-worktree", "Sub.lean")
        (self.sub / "Sub.lean").unlink()
        self.rejected("sparse source path is missing: deps/sub/Sub.lean")

    def test_unmerged_nested_index(self):
        blob = self.git(self.nested, "rev-parse", "HEAD:Nested.lean")
        subprocess.run(
            ["git", "-C", str(self.nested), "update-index", "--index-info"],
            input=f"0 {'0' * 40}\tNested.lean\n100644 {blob} 1\tNested.lean\n".encode(),
            env=self.env, check=True, capture_output=True,
        )
        self.rejected("unmerged source path: deps/sub/vendor/nested/Nested.lean")

    def test_empty_full_source_cannot_fall_back_to_fetch(self):
        self.repo = self.new_repo("empty", {})
        self.git(self.repo, "remote", "add", "origin", "https://example.invalid/empty.git")
        self.rejected("full-source bundle has no files")

    def test_gitlink_directory_must_be_its_own_checkout(self):
        (self.sub / ".git").unlink()
        (self.sub / ".git").mkdir()
        self.rejected("submodule deps/sub revision mismatch")

    def test_symlinked_submodule(self):
        moved = self.base / "moved"
        self.sub.rename(moved)
        self.sub.symlink_to(moved, target_is_directory=True)
        self.rejected("must not traverse a symlink: deps/sub")

    def test_symlinked_parent(self):
        moved = self.base / "assets"
        (self.nested / "assets").rename(moved)
        (self.nested / "assets").symlink_to(moved, target_is_directory=True)
        self.rejected("must not traverse a symlink: deps/sub/vendor/nested/assets/data.bin")

    def test_symlinked_source_to_git_config(self):
        source = self.sub / "Sub.lean"
        source.unlink()
        source.symlink_to(self.repo / ".git/config")
        self.rejected("must not traverse a symlink")

    def test_broken_symlink_is_not_a_deletion(self):
        source = self.nested / "Nested.lean"
        source.unlink()
        source.symlink_to("missing")
        self.rejected("must not traverse a symlink")

    def test_toolchain_symlink_does_not_read_credentials(self):
        source = self.repo / "lean-toolchain"
        source.unlink()
        source.symlink_to(self.repo / ".git/config")
        self.rejected("must not traverse a symlink: lean-toolchain")

    def test_special_file(self):
        source = self.sub / "Sub.lean"
        source.unlink()
        os.mkfifo(source)
        self.rejected("must be a regular file")

    def test_unsafe_nested_path(self):
        (self.nested / "unsafe name.lean").write_text("unsafe\n")
        self.rejected("unsafe source bundle path")

    def test_unsafe_gitlink_path(self):
        self.git(self.repo, "mv", "deps/sub", "deps/unsafe name")
        self.commit(self.repo)
        self.rejected("unsafe source bundle path")

    def test_tracked_lake_metadata_rejected(self):
        (self.nested / ".lake").mkdir()
        (self.nested / ".lake/Cache.lean").write_text("poison\n")
        self.git(self.nested, "add", ".lake/Cache.lean")
        self.rejected("unsafe source bundle path")

    def test_global_file_limit(self):
        self.rejected("operations; maximum is 10", REMOTE_BUILD_MAX_SOURCE_BUNDLE_FILES="10")

    def test_global_byte_limit(self):
        bundle = self.request()["source_bundle"]
        total = sum(len(base64.b64decode(f["data_base64"])) for f in bundle["files"])
        self.request(REMOTE_BUILD_MAX_SOURCE_BUNDLE_BYTES=str(total))
        self.rejected(f"maximum is {total - 1}", REMOTE_BUILD_MAX_SOURCE_BUNDLE_BYTES=str(total - 1))

    def test_full_and_overlay_without_submodules(self):
        self.repo = self.new_repo("plain", {"Root.lean": b"root\n", "Gone.lean": b"gone\n"})
        self.git(self.repo, "remote", "add", "origin", "https://example.invalid/plain.git")
        (self.repo / "Root.lean").write_text("dirty\n")
        (self.repo / "Gone.lean").unlink()
        full = self.request()
        self.assertEqual([f["path"] for f in full["source_bundle"]["files"]], ["Root.lean"])
        overlay = self.request(REMOTE_BUILD_SOURCE_MODE="overlay")
        self.assertIn("source_archive", overlay)
        self.assertFalse(overlay["source_bundle"]["complete"])
        self.assertEqual(overlay["source_bundle"]["deleted_paths"], ["Gone.lean"])


if __name__ == "__main__":
    if sys.argv[1:] == ["--emit-fixture"]:
        fixture = SourceTransportTest()
        try:
            fixture.setUp()
            print(json.dumps(fixture.request()))
        finally:
            fixture.doCleanups()
    else:
        unittest.main()
