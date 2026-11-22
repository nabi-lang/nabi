#!/bin/sh
# Install a nabi toolchain into $NABI_HOME (default ~/.nabi):
# $NABI_HOME/toolchains/<version>-<triple>/, made the default, with the
# proxies $NABI_HOME/bin/{nabi,nabic,nabicc,nabils}.
#
#   curl -fsSL https://nabi.sh | sh
#   sh install.sh --archive nabi-<version>-<triple>.tar.xz   (or .tar.gz)
#   nabi-<version>-<triple>/install.sh          (from an unpacked archive)
#   sh install.sh --version 0.1.0              (that release)
#   sh install.sh --no-packages                (without the official packages)
#
# Environment: NABI_HOME, NABI_VERSION (default below), NABI_DIST (where
# releases are downloaded from), NABI_NO_MODIFY_PATH, NABI_NO_PACKAGES,
# NO_COLOR, CLICOLOR_FORCE.
#
# A download is checked against the release's SHA256SUMS. With minisign
# on PATH, SHA256SUMS.minisig must verify with the release key below
# first; without it, SHA256SUMS comes over HTTPS only, and the end says
# how to check the signature by hand. Any mismatch installs nothing.
# --key <file> (a minisign .pub file) replaces the release key, to test a
# release set signed with a throwaway key; it warns, naming the key.
#
# A release carries the official packages, so builds that use them need no
# network. --no-packages (or NABI_NO_PACKAGES=1) runs `nabi component
# remove packages` after the install.
#
# Like rustup, it writes $NABI_HOME/env (and env.fish), which put
# $NABI_HOME/bin first on PATH, sources it from the shell startup files
# (~/.profile, ~/.bashrc, ~/.bash_profile, ~/.zshenv, fish's conf.d) and
# installs the shell completions. --no-modify-path (or
# NABI_NO_MODIFY_PATH=1) touches none of them and says what to add.
#
# Output goes to stderr in the style of `nabi toolchain update`: status
# words right-aligned, coloured as nabi colours them: on a
# terminal or with CLICOLOR_FORCE, never with NO_COLOR or TERM=dumb.
# -q prints errors only, -v adds notes (URLs, paths).
#
# Everything runs from main, called on the last line, so a download cut
# short runs nothing.
set -eu

version="${NABI_VERSION:-0.1.1}"
# The release key, keys/release.pub (key id B33E6FE61BE5314C).
release_key="RWRMMeUb5m8+s0rEtzbR47dmyrMdlGyGQSvWUn/Ob5QYh0GHgs1GvJEs"
dist="${NABI_DIST:-https://github.com/nabi-lang/nabi/releases}"
home="${NABI_HOME:-$HOME/.nabi}"
archive=""
key_file=""
quiet=""
verbose=""
case ${NABI_NO_MODIFY_PATH-} in
  "" | 0) modify_path=1 ;;
  *) modify_path="" ;;
esac
case ${NABI_NO_PACKAGES-} in
  "" | 0) no_packages="" ;;
  *) no_packages=1 ;;
esac

# The hosts with a release archive (scripts/check/targets.sh checks it).
supported="aarch64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-pc-windows-msvc aarch64-pc-windows-msvc x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-unknown-freebsd"

main() {
  # stderr is the test, so `curl .. | sh` colours too.
  if [ -n "${NO_COLOR-}" ]; then
    color=""
  elif [ -n "${CLICOLOR_FORCE-}" ] && [ "$CLICOLOR_FORCE" != 0 ]; then
    color=1
  elif [ "${TERM-}" = dumb ]; then
    color=""
  elif [ -t 2 ]; then
    color=1
  else
    color=""
  fi
  sky="" red="" dim_sky="" reset=""
  if [ -n "$color" ]; then
    e=$(printf '\033')
    case ${COLORTERM-}:${TERM-} in
      truecolor:* | 24bit:*)
        sky="${e}[1;38;2;56;189;248m" red="${e}[1;38;2;255;95;95m" dim_sky="${e}[38;2;56;189;248m"
        ;;
      *:*256color*)
        sky="${e}[1;38;5;81m" red="${e}[1;38;5;203m" dim_sky="${e}[38;5;81m"
        ;;
      *)
        sky="${e}[1;96m" red="${e}[1;91m" dim_sky="${e}[96m"
        ;;
    esac
    reset="${e}[0m"
  fi

  say() { printf '%s\n' "$*"; }
  # A status line: $1 right-aligned, then $2.
  status() { [ -n "$quiet" ] || printf '%s%12s%s %s\n' "$sky" "$1" "$reset" "$2" >&2; }
  note() { [ -z "$verbose" ] || printf '%s%12s:%s %s\n' "$dim_sky" note "$reset" "$*" >&2; }
  info() { [ -n "$quiet" ] || printf '%s\n' "$*" >&2; }
  err() { printf 'install.sh: %serror:%s %s\n' "$red" "$reset" "$*" >&2; }
  die() {
    err "$*"
    exit 1
  }
  usage() {
    say "usage: install.sh [-q | --quiet] [-v | --verbose] [--no-modify-path] [--no-packages] [--archive <nabi-*.tar.xz | nabi-*.tar.gz>] [--version <version>] [--key <minisign .pub file>]"
    say "installs into \$NABI_HOME (now $home)"
  }

  while [ $# -gt 0 ]; do
    case $1 in
      --archive)
        [ $# -ge 2 ] || {
          usage >&2
          exit 2
        }
        archive=$2
        shift 2
        ;;
      --key)
        [ $# -ge 2 ] || {
          usage >&2
          exit 2
        }
        key_file=$2
        shift 2
        ;;
      --version)
        [ $# -ge 2 ] || {
          usage >&2
          exit 2
        }
        version=$2
        shift 2
        ;;
      -q | --quiet)
        quiet=1
        shift
        ;;
      -v | --verbose)
        verbose=1
        shift
        ;;
      --no-modify-path)
        modify_path=""
        shift
        ;;
      --no-packages)
        no_packages=1
        shift
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      *)
        usage >&2
        exit 2
        ;;
    esac
  done

  # The glibc version (2.31), or nothing on another C library.
  glibc_version() {
    v=$(getconf GNU_LIBC_VERSION 2> /dev/null | awk '{ print $2 }') || v=""
    [ -n "$v" ] || v=$(ldd --version 2> /dev/null | awk 'NR == 1 && /GLIBC|GNU libc/ { print $NF }') || v=""
    printf '%s' "$v"
  }

  # Whether glibc version $1 is older than 2.34, the oldest that programs
  # linked for a -gnu target run on.
  glibc_old() {
    echo "$1" | awk -F. '{ exit !($1 < 2 || ($1 == 2 && $2 < 34)) }'
  }

  host_triple() {
    arch=$(uname -m)
    case $arch in
      arm64 | aarch64) arch=aarch64 ;;
      x86_64 | amd64) arch=x86_64 ;;
      *) die "no nabi toolchain for the $arch architecture; build nabi from source: https://github.com/nabi-lang/nabi#building-from-source" ;;
    esac
    case $(uname -s) in
      Darwin)
        # An x86_64 shell under Rosetta on Apple silicon.
        if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2> /dev/null)" = 1 ]; then
          arch=aarch64
        fi
        say "$arch-apple-darwin"
        ;;
      Linux)
        # A musl system has its loader, /lib/ld-musl-<arch>.so.1.
        if ls /lib/ld-musl-* > /dev/null 2>&1; then
          say "$arch-unknown-linux-musl"
          return
        fi
        g=$(glibc_version)
        if [ -n "$g" ] && glibc_old "$g"; then
          # The static musl toolchain runs anywhere, and so do its programs.
          info "glibc $g is older than 2.34, which programs built for $arch-unknown-linux-gnu need;"
          info "installing the static $arch-unknown-linux-musl toolchain instead (its programs run on any Linux)."
          say "$arch-unknown-linux-musl"
        else
          say "$arch-unknown-linux-gnu"
        fi
        ;;
      FreeBSD)
        [ "$arch" = x86_64 ] || die "no nabi toolchain for FreeBSD on $arch; build nabi from source: https://github.com/nabi-lang/nabi#building-from-source"
        say "$arch-unknown-freebsd"
        ;;
      MINGW* | MSYS* | CYGWIN*) die "on Windows use install.ps1 (PowerShell)" ;;
      *) die "no nabi toolchain for $(uname -s)" ;;
    esac
  }

  # Check archive $1 against the SHA-256 in $2 (a release's .sha256).
  check_sum() {
    want=$(awk '{ print tolower($1); exit }' "$2")
    if command -v sha256sum > /dev/null 2>&1; then
      got=$(sha256sum "$1" | awk '{ print $1 }')
    elif command -v shasum > /dev/null 2>&1; then
      got=$(shasum -a 256 "$1" | awk '{ print $1 }')
    else
      die "sha256sum or shasum is needed to check $1"
    fi
    [ "$got" = "$want" ] || die "$1 has SHA-256 $got, but its .sha256 says $want (a damaged or altered download)"
  }

  # The key id of minisign public key $1 (base64 "Ed" || id, little
  # endian || key), as minisign prints it.
  key_id() {
    printf '%s\n' "$1" | awk '{
      a = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
      n = 0
      for (i = 1; i <= 16; i += 4) {
        v = 0
        for (j = 0; j < 4; j++) v = v * 64 + index(a, substr($1, i + j, 1)) - 1
        b[n++] = int(v / 65536); b[n++] = int(v / 256) % 256; b[n++] = v % 256
      }
      if (b[0] != 69 || b[1] != 100) { print "(not a minisign key)"; exit }
      for (i = 9; i >= 2; i--) printf "%02X", b[i]
      print ""
    }'
  }

  # Check archive $1, release file $2, against the release's signed
  # SHA256SUMS in $tmp; signed=1 once minisign verified it.
  check_signed() {
    key=$release_key
    if [ -n "$key_file" ]; then
      [ -f "$key_file" ] || die "--key $key_file does not exist; give a minisign public key file, or leave out --key"
      key=$(awk 'NF && !/^untrusted comment:/ { k = $1 } END { print k }' "$key_file")
      printf 'install.sh: %swarning:%s trusting key %s from %s instead of the nabi release key B33E6FE61BE5314C\n' \
        "$red" "$reset" "$(key_id "$key")" "$key_file" >&2
    fi
    signed=""
    if command -v minisign > /dev/null 2>&1; then
      minisign -Vq -P "$key" -m "$tmp/SHA256SUMS" -x "$tmp/SHA256SUMS.minisig" > /dev/null 2>&1 ||
        die "SHA256SUMS.minisig does not verify with ${key_file:-the nabi release key}: the download is damaged or was altered, so nothing was installed; try again, and report it at https://github.com/nabi-lang/nabi/security if it persists"
      signed=1
      note "SHA256SUMS.minisig verifies with ${key_file:-the release key}"
    fi
    want=$(awk -v n="$2" '$2 == n || $2 == "*" n { print tolower($1); exit }' "$tmp/SHA256SUMS")
    [ -n "$want" ] || die "the release's SHA256SUMS lists no $2, so it cannot be checked; report it at https://github.com/nabi-lang/nabi/issues"
    printf '%s  %s\n' "$want" "$2" > "$1.sha256"
  }

  # https only, also after a redirect; plain http only for a NABI_DIST
  # that asks for it (a local test server).
  case $dist in
    http://*) proto='=http,https' ;;
    *) proto='=https' ;;
  esac

  # Download $1 to $2; fail with 1 on a 404, die on anything else.
  download() {
    command -v curl > /dev/null 2>&1 || die "curl is needed to download nabi; install it (e.g. apt install curl), or download the archive yourself and run: sh install.sh --archive <file>"
    code=$(curl -sSL --proto "$proto" --proto-redir "$proto" --tlsv1.2 -o "$2" -w '%{http_code}' "$1") ||
      die "cannot download $1 (no network, a proxy, or the host is unreachable?)"
    case $code in
      2??) ;;
      404) return 1 ;;
      *) die "cannot download $1: HTTP $code" ;;
    esac
  }

  # The archive for $triple is missing: no such host, or no such version.
  no_archive() {
    case " $supported " in
      *" $triple "*)
        die "nabi $version has no release archive at $url; check the version (--version, NABI_VERSION)"
        ;;
    esac
    err "there is no nabi $version release archive for $triple"
    printf '  release archives are built for: %s\n' "$supported" >&2
    printf '  on this host, build nabi from source: see "Building from source" in\n' >&2
    printf '  https://github.com/nabi-lang/nabi#building-from-source\n' >&2
    exit 1
  }

  tmp=$(mktemp -d "${TMPDIR:-/tmp}/nabi-install.XXXXXX") || die "cannot make a temporary directory in ${TMPDIR:-/tmp}; set TMPDIR to a writable directory"
  trap 'rm -rf "$tmp"' EXIT INT TERM

  # An unpacked toolchain around this script installs itself.
  here=$(cd "$(dirname "$0")" 2> /dev/null && pwd) || here=""
  if [ -z "$archive" ] && [ -n "$here" ] && [ -x "$here/bin/nabi" ] && [ -x "$here/bin/nabic" ]; then
    src=$here
  else
    if [ -z "$archive" ]; then
      triple=$(host_triple)
      case $version in
        [0-9]*.*.*) ;;
        *) die "--version takes a full version like 0.1.0, not $version" ;;
      esac
      base="$dist/download/v$version/nabi-$version-$triple"
      status Downloading "nabi $version ($triple)"
      # The .tar.xz is about half the .tar.gz; tar needs xz for it on Linux
      # (macOS's tar reads it itself). A release without one gets the .tar.gz.
      url=""
      if [ "$(uname -s)" = Darwin ] || command -v xz > /dev/null 2>&1; then
        url="$base.tar.xz"
        archive="$tmp/nabi.tar.xz"
        download "$url" "$archive" || url=""
      fi
      if [ -z "$url" ]; then
        url="$base.tar.gz"
        archive="$tmp/nabi.tar.gz"
        download "$url" "$archive" || no_archive
      fi
      note "from $url"
      rel="$dist/download/v$version"
      for f in SHA256SUMS SHA256SUMS.minisig; do
        download "$rel/$f" "$tmp/$f" || die "$rel/$f is missing (HTTP 404), so the archive cannot be checked; report it at https://github.com/nabi-lang/nabi/issues"
      done
      check_signed "$archive" "${url##*/}"
    fi
    [ -f "$archive" ] || die "$archive does not exist; give the path of a nabi-<version>-<triple>.tar.xz (or .tar.gz) to --archive"
    # A download is checked against its release's .sha256, an archive
    # file against the one beside it, if any.
    if [ -f "$archive.sha256" ]; then
      check_sum "$archive" "$archive.sha256"
      note "SHA-256 matches $archive.sha256"
    fi
    mkdir "$tmp/files"
    case $archive in
      *.tar.xz | *.txz) tar -xJf "$archive" -C "$tmp/files" || die "cannot unpack $archive (tar needs xz for a .tar.xz: install xz, or use the .tar.gz)" ;;
      *) tar -xzf "$archive" -C "$tmp/files" || die "cannot unpack $archive" ;;
    esac
    src=$(find "$tmp/files" -mindepth 2 -maxdepth 2 -name bin -type d | head -n 1)
    [ -n "$src" ] || die "$archive is not a nabi toolchain (no bin/)"
    src=$(dirname "$src")
  fi

  full=$("$src/bin/nabic" --version | awk 'NR == 1 { print $2 }') || die "cannot run $src/bin/nabic (an archive for another OS or CPU?)"
  case $(uname -s):$full in
    Linux:*-gnu)
      g=$(glibc_version)
      if [ -n "$g" ] && glibc_old "$g"; then
        err "glibc $g is older than 2.34: programs this -gnu toolchain builds will not run here"
        info "  install the -musl archive instead (its programs are static), or build for --target <arch>-unknown-linux-musl"
      fi
      ;;
  esac
  # The toolchain's own nabi installs it (and checks it).
  NABI_HOME=$home "$src/bin/nabi" -q toolchain install "$full" --from "$src"
  NABI_HOME=$home "$src/bin/nabi" -q toolchain default "$full"
  "$home/bin/nabi" --version > /dev/null || die "the installed proxy $home/bin/nabi does not run"

  # $1 as ~/.. when under $HOME, for display only.
  shown_path() {
    case ${HOME:+$1} in
      "${HOME-}"/*) printf '~%s' "${1#"$HOME"}" ;;
      *) printf '%s' "$1" ;;
    esac
  }
  # The installed toolchain's own copy, whatever a manifest here pins.
  if [ -n "$no_packages" ] && [ -d "$src/lib/nabi/packages" ]; then
    NABI_HOME=$home NABI_TOOLCHAIN=$full "$home/bin/nabi" -q component remove packages ||
      err "nabi is installed with the official packages; \`nabi component remove packages\` removes them"
  fi
  shown=$(shown_path "$home")
  status Installed "nabi $full to $shown"
  note "toolchain in $home/toolchains, proxies in $home/bin"

  # $1 as "$HOME/.." when under $HOME, for the shell files below.
  sh_path() {
    # shellcheck disable=SC2016 # $HOME is for the shell that reads it
    case ${HOME:+$1} in
      "${HOME-}"/*) printf '$HOME%s' "${1#"$HOME"}" ;;
      *) printf '%s' "$1" ;;
    esac
  }

  changed=""
  mark_changed() {
    changed="${changed:+$changed, }$(shown_path "$1")"
  }

  # Append $line to startup file $1 unless it is there already.
  add_line() {
    if [ -f "$1" ] && grep -qxF "$line" "$1"; then
      return 0
    fi
    mkdir -p "$(dirname "$1")"
    if [ -s "$1" ] && [ -n "$(tail -c 1 "$1")" ]; then
      printf '\n' >> "$1"
    fi
    printf '%s\n' "$line" >> "$1"
    mark_changed "$1"
  }

  # Write $2 to file $1 if it differs.
  put_file() {
    if [ -f "$1" ] && [ "$(cat "$1")" = "$2" ]; then
      return 0
    fi
    mkdir -p "$(dirname "$1")"
    printf '%s\n' "$2" > "$1"
    mark_changed "$1"
  }

  # $1's completion script for shell $2 into file $3; a failed
  # `completions` leaves it alone.
  completion() {
    mkdir -p "$(dirname "$3")"
    if "$home/bin/$1" completions "$2" > "$3.new" 2> /dev/null; then
      mv "$3.new" "$3"
      note "$2 completions for $1 in $3"
    else
      rm -f "$3.new"
    fi
  }

  setup_shells() {
    env=$(sh_path "$home")
    cat > "$home/env" << EOF
#!/bin/sh
# nabi shell setup: $env/bin first on PATH, once; zsh also
# finds the completions in $env/zsh. Written by install.sh.
case ":\${PATH}:" in
  *:"$env/bin":*) ;;
  *) export PATH="$env/bin:\$PATH" ;;
esac
if [ -n "\${ZSH_VERSION-}" ]; then
  eval 'fpath=("$env/zsh" \${fpath:#"$env/zsh"})'
fi
EOF
    cat > "$home/env.fish" << EOF
# nabi shell setup for fish: $env/bin first on PATH, once.
# Written by install.sh.
if not contains -- "$env/bin" \$PATH
    set -gx PATH "$env/bin" \$PATH
end
EOF
    # Guarded: with $NABI_HOME gone the shell still starts.
    line="[ -f \"$env/env\" ] && . \"$env/env\""
    zdot=${ZDOTDIR:-$HOME}
    # Unset in containers and some CI runners.
    login=${SHELL-}
    login=${login##*/}
    for f in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.bash_profile" "$zdot/.zshenv"; do
      if [ -f "$f" ]; then
        add_line "$f"
      fi
    done
    # The login shell's own file, made if missing. A new ~/.bash_profile
    # would hide ~/.profile, so bash gets ~/.profile instead.
    case $login in
      bash)
        add_line "$HOME/.bashrc"
        if [ ! -f "$HOME/.bash_profile" ] && [ ! -f "$HOME/.bash_login" ]; then
          add_line "$HOME/.profile"
        fi
        ;;
      zsh) add_line "$zdot/.zshenv" ;;
      fish) ;;
      *) add_line "$HOME/.profile" ;;
    esac
    fish_dir=${XDG_CONFIG_HOME:-$HOME/.config}/fish
    fish=""
    if [ "$login" = fish ] || [ -d "$fish_dir" ]; then
      fish=1
      put_file "$fish_dir/conf.d/nabi.fish" "test -f \"$env/env.fish\"; and source \"$env/env.fish\""
    fi
    bash_dir=${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions
    completion nabi bash "$bash_dir/nabi"
    completion nabi zsh "$home/zsh/_nabi"
    if [ -n "$fish" ]; then
      completion nabi fish "$fish_dir/completions/nabi.fish"
    fi
  }

  info ""
  if [ -n "$modify_path" ]; then
    setup_shells
    if [ -n "$changed" ]; then
      status Modified "$changed (PATH)"
      info ""
    fi
    case ":${PATH-}:" in
      *":$home/bin:"*) ;;
      *)
        info "A new shell finds nabi on PATH; to use it in this one, run:"
        info "  . \"$env/env\"            (sh, bash, zsh)"
        info "  source \"$env/env.fish\"  (fish)"
        info ""
        ;;
    esac
  else
    case ":${PATH-}:" in
      *":$home/bin:"*) ;;
      *)
        info "Add $shown/bin to PATH (then open a new shell):"
        info "  bash / zsh:  echo 'export PATH=\"$home/bin:\$PATH\"' >> ~/.zshrc   (or ~/.bashrc)"
        info "  fish:        fish_add_path $home/bin"
        info ""
        ;;
    esac
  fi
  # nabic has its own backend and linker, and on Linux its own C library
  # files (lib/nabi/targets); on macOS the link needs the SDK of Xcode's
  # command line tools.
  if [ "$(uname -s)" = Darwin ] && ! xcrun --show-sdk-path > /dev/null 2>&1; then
    info "nabi needs the macOS SDK to link programs: xcode-select --install"
    info ""
  fi
  # Downloaded, but the signature was not checked here: how to.
  if [ -n "${rel-}" ] && [ -z "${signed-}" ]; then
    info "SHA256SUMS was checked over HTTPS; its signature was not (no minisign on PATH)."
    info "To check it, download SHA256SUMS and SHA256SUMS.minisig from $rel/, then:"
    info "  minisign -Vm SHA256SUMS -P $release_key"
    info "  gh attestation verify ${url##*/} --repo nabi-lang/nabi"
    info ""
  fi
  info "Next steps:"
  info "  nabi --version"
  info "  nabi new hello && cd hello && nabi run"
}

main "$@"
