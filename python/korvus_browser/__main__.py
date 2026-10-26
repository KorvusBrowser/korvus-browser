"""korvus-browser — a thin launcher that fetches the matching korvus engine binary and runs it.

TEMPLATE / MVP (release infra): on first run this downloads the platform binary from this project's
GitHub Release, caches it under ~/.cache/korvus-browser/<version>/, and execs it; later runs reuse the
cache. Finalize alongside .github/workflows/release.yml: confirm the release asset names match, add
SHA256 verification against the release's SHA256SUMS, and test the Windows (.zip) path. The release
tarball is expected to contain the `korvus` binary together with its sibling libraries (libGLESv2/libEGL,
Skia), which korvus loads from beside the executable.
"""
from __future__ import annotations

import os
import platform
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path

VERSION = "152.0.0"                    # kept in sync with the engine release tag (v<VERSION>)
REPO = "KorvusBrowser/korvus-browser"


def _asset() -> tuple[str, str]:
    system, mach = platform.system(), platform.machine().lower()
    arch = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(mach)
    if arch is None:
        raise SystemExit(f"korvus: unsupported CPU architecture {mach!r}")
    if system == "Linux":
        return f"korvus-v{VERSION}-linux-{arch}.tar.gz", "korvus"
    if system == "Darwin":
        return f"korvus-v{VERSION}-macos-{arch}.tar.gz", "korvus"
    if system == "Windows":
        return f"korvus-v{VERSION}-windows-{arch}.zip", "korvus.exe"
    raise SystemExit(f"korvus: unsupported OS {system!r}")


def _ensure_binary() -> Path:
    asset, exe = _asset()
    root = Path(os.environ.get("KORVUS_CACHE") or (Path.home() / ".cache" / "korvus-browser")) / VERSION
    binary = root / exe
    if binary.exists():
        return binary
    root.mkdir(parents=True, exist_ok=True)
    url = f"https://github.com/{REPO}/releases/download/v{VERSION}/{asset}"
    suffix = asset[asset.rfind("."):]
    try:
        with urllib.request.urlopen(url) as resp:  # noqa: S310 (fixed, trusted host)
            data = resp.read()
    except Exception as e:  # pragma: no cover
        raise SystemExit(f"korvus: could not download {url}: {e}")
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
