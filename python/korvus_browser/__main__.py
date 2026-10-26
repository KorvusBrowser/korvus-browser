"""korvus-browser — a thin launcher that fetches the matching korvus engine binary and runs it.

TEMPLATE / MVP (release infra): on first run this downloads the platform binary from this project's
GitHub Release, caches it under ~/.cache/korvus-browser/<version>/, and execs it; later runs reuse the
cache. Finalize alongside .github/workflows/release.yml: confirm the release asset names match, add
SHA256 verification against the release's SHA256SUMS, and test the Windows (.zip) path. The release
tarball is expected to contain the `korvus` binary together with its sibling libraries (libGLESv2/libEGL,
Skia), which korvus loads from beside the executable.
"""
from __future__ import annotations

import hashlib
import os
import platform
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path

VERSION = "152.0.2"                    # kept in sync with the engine release tag (v<VERSION>)
REPO = "KorvusBrowser/korvus-browser"


def _verify_sha256(data: bytes, asset: str, base: str) -> None:
    """Check `data` against the release's SHA256SUMS. Skips silently if the release has no SHA256SUMS or does
    not list this asset (older releases keep working); only a real mismatch aborts."""
    try:
        with urllib.request.urlopen(f"{base}/SHA256SUMS") as resp:  # noqa: S310 (fixed, trusted host)
            sums = resp.read().decode("utf-8", "replace")
    except Exception:  # pragma: no cover
        return
    want = next((p[0].lower() for p in (ln.split() for ln in sums.splitlines())
                 if len(p) == 2 and p[1].lstrip("*") == asset), None)
    if want is None:
        return
    got = hashlib.sha256(data).hexdigest()
    if got != want:
        raise SystemExit(f"korvus: checksum mismatch for {asset}\n  expected {want}\n  got      {got}")


def _asset() -> tuple[str, str]:
    # Asset names match the release's install.sh / install.ps1 — unversioned, so install.sh's
    # /latest/download/<name> works for any version: korvus-linux-x86_64.tar.gz, korvus-macos-arm64.tar.gz,
    # korvus-windows-x86_64.zip. The launcher downloads them from the version-specific release dir below.
    system, mach = platform.system(), platform.machine().lower()
    if system == "Linux":
        if mach not in ("x86_64", "amd64"):
            raise SystemExit(f"korvus: no Linux build for {mach!r} yet (linux-x86_64 only)")
        return "korvus-linux-x86_64.tar.gz", "korvus"
    if system == "Darwin":
        return "korvus-macos-arm64.tar.gz", "korvus"  # arm64 build; Rosetta runs it on Intel Macs
    if system == "Windows":
        return "korvus-windows-x86_64.zip", "korvus.exe"
    raise SystemExit(f"korvus: unsupported OS {system!r}")


def _ensure_binary() -> Path:
    asset, exe = _asset()
    root = Path(os.environ.get("KORVUS_CACHE") or (Path.home() / ".cache" / "korvus-browser")) / VERSION
    binary = root / exe
    if binary.exists():
        return binary
    root.mkdir(parents=True, exist_ok=True)
    base = f"https://github.com/{REPO}/releases/download/v{VERSION}"
    url = f"{base}/{asset}"
    suffix = asset[asset.rfind("."):]
    try:
        with urllib.request.urlopen(url) as resp:  # noqa: S310 (fixed, trusted host)
            data = resp.read()
    except Exception as e:  # pragma: no cover
        raise SystemExit(f"korvus: could not download {url}: {e}")
    _verify_sha256(data, asset, base)
    with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as tmp:
        tmp.write(data)
        archive = tmp.name
    try:
        if asset.endswith(".zip"):
            with zipfile.ZipFile(archive) as z:
                z.extractall(root)
        else:
            with tarfile.open(archive) as t:
                t.extractall(root)
    finally:
        os.unlink(archive)
    if not binary.exists():
        found = next(iter(root.rglob(exe)), None)
        if found is None:
            raise SystemExit(f"korvus: {exe} not found in {asset}")
        binary = found
    binary.chmod(0o755)
    return binary


def main() -> None:
    binary = _ensure_binary()
    argv = [str(binary), *sys.argv[1:]]
    if os.name == "nt":
        import subprocess
        raise SystemExit(subprocess.call(argv))
    os.execv(str(binary), argv)


if __name__ == "__main__":
    main()
