# Managed Engine Packages (Bambu Studio & OrcaSlicer)

Documentation for the engine package manager: its security model, installation layout, manifest contract, and integration with the multi-engine slicer architecture.

> **Since 2026-11-08 the app uses the Rust port, `crates/app/src/engine_manager.rs`**, on Linux (AppImage/tar), Windows (portable zip for OrcaSlicer and Bambu Studio, unpacked to `%LOCALAPPDATA%\slicenv\engines\<family>\<version>\`) and macOS (dmg via `hdiutil`, untested on a real Mac). The manifest, `engines.json` contract and security rules below are unchanged. The Python script `crates/slicer/tools/manage_engines.py` remains a standalone Linux CLI; its command examples below still work.

---

## 1. Overview & Capabilities

The engine package manager provides command-line discovery, download, checksum verification, extraction, atomic installation, and removal for Linux x86_64 portable versions of **Bambu Studio** and **OrcaSlicer**.

### Key Capabilities
- **Official Upstream Release Discovery**: Queries the official GitHub APIs (`bambulab/BambuStudio` and `OrcaSlicer/OrcaSlicer`) to list available releases, asset IDs, official SHA-256 digests, and file sizes.
- **Release Channels**: Lists stable, beta (including alpha and release candidates), and explicitly named nightly, daily, or dev-build releases. Nightly discovery scans a bounded set of official published-release API pages and never treats Actions artifacts as releases.
- **Install Progress**: `install --progress-json` emits throttled progress NDJSON on stderr, leaving stdout available for the final JSON manifest. Download byte counts are reported when available; unknown content lengths remain indeterminate.
- **Strict Verification Before Extraction**: Verifies asset SHA-256 digests incrementally during download (or from user-supplied `--expected-sha256`). Rejects execution or extraction if the digest is missing or does not match.
- **Archive Path Checks**:
  - AppImages use `unsquashfs` when available. If unavailable or unsuccessful, the verified AppImage runtime may run `--appimage-extract` in a scratch directory. A scratch directory is not a security sandbox.
  - Tar and Zip archives enforce path traversal and symlink escape checks before any file is written.
  - Extracted directory trees are audited to ensure no symlink points outside the installation root and no dangerous device nodes are created.
- **Atomic Installation & Manifest**:
  - Packages are staged in a temporary directory on the destination filesystem and renamed atomically.
  - A structured `manifest.json` records metadata: engine family, version, asset URL, asset ID, SHA-256 digest, resolved executable path (`AppRun` or binary), discovered profile paths (`resources/profiles`), install timestamp, and `managed: true`.
- **Active Engine Isolation**:
  - Registers installed engines into `$XDG_CONFIG_HOME/slicenv/engines.json` under `"installed"`.
  - **Never** mutates the user's active engine selection (`"active"`). Active slicer choice remains strictly user-controlled.
- **Managed Removal**:
  - Refuses to delete unmanaged directories or arbitrary paths. Requires `manifest.json` with `"managed": true`.
  - Refuses removal if the active executable resolves anywhere inside that version directory. Renames the package to hidden trash, updates registration, then deletes it.
- **Zero Third-Party Dependencies**: Pure Python 3 standard library.

---

## 2. Upstream Release Packaging & Inspection

Audited against live official upstream releases and local installations (`/opt/orca-slicer` and `/usr/bin/bambustudio`).

| Engine Family | Official Upstream Repository | Linux x86_64 Packaging Format | Upstream Checksum Availability | Extracted Executable & Profiles |
| :--- | :--- | :--- | :--- | :--- |
| **Bambu Studio** | `bambulab/BambuStudio` | `BambuStudio_ubu24-*.AppImage`, `BambuStudio_ubuntu24.04-*.AppImage`, `Bambu_Studio_linux_fedora-*.AppImage` | GitHub API `asset.digest` (`sha256:...`) on release assets | Entry point: `AppRun` (or `bin/bambu-studio`), Profiles: `resources/profiles/` |
| **OrcaSlicer** | `OrcaSlicer/OrcaSlicer` | `OrcaSlicer_Linux_AppImage_Ubuntu2404_V*.AppImage` | GitHub API `asset.digest` (`sha256:...`) on release assets | Entry point: `AppRun` (or `bin/orca-slicer`), Profiles: `resources/profiles/` |

### Architectural Verification
- **AppRun Executable Entry**: Extracted AppImages contain `AppRun` at their root, configuring library paths (`APPDIR`, `LD_LIBRARY_PATH`, and renderer overrides) before invoking the slicer binary.
- **Profile Discovery**: Both slicers store system profiles under `resources/profiles/`. The Rust profile locator in `crates/slicer/src/presets.rs` automatically checks `dir.join("resources/profiles")` relative to the executable, enabling instant recognition without running the GUI.

---

## 3. Directory Layout & Manifest Specification

Managed engines reside in `XDG_DATA_HOME/slicenv/engines/` (default: `~/.local/share/slicenv/engines/`).

### Filesystem Structure
```
~/.local/share/slicenv/engines/
├── bambu/
│   └── 02.08.02.61/
│       ├── AppRun
│       ├── bin/
│       ├── resources/
│       │   └── profiles/
│       └── manifest.json
└── orca/
    └── 2.4.2/
        ├── AppRun
        ├── bin/
        ├── resources/
        │   └── profiles/
        └── manifest.json
```

### Manifest Format (`manifest.json`)
```json
{
  "family": "orca",
  "version": "2.4.2",
  "asset_name": "OrcaSlicer_Linux_AppImage_Ubuntu2404_V2.4.2.AppImage",
  "asset_url": "https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/OrcaSlicer_Linux_AppImage_Ubuntu2404_V2.4.2.AppImage",
  "asset_id": 468754790,
  "channel": "stable",
  "release_tag": "v2.4.2",
  "source_url": "https://github.com/OrcaSlicer/OrcaSlicer/releases/tag/v2.4.2",
  "sha256": "d12fb8c8eac1aecd2dfb6377acd48f994f8fa439ed5292fa532dd82880f029fd",
  "executable_path": "/home/user/.local/share/slicenv/engines/orca/2.4.2/AppRun",
  "profile_paths": [
    "/home/user/.local/share/slicenv/engines/orca/2.4.2/resources/profiles"
  ],
  "installed_at": "2026-10-30T14:30:00+00:00",
  "managed": true
}
```

---

## 4. Security & Safety Model

### 1. Official GitHub Domain Enforcement
Only requests to official GitHub domains and CDNs are permitted:
- `github.com` (restricted to official repository paths `/bambulab/BambuStudio/` and `/OrcaSlicer/OrcaSlicer/`)
- `api.github.com` (restricted to official repository API paths)
- `*.githubusercontent.com` (including `release-assets.githubusercontent.com` and `objects.githubusercontent.com`)
All redirects are intercepted by `SafeRedirectHandler`. Any redirection to an untrusted or non-HTTPS destination triggers an immediate `SecurityError`.

### 2. Mandatory Checksum Verification
- The SHA-256 digest is retrieved from the official GitHub release metadata (`asset.digest`) or supplied via `--expected-sha256`.
- Streams are hashed on the fly during download.
- If the hash differs from the expected value, the downloaded file is deleted immediately and installation halts.
- If no checksum is available, downloads are refused.

### 3. Path & Symlink Traversal Protection
- **Tar & Zip Archives**: Member filenames and link targets are validated against the extraction destination root before unpacking. Any `..` segment or absolute path escaping the sandbox raises `SecurityError`.
- **AppImage Extraction**: The extracted hierarchy is audited for symlinks escaping its root. The runtime fallback executes the verified AppImage in a scratch directory; that directory does not isolate the process from the host.

### 4. Non-Destructive App Configuration
`crates/app/src/engines.rs` reads `$XDG_CONFIG_HOME/slicenv/engines.json`:
```json
{
  "active": {
    "kind": "bambu",
    "path": "/usr/bin/bambustudio",
    "version": "02.08.04.57"
  },
  "installed": [ ... ]
}
```
`manage_engines.py` only updates the `"installed"` array. It preserves `"active"` without modification so that background downloads or managed updates cannot switch the slicer behind the user's back.

---

## 5. CLI Commands & Usage

### 1. List Available Upstream Releases
```bash
# List latest releases for both engines
python3 crates/slicer/tools/manage_engines.py list-releases

# List stable releases (default), or choose another release channel
python3 crates/slicer/tools/manage_engines.py list-releases --family orca --channel beta --limit 5
python3 crates/slicer/tools/manage_engines.py list-releases --family orca --channel nightly --limit 5
python3 crates/slicer/tools/manage_engines.py list-releases --channel all --json

# Legacy alias: --prereleases includes every non-stable channel
python3 crates/slicer/tools/manage_engines.py list-releases --family orca --prereleases --limit 5

# JSON output
python3 crates/slicer/tools/manage_engines.py list-releases --json
```

### 2. Install a Managed Version
```bash
# Install by release tag (automatically queries GitHub for official asset & digest)
python3 crates/slicer/tools/manage_engines.py install --family orca --tag v2.4.2

# Machine-readable progress events go to stderr; --json result remains on stdout
python3 crates/slicer/tools/manage_engines.py install --family orca --tag v2.4.2 --progress-json --json

# Install by official asset ID
python3 crates/slicer/tools/manage_engines.py install --family orca --asset-id 468754790

# Install by direct GitHub release URL
python3 crates/slicer/tools/manage_engines.py install \
  --asset-url https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/OrcaSlicer_Linux_AppImage_Ubuntu2404_V2.4.2.AppImage

# Install from a local AppImage file with explicit checksum verification
python3 crates/slicer/tools/manage_engines.py install \
  --file /path/to/downloaded.AppImage \
  --family bambu \
  --version 02.08.02.61 \
  --expected-sha256 d501b103fac5424513ec0e8d6bc145fb30719de2c7d94d7320d723740c81a7fd
```

### 3. List Installed Managed Engines
```bash
python3 crates/slicer/tools/manage_engines.py list

# Output as JSON
python3 crates/slicer/tools/manage_engines.py list --json
```

### 4. Remove a Managed Engine
```bash
# Remove by family and version
python3 crates/slicer/tools/manage_engines.py remove orca 2.4.2
# or
python3 crates/slicer/tools/manage_engines.py remove orca/2.4.2
```

---

## 6. Testing & Test Count

The test suite is located in `crates/slicer/tools/test_manage_engines.py` and uses Python standard library `unittest`:

```bash
python3 -I crates/slicer/tools/test_manage_engines.py -v
```

### Verified Test Cases
1. `test_url_validation_allows_official_github_endpoints`: Validates official repository download and API URLs.
2. `test_url_validation_rejects_unauthorized_hosts_and_schemes`: Rejects HTTP, FTP, and external domains.
3. `test_url_validation_rejects_unauthorized_github_repositories`: Rejects arbitrary third-party GitHub repos.
4. `test_safe_redirect_handler_blocks_redirection_to_untrusted_host`: Enforces redirect safety to authorized CDN endpoints only.
5. `test_download_asset_verified_rejects_missing_checksum`: Prohibits downloads without SHA-256 verification.
6. `test_download_asset_verified_rejects_invalid_checksum_format`: Validates checksum hexadecimal format.
7. `test_local_install_rejects_checksum_mismatch_and_preserves_target_clean`: Asserts mismatch detection and zero target pollution.
8. `test_tar_path_traversal_is_detected_and_rejected`: Detects directory traversal in tar headers.
9. `test_tar_link_traversal_is_detected_and_rejected`: Rejects symlink targets resolving outside extraction sandbox.
10. `test_zip_path_traversal_is_detected_and_rejected`: Detects zip directory traversal attacks.
11. `test_appimage_symlink_escape_is_detected_and_rejected`: Audits extracted AppImage files for symlink traversal.
12. `test_atomic_install_creates_manifest_and_does_not_alter_active_engine`: Verifies manifest generation, profile detection, and ensures active slicer setting in `engines.json` remains untouched.
13. `test_reinstall_requires_force_flag`: Enforces non-destructive overwrite guards.
14. `test_install_no_register_does_not_modify_engines_json`: Verifies isolated data directory installation when registration is disabled.
15. `test_list_managed_engines_reports_metadata`: Validates listing format, status check, and byte accounting.
16. `test_remove_managed_engine_rejects_unmanaged_directories`: Refuses to touch directories without `manifest.json`.
17. `test_remove_nonexistent_managed_engine_raises_file_not_found`: Validates proper error reporting for missing versions.
18. `test_remove_managed_engine_cleans_up_and_unregisters`: Tests directory removal and config deregistration.
19. `test_remove_active_engine_clears_active_and_reports_warning`: Tests safe clearing of active slicer when the active binary is uninstalled.
20. `test_cli_list_json_empty`: Tests CLI JSON output when no engines are installed.
21. `test_cli_install_and_list_json`: Tests end-to-end CLI workflow (install -> list -> remove) using mock AppImage fixtures.
22. `test_live_release_listing_returns_x86_64_assets_with_digests`: Tests live GitHub API querying and digest presence (network-dependent).
Additional regression tests cover hostile install arguments, family/version symlinks, malformed manifests and missing executables during listing, active binaries referenced through alternate paths, corrupt config preservation, forced reinstall rollback after staging or config failures, failed extraction cleanup, missing local checksums, authorization headers, explicit tar extraction filtering, channel classification and metadata, progress event formatting, immutable nightly identity, and duplicate-install rejection before extraction.

---

## 7. Honest Limitations & Non-Goals

1. **No Automatic Active Engine Switching**: Installing or updating a slicer does not switch the active app engine. The active engine is changed only when the user selects it in the Prepare UI or settings modal.
2. **Offline Mode Limitations**: Querying GitHub releases (`list-releases`) or resolving tags (`install --tag`) requires network connectivity. For offline installations, `--file` and `--expected-sha256` provide a completely offline path.
3. **App Parity**: Managing binary packages does not alter slicer compatibility or file format differences (such as older Orca versions rejecting newer Bambu 3MF parameters).
4. **UI Integration**: The CLI tool provides the management foundation; subsequent phases will integrate discovery and progress reporting into the GPUI desktop interface.

## Desktop package panel

Prepare → engine name → Install / remove versions opens cards with upstream
Bambu/Orca identification icons, platform builds, release channel and a direct
GitHub release/changelog link. Stable, Beta / RC (also alpha), Nightly and All
filters query official published releases. Installed versions remain visible
when their original channel is hidden or a nightly asset has rotated.

Install streams backend NDJSON progress on a background worker into the live
panel: downloaded MB, total and percentage where Content-Length is known;
SHA256, extraction and registration have indeterminate stage indicators.
Final JSON remains separate from progress events. Busy operations disable all
install buttons, and completed installs replace the button with status and a
remove action. A sibling platform build shares the version directory and is
labelled accordingly. Active packages cannot be removed. Removal asks for a
confirmation and refreshes the row after completion. Cancellation is pending.

## Independent profile catalogs

Engine installation now imports package JSON profiles into the separate
[profile library](PROFILE-LIBRARY.md), or fetches BBL JSON from the official
release tag when package JSON is absent. Orca 2.5 `.opc` caches are not decoded.
Profile download failures do not roll back a usable binary installation; the
settings panel offers a download-and-reload action when profiles are missing.
Missing executable paths are ignored in the installed engine picker and pruned
when configuration is next saved. Tests isolate HOME and both XDG directories.
