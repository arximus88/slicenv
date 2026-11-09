#!/usr/bin/env python3
"""Tests for manage_engines: security, checksum verification, extraction sandbox, atomic install, and config isolation."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
import unittest.mock
from unittest import mock
import urllib.error
import urllib.request
import zipfile

TOOL_PATH = Path(__file__).with_name("manage_engines.py")
spec = importlib.util.spec_from_file_location("manage_engines", TOOL_PATH)
manage_engines = importlib.util.module_from_spec(spec)
sys.modules["manage_engines"] = manage_engines
spec.loader.exec_module(manage_engines)


def create_mock_appimage(dest_path: Path, malicious_symlink_target: Path | None = None) -> str:
    """Create a minimal mock AppImage script that responds to --appimage-extract."""
    script_lines = [
        "#!/bin/sh",
        'if [ "$1" = "--appimage-extract" ]; then',
        "    mkdir -p squashfs-root/bin",
        "    mkdir -p squashfs-root/resources/profiles",
        "    cat << 'EOF' > squashfs-root/AppRun",
        "#!/bin/sh",
        'echo "OrcaSlicer-2.4.2: mock engine"',
        "exit 0",
        "EOF",
        "    chmod +x squashfs-root/AppRun",
        "    echo 'mock profile' > squashfs-root/resources/profiles/BBL.json",
    ]
    if malicious_symlink_target:
        script_lines.append(f"    ln -s '{malicious_symlink_target}' squashfs-root/evil_link")
    script_lines.extend([
        "    exit 0",
        "fi",
        'echo "Execute as standalone AppImage"',
        "exit 0",
    ])
    dest_path.write_text("\n".join(script_lines) + "\n", encoding="utf-8")
    dest_path.chmod(0o755)
    return hashlib.sha256(dest_path.read_bytes()).hexdigest()


class TestManageEngines(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.data_dir = self.root / "data"
        self.config_dir = self.root / "config"
        self.data_dir.mkdir(parents=True)
        self.config_dir.mkdir(parents=True)
        self.environment = unittest.mock.patch.dict(os.environ, {
            "HOME": str(self.root), "XDG_CONFIG_HOME": str(self.config_dir),
            "XDG_DATA_HOME": str(self.root / "xdg-data"),
        })
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def tearDown(self):
        self.temp_dir.cleanup()

    def test_profiles_import_manifest_and_isolated_home(self):
        package = self.root / "package"
        (package / "BBL/machine").mkdir(parents=True)
        (package / "BBL.json").write_text('{}')
        (package / "BBL/machine/A1.json").write_text('{"name":"A1"}')
        result = manage_engines.install_profiles("orca", "2.5.0-beta", package=package)
        catalog = self.root / "xdg-data/slicenv/profiles/orca/2.5.0-beta"
        self.assertEqual(result["source"], "engine-package")
        self.assertEqual(result["vendors"], ["BBL"])
        self.assertTrue((catalog / "BBL/machine/A1.json").is_file())
        self.assertEqual(manage_engines.get_config_home(), self.config_dir)

    def test_profiles_fetch_only_vendor_and_rollback(self):
        payload = b'{"name":"A1"}'
        sha = hashlib.sha1(b"blob " + str(len(payload)).encode() + b"\0" + payload).hexdigest()
        tree = {"tree": [{"path": path, "sha": sha, "type": "blob", "mode": "100644"}
                          for path in ["resources/profiles/BBL.json", "resources/profiles/BBL/machine/A1.json",
                                       "resources/profiles/Other.json", "src/main.cpp"]]}
        def request(url):
            if "/git/trees/" in url:
                return json.dumps(tree).encode()
            self.assertIn("/resources/profiles/BBL", url)
            return payload
        with unittest.mock.patch.object(manage_engines, "profile_request", side_effect=request):
            manage_engines.install_profiles("orca", "2.5.0-beta")
        catalog = self.root / "xdg-data/slicenv/profiles/orca/2.5.0-beta"
        before = (catalog / "catalog.json").read_bytes()
        with unittest.mock.patch.object(manage_engines, "profile_request", side_effect=RuntimeError("offline")):
            with self.assertRaises(RuntimeError):
                manage_engines.install_profiles("orca", "2.5.0-beta")
        self.assertEqual((catalog / "catalog.json").read_bytes(), before)
        self.assertFalse((catalog / "Other.json").exists())

    def test_profiles_reject_paths_urls_and_truncated_tree(self):
        for url in ["http://api.github.com/x", "https://evil.test/x", "https://api.github.com.evil.test/x", "https://foo.github.com/x"]:
            with self.assertRaises(manage_engines.SecurityError):
                manage_engines.profile_request(url)
        with unittest.mock.patch.object(manage_engines, "profile_request", return_value=b'{"truncated":true}'):
            with self.assertRaises(ValueError):
                manage_engines.install_profiles("orca", "2.5.0-beta")
        with self.assertRaises(ValueError):
            manage_engines.install_profiles("orca", "../escape")

    def test_profiles_redirect_policy_and_tree_traversal(self):
        def opener(handler):
            with self.assertRaises(manage_engines.SecurityError):
                handler().redirect_request(None, None, 302, "", {}, "http://raw.githubusercontent.com/file")
            with self.assertRaises(manage_engines.SecurityError):
                handler().redirect_request(None, None, 302, "", {}, "https://evil.test/file")
            raise RuntimeError("policy checked")
        with unittest.mock.patch.object(manage_engines.urllib.request, "build_opener", side_effect=opener):
            with self.assertRaisesRegex(RuntimeError, "policy checked"):
                manage_engines.profile_request("https://api.github.com/repos/OrcaSlicer/OrcaSlicer/git/trees/v1")
        tree = {"tree": [{"path":"resources/profiles/BBL/../../escape.json", "type":"blob", "mode":"100644", "sha":"x"}]}
        with unittest.mock.patch.object(manage_engines, "profile_request", return_value=json.dumps(tree).encode()):
            with self.assertRaises(manage_engines.SecurityError):
                manage_engines.install_profiles("orca", "2.5.0-beta")

    def test_config_save_prunes_missing_but_preserves_existing_and_active(self):
        old = self.root / "old-engine"
        old.touch()
        new = self.root / "new-engine"
        new.touch()
        config = self.config_dir / "slicenv/engines.json"
        config.parent.mkdir()
        active = {"kind":"bambu", "path":"/missing/active"}
        config.write_text(json.dumps({"active":active, "installed":[
            {"kind":"bambu", "path":str(old)}, {"kind":"orca", "path":"/tmp/missing-engine"}]}))
        manage_engines.update_engines_config("orca", "2.5.0-beta", new)
        updated = json.loads(config.read_text())
        self.assertEqual(updated["active"], active)
        self.assertEqual({entry["path"] for entry in updated["installed"]}, {str(old), str(new)})

    def test_moved_data_dir_heals_manifest_and_registry(self):
        # Installed under the old data dir name: manifest paths point to a directory
        # that no longer exists, while the files moved under the new root.
        entry = self.data_dir / "orca" / "2.5.0-beta"
        (entry / "resources" / "profiles").mkdir(parents=True)
        executable = entry / "AppRun"
        executable.write_text("#!/bin/sh\n")
        executable.chmod(0o755)
        old = "/gone/bambu-lab/engines/orca/2.5.0-beta"
        (entry / "manifest.json").write_text(json.dumps({
            "managed": True, "family": "orca", "version": "2.5.0-beta",
            "executable_path": f"{old}/AppRun", "profile_paths": [f"{old}/resources/profiles"],
        }))
        listed = manage_engines.list_managed_engines(data_dir=self.data_dir)
        self.assertEqual([(e["version"], e["valid"], e["executable_path"]) for e in listed],
                         [("2.5.0-beta", True, str(executable))])
        healed = json.loads((entry / "manifest.json").read_text())
        self.assertEqual(healed["profile_paths"], [str(entry / "resources" / "profiles")])

        other = self.root / "alpha"
        other.touch()
        manage_engines.update_engines_config("orca", "2.5.0-alpha", other, data_dir=self.data_dir)
        config = json.loads((self.config_dir / "slicenv/engines.json").read_text())
        self.assertEqual({e["path"] for e in config["installed"]}, {str(other), str(executable)})

    def test_install_rejects_hostile_family_version_and_asset_names_without_changes(self):
        source = self.root / "mock.AppImage"
        sha = create_mock_appimage(source)
        for family, version, name in [("../orca", "1.0", "mock.AppImage"), ("orca", "../1.0", "mock.AppImage"), ("orca", "1.0", "../mock.AppImage"), ("orca", "1.0", "/tmp/mock.AppImage")]:
            with self.subTest(family=family, version=version, name=name):
                with self.assertRaises((ValueError, manage_engines.SecurityError)):
                    manage_engines.install_engine(family, version, name, "file://x", sha, local_file=source, data_dir=self.data_dir)
        self.assertFalse((self.data_dir / "orca").exists())

    def test_install_rejects_symlink_family_and_version_targets(self):
        source = self.root / "mock.AppImage"
        sha = create_mock_appimage(source)
        root = self.data_dir
        root.mkdir(parents=True, exist_ok=True)
        sibling = root / "other"
        sibling.mkdir()
        (root / "orca").symlink_to(sibling, target_is_directory=True)
        with self.assertRaises(manage_engines.SecurityError):
            manage_engines.install_engine("orca", "1.0", source.name, "file://x", sha, local_file=source, data_dir=self.data_dir)
        (root / "orca").unlink()
        (root / "orca").mkdir()
        (root / "orca" / "1.0").symlink_to(sibling, target_is_directory=True)
        with self.assertRaises(manage_engines.SecurityError):
            manage_engines.install_engine("orca", "1.0", source.name, "file://x", sha, local_file=source, data_dir=self.data_dir)

    def test_remove_rejects_symlinked_ancestor_and_external_target(self):
        elsewhere = self.root / "elsewhere"
        elsewhere.mkdir()
        (self.data_dir / "orca").symlink_to(elsewhere, target_is_directory=True)
        with self.assertRaises(manage_engines.SecurityError):
            manage_engines.remove_managed_engine("orca", "1.0", data_dir=self.data_dir)
        (self.data_dir / "orca").unlink()
        family = self.data_dir / "orca"
        family.mkdir()
        outside = elsewhere / "1.0"
        outside.mkdir()
        (family / "1.0").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(manage_engines.SecurityError):
            manage_engines.remove_managed_engine("orca", "1.0", data_dir=self.data_dir)

    def test_listing_skips_or_marks_malformed_manifests_and_missing_files(self):
        family = self.data_dir / "orca"
        family.mkdir(parents=True)
        for version, content in [("1", "[]"), ("2", '{"managed":true,"family":"orca","version":"2","executable_path":null}'), ("3", '{"managed":true,"family":"orca","version":"3","executable_path":"/missing"}')]:
            d = family / version
            d.mkdir()
            (d / "manifest.json").write_text(content)
        listed = manage_engines.list_managed_engines(data_dir=self.data_dir)
        self.assertEqual([e["version"] for e in listed], ["2", "3"])
        self.assertTrue(all(e["valid"] is False for e in listed))

    def test_remove_refuses_active_binary_via_alternate_path(self):
        source = self.root / "mock.AppImage"
        sha = create_mock_appimage(source)
        manifest = manage_engines.install_engine("orca", "1.0", source.name, "file://x", sha, local_file=source, data_dir=self.data_dir, config_dir=self.config_dir)
        cfg = self.config_dir / "slicenv" / "engines.json"
        data = json.loads(cfg.read_text())
        data["active"] = {"kind": "orca", "path": str(Path(manifest.executable_path).parent / "sub" / ".." / "AppRun")}
        cfg.write_text(json.dumps(data))
        with self.assertRaises(manage_engines.SecurityError):
            manage_engines.remove_managed_engine("orca", "1.0", data_dir=self.data_dir, config_dir=self.config_dir)

    def test_corrupt_config_fails_force_install_before_replacing_old_version(self):
        source = self.root / "mock.AppImage"
        sha = create_mock_appimage(source)
        args = dict(family="orca", version="1.0", asset_name=source.name, asset_url="file://x", expected_sha256=sha, local_file=source, data_dir=self.data_dir, config_dir=self.config_dir, no_register=True)
        manage_engines.install_engine(**args)
        target = self.data_dir / "orca" / "1.0"
        old_manifest = (target / "manifest.json").read_bytes()
        old_binary = (target / "AppRun").read_bytes()
        cfg = self.config_dir / "slicenv" / "engines.json"
        cfg.parent.mkdir(parents=True)
        cfg.write_text("[]")
        with self.assertRaises(ValueError):
            retry = dict(args, force=True, no_register=False)
            manage_engines.install_engine(**retry)
        self.assertEqual((target / "manifest.json").read_bytes(), old_manifest)
        self.assertEqual((target / "AppRun").read_bytes(), old_binary)

    def test_force_staging_rename_failure_restores_old_files_and_manifest_bytes(self):
        source = self.root / "mock.AppImage"
        sha = create_mock_appimage(source)
        args = dict(family="orca", version="1.0", asset_name=source.name, asset_url="file://x", expected_sha256=sha, local_file=source, data_dir=self.data_dir, no_register=True)
        manage_engines.install_engine(**args)
        target = self.data_dir / "orca" / "1.0"
        old_manifest = (target / "manifest.json").read_bytes()
        old_run = (target / "AppRun").read_bytes()
        original_rename = Path.rename
        def fail_stage(path, dest):
            if ".staging." in path.name and Path(dest) == target:
                raise OSError("injected rename failure")
            return original_rename(path, dest)
        with mock.patch.object(Path, "rename", fail_stage):
            with self.assertRaises(OSError):
                manage_engines.install_engine(**args, force=True)
        self.assertEqual((target / "manifest.json").read_bytes(), old_manifest)
        self.assertEqual((target / "AppRun").read_bytes(), old_run)

    def test_force_config_write_failure_restores_old_version_byte_exact(self):
        source = self.root / "mock.AppImage"
        sha = create_mock_appimage(source)
        args = dict(family="orca", version="1.0", asset_name=source.name, asset_url="file://x", expected_sha256=sha, local_file=source, data_dir=self.data_dir)
        manage_engines.install_engine(**args)
        target = self.data_dir / "orca" / "1.0"
        before = {p.relative_to(target): p.read_bytes() for p in target.rglob("*") if p.is_file()}
        with mock.patch.object(manage_engines, "update_engines_config", side_effect=OSError("injected config write failure")):
            with self.assertRaises(OSError):
                manage_engines.install_engine(**args, force=True)
        after = {p.relative_to(target): p.read_bytes() for p in target.rglob("*") if p.is_file()}
        self.assertEqual(after, before)

    def test_failed_extraction_leaves_no_staging_directory(self):
        source = self.root / "bad.tar.gz"
        source.write_bytes(b"bad archive")
        with self.assertRaises((tarfile.ReadError, OSError)):
            manage_engines.install_engine("orca", "1.0", source.name, "file://x", hashlib.sha256(source.read_bytes()).hexdigest(), local_file=source, data_dir=self.data_dir)
        self.assertFalse(list((self.data_dir / "orca").glob(".*staging*")))

    def test_missing_local_checksum_rejected_before_execution(self):
        source = self.root / "mock.AppImage"
        create_mock_appimage(source)
        with self.assertRaises(manage_engines.ChecksumError):
            manage_engines.install_engine("orca", "1.0", source.name, "file://x", None, local_file=source, data_dir=self.data_dir)
        self.assertFalse(self.data_dir.exists() and list(self.data_dir.iterdir()))

    def test_request_has_no_authorization_header_by_default(self):
        req = manage_engines.make_request("https://api.github.com/repos/OrcaSlicer/OrcaSlicer/releases")
        self.assertIsNone(req.get_header("Authorization"))

    def test_release_channel_classification(self):
        cases = [
            ({"tag_name": "v2.4.0", "prerelease": False}, "stable"),
            ({"tag_name": "v2.5.0-beta", "prerelease": True}, "beta"),
            ({"tag_name": "v2.5.0-rc1", "prerelease": False}, "beta"),
            ({"tag_name": "nightly-20261030", "prerelease": False}, "nightly"),
            ({"name": "Daily dev build", "prerelease": False}, "nightly"),
        ]
        for release, expected in cases:
            with self.subTest(release=release):
                self.assertEqual(manage_engines.classify_release_channel(release), expected)

    def test_release_listing_has_channel_and_official_metadata(self):
        release_data = [{
            "id": 321, "tag_name": "v2.5.0-beta", "name": "Beta", "draft": False,
            "prerelease": True, "published_at": "2026-10-30T00:00:00Z",
            "html_url": "https://github.com/OrcaSlicer/OrcaSlicer/releases/tag/v2.5.0-beta",
            "assets": [{"id": 654, "name": "Orca.AppImage", "size": 12,
                        "browser_download_url": "https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.5.0-beta/Orca.AppImage",
                        "digest": "sha256:" + "a" * 64, "updated_at": "2026-10-30T01:00:00Z"}],
        }]
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = json.dumps(release_data).encode()
        response.__enter__.return_value.__exit__.return_value = False
        with mock.patch.object(manage_engines.urllib.request, "build_opener") as build:
            build.return_value.open.return_value = response
            beta = manage_engines.fetch_releases("orca", 5, channel="beta")
            self.assertEqual(build.return_value.open.call_count, 1)
        self.assertEqual(beta[0]["channel"], "beta")
        self.assertEqual(beta[0]["release_id"], 321)
        self.assertEqual(beta[0]["html_url"], release_data[0]["html_url"])
        self.assertEqual(beta[0]["assets"][0]["asset_updated_at"], "2026-10-30T01:00:00Z")

    def test_progress_json_is_ndjson_on_stderr_and_unknown_length_omits_percent(self):
        payload = b"verified bytes"
        response = mock.MagicMock()
        response.headers = {}
        response.read.side_effect = [payload, b""]
        response.__enter__.return_value = response
        response.__exit__.return_value = False
        stderr = io.StringIO()
        stdout = io.StringIO()
        reporter = manage_engines.make_progress_reporter(stderr)
        with mock.patch.object(manage_engines.urllib.request, "build_opener") as build, mock.patch("sys.stdout", stdout):
            build.return_value.open.return_value = response
            digest = hashlib.sha256(payload).hexdigest()
            manage_engines.download_asset_verified(
                "https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/test.AppImage",
                self.root / "download", digest,
                progress_callback=lambda done, total: reporter("download", done, total),
            )
        events = [json.loads(line) for line in stderr.getvalue().splitlines()]
        self.assertEqual(events[0]["event"], "progress")
        self.assertEqual(events[0]["stage"], "download")
        self.assertNotIn("percent", events[0])
        self.assertEqual(stdout.getvalue(), "")

    def test_nightly_install_identity_uses_release_and_asset_ids(self):
        self.assertEqual(manage_engines.validate_version("nightly-123456-987654"), "nightly-123456-987654")

    def test_duplicate_install_fails_before_second_extraction(self):
        source = self.root / "mock.AppImage"
        sha = create_mock_appimage(source)
        args = dict(family="orca", version="2.4.2", asset_name=source.name, asset_url="file://x", expected_sha256=sha,
                    local_file=source, data_dir=self.data_dir, no_register=True)
        manage_engines.install_engine(**args)
        with mock.patch.object(manage_engines, "extract_appimage_sandboxed") as extract:
            with self.assertRaises(FileExistsError):
                manage_engines.install_engine(**args)
            extract.assert_not_called()

    def test_tar_extraction_uses_explicit_data_filter(self):
        archive = self.root / "one.tar"
        with tarfile.open(archive, "w") as tar:
            item = tarfile.TarInfo("ok.txt")
            item.size = 2
            tar.addfile(item, io.BytesIO(b"ok"))
        dest = self.root / "extract"
        dest.mkdir()
        original_extractall = tarfile.TarFile.extractall
        calls = []
        def record_extractall(instance, *args, **kwargs):
            calls.append(kwargs)
            return original_extractall(instance, *args, **kwargs)
        with mock.patch.object(tarfile.TarFile, "extractall", record_extractall):
            manage_engines.safe_extract_tar(archive, dest)
        self.assertEqual((dest / "ok.txt").read_bytes(), b"ok")
        self.assertEqual(calls[0].get("filter"), "data")

    # --- Security & URL Allowlist Tests ---

    def test_url_validation_allows_official_github_endpoints(self):
        valid_urls = [
            "https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/OrcaSlicer_Linux_AppImage_Ubuntu2404_V2.4.2.AppImage",
            "https://github.com/bambulab/BambuStudio/releases/download/v02.08.02.61/BambuStudio_ubuntu24.04-v02.08.02.61-20260820225108.AppImage",
            "https://api.github.com/repos/OrcaSlicer/OrcaSlicer/releases/assets/468754790",
            "https://api.github.com/repos/bambulab/BambuStudio/releases/assets/523226841",
            "https://release-assets.githubusercontent.com/github-production-release-asset/514553345/foo.AppImage",
            "https://objects.githubusercontent.com/github-production-release-asset/12345/bar.AppImage",
        ]
        for url in valid_urls:
            self.assertTrue(manage_engines.is_url_allowed(url), f"Should allow {url}")
            manage_engines.validate_initial_download_url(url)

    def test_url_validation_rejects_unauthorized_hosts_and_schemes(self):
        invalid_urls = [
            "http://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/foo.AppImage",
            "https://evil.com/OrcaSlicer.AppImage",
            "https://malicious.githubusercontent.com.attacker.com/foo.AppImage",
            "ftp://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/foo.AppImage",
        ]
        for url in invalid_urls:
            self.assertFalse(manage_engines.is_url_allowed(url), f"Should reject {url}")
            with self.assertRaises(manage_engines.SecurityError):
                manage_engines.validate_initial_download_url(url)

    def test_url_validation_rejects_unauthorized_github_repositories(self):
        unauthorized_repo_urls = [
            "https://github.com/attacker/malware/releases/download/v1.0/hack.AppImage",
            "https://api.github.com/repos/attacker/malware/releases/assets/999",
        ]
        for url in unauthorized_repo_urls:
            with self.assertRaises(manage_engines.SecurityError):
                manage_engines.validate_initial_download_url(url)

    def test_safe_redirect_handler_blocks_redirection_to_untrusted_host(self):
        handler = manage_engines.SafeRedirectHandler()
        # Legitimate redirect to GitHub release asset CDN
        req = urllib.request.Request("https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/test.AppImage")
        new_url_ok = "https://release-assets.githubusercontent.com/asset?token=xyz"
        redir = handler.redirect_request(req, None, 302, "Found", {}, new_url_ok)
        self.assertEqual(redir.full_url, new_url_ok)

        # Malicious redirect to external domain
        new_url_evil = "https://evil-server.org/stolen.AppImage"
        with self.assertRaises(manage_engines.SecurityError) as ctx:
            handler.redirect_request(req, None, 302, "Found", {}, new_url_evil)
        self.assertIn("Redirect rejected", str(ctx.exception))

    # --- Checksum Verification Tests ---

    def test_download_asset_verified_rejects_missing_checksum(self):
        dummy_file = self.root / "dummy.AppImage"
        with self.assertRaises(manage_engines.ChecksumError) as ctx:
            manage_engines.download_asset_verified(
                "https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/test.AppImage",
                dummy_file,
                expected_sha256=None,
            )
        self.assertIn("refusing unverified download", str(ctx.exception))

    def test_download_asset_verified_rejects_invalid_checksum_format(self):
        dummy_file = self.root / "dummy.AppImage"
        with self.assertRaises(manage_engines.ChecksumError) as ctx:
            manage_engines.download_asset_verified(
                "https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/test.AppImage",
                dummy_file,
                expected_sha256="not-a-valid-hex-digest",
            )
        self.assertIn("Invalid SHA-256 format", str(ctx.exception))

    def test_local_install_rejects_checksum_mismatch_and_preserves_target_clean(self):
        mock_file = self.root / "mock.AppImage"
        actual_sha = create_mock_appimage(mock_file)
        tampered_sha = "a" * 64

        with self.assertRaises(manage_engines.ChecksumError) as ctx:
            manage_engines.install_engine(
                family="orca",
                version="2.4.2",
                asset_name="mock.AppImage",
                asset_url="file://mock.AppImage",
                expected_sha256=tampered_sha,
                local_file=mock_file,
                data_dir=self.data_dir,
                config_dir=self.config_dir,
            )
        self.assertIn("Checksum mismatch", str(ctx.exception))
        # Ensure no half-baked install exists in data_dir
        installed_target = self.data_dir / "orca" / "2.4.2"
        self.assertFalse(installed_target.exists())
        family_dir = self.data_dir / "orca"
        self.assertFalse(family_dir.exists() and any(p.name.startswith(".2.4.2.staging.") for p in family_dir.iterdir()))

    # --- Path & Link Traversal Tests ---

    def test_tar_path_traversal_is_detected_and_rejected(self):
        tar_path = self.root / "evil_path.tar.gz"
        content = b"evil payload"
        with tarfile.open(tar_path, "w:gz") as tar:
            ti = tarfile.TarInfo(name="../../etc/evil_payload.txt")
            ti.size = len(content)
            tar.addfile(ti, fileobj=io.BytesIO(content))

        extract_dest = self.root / "extract_dest"
        extract_dest.mkdir()
        with self.assertRaises(manage_engines.SecurityError) as ctx:
            manage_engines.safe_extract_tar(tar_path, extract_dest)
        self.assertIn("Path traversal detected", str(ctx.exception))

    def test_tar_link_traversal_is_detected_and_rejected(self):
        tar_path = self.root / "evil_link.tar.gz"
        with tarfile.open(tar_path, "w:gz") as tar:
            ti = tarfile.TarInfo(name="symlink_escape")
            ti.type = tarfile.SYMTYPE
            ti.linkname = "/etc/passwd"
            tar.addfile(ti)

        extract_dest = self.root / "extract_dest"
        extract_dest.mkdir()
        with self.assertRaises(manage_engines.SecurityError) as ctx:
            manage_engines.safe_extract_tar(tar_path, extract_dest)
        self.assertIn("Link traversal detected", str(ctx.exception))

    def test_zip_path_traversal_is_detected_and_rejected(self):
        zip_path = self.root / "evil.zip"
        with zipfile.ZipFile(zip_path, "w") as zf:
            zf.writestr("../../etc/evil.txt", b"evil")

        extract_dest = self.root / "extract_dest"
        extract_dest.mkdir()
        with self.assertRaises(manage_engines.SecurityError) as ctx:
            manage_engines.safe_extract_zip(zip_path, extract_dest)
        self.assertIn("Path traversal detected", str(ctx.exception))

    def test_appimage_symlink_escape_is_detected_and_rejected(self):
        mock_file = self.root / "mock_evil_appimage.AppImage"
        # Create an AppImage whose extraction produces a symlink to /etc/shadow
        sha = create_mock_appimage(mock_file, malicious_symlink_target=Path("/etc/shadow"))

        with self.assertRaises(manage_engines.SecurityError) as ctx:
            manage_engines.install_engine(
                family="orca",
                version="2.4.2",
                asset_name="mock_evil.AppImage",
                asset_url="file://mock_evil.AppImage",
                expected_sha256=sha,
                local_file=mock_file,
                data_dir=self.data_dir,
                config_dir=self.config_dir,
            )
        self.assertIn("Link traversal rejected", str(ctx.exception))
        # Verify clean failure: no directory created at destination
        self.assertFalse((self.data_dir / "orca" / "2.4.2").exists())

    # --- Atomic Install, Manifest, and Active Engine Isolation Tests ---

    def test_atomic_install_creates_manifest_and_does_not_alter_active_engine(self):
        # 1. Pre-seed engines.json with an existing active Bambu slicer
        config_file = self.config_dir / "slicenv" / "engines.json"
        config_file.parent.mkdir(parents=True)
        initial_config = {
            "active": {
                "kind": "bambu",
                "path": "/opt/bambu-studio/bambu-studio",
                "version": "02.08.04.57",
            },
            "installed": [
                {
                    "kind": "bambu",
                    "path": "/opt/bambu-studio/bambu-studio",
                    "version": "02.08.04.57",
                }
            ],
        }
        config_file.write_text(json.dumps(initial_config), encoding="utf-8")

        # 2. Install a managed OrcaSlicer 2.4.2
        mock_file = self.root / "OrcaSlicer_Linux_AppImage_Ubuntu2404_V2.4.2.AppImage"
        sha = create_mock_appimage(mock_file)

        manifest = manage_engines.install_engine(
            family="orca",
            version="2.4.2",
            asset_name=mock_file.name,
            asset_url="https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/" + mock_file.name,
            expected_sha256=sha,
            asset_id=468754790,
            local_file=mock_file,
            data_dir=self.data_dir,
            config_dir=self.config_dir,
        )

        # 3. Verify manifest contents
        self.assertEqual(manifest.family, "orca")
        self.assertEqual(manifest.version, "2.4.2")
        self.assertEqual(manifest.sha256, sha)
        self.assertTrue(manifest.managed)
        target_dir = self.data_dir / "orca" / "2.4.2"
        self.assertTrue(target_dir.is_dir())
        manifest_path = target_dir / "manifest.json"
        self.assertTrue(manifest_path.is_file())
        with open(manifest_path, encoding="utf-8") as f:
            saved_manifest = json.load(f)
        self.assertEqual(saved_manifest["version"], "2.4.2")
        self.assertTrue(saved_manifest["managed"])

        # 4. Verify executable and profiles
        exec_path = Path(manifest.executable_path)
        self.assertTrue(exec_path.is_file())
        self.assertTrue(os.access(exec_path, os.X_OK))
        self.assertTrue(any("resources/profiles" in p for p in manifest.profile_paths))

        # 5. CRITICAL: Verify active engine choice in engines.json was NOT changed
        with open(config_file, encoding="utf-8") as f:
            updated_config = json.load(f)

        self.assertEqual(
            updated_config["active"]["kind"],
            "bambu",
            "Active slicer family must remain untouched!",
        )
        self.assertEqual(
            updated_config["active"]["path"],
            "/opt/bambu-studio/bambu-studio",
            "Active slicer path must remain untouched!",
        )
        # Verify the new Orca engine was added to installed
        installed_paths = [e["path"] for e in updated_config["installed"]]
        self.assertIn(str(exec_path), installed_paths)
        self.assertNotIn("/opt/bambu-studio/bambu-studio", installed_paths)

    def test_reinstall_requires_force_flag(self):
        mock_file = self.root / "orca.AppImage"
        sha = create_mock_appimage(mock_file)

        # First install
        manage_engines.install_engine(
            family="orca",
            version="2.4.2",
            asset_name="orca.AppImage",
            asset_url="file://orca.AppImage",
            expected_sha256=sha,
            local_file=mock_file,
            data_dir=self.data_dir,
            config_dir=self.config_dir,
        )

        # Second install without force should fail
        with self.assertRaises(FileExistsError):
            manage_engines.install_engine(
                family="orca",
                version="2.4.2",
                asset_name="orca.AppImage",
                asset_url="file://orca.AppImage",
                expected_sha256=sha,
                local_file=mock_file,
                data_dir=self.data_dir,
                config_dir=self.config_dir,
                force=False,
            )

        # Second install with force should succeed
        manage_engines.install_engine(
            family="orca",
            version="2.4.2",
            asset_name="orca.AppImage",
            asset_url="file://orca.AppImage",
            expected_sha256=sha,
            local_file=mock_file,
            data_dir=self.data_dir,
            config_dir=self.config_dir,
            force=True,
        )

    def test_install_no_register_does_not_modify_engines_json(self):
        mock_file = self.root / "orca.AppImage"
        sha = create_mock_appimage(mock_file)
        config_file = self.config_dir / "slicenv" / "engines.json"

        manage_engines.install_engine(
            family="orca",
            version="2.4.2",
            asset_name="orca.AppImage",
            asset_url="file://orca.AppImage",
            expected_sha256=sha,
            local_file=mock_file,
            data_dir=self.data_dir,
            config_dir=self.config_dir,
            no_register=True,
        )

        self.assertFalse(config_file.exists(), "engines.json must not be touched with no_register=True")
        self.assertTrue((self.data_dir / "orca" / "2.4.2" / "manifest.json").is_file())

    # --- Listing & Removal Tests ---

    def test_list_managed_engines_reports_metadata(self):
        mock_file = self.root / "mock.AppImage"
        sha = create_mock_appimage(mock_file)
        manage_engines.install_engine(
            family="bambu",
            version="02.08.02.61",
            asset_name="mock.AppImage",
            asset_url="file://mock.AppImage",
            expected_sha256=sha,
            local_file=mock_file,
            data_dir=self.data_dir,
            config_dir=self.config_dir,
        )

        engines = manage_engines.list_managed_engines("all", data_dir=self.data_dir)
        self.assertEqual(len(engines), 1)
        e = engines[0]
        self.assertEqual(e["family"], "bambu")
        self.assertEqual(e["version"], "02.08.02.61")
        self.assertTrue(e["valid"])
        self.assertGreater(e["size_bytes"], 0)

    def test_remove_managed_engine_rejects_unmanaged_directories(self):
        # Create an unmanaged directory without manifest.json
        unmanaged_dir = self.data_dir / "orca" / "unmanaged-1.0"
        unmanaged_dir.mkdir(parents=True)
        (unmanaged_dir / "orca-slicer").write_text("binary")

        with self.assertRaises(manage_engines.SecurityError) as ctx:
            manage_engines.remove_managed_engine("orca", "unmanaged-1.0", data_dir=self.data_dir)
        self.assertIn("lacks manifest.json", str(ctx.exception))
        self.assertTrue(unmanaged_dir.exists(), "Unmanaged directory must not be deleted")

    def test_remove_nonexistent_managed_engine_raises_file_not_found(self):
        with self.assertRaises(FileNotFoundError):
            manage_engines.remove_managed_engine("orca", "9.9.9", data_dir=self.data_dir)

    def test_remove_managed_engine_cleans_up_and_unregisters(self):
        mock_file = self.root / "mock.AppImage"
        sha = create_mock_appimage(mock_file)
        manage_engines.install_engine(
            family="orca",
            version="2.4.2",
            asset_name="mock.AppImage",
            asset_url="file://mock.AppImage",
            expected_sha256=sha,
            local_file=mock_file,
            data_dir=self.data_dir,
            config_dir=self.config_dir,
        )

        target_dir = self.data_dir / "orca" / "2.4.2"
        self.assertTrue(target_dir.is_dir())

        result = manage_engines.remove_managed_engine(
            "orca", "2.4.2", data_dir=self.data_dir, config_dir=self.config_dir
        )
        self.assertEqual(result["family"], "orca")
        self.assertEqual(result["version"], "2.4.2")
        self.assertFalse(result["was_active"])
        self.assertFalse(target_dir.exists())

        # Verify it was removed from engines.json installed list
        config_file = self.config_dir / "slicenv" / "engines.json"
        with open(config_file, encoding="utf-8") as f:
            cfg = json.load(f)
        self.assertEqual(len(cfg.get("installed", [])), 0)

    def test_remove_active_engine_is_refused(self):
        mock_file = self.root / "mock.AppImage"
        sha = create_mock_appimage(mock_file)
        manifest = manage_engines.install_engine(
            family="orca",
            version="2.4.2",
            asset_name="mock.AppImage",
            asset_url="file://mock.AppImage",
            expected_sha256=sha,
            local_file=mock_file,
            data_dir=self.data_dir,
            config_dir=self.config_dir,
        )

        # Manually set this engine as active in engines.json
        config_file = self.config_dir / "slicenv" / "engines.json"
        with open(config_file, "r", encoding="utf-8") as f:
            cfg = json.load(f)
        cfg["active"] = {
            "kind": "orca",
            "path": manifest.executable_path,
            "version": "2.4.2",
        }
        with open(config_file, "w", encoding="utf-8") as f:
            json.dump(cfg, f)

        # Remove the active engine
        with self.assertRaises(manage_engines.SecurityError):
            manage_engines.remove_managed_engine("orca", "2.4.2", data_dir=self.data_dir, config_dir=self.config_dir)
        self.assertTrue((self.data_dir / "orca" / "2.4.2").exists())

    # --- CLI Dispatch Integration Tests ---

    def test_cli_list_json_empty(self):
        output = io.StringIO()
        old_stdout = sys.stdout
        try:
            sys.stdout = output
            code = manage_engines.main(["list", "--data-dir", str(self.data_dir), "--json"])
            self.assertEqual(code, 0)
        finally:
            sys.stdout = old_stdout

        data = json.loads(output.getvalue())
        self.assertEqual(data, [])

    def test_cli_install_and_list_json(self):
        mock_file = self.root / "orca-2.4.2.AppImage"
        sha = create_mock_appimage(mock_file)

        # Install via CLI
        out_install = io.StringIO()
        old_stdout = sys.stdout
        try:
            sys.stdout = out_install
            code = manage_engines.main([
                "install",
                "--family", "orca",
                "--version", "2.4.2",
                "--file", str(mock_file),
                "--expected-sha256", sha,
                "--data-dir", str(self.data_dir),
                "--config-dir", str(self.config_dir),
                "--json",
            ])
            self.assertEqual(code, 0)
        finally:
            sys.stdout = old_stdout

        installed_info = json.loads(out_install.getvalue())
        self.assertEqual(installed_info["version"], "2.4.2")

        # List via CLI
        out_list = io.StringIO()
        try:
            sys.stdout = out_list
            code = manage_engines.main(["list", "--data-dir", str(self.data_dir), "--json"])
            self.assertEqual(code, 0)
        finally:
            sys.stdout = old_stdout

        listed_info = json.loads(out_list.getvalue())
        self.assertEqual(len(listed_info), 1)
        self.assertEqual(listed_info[0]["version"], "2.4.2")

        # Remove via CLI
        out_remove = io.StringIO()
        try:
            sys.stdout = out_remove
            code = manage_engines.main([
                "remove", "orca/2.4.2",
                "--data-dir", str(self.data_dir),
                "--config-dir", str(self.config_dir),
                "--json",
            ])
            self.assertEqual(code, 0)
        finally:
            sys.stdout = old_stdout

        removed_info = json.loads(out_remove.getvalue())
        self.assertEqual(removed_info["version"], "2.4.2")

    # --- Live Release Query Smoke Test ---

    def test_live_release_listing_returns_x86_64_assets_with_digests(self):
        try:
            releases = manage_engines.fetch_releases(family="orca", limit=1, include_prereleases=False)
        except RuntimeError as e:
            if "rate limit exceeded" in str(e).lower():
                self.skipTest("GitHub API rate limit reached, skipping live test.")
            raise

        self.assertGreaterEqual(len(releases), 1)
        r = releases[0]
        self.assertEqual(r["family"], "orca")
        self.assertTrue(len(r["assets"]) > 0)
        asset = r["assets"][0]
        self.assertTrue(asset["name"].endswith(".AppImage"))
        # Digest must be a 64-char sha256 or non-empty string from GitHub
        self.assertTrue(len(asset["sha256"]) == 64 or asset["sha256"] != "")


if __name__ == "__main__":
    unittest.main()
