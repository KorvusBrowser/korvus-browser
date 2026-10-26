#!/bin/sh
# korvus installer for Linux and macOS: downloads this platform's korvus from the latest release, checks it against
# the release's SHA256SUMS, unpacks it under ~/.korvus, puts it on your PATH, and runs `korvus setup`.
#
#   curl -fsSL https://github.com/KorvusBrowser/korvus-browser/releases/latest/download/install.sh | sh
#
# Run it again to update. Each release unpacks into a directory of its own (~/.korvus/dist/<version>) and
# ~/.korvus/current points at the newest, so a `korvus serve` already running keeps its own files. korvus keeps its
# configuration, license and named personas in ~/.korvus too.
#
# Environment:
#   KORVUS_HOME                install under here instead of ~/.korvus
#   KORVUS_VERSION=vX.Y.Z      a specific release instead of the latest
#   KORVUS_RELEASE_BASE=URL    the whole release URL prefix (a directory of a release's assets; file:// for a local one)
#   KORVUS_NO_MODIFY_PATH=1    do not touch any shell rc file
#   KORVUS_NO_PROMPT=1 / CI    never prompt: `korvus setup --non-interactive` (stealth, a new persona per session)

RELEASES_URL=https://github.com/KorvusBrowser/korvus-browser/releases

korvus_say() { echo "korvus: $*" >&2; }

# <os>-<arch> of this machine's build, or non-zero with a message when there is none.
korvus_platform() {
  case "$(uname -s)" in
    Linux) _os=linux ;;
    Darwin) _os=macos ;;
    *) korvus_say "no build for $(uname -s) (Windows: install.ps1)"; return 1 ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) _arch=x86_64 ;;
    aarch64 | arm64) _arch=arm64 ;;
    *) korvus_say "no build for $(uname -s) $(uname -m)"; return 1 ;;
  esac
  # Rosetta: an x86_64 shell on a Mac that is really Apple silicon
  if [ "$_os" = macos ] && [ "$_arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then
    _arch=arm64
  fi
  case "$_os-$_arch" in
    linux-x86_64 | macos-arm64) printf '%s-%s\n' "$_os" "$_arch" ;;
    *) korvus_say "no build for $_os-$_arch yet (there are linux-x86_64, macos-arm64 and windows-x86_64)"; return 1 ;;
  esac
}

korvus_base() {
  if [ -n "${KORVUS_RELEASE_BASE:-}" ]; then
    printf '%s\n' "${KORVUS_RELEASE_BASE%/}"
  elif [ -n "${KORVUS_VERSION:-}" ]; then
    printf '%s\n' "$RELEASES_URL/download/$KORVUS_VERSION"
  else
    printf '%s\n' "$RELEASES_URL/latest/download"
  fi
}

# Download URL ($1) to file ($2).
korvus_download() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https,file' --retry 2 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$2" "$1"
  else
    korvus_say "needs curl or wget"
    return 1
  fi
}

korvus_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Put DIR ($1) on the PATH through the shell's rc file, once.
korvus_add_path() {
  [ "${KORVUS_NO_MODIFY_PATH:-}" = 1 ] && return 0
  # shellcheck disable=SC2016  # $HOME stays literal: the rc line expands it when the shell starts
  case "$1" in
    "$HOME"/*) _dir='$HOME/'"${1#"$HOME"/}" ;;
    *) _dir=$1 ;;
  esac
  case "$(basename "${SHELL:-/bin/sh}")" in
    fish)
      mkdir -p "$HOME/.config/fish/conf.d"
      printf 'fish_add_path %s\n' "$_dir" >"$HOME/.config/fish/conf.d/korvus.fish"
      return 0 ;;
    zsh) _rcs="${ZDOTDIR:-$HOME}/.zshrc" ;;
    bash) _rcs="$HOME/.bashrc"; [ "$(uname -s)" = Darwin ] && _rcs="$_rcs $HOME/.bash_profile" ;;
    *) _rcs="$HOME/.profile" ;;
  esac
  for _rc in $_rcs; do
    if [ -f "$_rc" ] && grep -q '# korvus$' "$_rc"; then
      continue
    fi
    printf 'export PATH="%s:$PATH"  # korvus\n' "$_dir" >>"$_rc"
  done
}

# Whether `korvus setup` may ask its two questions on the terminal.
korvus_can_prompt() {
  [ -z "${CI:-}" ] && [ -z "${KORVUS_NO_PROMPT:-}" ] && [ ! -e /.dockerenv ] && (: </dev/tty) 2>/dev/null
}

korvus_install() {
  set -eu
  platform=$(korvus_platform)
  if [ "${platform%-*}" = linux ] && command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
    korvus_say "needs glibc (Alpine and other musl systems are not supported)"
    exit 1
  fi
  home=${KORVUS_HOME:-$HOME/.korvus}
  asset=korvus-$platform.tar.gz
  base=$(korvus_base)
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  korvus_download "$base/SHA256SUMS" "$work/SHA256SUMS" || { korvus_say "no release at $base"; exit 1; }
  korvus_download "$base/$asset" "$work/$asset" || { korvus_say "no $asset at $base"; exit 1; }
  want=$(awk -v n="$asset" '{ k = $2; sub(/^\*/, "", k); if (k == n) print $1 }' "$work/SHA256SUMS")
  if [ -z "$want" ] || [ "$want" != "$(korvus_sha256 "$work/$asset")" ]; then
    korvus_say "$asset does not match the release's SHA256SUMS"
    exit 1
  fi
  mkdir -p "$work/x"
  tar -xzf "$work/$asset" -C "$work/x"
  top=$(ls "$work/x")
  version=${top#korvus-}
  version=${version%-"$platform"}
  [ -x "$work/x/$top/korvus" ] || { korvus_say "$asset holds no korvus"; exit 1; }
  mkdir -p "$home/dist"
  if [ -e "$home/dist/$version" ]; then
    korvus_say "$version is installed already: $home/dist/$version"
  else
    mv "$work/x/$top" "$home/dist/.$version.new"
    mv "$home/dist/.$version.new" "$home/dist/$version"
  fi
  ln -sfn "dist/$version" "$home/current"
  korvus_say "installed $version: $home/current/korvus"
  if [ "${platform%-*}" = linux ] && command -v ldd >/dev/null 2>&1; then
    missing=$(ldd "$home/current/korvus" 2>/dev/null | awk '/not found/ { print $1 }' | paste -sd' ' -)
    if [ -n "$missing" ]; then
      korvus_say "this machine lacks libraries korvus needs: $missing"
      korvus_say "  Debian/Ubuntu: sudo apt-get install libfontconfig1    Fedora: sudo dnf install fontconfig"
    fi
  fi
  korvus_add_path "$home/current"
  # shellcheck disable=SC2016  # $PATH is literal: it is the line the user pastes into their own shell
  case ":$PATH:" in
    *":$home/current:"*) ;;
    *) printf 'Restart your shell, or run: export PATH="%s:$PATH"\n' "$home/current" >&2 ;;
  esac
  if [ -e "${KORVUS_CONFIG:-$home/config.toml}" ]; then
    korvus_say "keeping your settings (${KORVUS_CONFIG:-$home/config.toml}); \`korvus setup\` changes them"
  elif korvus_can_prompt; then
    KORVUS_HOME=$home "$home/current/korvus" setup </dev/tty
  else
    KORVUS_HOME=$home "$home/current/korvus" setup --non-interactive </dev/null
  fi
  echo "Next: \`korvus serve\` (a DevTools endpoint on :9222), \`korvus login\` for a plan. Docs:"
  echo "https://github.com/KorvusBrowser/korvus-browser"
}

[ "${KORVUS_INSTALL_LIB:-}" = 1 ] || korvus_install
