#!/bin/bash
# The ANGLE every korvus WebGL context draws through: the revision Chrome ships, built from ANGLE's own repository.
#
# WHY. WebGL in Chrome is ANGLE: its shader translator, its WebGL validation and its driver workarounds decide what a
# page reads back (a GPU float fingerprint, the parameter answers, readPixels). The same revision gives the
# same answers on the same GPU and driver. korvus loads `libGLESv2.so` + `libEGL.so` at run time (gpu::device):
# `KORVUS_ANGLE_DIR`, else beside the korvus executable (a release ships them there), else the build this script
# makes for the Chrome release the checkout pins, ~/.cache/korvus/angle-<release>.
#
# WHAT. Fetches ANGLE at the pinned revision with the dependencies its DEPS names (gclient, from depot_tools; no
# history, no Android or test-only checkouts) and builds the two libraries in release into
# ~/.cache/korvus/angle-<release>. One directory per release and no shared link: a checkout on another release (an
# update worktree) builds its own beside it, and neither loads the other's. The back end korvus uses is ANGLE's GLES one over the GPU's EGL driver; the Vulkan back end is built too
# because Chrome's is. SwiftShader is not: korvus treats a software renderer as no GPU, as Chrome 150 on Linux does.
#
#   scripts/angle-source.sh         (KORVUS_ANGLE_CACHE moves the cache; DEPOT_TOOLS names a depot_tools checkout,
#                                    else one is cloned into the cache)
#
# THE PIN comes from tools/version/chrome.toml (angle_revision), which `tools/version/next.py pins` writes for a
# Chrome release.
set -euo pipefail
PINS_FILE=$(cd "$(dirname "$0")/.." && pwd)/tools/version/chrome.toml
pin() { sed -n "s/^$1 *= *\"\(.*\)\"\$/\1/p" "$PINS_FILE"; }
CHROME=$(pin chrome)
ANGLE_REV=$(pin angle_revision)
[ -n "$CHROME" ] && [ -n "$ANGLE_REV" ] || { echo "angle-source: no pins in $PINS_FILE" >&2; exit 1; }
BASE=${KORVUS_ANGLE_CACHE:-$HOME/.cache/korvus}
SRC=$BASE/angle-src-${ANGLE_REV:0:8}
OUT=$BASE/angle-$CHROME
ARGS='is_debug=false is_component_build=false dcheck_always_on=false symbol_level=0 angle_build_tests=false
      angle_enable_swiftshader=false use_siso=false use_remoteexec=false treat_warnings_as_errors=false'
# Per OS: the prebuilt gn for the host, and the library names ANGLE emits. The back end is Chrome's
# default for each -- GLES-over-native-EGL on Linux, Metal on macOS, D3D11 on Windows -- which gn picks
# itself, so ARGS stays OS-agnostic. gclient sync fetches the host's own deps.
case "$(uname)" in
  # use_lld=false on macOS: Chrome's bundled ld64.lld can't parse a very new macOS SDK's libSystem.tbd
  # (e.g. SDK 27 on the owner's Mac -> "undefined symbol: pthread_mutex_lock" from libc++), so link with
  # Apple's own ld from the installed Xcode, which understands its SDK. Linux/Windows keep lld.
  Darwin)       GN=buildtools/mac/gn;     LIBEGL=libEGL.dylib; LIBGLES=libGLESv2.dylib; ARGS="$ARGS use_lld=false" ;;
  MINGW*|MSYS*) GN=buildtools/win/gn.exe; LIBEGL=libEGL.dll;   LIBGLES=libGLESv2.dll ;;
  *)            GN=buildtools/linux64/gn; LIBEGL=libEGL.so;    LIBGLES=libGLESv2.so ;;
esac

if [ ! -f "$OUT/.complete" ]; then
  DT=${DEPOT_TOOLS:-$BASE/depot_tools}
  [ -x "$DT/gclient" ] || git clone -q --depth 1 https://chromium.googlesource.com/chromium/tools/depot_tools.git "$DT"
  # DEPOT_TOOLS_WIN_TOOLCHAIN=0: use the machine's installed Visual Studio, not Chromium's hermetic
  # win_toolchain from the Google-internal `chrome-wintoolchain` bucket (a 401 for anyone outside Google).
  # Harmless off Windows. depot_tools runs its own Python via vpython3, so python3 need not be on PATH.
  export PATH=$DT:$PATH DEPOT_TOOLS_UPDATE=0 GCLIENT_SUPPRESS_GIT_VERSION_WARNING=1 DEPOT_TOOLS_WIN_TOOLCHAIN=0
  if [ ! -f "$SRC/.fetched" ]; then
    mkdir -p "$SRC/angle"
    git -C "$SRC/angle" init -q
    git -C "$SRC/angle" fetch -q --depth 1 https://chromium.googlesource.com/angle/angle "$ANGLE_REV"
    git -C "$SRC/angle" -c advice.detachedHead=false checkout -q FETCH_HEAD
    # ANGLE's scripts/bootstrap.py writes this .gclient with target_os android; korvus needs Linux only, and none of
    # the test suites
    cat > "$SRC/angle/.gclient" <<EOF
solutions = [{
  "name": ".", "url": "https://chromium.googlesource.com/angle/angle.git", "deps_file": "DEPS", "managed": False,
  "custom_vars": {"checkout_angle_dawn_deps": False, "checkout_angle_cl_deps": False},
  "custom_deps": {
    "third_party/VK-GL-CTS/src": None, "third_party/catapult": None, "third_party/dawn": None,
    "third_party/llvm/src": None, "third_party/jdk/current": None, "third_party/SwiftShader": None,
  },
}]
EOF
    (cd "$SRC/angle" && gclient sync --no-history --shallow -j8)
    touch "$SRC/.fetched"
  fi
  cd "$SRC/angle"
  "$GN" gen out/Korvus --args="$ARGS"
  # ANGLE's own ninja (its DEPS fetches it): depot_tools' `ninja` is a wrapper that needs a bootstrapped depot_tools
  third_party/ninja/ninja -C out/Korvus libEGL libGLESv2
  rm -rf "$OUT.tmp"
  mkdir -p "$OUT.tmp"
  install -m 755 "out/Korvus/$LIBEGL" "out/Korvus/$LIBGLES" "$OUT.tmp/"
  echo "$CHROME $ANGLE_REV" > "$OUT.tmp/.complete"
  rm -rf "$OUT"
  mv "$OUT.tmp" "$OUT"
fi
echo "angle-source: $OUT (Chrome $(cat "$OUT/.complete"))"
