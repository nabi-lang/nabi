#!/bin/sh
# Builds what nabic links for a --target into
# <cargo target dir>/lib/nabi/targets/<triple>/.
#
#   targets/build.sh [<triple>...]
#
# Default: every shipped target the host can build (Apple ones only on
# macOS, and not the host's: its runtime is lib/libnabir.a). The targets
# in progress (riscv64gc, powerpc64le, s390x, riscv32imac,
# aarch64-linux-android, aarch64-apple-ios) are not shipped: name them to
# build their files, and build with NABI_IN_PROGRESS_TARGETS=1.
#
# Per target:
#   libnabir.a (Windows: nabir.lib)  the runtime, localized by
#       runtime/nabir/localize.sh.
#   -musl   musl's crt objects and libc.a from the pinned Rust toolchain,
#       plus musl's empty libm.a, libpthread.a, ...
#   -gnu    crt objects, libc_nonshared.a, the empty libpthread.a etc. and
#       stub libc.so / libm.so made by stubs.py from glibc 2.36
#       (Debian 12; riscv64 from debian-ports; packages SHA-256 pinned).
#       Executables need glibc >= 2.34 (32-bit runtimes use time64): the
#       stubs leave out newer symbol versions.
#   -freebsd  crt objects, libc_nonshared.a and libc / libm / libthr
#       (as lib<name>.so) from FreeBSD 14.4's base.txz (SHA-256 pinned).
#       Executables need FreeBSD >= 14.
#   -android  nothing more: the crt objects and bionic's stub libraries
#       come from the user's NDK (nabic_target's android.rs).
#   -apple-ios*  nothing more: Xcode's SDK has libSystem's stubs. The
#       simulator's also simrun.sh, its runner (xcrun simctl spawn).
#   -none-elf  nothing more: no C library. The runtime brings `_start`,
#       `malloc` and weak board functions (runtime/nabir_common/
#       src/sys/none.rs); compiler_builtins in it the soft-float and 64-bit
#       division helpers.
#   wasm32  wasi-libc's crt objects and libc.a from the pinned Rust
#       toolchain (wasm32-wasip2, plus the pinned wasi_snapshot_preview1
#       adapters; wasip1-threads; wasip1 for the browser, plus nabi.mjs,
#       its JS host).
#   sysroot/  the C library's headers `nabicc` compiles with: glibc's and
#       Linux's from the same Debian packages (-gnu), wasi-libc's from the
#       pinned wasi-sdk sysroot (wasm32).
#
# Downloads go to NABI_CACHE (default ~/.cache/nabi; %LOCALAPPDATA%\nabi
# on Windows). Needs rustup (llvm-tools: llvm-ar, llvm-objcopy, rust-lld
# for localize.sh), curl, python3 and tar with xz; a macOS host also cc
# (localize.sh's Mach-O link). Windows images name python3 python.
set -eu

python=$(command -v python3 || command -v python || echo python3)

TARGETS="aarch64-apple-darwin aarch64-apple-ios-sim x86_64-unknown-linux-gnu
aarch64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-unknown-linux-musl
x86_64-pc-windows-msvc aarch64-pc-windows-msvc i686-unknown-linux-gnu
i686-pc-windows-msvc armv7-unknown-linux-gnueabihf armv7-unknown-linux-musleabihf
wasm32-wasip2 wasm32-wasip1-threads wasm32-unknown-unknown x86_64-linux-android
x86_64-unknown-freebsd"
GLIBC=2.36-9+deb12u14
GLIBC_URL=https://snapshot.debian.org/archive/debian/20260504T025922Z/pool/main/g/glibc
LINUX=6.1.159-1
WASI_SYSROOT=25.0
WASI_SYSROOT_SHA256=d09c62c18efcddffe4b2fdd8c5830109cc8e36130cdbc9acdc0bd1b204c942bb
LINUX_URL=https://snapshot.debian.org/archive/debian/20260504T025922Z/pool/main/l/linux
# <package>_<arch> <sha256>
DEB_SHA256="
libc6_amd64 ba4f88f73dbc3ae9055f3c20f4523bfdbaf1ad13ff95e258924f77d20b4fbedf
libc6_arm64 01f4330719fd4f65580e16ea5a0527f372fca750e8f588d26deaf09f2d3b1cf4
libc6-dev_amd64 0218fc2befcd784c1b0c6292c0a137ce89fad054efaa579ad083bee0f2c01aae
libc6-dev_arm64 8bc395bf2ee9939f1a6ae395f974cffdcf541d98f99adfe89ce7e4bef36d159e
libc6_i386 76b12e06be66ec3fc2c1791d7d8cf34c2b828ed210e3913feeeb8edc9688f820
libc6-dev_i386 c02c0adacabf53b8f34364940c7c4fe5aedaf921765e915742cc31ffc7a9d1ad
libc6_armhf 758c68b92654747025b48476a45c315bf16df1321224abdd9be672bfd120be45
libc6-dev_armhf ca9486dc7f2dfb25c61c0c93d561c01f8abdf9866bd4c157f0ccd518295bbbb7
linux-libc-dev_amd64 117349f98e0d9c4522bf1b8f33c6239ab9c4c5904c56f6379be3eb790a97e7dc
linux-libc-dev_arm64 6da0f62a7a63524364bc0f4da96d11181cc15010051ff2f4c0dbd4a0ca5e085c
linux-libc-dev_i386 72e870c8b56344008990a7939edd44f71ae728152b5473c0fcc95a191a04d05f
linux-libc-dev_armhf 536d83afebd9a89e35dd104a4f3529b34833fd5fe279c4df18f2c6e9174f0e47
libc6_ppc64el b3fbe86d5b7cf02d8653b34a0753c6b5bc89dd962ef843c355cdd154ea87baff
libc6-dev_ppc64el b3d02b91ed9548cd527c3e56d05e25ef8593a77342f5e8c4969f79e12d7146be
linux-libc-dev_ppc64el a924243de6e6435ae84602014b12dcae0dbda316493ca2ee71cfe38111a9f125
libc6_s390x bbca8c91bad7f799114fe865233af0018f58b903da11c1c58a7f2d2484103d45
libc6-dev_s390x 6f128e522bcd58281fcdb1c57952549ef818ebf22375ca2608126c01b3313607
linux-libc-dev_s390x 58b30af51f05010573b621bc14acc40dcf59a1d1c486d8f858afecdc4594b6f2
libc6_riscv64 fd7b16d34b67f9ca97dd048636c229fa0a0e2dbcc927ade3731885f33284910f
libc6-dev_riscv64 293cde3b863a8502d461aa232ff5e28fb1821384e7380b855e9f892d05b5e2c3
linux-libc-dev_riscv64 52234ad2798c2e58f0aebadf925d4c51d07fbf525e7721a36780c12abd4d250b"
FREEBSD=14.4-RELEASE
FREEBSD_URL=https://download.freebsd.org/releases/amd64/$FREEBSD
FREEBSD_BASE_SHA256=769f60a6eea2938ad6b7943cfbcc17dfaf7d1f7ba59a32b869a00349302df853
# riscv64, not in Debian 12: debian-ports' glibc 2.36-9 (bookworm's base
# release) and linux 6.1.20, from one snapshot.
PORTS_GLIBC=2.36-9
PORTS_LINUX=6.1.20-2
PORTS_URL=https://snapshot.debian.org/archive/debian-ports/20230414T140955Z/pool-riscv64/main

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.."
case $(uname -s) in
  # MSYS paths: GNU tar takes `C:/..` for a remote host.
  MINGW* | MSYS* | CYGWIN*)
    cache=$(cygpath -u "${NABI_CACHE:-$LOCALAPPDATA/nabi}")
    # MSYS's /usr/bin/link (coreutils) would be rustc's link.exe.
    [ -z "${VCToolsInstallDir-}" ] ||
      PATH=$(cygpath -u "${VCToolsInstallDir}bin/Host$VSCMD_ARG_HOST_ARCH/$VSCMD_ARG_TGT_ARCH"):$PATH
    ;;
  *)
    cache=${NABI_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/nabi}
    ;;
esac
out=${CARGO_TARGET_DIR:-target}/lib/nabi/targets
# A Windows CARGO_TARGET_DIR (C:\...), for tar too.
case $(uname -s) in MINGW* | MSYS* | CYGWIN*) out=$(cygpath -u "$out") ;; esac
host=$(rustc -vV | sed -n 's/^host: //p')
sysroot=$(rustc --print sysroot)
rustup component add llvm-tools > /dev/null
ar=$sysroot/lib/rustlib/$host/bin/llvm-ar

# The artifacts bucket's copy of every download, keyed by SHA-256
# (scripts/mirror.sh fills it): upstream hosts go down.
mirror=${NABI_MIRROR:-https://nabi-lang-artifacts.s3.us-east-1.amazonaws.com/mirror}

sha256() { (shasum -a 256 "$1" 2> /dev/null || sha256sum "$1") | cut -d' ' -f1; }

# $1 downloaded into the cache, checked against SHA-256 $3: from the
# mirror, else from $2.
fetch() {
  file=$cache/$1
  if [ ! -f "$file" ]; then
    mkdir -p "$(dirname "$file")"
    if ! curl -fsSL --retry 3 -o "$file.part" "$mirror/$3" 2> /dev/null ||
      [ "$(sha256 "$file.part")" != "$3" ]; then
      curl -fsSL --retry 3 -o "$file.part" "$2"
    fi
    mv "$file.part" "$file"
  fi
  got=$(sha256 "$file")
  if [ "$got" != "$3" ]; then
    echo "checksum mismatch for $file: $got" >&2
    exit 1
  fi
}

# The wasi_snapshot_preview1 adapters into $1 (command and reactor), which
# nabic's componentizer wraps a wasm32-wasip2 core module with: those of
# the wasmtime release the tests run under (scripts/test/wasm.sh).
ADAPTER=v46.0.3
ADAPTER_COMMAND_SHA256=fb2c64f9a431177f75246896bf92ea646eebf40e31a7b7aafd98e3989f34e8c1
ADAPTER_REACTOR_SHA256=758dfcc14d67a5dd323f1481111db4848d2e93154eb5cefdad685dd665ce7427
adapters() {
  for k in command reactor; do
    n=wasi_snapshot_preview1.$k.wasm
    case $k in
      command) s=$ADAPTER_COMMAND_SHA256 ;;
      *) s=$ADAPTER_REACTOR_SHA256 ;;
    esac
    fetch "wasmtime-$ADAPTER/$n" "https://github.com/bytecodealliance/wasmtime/releases/download/$ADAPTER/$n" "$s"
    cp "$cache/wasmtime-$ADAPTER/$n" "$1/"
  done
}

# A Debian package's data.tar.xz (the .deb is an ar archive), verified,
# unpacked under $1.
deb() {
  into=$1 pkg=$2 darch=$3
  [ -f "$into/.$pkg" ] && return
  case $darch,$pkg in
    riscv64,linux-*) version=$PORTS_LINUX url=$PORTS_URL/l/linux ;;
    riscv64,*) version=$PORTS_GLIBC url=$PORTS_URL/g/glibc ;;
    *,linux-*) version=$LINUX url=$LINUX_URL ;;
    *) version=$GLIBC url=$GLIBC_URL ;;
  esac
  name=${pkg}_${version}_$darch.deb
  fetch "glibc-$GLIBC/$name" "$url/$name" "$(echo "$DEB_SHA256" | sed -n "s/^${pkg}_$darch //p")"
  mkdir -p "$into"
  # Not the symlinks to /lib/.. (the loader, unversioned .so): unused,
  # and MSYS cannot make them.
  "$ar" p "$cache/glibc-$GLIBC/$name" data.tar.xz |
    tar -xJf - -C "$into" --exclude './lib64' --exclude './lib/ld-linux*' --exclude './usr/lib/*/*.so'
  touch "$into/.$pkg"
}

# wasi-libc's headers for wasi-sdk triple $2 into $1/sysroot/include.
wasi_headers() {
  v=$WASI_SYSROOT
  fetch "wasi-sdk-$v/wasi-sysroot-$v.tar.gz" \
    "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${v%%.*}/wasi-sysroot-$v.tar.gz" \
    "$WASI_SYSROOT_SHA256"
  rm -rf "$1/sysroot" "$1/.wasi"
  mkdir -p "$1/.wasi" "$1/sysroot"
  tar -xzf "$cache/wasi-sdk-$v/wasi-sysroot-$v.tar.gz" -C "$1/.wasi" "wasi-sysroot-$v/include/$2"
  mv "$1/.wasi/wasi-sysroot-$v/include/$2" "$1/sysroot/include"
  rm -rf "$1/.wasi"
}

# Debian's architecture and multiarch directory of a Linux target.
debian() {
  case ${1%%-*} in
    x86_64) deb=amd64 multi=x86_64-linux-gnu ;;
    aarch64) deb=arm64 multi=aarch64-linux-gnu ;;
    i686) deb=i386 multi=i386-linux-gnu ;;
    armv7) deb=armhf multi=arm-linux-gnueabihf ;;
    riscv64gc) deb=riscv64 multi=riscv64-linux-gnu ;;
    powerpc64le) deb=ppc64el multi=powerpc64le-linux-gnu ;;
    s390x) deb=s390x multi=s390x-linux-gnu ;;
  esac
  root=$cache/glibc-$GLIBC/$deb
}

# FreeBSD's base system, the parts a link uses, unpacked once into
# $fbsd.
freebsd() {
  fbsd=$cache/freebsd-$FREEBSD/root
  [ -f "$fbsd/.done" ] && return
  fetch "freebsd-$FREEBSD/base.txz" "$FREEBSD_URL/base.txz" "$FREEBSD_BASE_SHA256"
  rm -rf "$fbsd"
  mkdir -p "$fbsd"
  tar -xJf "$cache/freebsd-$FREEBSD/base.txz" -C "$fbsd" ./usr/lib/Scrt1.o \
    ./usr/lib/crti.o ./usr/lib/crtn.o ./usr/lib/libc_nonshared.a ./lib/libc.so.7 \
    ./lib/libm.so.5 ./lib/libthr.so.3
  rm -f "$cache/freebsd-$FREEBSD/base.txz"
  touch "$fbsd/.done"
}

# shellcheck disable=SC2048 # split the given args, or $TARGETS when none
for t in ${*:-$TARGETS}; do
  dir=$out/$t
  case $t in *-apple-*)
    # The Mach-O runtime is localized by ld64: a macOS host's job. The
    # host's own is lib/libnabir.a.
    if [ "$(uname -s)" != Darwin ]; then
      [ $# -eq 0 ] || {
        echo "$t: needs a macOS host" >&2
        exit 1
      }
      continue
    fi
    [ "$t" != "$host" ] || continue
    ;;
  esac
  case $t in
    wasm32-unknown-unknown) rust=wasm32-wasip1 ;;
    *) rust=$t ;;
  esac
  # The runtime's objects name the iOS that nabic's target does.
  case $t in *-apple-ios*) export IPHONEOS_DEPLOYMENT_TARGET=13.4 ;; esac
  rustup target add "$rust" > /dev/null
  # One object (fat LTO), as dist/native.sh builds the host's runtime.
  CARGO_PROFILE_RELEASE_LTO=fat cargo build --release --locked -p nabir --target "$rust"
  rm -rf "$dir"
  mkdir -p "$dir"
  case $t in *-windows-*) rt=nabir.lib ;; *) rt=libnabir.a ;; esac
  built=${CARGO_TARGET_DIR:-target}/$rust/release/$rt
  case $t in
    # Localized, as the host's lib/libnabir.a (dist/native.sh).
    *-linux-* | *-apple-* | *-windows-* | *-freebsd) runtime/nabir/localize.sh "$built" "$dir/$rt" ;;
    *) cp "$built" "$dir/" ;;
  esac
  case $t in
    wasm32-*)
      case $t in
        wasm32-unknown-unknown) crt1="crt1-reactor.o" ;;
        wasm32-wasip2) crt1="crt1-command.o crt1-reactor.o" ;;
        *) crt1="crt1-command.o" ;;
      esac
      for f in $crt1 libc.a; do
        cp "$sysroot/lib/rustlib/$rust/lib/self-contained/$f" "$dir/"
      done
      case $t in
        wasm32-wasip2) adapters "$dir" ;;
        wasm32-unknown-unknown) cp "$here/nabi.mjs" "$dir/" ;;
      esac
      case $t in
        wasm32-wasip2) wasi_headers "$dir" wasm32-wasip2 ;;
        wasm32-wasip1-threads) wasi_headers "$dir" wasm32-wasip1-threads ;;
        *) wasi_headers "$dir" wasm32-wasip1 ;;
      esac
      ;;
    *-apple-ios-sim) cp "$here/ios/simrun.sh" "$dir/" ;;
    *-linux-musl | *-linux-musleabihf)
      for f in crt1.o crti.o crtn.o libc.a; do
        cp "$sysroot/lib/rustlib/$t/lib/self-contained/$f" "$dir/"
      done
      # libc.a holds them all; musl installs these empty, so
      # `-lm` / `-lpthread` link.
      for lib in m rt pthread crypt util xnet resolv dl; do
        printf '!<arch>\n' > "$dir/lib$lib.a"
      done
      ;;
    *-linux-gnu | *-linux-gnueabihf)
      debian "$t"
      deb "$root" libc6 "$deb"
      deb "$root" libc6-dev "$deb"
      deb "$root" linux-libc-dev "$deb"
      mkdir -p "$dir/sysroot/usr"
      cp -R "$root/usr/include" "$dir/sysroot/usr/"
      # The empty archives of the libraries glibc 2.34 merged into libc.
      for f in Scrt1.o crti.o crtn.o libc_nonshared.a libpthread.a libdl.a librt.a libutil.a libanl.a; do
        cp "$root/usr/lib/$multi/$f" "$dir/"
      done
      for lib in c m; do
        "$python" "$here/stubs.py" "$root/lib/$multi/lib$lib.so.6" "$dir/lib$lib.so"
      done
      ;;
    *-freebsd)
      freebsd
      for f in Scrt1.o crti.o crtn.o libc_nonshared.a; do
        cp "$fbsd/usr/lib/$f" "$dir/"
      done
      # The shared libraries under their link names (the sonames inside
      # stay libc.so.7, ..): /usr/lib/libc.so is a script of absolute
      # paths.
      for f in libc.so.7 libm.so.5 libthr.so.3; do
        cp "$fbsd/lib/$f" "$dir/${f%.*}"
      done
      ;;
  esac
  echo "$dir"
done
