#!/usr/bin/env python3
"""Manage portable slicer engine versions (Bambu Studio & OrcaSlicer) on Linux.

Provides safe downloading, checksum verification, AppImage extraction,
atomic installation, manifest recording, and removal under XDG_DATA_HOME.
Does not alter active slicer application choice in engines.json.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import zipfile
import uuid

OFFICIAL_REPOS = {
    "bambu": "bambulab/BambuStudio",
    "orca": "OrcaSlicer/OrcaSlicer",
}

ALLOWED_DOMAINS = (
    "github.com",
    "api.github.com",
    "raw.githubusercontent.com",
)

ALLOWED_DOMAIN_SUFFIXES = (
    ".github.com",
    ".githubusercontent.com",
)


class SecurityError(Exception):
    """Raised when an operation violates security policies (URL, path, or link traversal)."""


class ChecksumError(Exception):
    """Raised when asset checksum cannot be verified or does not match."""


VERSION_RE = re.compile(r"[0-9A-Za-z][0-9A-Za-z._+-]{0,63}\Z")


def validate_family(family: str) -> str:
    if not isinstance(family, str) or family not in OFFICIAL_REPOS:
        raise ValueError(f"Unknown engine family {family!r}. Expected 'bambu' or 'orca'.")
    return family


def validate_version(version: str) -> str:
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise ValueError("Version must be a single safe path component (1-64 allowed characters).")
    return version


def reject_symlink_components(path: Path) -> None:
    cursor = path
    while True:
        if cursor.is_symlink():
            raise SecurityError(f"Managed storage path contains a symlink: {cursor}")
        if cursor.parent == cursor:
            return
        cursor = cursor.parent


def validate_managed_target(target_dir: Path, family: str, version: str) -> dict:
    reject_symlink_components(target_dir)
    manifest_file = target_dir / "manifest.json"
    try:
        data = json.loads(manifest_file.read_text(encoding="utf-8"))
    except Exception as e:
        raise SecurityError(f"Invalid managed manifest in {target_dir}: {e}") from e
    if not isinstance(data, dict) or data.get("managed") is not True or data.get("family") != family or data.get("version") != version:
        raise SecurityError(f"Target is not a valid managed {family} {version} installation")
    raw_exec = data.get("executable_path")
    if not isinstance(raw_exec, str):
        raise SecurityError("Managed manifest has no valid executable_path")
    executable = Path(raw_exec)
    resolved_target = target_dir.resolve()
    try:
        if executable.is_symlink():
            raise ValueError("symlink executable")
        executable.resolve(strict=True).relative_to(resolved_target)
    except (OSError, ValueError):
        raise SecurityError("Managed executable must resolve inside its version directory")
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise SecurityError("Managed executable is missing or not executable")
    return data


def get_data_home() -> Path:
    xdg_data = os.environ.get("XDG_DATA_HOME")
    if xdg_data:
        return Path(xdg_data)
    return Path.home() / ".local" / "share"


def get_config_home() -> Path:
    xdg_config = os.environ.get("XDG_CONFIG_HOME")
    if xdg_config:
        return Path(xdg_config)
    return Path.home() / ".config"


def get_engines_root(custom_data_dir: Path | str | None = None) -> Path:
    if custom_data_dir:
        return Path(custom_data_dir)
    return get_data_home() / "slicenv" / "engines"


def get_config_file(custom_config_dir: Path | str | None = None) -> Path:
    if custom_config_dir:
        return Path(custom_config_dir) / "slicenv" / "engines.json"
    return get_config_home() / "slicenv" / "engines.json"


def _read_engines_config(config_file: Path) -> dict:
    if not config_file.is_file():
        return {}
    try:
        config = json.loads(config_file.read_text(encoding="utf-8"))
    except Exception as e:
        raise ValueError(f"Corrupt engines configuration: {config_file}") from e
    if not isinstance(config, dict):
        raise ValueError(f"engines.json must contain an object: {config_file}")
    return config


def is_url_allowed(url: str) -> bool:
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https":
        return False
    host = (parsed.hostname or "").lower()
    if host in ALLOWED_DOMAINS:
        return True
    if any(host.endswith(suffix) for suffix in ALLOWED_DOMAIN_SUFFIXES):
        return True
    return False


def validate_initial_download_url(url: str) -> None:
    if not is_url_allowed(url):
        raise SecurityError(
            f"URL '{url}' rejected. Downloads must originate from official GitHub domains."
        )
    parsed = urllib.parse.urlparse(url)
    host = (parsed.hostname or "").lower()
    path = parsed.path.lower()
    if host == "github.com":
        valid_paths = [f"/{repo.lower()}/" for repo in OFFICIAL_REPOS.values()]
        if not any(path.startswith(vp) for vp in valid_paths):
            raise SecurityError(
                f"URL '{url}' is not from an official repository ({', '.join(OFFICIAL_REPOS.values())})."
            )
    elif host == "api.github.com":
        valid_api_paths = [f"/repos/{repo.lower()}/" for repo in OFFICIAL_REPOS.values()]
        if not any(path.startswith(vp) for vp in valid_api_paths):
            raise SecurityError(
                f"API URL '{url}' does not target an official repository."
            )


class SafeRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Enforces that all HTTP redirects remain within verified GitHub domains."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not is_url_allowed(newurl):
            raise SecurityError(
                f"Redirect rejected: target host '{newurl}' is outside authorized GitHub domains."
            )
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def make_request(url: str, extra_headers: dict | None = None) -> urllib.request.Request:
    headers = {
        "User-Agent": "slicenv-engine-manager/1.0 (Linux x86_64)",
        "Accept": "application/vnd.github+json",
    }
    if extra_headers:
        headers.update(extra_headers)
    return urllib.request.Request(url, headers=headers)


def is_linux_x86_64_asset(name: str) -> bool:
    lower = name.lower()
    if not (lower.endswith(".appimage") or lower.endswith(".tar.gz") or lower.endswith(".tar.xz")):
        return False
    if any(arch in lower for arch in ("aarch64", "arm64", "armv7", "armhf", "i386", "i686")):
        return False
    if any(ext in lower for ext in (".dmg", ".exe", ".msi", ".pdb", ".7z", ".flatpak", ".zip")):
        return False
    return True


def classify_release_channel(release: dict) -> str:
    text = f"{release.get('tag_name') or ''} {release.get('name') or ''}".lower()
    if re.search(r"(?:nightly|daily|dev[-_ ]?build)", text):
        return "nightly"
    if release.get("prerelease") or re.search(r"(?:alpha|beta|rc)(?:[0-9.]|\b)", text):
        return "beta"
    return "stable"


def fetch_releases(
    family: str = "all", limit: int = 10, include_prereleases: bool = False,
    channel: str | None = None,
) -> list[dict]:
    if channel is None:
        channel = "all" if include_prereleases else "stable"
    if channel not in {"stable", "beta", "nightly", "all"}:
        raise ValueError("Channel must be stable, beta, nightly, or all.")
    if family != "all":
        validate_family(family)
    families = [family] if family in OFFICIAL_REPOS else list(OFFICIAL_REPOS.keys())
    opener = urllib.request.build_opener(SafeRedirectHandler)
    all_releases = []

    for fam in families:
        repo = OFFICIAL_REPOS[fam]
        data = []
        # GitHub's releases endpoint contains published releases (including nightly
        # releases), never workflow artifacts. Scan a bounded number of pages.
        page_limit = 1 if channel == "stable" else 5
        for page in range(1, page_limit + 1):
            url = f"https://api.github.com/repos/{repo}/releases?per_page=100&page={page}"
            try:
                with opener.open(make_request(url), timeout=15) as resp:
                    page_data = json.loads(resp.read().decode("utf-8"))
            except urllib.error.HTTPError as e:
                if e.code == 403:
                    raise RuntimeError(
                        f"GitHub API rate limit exceeded while querying {repo}. "
                        "Try again after the rate limit resets."
                    ) from e
                raise RuntimeError(f"GitHub API error fetching {repo}: {e}") from e
            if not isinstance(page_data, list):
                break
            data.extend(page_data)
            if len(page_data) < 100:
                break
        count = 0
        for rel in data:
            if rel.get("draft"):
                continue
            is_pre = bool(rel.get("prerelease"))
            release_channel = classify_release_channel(rel)
            if channel != "all" and release_channel != channel:
                continue

            matching_assets = []
            for asset in rel.get("assets", []):
                name = asset.get("name", "")
                if is_linux_x86_64_asset(name):
                    digest = asset.get("digest") or ""
                    sha256 = ""
                    if digest.startswith("sha256:"):
                        sha256 = digest[len("sha256:"):]
                    matching_assets.append({
                        "id": asset.get("id"),
                        "name": name,
                        "size": asset.get("size", 0),
                        "download_url": asset.get("browser_download_url"),
                        "sha256": sha256,
                        "asset_updated_at": asset.get("updated_at"),
                    })

            if matching_assets:
                all_releases.append({
                    "family": fam,
                    "repo": repo,
                    "tag": rel.get("tag_name"),
                    "release_id": rel.get("id"),
                    "html_url": rel.get("html_url"),
                    "name": rel.get("name") or rel.get("tag_name"),
                    "channel": release_channel,
                    "prerelease": is_pre,
                    "published_at": rel.get("published_at"),
                    "assets": matching_assets,
                })
                count += 1
                if count >= limit:
                    break

    return all_releases


def resolve_release_asset_info(
    family: str | None,
    asset_id: int | None,
    asset_url: str | None,
    tag: str | None,
) -> tuple[str, str, str, str, str, int | None]:
    """Resolve (family, version, asset_name, asset_url, sha256, asset_id) from GitHub API."""
    opener = urllib.request.build_opener(SafeRedirectHandler)

    # Strategy 1: Explicit asset_id
    if asset_id:
        repos_to_try = [OFFICIAL_REPOS[family]] if family in OFFICIAL_REPOS else list(OFFICIAL_REPOS.values())
        for repo in repos_to_try:
            api_url = f"https://api.github.com/repos/{repo}/releases/assets/{asset_id}"
            try:
                with opener.open(make_request(api_url), timeout=15) as resp:
                    adata = json.loads(resp.read().decode("utf-8"))
                    fam = "orca" if "orca" in repo.lower() else "bambu"
                    name = adata.get("name", "")
                    dl_url = adata.get("browser_download_url", "")
                    digest = adata.get("digest") or ""
                    sha256 = digest[len("sha256:"):] if digest.startswith("sha256:") else ""
                    version = clean_version_tag(extract_version_from_name(name))
                    return fam, version, name, dl_url, sha256, asset_id
            except Exception:
                continue
        raise RuntimeError(f"Could not resolve asset ID {asset_id} in official repositories.")

    # Strategy 2: Official download URL
    if asset_url:
        validate_initial_download_url(asset_url)
        parsed = urllib.parse.urlparse(asset_url)
        path = parsed.path
        # Match https://github.com/{owner}/{repo}/releases/download/{tag}/{filename}
        m = re.match(r"^/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)$", path)
        if m:
            owner, repo_name, rel_tag, fname = m.groups()
            fam = "orca" if "orca" in repo_name.lower() else "bambu"
            api_url = f"https://api.github.com/repos/{owner}/{repo_name}/releases/tags/{rel_tag}"
            sha256 = ""
            aid = None
            try:
                with opener.open(make_request(api_url), timeout=15) as resp:
                    rdata = json.loads(resp.read().decode("utf-8"))
                    for a in rdata.get("assets", []):
                        if a.get("name") == fname:
                            aid = a.get("id")
                            digest = a.get("digest") or ""
                            if digest.startswith("sha256:"):
                                sha256 = digest[len("sha256:"):]
                            break
            except Exception:
                pass
            version = clean_version_tag(rel_tag)
            return fam, version, fname, asset_url, sha256, aid

    # Strategy 3: Tag lookup
    if tag:
        if not family or family not in OFFICIAL_REPOS:
            raise ValueError(f"Valid --family (bambu or orca) is required when using --tag.")
        repo = OFFICIAL_REPOS[family]
        api_url = f"https://api.github.com/repos/{repo}/releases/tags/{tag}"
        try:
            with opener.open(make_request(api_url), timeout=15) as resp:
                rdata = json.loads(resp.read().decode("utf-8"))
        except Exception as e:
            raise RuntimeError(f"Could not find release tag '{tag}' in {repo}: {e}") from e

        matching = [a for a in rdata.get("assets", []) if is_linux_x86_64_asset(a.get("name", ""))]
        if not matching:
            raise RuntimeError(f"No Linux x86_64 assets found in release {tag} for {family}.")

        first = matching[0]
        digest = first.get("digest") or ""
        sha256 = digest[len("sha256:"):] if digest.startswith("sha256:") else ""
        return (
            family,
            clean_version_tag(tag),
            first.get("name", ""),
            first.get("browser_download_url", ""),
            sha256,
            first.get("id"),
        )

    raise ValueError("Must provide asset-url, asset-id, tag, or local file.")


def compute_file_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as stream:
        while chunk := stream.read(65536):
            h.update(chunk)
    return h.hexdigest().lower()


def download_asset_verified(
    url: str,
    target_path: Path,
    expected_sha256: str | None,
    progress_callback=None,
) -> str:
    validate_initial_download_url(url)
    if not expected_sha256:
        raise ChecksumError(
            "Security policy violation: refusing unverified download without expected SHA-256 digest."
        )

    expected = expected_sha256.strip().lower()
    if len(expected) != 64 or not all(c in "0123456789abcdef" for c in expected):
        raise ChecksumError(f"Invalid SHA-256 format: '{expected_sha256}'")

    opener = urllib.request.build_opener(SafeRedirectHandler)
    req = make_request(url, {"Accept": "application/octet-stream"})

    h = hashlib.sha256()
    downloaded = 0
    target_path.parent.mkdir(parents=True, exist_ok=True)

    try:
        with opener.open(req, timeout=30) as resp, open(target_path, "wb") as out:
            total_size = int(resp.headers.get("Content-Length") or 0)
            while chunk := resp.read(65536):
                h.update(chunk)
                out.write(chunk)
                downloaded += len(chunk)
                if progress_callback:
                    progress_callback(downloaded, total_size)
    except Exception:
        if target_path.exists():
            target_path.unlink()
        raise

    actual = h.hexdigest().lower()
    if actual != expected:
        target_path.unlink(missing_ok=True)
        raise ChecksumError(
            f"SHA-256 checksum mismatch for {url}!\n"
            f"  Expected: {expected}\n"
            f"  Actual:   {actual}"
        )
    return actual


def audit_extracted_tree(root_dir: Path) -> None:
    root_resolved = root_dir.resolve()
    for item in root_dir.rglob("*"):
        if item.is_symlink():
            try:
                resolved = item.resolve(strict=False)
            except RuntimeError as e:
                raise SecurityError(f"Symlink loop or unresolvable link: {item}") from e
            if not resolved.is_relative_to(root_resolved):
                raise SecurityError(
                    f"Link traversal rejected: symlink '{item.relative_to(root_dir)}' "
                    f"points outside extraction sandbox to '{resolved}'."
                )
        if item.is_fifo() or item.is_char_device() or item.is_block_device():
            raise SecurityError(f"Dangerous device node rejected in extraction: {item}")


def safe_extract_tar(tar_path: Path, dest_dir: Path) -> None:
    dest_dir = dest_dir.resolve()
    with tarfile.open(tar_path, "r:*") as tar:
        for member in tar.getmembers():
            member_dest = (dest_dir / member.name).resolve()
            if not member_dest.is_relative_to(dest_dir):
                raise SecurityError(
                    f"Path traversal detected in archive member: '{member.name}'"
                )
            if member.issym() or member.islnk():
                link_base = dest_dir if member.islnk() else member_dest.parent
                link_dest = (link_base / member.linkname).resolve()
                if not link_dest.is_relative_to(dest_dir):
                    raise SecurityError(
                        f"Link traversal detected in archive member: '{member.name}' -> '{member.linkname}'"
                    )
        tar.extractall(dest_dir, filter="data")
    audit_extracted_tree(dest_dir)


def safe_extract_zip(zip_path: Path, dest_dir: Path) -> None:
    dest_dir = dest_dir.resolve()
    with zipfile.ZipFile(zip_path, "r") as zf:
        for info in zf.infolist():
            member_dest = (dest_dir / info.filename).resolve()
            if not member_dest.is_relative_to(dest_dir):
                raise SecurityError(
                    f"Path traversal detected in zip entry: '{info.filename}'"
                )
        zf.extractall(dest_dir)
    audit_extracted_tree(dest_dir)


def extract_appimage_sandboxed(appimage_path: Path, scratch_dir: Path) -> Path:
    squashfs_root = scratch_dir / "squashfs-root"
    unsquashfs = shutil.which("unsquashfs")
    proc2 = None
    if unsquashfs:
        proc2 = subprocess.run(
                [unsquashfs, "-d", str(squashfs_root), str(appimage_path.resolve())],
                cwd=scratch_dir,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
    if not proc2 or proc2.returncode != 0 or not squashfs_root.is_dir():
        # This runs the verified AppImage runtime; the scratch directory is not a security sandbox.
        os.chmod(appimage_path, 0o755)
        proc = subprocess.run(
            [str(appimage_path.resolve()), "--appimage-extract"], cwd=scratch_dir,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        if proc.returncode != 0 or not squashfs_root.is_dir():
            unsquash_error = proc2.stderr.strip() if proc2 else "unsquashfs unavailable"
            raise RuntimeError(f"AppImage extraction failed (unsquashfs: {unsquash_error}; runtime: {proc.stderr.strip()})")

    audit_extracted_tree(squashfs_root)
    return squashfs_root


def find_executable_and_profiles(install_dir: Path, family: str) -> tuple[Path, list[Path]]:
    executable = None

    # Priority 1: AppRun
    apprun = install_dir / "AppRun"
    if apprun.is_file():
        os.chmod(apprun, 0o755)
        executable = apprun

    # Priority 2: Standard bin/ directories
    if not executable:
        bin_candidates = [
            install_dir / "bin" / ("orca-slicer" if family == "orca" else "bambu-studio"),
            install_dir / "bin" / ("OrcaSlicer" if family == "orca" else "BambuStudio"),
            install_dir / ("orca-slicer" if family == "orca" else "bambu-studio"),
        ]
        for c in bin_candidates:
            if c.is_file():
                os.chmod(c, 0o755)
                executable = c
                break

    if not executable:
        # Search anywhere inside install_dir
        for item in install_dir.rglob("*"):
            if item.is_file() and os.access(item, os.X_OK) and not item.is_symlink():
                name = item.name.lower()
                if "slicer" in name or "studio" in name:
                    executable = item
                    break

    if not executable:
        raise RuntimeError(f"Could not locate main executable in extracted {family} package.")

    profiles = []
    candidates = [
        install_dir / "resources" / "profiles",
        install_dir / "resources" / "printers",
        install_dir / "profiles",
    ]
    for c in candidates:
        if c.is_dir():
            profiles.append(c)

    return executable, profiles


def update_engines_config(
    family: str,
    version: str,
    executable_path: Path,
    config_dir: Path | str | None = None,
    data_dir: Path | str | None = None,
) -> None:
    """Register managed engine in engines.json without altering active engine."""
    config_file = get_config_file(config_dir)
    config = _read_engines_config(config_file)

    installed = config.get("installed")
    if not isinstance(installed, list):
        installed = []

    # Preserve all other installations, replacing any matching family and path
    str_path = str(executable_path)
    new_installed = [
        e for e in installed
        if isinstance(e, dict) and isinstance(e.get("path"), str) and Path(e["path"]).is_file()
        and not (e.get("kind") == family and e.get("path") == str_path)
    ]
    new_installed.append({
        "kind": family,
        "path": str_path,
        "version": version,
    })
    # Managed versions whose entries were dropped (e.g. the data directory moved)
    # come back from their manifests.
    try:
        managed = list_managed_engines(data_dir=data_dir)
    except Exception:
        managed = []
    for m in managed:
        if m["valid"] and not any(e.get("kind") == m["family"] and e.get("path") == m["executable_path"] for e in new_installed):
            new_installed.append({"kind": m["family"], "path": m["executable_path"], "version": m["version"]})

    # Sort installed entries deterministically
    new_installed.sort(key=lambda x: (x.get("kind", ""), x.get("version", ""), x.get("path", "")))

    # CRITICAL: Preserve 'active' as-is; never auto-switch active slicer
    config["installed"] = new_installed

    config_file.parent.mkdir(parents=True, exist_ok=True)
    temp_file = config_file.parent / f".engines.json.tmp.{os.getpid()}"
    with open(temp_file, "w", encoding="utf-8") as stream:
        json.dump(config, stream, indent=2, ensure_ascii=False)
        stream.write("\n")
    os.replace(temp_file, config_file)


def remove_from_engines_config(
    family: str,
    executable_path: Path,
    config_dir: Path | str | None = None,
) -> bool:
    """Remove engine from engines.json. Returns True if was active."""
    config_file = get_config_file(config_dir)
    if not config_file.is_file():
        return False

    try:
        with open(config_file, "r", encoding="utf-8") as stream:
            config = json.load(stream)
    except Exception as e:
        raise ValueError(f"Corrupt engines configuration: {config_file}") from e
    if not isinstance(config, dict):
        raise ValueError(f"engines.json must contain an object: {config_file}")

    installed = config.get("installed")
    str_path = str(executable_path)
    if isinstance(installed, list):
        config["installed"] = [
            e for e in installed
            if isinstance(e, dict) and isinstance(e.get("path"), str) and Path(e["path"]).is_file()
        and not (e.get("kind") == family and e.get("path") == str_path)
        ]

    was_active = False
    active = config.get("active")
    if isinstance(active, dict) and active.get("kind") == family and active.get("path") == str_path:
        was_active = True
        config.pop("active", None)

    temp_file = config_file.parent / f".engines.json.tmp.{os.getpid()}"
    with open(temp_file, "w", encoding="utf-8") as stream:
        json.dump(config, stream, indent=2, ensure_ascii=False)
        stream.write("\n")
    os.replace(temp_file, config_file)
    return was_active


@dataclass
class Manifest:
    family: str
    version: str
    asset_name: str
    asset_url: str
    asset_id: int | None
    sha256: str
    executable_path: str
    profile_paths: list[str]
    installed_at: str
    managed: bool = True
    channel: str | None = None
    release_tag: str | None = None
    source_url: str | None = None

    def save(self, destination: Path) -> None:
        manifest_path = destination / "manifest.json"
        with open(manifest_path, "w", encoding="utf-8") as stream:
            json.dump(asdict(self), stream, indent=2, ensure_ascii=False)
            stream.write("\n")


def install_engine(
    family: str,
    version: str,
    asset_name: str,
    asset_url: str,
    expected_sha256: str | None,
    asset_id: int | None = None,
    local_file: Path | None = None,
    force: bool = False,
    data_dir: Path | str | None = None,
    config_dir: Path | str | None = None,
    no_register: bool = False,
    progress_callback=None,
    channel: str | None = None,
    release_tag: str | None = None,
    source_url: str | None = None,
) -> Manifest:
    """Install an engine package atomically into XDG_DATA_HOME/slicenv/engines/<family>/<version>."""
    family = validate_family(family)
    version = validate_version(version)
    if not isinstance(asset_name, str) or not asset_name or Path(asset_name).name != asset_name or asset_name in {".", ".."}:
        raise SecurityError("Asset name must be a single filename")
    if not expected_sha256 or not re.fullmatch(r"[0-9a-fA-F]{64}", expected_sha256.strip()):
        raise ChecksumError("A valid SHA-256 checksum is required for every installation.")
    root = get_engines_root(data_dir)
    reject_symlink_components(root)
    dest_root = root / family
    reject_symlink_components(dest_root)
    dest_root.mkdir(parents=True, exist_ok=True)
    dest_root = dest_root.resolve()
    if dest_root.parent != root.resolve():
        raise SecurityError("Managed family directory resolved outside engine root")
    target_dir = dest_root / version
    reject_symlink_components(target_dir)
    if target_dir.is_symlink() or (target_dir.exists() and target_dir.resolve().parent != dest_root):
        raise SecurityError("Managed version target must be a real directory inside its family")
    if target_dir.exists():
        if not force:
            raise FileExistsError(
                f"Engine {family} {version} is already installed at {target_dir}. Use --force to reinstall."
            )
        validate_managed_target(target_dir, family, version)
    if not no_register:
        _read_engines_config(get_config_file(config_dir))
    if family not in OFFICIAL_REPOS:
        raise ValueError(f"Unknown engine family '{family}'. Expected 'bambu' or 'orca'.")
    staging_dir = dest_root / f".{version}.staging.{uuid.uuid4().hex}"
    staging_dir.mkdir(parents=True)

    verified_sha256 = ""
    try:
        with tempfile.TemporaryDirectory() as scratch_temp:
            scratch_path = Path(scratch_temp)

            if local_file:
                local_path = Path(local_file).resolve()
                if not local_path.is_file():
                    raise FileNotFoundError(f"Local package file not found: {local_file}")
                if progress_callback:
                    progress_callback("verify", None, None)
                verified_sha256 = compute_file_sha256(local_path)
                if verified_sha256.lower() != expected_sha256.strip().lower():
                    raise ChecksumError(
                        f"Checksum mismatch for local file {local_file}!\n"
                        f"  Expected: {expected_sha256}\n"
                        f"  Actual:   {verified_sha256}"
                    )
                archive_path = local_path
            else:
                download_dest = scratch_path / asset_name
                verified_sha256 = download_asset_verified(
                    asset_url, download_dest, expected_sha256,
                    progress_callback=(lambda done, total: progress_callback("download", done, total)) if progress_callback else None,
                )
                archive_path = download_dest
                if progress_callback:
                    progress_callback("verify", None, None)

            # Extract safely based on package type
            lower_name = archive_path.name.lower()
            if lower_name.endswith(".appimage"):
                if progress_callback:
                    progress_callback("extract", None, None)
                extracted_tree = extract_appimage_sandboxed(archive_path, scratch_path)
                for item in extracted_tree.iterdir():
                    shutil.move(str(item), str(staging_dir / item.name))
            elif lower_name.endswith((".tar.gz", ".tar.xz", ".tar")):
                if progress_callback:
                    progress_callback("extract", None, None)
                safe_extract_tar(archive_path, staging_dir)
            elif lower_name.endswith(".zip"):
                if progress_callback:
                    progress_callback("extract", None, None)
                safe_extract_zip(archive_path, staging_dir)
            else:
                raise ValueError(f"Unsupported archive format: {archive_path.name}")

        executable, profiles = find_executable_and_profiles(staging_dir, family)
        final_executable = target_dir / executable.relative_to(staging_dir)
        final_profiles = [target_dir / p.relative_to(staging_dir) for p in profiles]

        manifest = Manifest(
            family=family,
            version=version,
            asset_name=asset_name,
            asset_url=asset_url,
            asset_id=asset_id,
            sha256=verified_sha256,
            executable_path=str(final_executable),
            profile_paths=[str(p) for p in final_profiles],
            installed_at=datetime.now(timezone.utc).isoformat(),
            managed=True,
            channel=channel,
            release_tag=release_tag,
            source_url=source_url or asset_url,
        )
        manifest.save(staging_dir)

        backup_dir = None
        if target_dir.exists():
            backup_dir = dest_root / f".{version}.backup.{uuid.uuid4().hex}"
            target_dir.rename(backup_dir)
        try:
            staging_dir.rename(target_dir)
        except Exception:
            if backup_dir and backup_dir.exists():
                backup_dir.rename(target_dir)
            raise
        try:
            if progress_callback:
                progress_callback("register", None, None)
            if not no_register:
                update_engines_config(family, version, final_executable, config_dir, data_dir)
        except Exception:
            shutil.rmtree(target_dir, ignore_errors=True)
            if backup_dir and backup_dir.exists():
                backup_dir.rename(target_dir)
            raise
        if backup_dir:
            shutil.rmtree(backup_dir)

        # "done" emits the caller after the profile catalog: that step can take minutes.
        return manifest
    except Exception:
        if staging_dir.exists():
            shutil.rmtree(staging_dir, ignore_errors=True)
        raise


def relocate_manifest(entry: Path, family: str, data: dict) -> dict:
    """Heal a manifest whose paths point at a moved data directory.

    Installations made before the data directory was renamed (bambu-lab -> slicenv)
    still record the old absolute paths. The relative layout under
    `<family>/<version>/` is unchanged, so rebase it onto the current directory.
    """
    marker = f"/{family}/{entry.name}/"

    def rebase(raw):
        if not isinstance(raw, str) or marker not in raw:
            return raw
        candidate = entry / raw.split(marker, 1)[1]
        return str(candidate) if candidate.exists() and not candidate.is_symlink() else raw

    raw_exec = data.get("executable_path")
    if not isinstance(raw_exec, str) or Path(raw_exec).is_file():
        return data
    healed = dict(data)
    healed["executable_path"] = rebase(raw_exec)
    if healed["executable_path"] == raw_exec:
        return data
    if isinstance(data.get("profile_paths"), list):
        healed["profile_paths"] = [rebase(p) for p in data["profile_paths"]]
    manifest_file = entry / "manifest.json"
    temp_file = entry / f".manifest.json.tmp.{os.getpid()}"
    temp_file.write_text(json.dumps(healed, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    os.replace(temp_file, manifest_file)
    return healed


def list_managed_engines(family: str = "all", data_dir: Path | str | None = None) -> list[dict]:
    if family != "all":
        validate_family(family)
    engines_root = get_engines_root(data_dir)
    reject_symlink_components(engines_root)
    families = [family] if family in OFFICIAL_REPOS else list(OFFICIAL_REPOS.keys())
    results = []

    for fam in families:
        fam_dir = engines_root / fam
        if fam_dir.is_symlink():
            continue
        if not fam_dir.is_dir():
            continue
        for entry in sorted(fam_dir.iterdir()):
            if entry.is_dir() and not entry.name.startswith("."):
                manifest_file = entry / "manifest.json"
                if not manifest_file.is_file():
                    continue
                try:
                    with open(manifest_file, "r", encoding="utf-8") as stream:
                        data = json.load(stream)
                except Exception:
                    continue

                if not isinstance(data, dict) or data.get("managed") is not True:
                    continue
                try:
                    data = relocate_manifest(entry, fam, data)
                except OSError:
                    pass
                try:
                    validate_managed_target(entry, fam, entry.name)
                    exec_path = Path(data["executable_path"])
                    is_valid = True
                except Exception:
                    raw_path = data.get("executable_path")
                    exec_path = Path(raw_path) if isinstance(raw_path, str) else Path("")
                    is_valid = False

                # Disk usage
                total_bytes = 0
                for f in entry.rglob("*"):
                    try:
                        if f.is_file():
                            total_bytes += f.stat().st_size
                    except OSError:
                        continue

                results.append({
                    "family": fam,
                    "version": data.get("version", entry.name),
                    "valid": is_valid,
                    "executable_path": str(exec_path),
                    "profile_paths": data.get("profile_paths", []),
                    "sha256": data.get("sha256", ""),
                    "asset_id": data.get("asset_id"),
                    "channel": data.get("channel"),
                    "release_tag": data.get("release_tag"),
                    "source_url": data.get("source_url"),
                    "installed_at": data.get("installed_at", ""),
                    "size_bytes": total_bytes,
                    "directory": str(entry),
                })

    return results


def remove_managed_engine(
    family: str,
    version: str,
    data_dir: Path | str | None = None,
    config_dir: Path | str | None = None,
) -> dict:
    family = validate_family(family)
    version = validate_version(version)
    engines_root = get_engines_root(data_dir)
    family_dir = engines_root / family
    reject_symlink_components(family_dir)
    real_family = family_dir.resolve()
    if real_family.parent != engines_root.resolve():
        raise SecurityError("Managed family directory resolves outside engine root")
    target_dir = family_dir / version
    reject_symlink_components(target_dir)
    if target_dir.resolve(strict=False).parent != real_family:
        raise SecurityError("Managed version target resolves outside its family directory")

    if not target_dir.is_dir():
        raise FileNotFoundError(f"Managed engine {family} {version} not found at {target_dir}")

    manifest_file = target_dir / "manifest.json"
    if not manifest_file.is_file():
        raise SecurityError(
            f"Refusing to delete unmanaged directory: {target_dir} lacks manifest.json"
        )

    manifest_data = validate_managed_target(target_dir, family, version)
    exec_path = Path(manifest_data["executable_path"])
    config_file = get_config_file(config_dir)
    if config_file.is_file():
        try:
            config = json.loads(config_file.read_text(encoding="utf-8"))
        except Exception as e:
            raise ValueError(f"Corrupt engines configuration: {config_file}") from e
        if not isinstance(config, dict):
            raise ValueError(f"engines.json must contain an object: {config_file}")
        active = config.get("active")
        active_path = active.get("path") if isinstance(active, dict) else None
        try:
            active_resolved = Path(active_path).resolve(strict=False) if isinstance(active_path, str) else None
        except (OSError, RuntimeError):
            active_resolved = None
        if active_resolved is not None and active_resolved.is_relative_to(target_dir.resolve()):
            raise SecurityError("Refusing to remove an engine containing the active executable")
    trash = family_dir / f".{version}.trash.{uuid.uuid4().hex}"
    target_dir.rename(trash)
    try:
        was_active = remove_from_engines_config(family, exec_path, config_dir)
    except Exception:
        trash.rename(target_dir)
        raise
    shutil.rmtree(trash)
    return {
        "family": family,
        "version": version,
        "removed_directory": str(target_dir),
        "was_active": was_active,
    }


def clean_version_tag(tag: str) -> str:
    if tag.startswith("v") or tag.startswith("V"):
        return tag[1:]
    return tag


def extract_version_from_name(name: str) -> str:
    m = re.search(r"[-_vV]?(\d+\.\d+\.\d+(?:\.\d+)?(?:-[a-zA-Z0-9]+)?)", name)
    if m:
        return m.group(1)
    return name


def fetch_release_metadata(family: str, tag: str | None = None, asset_id: int | None = None) -> dict:
    """Return official published release metadata for an install selection."""
    repo = OFFICIAL_REPOS[validate_family(family)]
    opener = urllib.request.build_opener(SafeRedirectHandler)
    if tag:
        url = f"https://api.github.com/repos/{repo}/releases/tags/{urllib.parse.quote(tag, safe='')}"
        with opener.open(make_request(url), timeout=15) as resp:
            return json.loads(resp.read().decode("utf-8"))
    for page in range(1, 6):
        url = f"https://api.github.com/repos/{repo}/releases?per_page=100&page={page}"
        with opener.open(make_request(url), timeout=15) as resp:
            releases = json.loads(resp.read().decode("utf-8"))
        for release in releases:
            if any(asset.get("id") == asset_id for asset in release.get("assets", [])):
                return release
        if len(releases) < 100:
            break
    return {}


def make_progress_reporter(stream=None):
    stream = stream or sys.stderr
    last_emitted = [0.0]

    def report(stage: str, downloaded: int | None, total: int | None):
        import time
        now = time.monotonic()
        if stage in ("download", "profiles") and downloaded is not None and downloaded != total and now - last_emitted[0] < 0.2:
            return
        last_emitted[0] = now
        event = {"event": "progress", "stage": stage}
        if downloaded is not None:
            event["downloaded"] = downloaded
        if total and total > 0:
            event["total"] = total
            if stage in ("download", "profiles"):
                event["percent"] = min(100, round(downloaded * 100 / total, 1))
        stream.write(json.dumps(event, separators=(",", ":")) + "\n")
        stream.flush()
    return report


def profile_request(url: str) -> bytes:
    host = (urllib.parse.urlparse(url).hostname or "").lower()
    if not is_url_allowed(url) or not (host in {"github.com", "api.github.com"} or host.endswith(".githubusercontent.com")):
        raise SecurityError(f"Untrusted profile URL: {url}")
    class ProfileRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            parsed = urllib.parse.urlparse(newurl)
            host = (parsed.hostname or "").lower()
            if parsed.scheme != "https" or not (host in {"github.com", "api.github.com"} or host.endswith(".githubusercontent.com")):
                raise SecurityError("Untrusted profile redirect")
            return super().redirect_request(req, fp, code, msg, headers, newurl)
    with urllib.request.build_opener(ProfileRedirect).open(make_request(url), timeout=60) as response:
        return response.read()


def install_profiles(family, version, vendors=("BBL",), ref=None, package=None, data_home=None, progress_callback=None):
    """Publish a complete JSON catalog; never expose partial downloads."""
    from concurrent.futures import ThreadPoolExecutor
    family = validate_family(family)
    version = validate_version(version)
    vendors = list(dict.fromkeys(validate_version(v) for v in vendors))
    root = (Path(data_home) if data_home else get_data_home()) / "slicenv/profiles" / family
    reject_symlink_components(root)
    root.mkdir(parents=True, exist_ok=True)
    target = root / version
    reject_symlink_components(target)
    with tempfile.TemporaryDirectory(prefix=".profiles-", dir=root) as scratch:
        stage = Path(scratch) / "catalog"
        stage.mkdir()
        # Preserve vendors fetched previously for this exact catalog.
        if target.exists():
            reject_symlink_components(target)
            if any(p.is_symlink() for p in target.rglob("*")):
                raise SecurityError("Symlink in existing catalog")
            shutil.copytree(target, stage, dirs_exist_ok=True)
        source = "engine-package" if package else "git-tag"
        if package:
            package = Path(package)
            for vendor in vendors:
                metadata = package / f"{vendor}.json"
                if not metadata.is_file() or not (package / vendor).is_dir():
                    raise ValueError(f"No JSON profiles for {vendor}")
                paths = [metadata, *(package / vendor).rglob("*.json")]
                if (stage / vendor).exists():
                    shutil.rmtree(stage / vendor)
                for kind in ("machine", "process", "filament"):
                    (stage / vendor / kind).mkdir(parents=True, exist_ok=True)
                for path in paths:
                    if path.is_symlink() or not path.resolve().is_relative_to(package.resolve()):
                        raise SecurityError("Profile escapes package")
                    json.loads(path.read_bytes())
                    dest = stage / path.relative_to(package)
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(path, dest)
        else:
            repo = OFFICIAL_REPOS[family]
            ref = ref or f"v{version}"
            validate_version(ref)
            tree = json.loads(profile_request(f"https://api.github.com/repos/{repo}/git/trees/{urllib.parse.quote(ref, safe='')}?recursive=1"))
            if tree.get("truncated"):
                raise ValueError("GitHub returned a truncated profile tree")
            paths = []
            for entry in tree.get("tree", []):
                name = entry["path"]
                if not name.startswith("resources/profiles/"):
                    continue
                relative = name.removeprefix("resources/profiles/")
                if not any(relative == f"{v}.json" or relative.startswith(f"{v}/") for v in vendors):
                    continue
                if entry["type"] != "blob" or not relative.endswith(".json"):
                    continue
                if entry.get("mode") == "120000" or any(p in {".", "..", ""} for p in relative.split("/")) or "\\" in relative:
                    raise SecurityError("Unsafe profile tree path")
                paths.append((name, relative, entry["sha"]))
            for vendor in vendors:
                if not any(rel == f"{vendor}.json" for _, rel, _ in paths):
                    raise ValueError(f"Vendor {vendor} missing at {ref}")
            for vendor in vendors:
                if (stage / vendor).exists():
                    shutil.rmtree(stage / vendor)
            def fetch(item):
                name, relative, sha = item
                # Pin each blob to its Git object hash; verify the Git SHA-1 too.
                payload = profile_request(f"https://raw.githubusercontent.com/{repo}/{urllib.parse.quote(ref, safe='')}/{urllib.parse.quote(name)}")
                if hashlib.sha1(f"blob {len(payload)}\0".encode() + payload).hexdigest() != sha:
                    raise ValueError(f"Profile changed during download: {name}")
                json.loads(payload)
                dest = stage / relative
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(payload)
            if progress_callback:
                progress_callback("profiles", 0, len(paths))
            with ThreadPoolExecutor(max_workers=8) as pool:
                for done, _ in enumerate(pool.map(fetch, paths), 1):
                    if progress_callback:
                        progress_callback("profiles", done, len(paths))
        all_vendors = sorted(p.stem for p in stage.glob("*.json") if p.name != "catalog.json")
        manifest = dict(family=family, version=version, source=source,
                        source_ref=str(package) if package else ref,
                        fetched_at=datetime.now(timezone.utc).isoformat(), vendors=all_vendors)
        (stage / "catalog.json").write_text(json.dumps(manifest, indent=2) + "\n")
        backup = root / f".backup-{uuid.uuid4().hex}"
        if target.exists():
            target.rename(backup)
        try:
            stage.rename(target)
        except Exception:
            if backup.exists():
                backup.rename(target)
            raise
        if backup.exists():
            shutil.rmtree(backup)
    return manifest


def parse_args(args=None):
    parser = argparse.ArgumentParser(
        prog="manage_engines.py",
        description="Install and manage portable Bambu Studio and OrcaSlicer versions.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    profiles = sub.add_parser("profiles", help="Manage independent JSON profile catalogs")
    actions = profiles.add_subparsers(dest="profile_action", required=True)
    for action in ("fetch", "import"):
        command = actions.add_parser(action)
        command.add_argument("--family", choices=["bambu", "orca"], required=True)
        command.add_argument("--ref")
        command.add_argument("--version")
        command.add_argument("--vendor", action="append")
        command.add_argument("--package", type=Path)
        command.add_argument("--data-home", type=Path)

    # list-releases
    p_releases = sub.add_parser("list-releases", help="List official Linux x86_64 releases from GitHub.")
    p_releases.add_argument("--family", choices=["bambu", "orca", "all"], default="all", help="Engine family filter.")
    p_releases.add_argument("--limit", type=int, default=10, help="Max releases per family (default 10).")
    p_releases.add_argument("--prereleases", action="store_true", help="Include alpha/beta/prerelease builds.")
    p_releases.add_argument("--channel", choices=["stable", "beta", "nightly", "all"], help="Release channel (default: stable).")
    p_releases.add_argument("--json", action="store_true", help="Output results as JSON.")

    # install
    p_install = sub.add_parser("install", help="Download, verify, and install a managed engine package.")
    p_install.add_argument("--family", choices=["bambu", "orca"], help="Engine family (bambu or orca).")
    p_install.add_argument("--version", help="Version name to assign (defaults to release version).")
    p_install.add_argument("--asset-id", type=int, help="Official GitHub asset ID to download.")
    p_install.add_argument("--asset-url", help="Official GitHub download URL.")
    p_install.add_argument("--tag", help="Release tag to install (picks first matching Linux x86_64 asset).")
    p_install.add_argument("--file", help="Install from a local AppImage/archive instead of downloading.")
    p_install.add_argument("--expected-sha256", help="Required or verified SHA-256 digest.")
    p_install.add_argument("--profile-vendor", action="append", help="Additional selected printer vendor")
    p_install.add_argument("--force", action="store_true", help="Overwrite existing managed version.")
    p_install.add_argument("--data-dir", help="Override engines root directory (defaults to XDG_DATA_HOME).")
    p_install.add_argument("--config-dir", help="Override engines.json config directory (defaults to XDG_CONFIG_HOME).")
    p_install.add_argument("--no-register", action="store_true", help="Do not register in engines.json installed list.")
    p_install.add_argument("--json", action="store_true", help="Output result as JSON.")
    p_install.add_argument("--progress-json", action="store_true", help="Write throttled progress NDJSON events to stderr.")

    # list
    p_list = sub.add_parser("list", help="List locally installed managed engine versions.")
    p_list.add_argument("--family", choices=["bambu", "orca", "all"], default="all", help="Engine family filter.")
    p_list.add_argument("--data-dir", help="Override engines root directory.")
    p_list.add_argument("--json", action="store_true", help="Output results as JSON.")

    # remove
    p_remove = sub.add_parser("remove", help="Safely remove an installed managed engine version.")
    p_remove.add_argument("target", nargs="?", help="Engine target format '<family> <version>' or '<family>/<version>'.")
    p_remove.add_argument("--family", choices=["bambu", "orca"], help="Engine family.")
    p_remove.add_argument("--version", help="Version to remove.")
    p_remove.add_argument("--data-dir", help="Override engines root directory.")
    p_remove.add_argument("--config-dir", help="Override engines.json config directory.")
    p_remove.add_argument("--json", action="store_true", help="Output result as JSON.")

    return parser.parse_args(args)


def main(argv=None) -> int:
    args = parse_args(argv)

    if args.command == "profiles":
        try:
            if args.profile_action == "import" and not args.package:
                raise ValueError("--package is required for import")
            if not args.version and not args.ref:
                raise ValueError("--version or --ref is required")
            result = install_profiles(args.family, args.version or clean_version_tag(args.ref),
                                      args.vendor or ["BBL"], args.ref,
                                      args.package if args.profile_action == "import" else None, args.data_home)
            print(json.dumps(result))
            return 0
        except Exception as e:
            sys.stderr.write(f"Profiles failed: {e}\n")
            return 1

    if args.command == "list-releases":
        try:
            releases = fetch_releases(
                family=args.family,
                limit=args.limit,
                include_prereleases=args.prereleases,
                channel=args.channel,
            )
        except Exception as e:
            sys.stderr.write(f"Error fetching releases: {e}\n")
            return 1

        if args.json:
            print(json.dumps(releases, indent=2, ensure_ascii=False))
            return 0

        for r in releases:
            pre = " [prerelease]" if r["prerelease"] else ""
            print(f"[{r['family']}] {r['name']} ({r['tag']}) [{r['channel']}]{pre} - {(r['published_at'] or '')[:10]}")
            for a in r["assets"]:
                size_mb = round(a["size"] / 1024 / 1024, 1)
                digest = a["sha256"] or "none"
                print(f"  * ID: {a['id']} | {a['name']} ({size_mb} MB)")
                print(f"    SHA-256: {digest}")
                print(f"    URL:     {a['download_url']}")
        return 0

    if args.command == "install":
        family = args.family
        version = args.version
        asset_url = args.asset_url
        asset_name = ""
        asset_id = args.asset_id
        expected_sha256 = args.expected_sha256
        release_metadata = {}

        # Local file installation
        if args.file:
            local_path = Path(args.file)
            asset_name = local_path.name
            if not family:
                lower = asset_name.lower()
                if "orca" in lower:
                    family = "orca"
                elif "bambu" in lower:
                    family = "bambu"
                else:
                    sys.stderr.write("Error: --family (bambu or orca) is required.\n")
                    return 1
            if not version:
                version = clean_version_tag(extract_version_from_name(asset_name))
                if not version or version == asset_name:
                    sys.stderr.write("Error: --version is required when installing from local file.\n")
                    return 1

        else:
            # Resolve from GitHub API
            try:
                resolved_fam, resolved_ver, resolved_name, resolved_url, resolved_digest, resolved_id = (
                    resolve_release_asset_info(
                        family=family,
                        asset_id=asset_id,
                        asset_url=asset_url,
                        tag=args.tag,
                    )
                )
                if not family:
                    family = resolved_fam
                if not version:
                    version = resolved_ver
                asset_name = resolved_name
                asset_url = resolved_url
                asset_id = resolved_id
                if not expected_sha256 and resolved_digest:
                    expected_sha256 = resolved_digest
                if args.tag or asset_id:
                    release_metadata = fetch_release_metadata(family, args.tag, asset_id)
                    release_channel = classify_release_channel(release_metadata) if release_metadata else None
                    if release_channel == "nightly":
                        release_id = release_metadata.get("id")
                        if not release_id or not asset_id:
                            raise RuntimeError("Nightly install requires official release and asset IDs for immutable identity.")
                        version = validate_version(f"nightly-{release_id}-{asset_id}")
            except Exception as e:
                sys.stderr.write(f"Error resolving release asset: {e}\n")
                return 1

        if not expected_sha256:
            sys.stderr.write(
                "Error: No verified SHA-256 digest available from GitHub metadata.\n"
                "Provide --expected-sha256 explicitly to prevent unverified execution.\n"
            )
            return 1

        reporter = make_progress_reporter() if args.progress_json else None
        try:
            manifest = install_engine(
                family=family,
                version=version,
                asset_name=asset_name,
                asset_url=asset_url or f"file://{Path(args.file).resolve()}",
                expected_sha256=expected_sha256,
                asset_id=asset_id,
                local_file=Path(args.file) if args.file else None,
                force=args.force,
                data_dir=args.data_dir,
                config_dir=args.config_dir,
                no_register=args.no_register,
                progress_callback=reporter,
                channel=classify_release_channel(release_metadata) if release_metadata else None,
                release_tag=release_metadata.get("tag_name") if release_metadata else args.tag,
                source_url=release_metadata.get("html_url") if release_metadata else None,
            )
        except Exception as e:
            sys.stderr.write(f"Installation failed: {e}\n")
            return 1

        profile_error = None
        try:
            package = next((Path(p) for p in manifest.profile_paths if (Path(p) / "BBL.json").is_file()), None)
            install_profiles(manifest.family, manifest.version, vendors=["BBL", *(args.profile_vendor or [])], ref=manifest.release_tag or f"v{manifest.version}", package=package, progress_callback=reporter)
        except Exception as e:
            profile_error = str(e)
            sys.stderr.write(f"Engine installed; profile catalog unavailable: {e}\n")
        if reporter:
            reporter("done", None, None)

        if args.json:
            print(json.dumps({**asdict(manifest), "profile_error": profile_error}, indent=2, ensure_ascii=False))
        else:
            print(f"Successfully installed managed {manifest.family} {manifest.version}:")
            print(f"  Executable: {manifest.executable_path}")
            print(f"  Profiles:   {', '.join(manifest.profile_paths) or 'None found'}")
            print(f"  SHA-256:    {manifest.sha256}")
            print("  Note: Active slicer application was NOT changed.")
        return 0

    if args.command == "list":
        engines = list_managed_engines(family=args.family, data_dir=args.data_dir)
        if args.json:
            print(json.dumps(engines, indent=2, ensure_ascii=False))
            return 0

        if not engines:
            print("No managed engines found in storage.")
            return 0

        print(f"{'FAMILY':<8} {'VERSION':<14} {'STATUS':<7} {'SIZE (MB)':<10} {'EXECUTABLE'}")
        print("-" * 80)
        for e in engines:
            status = "OK" if e["valid"] else "BROKEN"
            size_mb = f"{round(e['size_bytes'] / 1024 / 1024, 1):.1f}"
            print(f"{e['family']:<8} {e['version']:<14} {status:<7} {size_mb:<10} {e['executable_path']}")
        return 0

    if args.command == "remove":
        target = args.target
        family = args.family
        version = args.version
        if target:
            if "/" in target:
                family, version = target.split("/", 1)
            elif " " in target:
                family, version = target.split(" ", 1)
            elif not family and target in ("bambu", "orca"):
                family = target

        if not family or not version:
            sys.stderr.write("Error: Specify target as '<family> <version>' or via --family and --version.\n")
            return 1

        try:
            res = remove_managed_engine(
                family=family,
                version=version,
                data_dir=args.data_dir,
                config_dir=args.config_dir,
            )
        except Exception as e:
            sys.stderr.write(f"Removal failed: {e}\n")
            return 1

        if args.json:
            print(json.dumps(res, indent=2, ensure_ascii=False))
        else:
            print(f"Removed managed engine {res['family']} {res['version']} from {res['removed_directory']}")
            if res["was_active"]:
                print("Warning: The removed engine was the active slicer in engines.json. Active slicer is now cleared.")
        return 0

    return 0


if __name__ == "__main__":
    sys.exit(main())
