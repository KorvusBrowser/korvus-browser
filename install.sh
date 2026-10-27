#!/bin/sh
# korvus installer for Linux and macOS: downloads this platform's korvus from the latest release, checks it against
# the release's SHA256SUMS, unpacks it under ~/.korvus, puts it on your PATH, and runs `korvus setup`.
#
#   curl -fsSL https://korvusbrowser.com/install.sh | bash
#
# (korvusbrowser.com hands out the latest release's copy of this file, which is also each release's install.sh.)
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

# What the installer says goes to stderr, one line a step. On a terminal it is coloured and the download shows a
# progress bar; in a log, a pipe, under NO_COLOR or TERM=dumb it is plain text.
if [ -t 2 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != dumb ]; then
  K_TTY=1 K_B=$(printf '\033[1m') K_D=$(printf '\033[2m') K_G=$(printf '\033[32m') K_C=$(printf '\033[36m')
  K_Y=$(printf '\033[33m') K_R=$(printf '\033[31m') K_0=$(printf '\033[0m')
else
  K_TTY='' K_B='' K_D='' K_G='' K_C='' K_Y='' K_R='' K_0=''
fi

# A step done (✓), a step under way (→), a warning (!), a failure (✗); each a label and what it says.
korvus_ok() { printf '  %s✓%s %-10s %s\n' "$K_G" "$K_0" "$1" "$2" >&2; }
korvus_step() { printf '  %s→%s %-10s %s\n' "$K_C" "$K_0" "$1" "$2" >&2; }
korvus_warn() { printf '  %s!%s %s\n' "$K_Y" "$K_0" "$*" >&2; }
korvus_say() { printf '  %s✗ %s%s\n' "$K_R" "$*" "$K_0" >&2; }

# A path as a person reads it: ~/... under the home directory.
# shellcheck disable=SC2088  # the ~ is printed for the reader, not expanded
korvus_tilde() {
  case "$1" in
    "$HOME"/*) printf '~/%s\n' "${1#"$HOME"/}" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

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

# The release the installer fetches, as the log names it: KORVUS_VERSION, the latest's tag (GitHub's /latest
# redirects to /tag/vX.Y.Z), or the address KORVUS_RELEASE_BASE names.
korvus_release_name() {
  if [ -n "${KORVUS_RELEASE_BASE:-}" ]; then
    printf '%s\n' "${KORVUS_RELEASE_BASE%/}"
  elif [ -n "${KORVUS_VERSION:-}" ]; then
    printf '%s\n' "$KORVUS_VERSION"
  else
    _u=$(curl -fsSLI -o /dev/null -w '%{url_effective}' --max-time 10 "$RELEASES_URL/latest" 2>/dev/null) || _u=
    case "$_u" in
      */tag/v*) printf '%s (the latest)\n' "${_u##*/tag/}" ;;
      *) printf 'the latest\n' ;;
    esac
  fi
}

# The size of what URL ($1) serves, as "86 MB", when the server says it; nothing otherwise.
korvus_size() {
  command -v curl >/dev/null 2>&1 || return 0
  curl -fsSLI --max-time 10 "$1" 2>/dev/null | awk 'tolower($1) == "content-length:" { n = $2 } END { if (n + 0 > 0) printf "%d MB\n", (n + 0) / 1048576 + 0.5 }'
}

# Download URL ($1) to file ($2); with a progress bar on a terminal when $3 is "progress".
korvus_download() {
  if command -v curl >/dev/null 2>&1; then
    if [ "${3:-}" = progress ] && [ -n "$K_TTY" ]; then
      curl -fL --proto '=https,file' --retry 2 --progress-bar -o "$2" "$1"
    else
      curl -fsSL --proto '=https,file' --retry 2 -o "$2" "$1"
    fi
  elif command -v wget >/dev/null 2>&1; then
    if [ "${3:-}" = progress ] && [ -n "$K_TTY" ]; then
      wget -q --show-progress -O "$2" "$1" 2>/dev/null || wget -qO "$2" "$1"
    else
      wget -qO "$2" "$1"
    fi
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

# Put DIR ($1) on the PATH through the shell's rc file, once, and say what was done.
korvus_add_path() {
  if [ "${KORVUS_NO_MODIFY_PATH:-}" = 1 ]; then
    korvus_ok PATH "left as it is (KORVUS_NO_MODIFY_PATH)"
    return 0
  fi
  # shellcheck disable=SC2016  # $HOME stays literal: the rc line expands it when the shell starts
  case "$1" in
    "$HOME"/*) _dir='$HOME/'"${1#"$HOME"/}" ;;
    *) _dir=$1 ;;
  esac
  case "$(basename "${SHELL:-/bin/sh}")" in
    fish)
      mkdir -p "$HOME/.config/fish/conf.d"
      printf 'fish_add_path %s\n' "$_dir" >"$HOME/.config/fish/conf.d/korvus.fish"
      korvus_ok PATH "added in $(korvus_tilde "$HOME/.config/fish/conf.d/korvus.fish")"
      return 0 ;;
    zsh) _rcs="${ZDOTDIR:-$HOME}/.zshrc" ;;
    bash) _rcs="$HOME/.bashrc"; [ "$(uname -s)" = Darwin ] && _rcs="$_rcs $HOME/.bash_profile" ;;
    *) _rcs="$HOME/.profile" ;;
  esac
  for _rc in $_rcs; do
    if [ -f "$_rc" ] && grep -q '# korvus$' "$_rc"; then
      korvus_ok PATH "already in $(korvus_tilde "$_rc")"
      continue
    fi
    # shellcheck disable=SC2016  # $PATH stays literal: the rc line expands it when the shell starts
    printf 'export PATH="%s:$PATH"  # korvus\n' "$_dir" >>"$_rc"
    korvus_ok PATH "added to $(korvus_tilde "$_rc")"
  done
}

# The version the korvus binary $1 says it is: `korvus --version` (152.0.2 on), or the first line of the banner older
# releases print for it; KORVUS_VERSION's when it says neither.
korvus_version_of() {
  _v=$("$1" --version 2>/dev/null </dev/null | awk '$1 == "korvus" && $2 ~ /^[0-9]+[.][0-9]+[.][0-9]+$/ { print $2; exit }')
  [ -n "$_v" ] || _v=${KORVUS_VERSION#v}
  printf '%s\n' "$_v"
}

# Whether `korvus setup` may ask its two questions on the terminal.
korvus_can_prompt() {
  [ -z "${CI:-}" ] && [ -z "${KORVUS_NO_PROMPT:-}" ] && [ ! -e /.dockerenv ] && (: </dev/tty) 2>/dev/null
}

korvus_install() {
  set -eu
  printf '\n%skorvus%s installer\n\n' "$K_B" "$K_0" >&2
  platform=$(korvus_platform)
  if [ "${platform%-*}" = linux ] && command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
    korvus_say "needs glibc (Alpine and other musl systems are not supported)"
    exit 1
  fi
  korvus_ok Platform "$platform"
  home=${KORVUS_HOME:-$HOME/.korvus}
  asset=korvus-$platform.tar.gz
  base=$(korvus_base)
  korvus_ok Release "$(korvus_release_name)"
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  korvus_download "$base/SHA256SUMS" "$work/SHA256SUMS" || { korvus_say "no release at $base"; exit 1; }
  size=$(korvus_size "$base/$asset")
  korvus_step Download "$asset${size:+ ($size)}"
  korvus_download "$base/$asset" "$work/$asset" progress || { korvus_say "no $asset at $base"; exit 1; }
  want=$(awk -v n="$asset" '{ k = $2; sub(/^\*/, "", k); if (k == n) print $1 }' "$work/SHA256SUMS")
  if [ -z "$want" ] || [ "$want" != "$(korvus_sha256 "$work/$asset")" ]; then
    korvus_say "$asset does not match the release's SHA256SUMS"
    exit 1
  fi
  korvus_ok Verified "SHA-256 matches the release's SHA256SUMS"
  mkdir -p "$work/x"
  tar -xzf "$work/$asset" -C "$work/x"
  # The archive holds korvus at its root (152.0.0 on), or in one folder named korvus-<version>-<platform> (before).
  if [ -x "$work/x/korvus" ]; then
    src=$work/x
    version=$(korvus_version_of "$src/korvus")
  else
    top=$(ls "$work/x")
    src=$work/x/$top
    version=${top#korvus-}
    version=${version%-"$platform"}
  fi
  [ -x "$src/korvus" ] || { korvus_say "$asset holds no korvus"; exit 1; }
  [ -n "$version" ] || { korvus_say "cannot tell which version $asset is: set KORVUS_VERSION=vX.Y.Z"; exit 1; }
  mkdir -p "$home/dist"
  if [ -e "$home/dist/$version" ]; then
    korvus_ok Installed "korvus $version was already in $(korvus_tilde "$home/dist/$version")"
  else
    mv "$src" "$home/dist/.$version.new"
    mv "$home/dist/.$version.new" "$home/dist/$version"
    korvus_ok Installed "korvus $version in $(korvus_tilde "$home/dist/$version")"
  fi
  ln -sfn "dist/$version" "$home/current"
  korvus_ok Linked "$(korvus_tilde "$home/current") → $version"
  if [ "${platform%-*}" = linux ] && command -v ldd >/dev/null 2>&1; then
    missing=$(ldd "$home/current/korvus" 2>/dev/null | awk '/not found/ { print $1 }' | paste -sd' ' -)
    if [ -n "$missing" ]; then
      korvus_warn "this machine lacks libraries korvus needs: $missing"
      korvus_warn "  Debian/Ubuntu: sudo apt-get install libfontconfig1    Fedora: sudo dnf install fontconfig"
    fi
  fi
  korvus_add_path "$home/current"
  if [ -e "${KORVUS_CONFIG:-$home/config.toml}" ]; then
    korvus_ok Settings "kept ($(korvus_tilde "${KORVUS_CONFIG:-$home/config.toml}")); \`korvus setup\` changes them"
  elif korvus_can_prompt; then
    korvus_step Setup "two questions, written to $(korvus_tilde "$home/config.toml")"
    KORVUS_HOME=$home "$home/current/korvus" setup </dev/tty
  else
    KORVUS_HOME=$home "$home/current/korvus" setup --non-interactive </dev/null
  fi
  printf '\n%s%skorvus %s is ready.%s\n' "$K_B" "$K_G" "$version" "$K_0" >&2
  # shellcheck disable=SC2016  # $PATH is literal: it is the line the user pastes into their own shell
  case ":$PATH:" in
    *":$home/current:"*) ;;
    *) printf '  Restart your shell, or run:  %sexport PATH="%s:$PATH"%s\n' "$K_C" "$home/current" "$K_0" >&2 ;;
  esac
  printf '  Then:  %skorvus serve%s   %s(a DevTools endpoint on :9222)%s\n' "$K_C" "$K_0" "$K_D" "$K_0" >&2
  printf '  Later: %skorvus update%s  %s(korvus also asks when a release is out)%s\n' "$K_C" "$K_0" "$K_D" "$K_0" >&2
  printf '  Docs:  https://korvusbrowser.com/docs\n\n' >&2
}

[ "${KORVUS_INSTALL_LIB:-}" = 1 ] || korvus_install
