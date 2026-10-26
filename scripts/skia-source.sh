#!/bin/bash
# The Skia every korvus build compiles against: the revision Chrome ships, built with Chrome's defines.
#
# WHY. rust-skia's prebuilt libraries are Skia built with Skia's standalone configuration. Chrome builds the same
# Skia revision with its own defines (SKIA_GN_ARGS in .cargo/config.toml), and some of them change pixels:
# SK_ENABLE_LEGACY_SHADERCONTEXT samples an N32 image through the legacy shader context (4-bit bilinear weights -- an
# 8x1 strip drawn 3 px wide reads 207/147/98 in Chrome, 213/148/102 through the raster pipeline),
# SK_SUPPORT_LEGACY_CONIC_CHOP and SK_SUPPORT_LEGACY_ANISOTROPIC_MIPMAP_SCALE move an oval's edge and a mipmapped
# draw. Measured against stock Chrome 150: 11 canvas probes differ with the prebuilt Skia and are byte-identical
# with this one (Chrome's clang and the system clang build the same pixels, so only the SOURCE and the defines matter).
#
# WHAT. `.cargo/config.toml` points skia-bindings' offline build (SKIA_SOURCE_DIR, SKIA_GN_ARGS) at
# <checkout>/third_party/skia-chrome, which is NOT tracked: this script makes it -- a cache holding Skia at Chrome's
# skia_revision and the codec libraries Skia's build asks for at the revisions CHROME pins (not Skia's own DEPS pins:
# Chrome links other libjpeg-turbo, libwebp and wuffs revisions, and libwebp's encoder output is what a page hashes
# through toDataURL('image/webp')). Every source comes from its own repository; no Chromium checkout is read. The one
# Chrome-specific input is libpng's build configuration, kept in this repository (skia-source.d/libpng/pnglibconf.h,
# libpng's license): png.h includes "pnglibconf.h" from its own directory, so the file placed there decides which
# chunks are read and the chunk limits.
#
#   scripts/skia-source.sh          (KORVUS_SKIA_CACHE moves the cache)
#
# Run it once per machine, and once in each new worktree (then it only makes the link). The build then needs
# `clang`/`clang++` and `ninja` on PATH; the first build of each target directory compiles Skia.
#
# THE PINS come from tools/version/chrome.toml (skia_revision, skia_dep.*), which `tools/version/next.py pins`
# writes for a Chrome release; nothing in this file names a version.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
HERE=$REPO/scripts/skia-source.d
PINS_FILE=$REPO/tools/version/chrome.toml
pin() { sed -n "s/^$1 *= *\"\(.*\)\"\$/\1/p" "$PINS_FILE"; }
CHROME=$(pin chrome)
SKIA_REV=$(pin skia_revision)
# name | repository | revision | path in the repository | path under skia/third_party/externals
PINS=$(sed -n 's/^skia_dep\.\([^ =]*\) *= *"\(.*\)"$/\1 | \2/p' "$PINS_FILE" | awk '{print $1" | "$3" | "$4" | "$5" | "$6}')
[ -n "$CHROME" ] && [ -n "$SKIA_REV" ] && [ -n "$PINS" ] || { echo "skia-source: no pins in $PINS_FILE" >&2; exit 1; }
# sha256 of stdin -> hex on the first field. `sha256sum` on Linux, `shasum -a 256` on macOS (no sha256sum there).
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi; }
KEY=$(printf '%s\n%s\n%s' "$SKIA_REV" "$PINS" "$(cat "$HERE/libpng/pnglibconf.h")" | sha256 | cut -c1-8)
CACHE=${KORVUS_SKIA_CACHE:-$HOME/.cache/korvus/skia-$CHROME-$KEY}
LINK=$REPO/third_party/skia-chrome

# fetch <repository> <revision> <dir>: one commit, no history, into an empty <dir>.
fetch() {
  mkdir -p "$3"
  git -C "$3" init -q
  git -C "$3" fetch -q --depth 1 "$1" "$2"
  git -C "$3" -c advice.detachedHead=false checkout -q FETCH_HEAD
  [ "$(git -C "$3" rev-parse HEAD)" = "$2" ] || { echo "skia-source: $1 gave $(git -C "$3" rev-parse HEAD), not $2" >&2; exit 1; }
}

if [ ! -f "$CACHE/.complete" ]; then
  T=$CACHE.tmp
  rm -rf "$T"
  fetch https://skia.googlesource.com/skia.git "$SKIA_REV" "$T/skia"
  rm -rf "$T/skia/.git"
  X=$T/skia/third_party/externals
  while IFS='|' read -r name url rev from to; do
    name=$(echo $name); [ -n "$name" ] || continue
    url=$(echo $url); rev=$(echo $rev); from=$(echo $from); to=$(echo $to)
    fetch "$url" "$rev" "$T/src/$name"
    # Copy the dependency's `$from` subtree into externals, minus its .git. rsync is absent from the
    # Windows runner's git-bash, so pipe through tar, which every runner has (BSD tar on macOS, GNU on
    # Linux, git-bash's on Windows) and whose --exclude drops the .git the fetch left at the repo root.
    mkdir -p "$X/$to"
    ( cd "$T/src/$name/$from" && tar --exclude=.git -cf - . ) | ( cd "$X/$to" && tar -xf - )
  done <<< "$PINS"
  install -m 644 "$HERE/libpng/pnglibconf.h" "$X/libpng/pnglibconf.h"
  rm -rf "$T/src"
  # gn, as Skia's own checkout gets it
  (cd "$T/skia" && python3 bin/fetch-gn >/dev/null)
  echo "$CHROME $SKIA_REV" > "$T/.complete"
  rm -rf "$CACHE"
  mv "$T" "$CACHE"
fi
mkdir -p "$REPO/third_party"
case "$(uname)" in
  MINGW*|MSYS*)
    # git-bash's `ln -s` has no symlink privilege on an unelevated runner, so it deep-COPIES the tree
    # and then can't overwrite that directory on the next build. Use an NTFS junction (no admin, no
    # copy, re-points when the cache key changes); fall back to a copy if mklink is unavailable.
    rm -rf "$LINK" 2>/dev/null || true
    cmd //c mklink //J "$(cygpath -w "$LINK")" "$(cygpath -w "$CACHE/skia")" >/dev/null 2>&1 || cp -a "$CACHE/skia" "$LINK" ;;
  *)
    ln -sfn "$CACHE/skia" "$LINK" ;;
esac

# macOS libc++ build fix. `src/pdf/SkPDFTag.cpp` does `std::optional<Data>::emplace(ci, ci)` where
# `Data` is an aggregate -- C++20 parenthesized aggregate initialisation (P0960) that libstdc++ takes
# and libc++ (macOS) rejects with "no matching emplace". Brace-init is identical in effect and both
# compilers accept it. Applied on Darwin only, so the Linux cache (and its Skia build) is untouched;
# idempotent (the second run finds nothing to replace). PDF tagging is not a raster path -- no pixel moves.
if [ "$(uname)" = "Darwin" ]; then
  TAG="$CACHE/skia/src/pdf/SkPDFTag.cpp"
  [ -f "$TAG" ] && sed -i.bak 's/fData\.emplace(ci, ci);/fData = Data{ci, ci};/' "$TAG" && rm -f "$TAG.bak"
fi
echo "skia-source: $LINK -> $CACHE/skia (Chrome $(cat "$CACHE/.complete"))"
