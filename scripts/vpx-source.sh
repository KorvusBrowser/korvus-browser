#!/bin/bash
# libvpx (VP8 + VP9), the revision Chrome ships: `<video>` decode (the DEFAULT build, D144) + the WebRTC
# send encoder.
#
# WHY. A `<video>`'s VP9 and VP8 are decoded with libvpx's decoders, the decoders Chrome uses, so the
# frames a page gets are bit-for-bit Chrome's (YouTube serves VP9). korvus also SENDS WebRTC video as VP8,
# and the bitstream a peer (or a bot-detector decoding it) sees is determined by the encoder's source
# revision. Either way korvus builds libvpx at the exact commit Chrome pins -- the same discipline
# skia-source.sh uses for Skia. A system libvpx (pkg-config) would be a different build and is forbidden
# (J6/I6): nothing links a library the machine happened to have.
#
# WHAT. Builds a STATIC VP8+VP9 libvpx (libvpx.a + its public headers) at tools/version/chrome.toml's
# `libvpx_revision` into an untracked cache, and links third_party/libvpx-chrome at it. The vendored
# `vendor/libvpx-sys` crate links that via VPX_LIB_DIR/VPX_INCLUDE_DIR (set in .cargo/config.toml).
#
#   scripts/vpx-source.sh           (KORVUS_VPX_CACHE moves the cache)
#
# REQUIRED for the default build (D144): korvus-render links libvpx for VP9/VP8 `<video>` decode, so run
# this once per machine and once per new worktree (then it only makes the link) BEFORE `cargo build`. Needs
# a C toolchain + nasm (the box korvus builds Skia on already has clang/ninja; nasm assembles libvpx's x86
# code). The webrtc-media feature additionally uses its VP8 encoder.
#
# THE PIN comes from tools/version/chrome.toml (libvpx_revision), which tools/version/next.py pins for a
# Chrome release; nothing here names a version.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
PINS_FILE=$REPO/tools/version/chrome.toml
pin() { sed -n "s/^$1 *= *\"\(.*\)\"\$/\1/p" "$PINS_FILE"; }
CHROME=$(pin chrome)
VPX_REV=$(pin libvpx_revision)
[ -n "$CHROME" ] && [ -n "$VPX_REV" ] || { echo "vpx-source: no pins in $PINS_FILE" >&2; exit 1; }

case "$(uname)" in
  MINGW*|MSYS*)
    # TODO(windows): libvpx's MSVC/clang-cl target under MSYS2 emits vpxmd.lib (/MD); rename to libvpx.lib
    # to match the `vpx` link name, and add nasm to the runner. Windows VP8 send is its own later slice;
    # until then webrtc-media is a Linux/macOS feature and the Windows default build never links libvpx.
    echo "vpx-source: Windows libvpx (VP8 send) is deferred to a later slice; skipping." >&2
    exit 1 ;;
esac

CACHE=${KORVUS_VPX_CACHE:-$HOME/.cache/korvus/libvpx-$CHROME-${VPX_REV:0:8}}
LINK=$REPO/third_party/libvpx-chrome

# fetch <repository> <revision> <dir>: one commit, no history, into an empty <dir>.
fetch() {
  mkdir -p "$3"
  git -C "$3" init -q
  git -C "$3" fetch -q --depth 1 "$1" "$2"
  git -C "$3" -c advice.detachedHead=false checkout -q FETCH_HEAD
  [ "$(git -C "$3" rev-parse HEAD)" = "$2" ] || { echo "vpx-source: $1 gave $(git -C "$3" rev-parse HEAD), not $2" >&2; exit 1; }
}

if [ ! -f "$CACHE/.complete" ]; then
  T=$CACHE.tmp
  rm -rf "$T"
  fetch https://chromium.googlesource.com/webm/libvpx.git "$VPX_REV" "$T/src"
  rm -rf "$T/src/.git"
  # nasm assembles libvpx's x86 code (this box has nasm, not yasm); without an assembler the C-only build
  # produces the SAME bitstream, just slower, so fall back to it rather than fail.
  if command -v nasm >/dev/null 2>&1; then ASFLAG=(--as=nasm); else ASFLAG=(--disable-optimizations); fi
  (
    cd "$T/src"
    # VP8 + VP9, both codecs' encoder and decoder (the `<video>` decoders for the default build, the VP8
    # encoder for webrtc-media). Everything else off: no tools/examples/tests/docs/webm-io.
    ./configure --prefix="$T/install" \
      --enable-vp8 --enable-vp9 --enable-static --disable-shared --enable-pic \
      --disable-examples --disable-unit-tests --disable-tools --disable-docs --disable-webm-io \
      "${ASFLAG[@]}" >/dev/null
    make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)" >/dev/null
    make install >/dev/null
  )
  [ -f "$T/install/lib/libvpx.a" ] || { echo "vpx-source: build produced no libvpx.a" >&2; exit 1; }
  echo "$CHROME $VPX_REV" > "$T/.complete"
  rm -rf "$CACHE"
  mv "$T" "$CACHE"
fi
mkdir -p "$REPO/third_party"
ln -sfn "$CACHE/install" "$LINK"
echo "vpx-source: $LINK -> $CACHE/install (Chrome $(cat "$CACHE/.complete"))"
