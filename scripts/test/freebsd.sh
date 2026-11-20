#!/bin/sh
# A FreeBSD 14.4 VM (qemu + KVM) that runs x86_64-unknown-freebsd
# executables for the corpora:
#
#   scripts/test/freebsd.sh up            boot it (image fetched, SHA-256 checked)
#   scripts/test/freebsd.sh run <exe> ..  run <exe> in it (NABI_TEST_RUNNER)
#   scripts/test/freebsd.sh ssh <cmd>     run shell command <cmd> in it as
#                                         user freebsd (`su root -c ..`
#                                         for root), stdin passed on
#   scripts/test/freebsd.sh down          stop it
#
#   scripts/test/freebsd.sh up
#   NABI_TEST_TARGET=x86_64-unknown-freebsd \
#     NABI_TEST_RUNNER="$PWD/scripts/test/freebsd.sh run" \
#     cargo test -p nabic --test main -- integration::
#
# State lives in NABI_FREEBSD_DIR (default <cache>/freebsd-vm); ssh on
# 127.0.0.1:NABI_FREEBSD_PORT (default 2222); NABI_FREEBSD_DISK grows the
# disk to that size (qemu-img, e.g. 40G) when the image is made, for a
# build in the VM (dist/freebsd.sh). Needs qemu-system-x86_64, qemu-img,
# /dev/kvm, xz, ssh and genisoimage (or xorriso).
set -eu

cache=${NABI_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/nabi}
dir=${NABI_FREEBSD_DIR:-$cache/freebsd-vm}
port=${NABI_FREEBSD_PORT:-2222}
IMG=FreeBSD-14.4-RELEASE-amd64-BASIC-CLOUDINIT-ufs.qcow2.xz
URL=https://download.freebsd.org/releases/VM-IMAGES/14.4-RELEASE/amd64/Latest/$IMG
SHA=d957fe56fb596280428328e78e9e4feae00dd361701caa79a061f9bd289eff67

vm_ssh() {
  ssh -q -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
    -o ConnectTimeout=5 -o BatchMode=yes -i "$dir/id_ed25519" -p "$port" freebsd@127.0.0.1 "$@"
}

up() {
  # Already up: nothing to do.
  if [ -f "$dir/qemu.pid" ] && kill -0 "$(cat "$dir/qemu.pid")" 2> /dev/null && vm_ssh true < /dev/null; then
    return 0
  fi
  mkdir -p "$dir" "$cache"
  if [ ! -f "$cache/$IMG" ]; then
    curl -fsSL --retry 3 -o "$cache/$IMG.part" "$URL"
    mv "$cache/$IMG.part" "$cache/$IMG"
  fi
  got=$( (sha256sum "$cache/$IMG" 2> /dev/null || shasum -a 256 "$cache/$IMG") | cut -d' ' -f1)
  [ "$got" = "$SHA" ] || {
    echo "test/freebsd.sh: checksum mismatch for $cache/$IMG: $got" >&2
    exit 1
  }
  # The image's first boot runs freebsd-update (fetch, install, reboot)
  # before sshd starts: about ten minutes, more on a slow mirror.
  wait=60
  if [ ! -f "$dir/img.qcow2" ]; then
    wait=360
    xz -dc "$cache/$IMG" > "$dir/img.qcow2.part"
    # growfs grows the root file system at the first boot.
    [ -z "${NABI_FREEBSD_DISK-}" ] || qemu-img resize -q "$dir/img.qcow2.part" "$NABI_FREEBSD_DISK"
    mv "$dir/img.qcow2.part" "$dir/img.qcow2"
  fi
  [ -f "$dir/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -f "$dir/id_ed25519"
  mkdir -p "$dir/seed"
  printf 'instance-id: nabi-freebsd\nlocal-hostname: nabi-freebsd\n' > "$dir/seed/meta-data"
  printf '#cloud-config\nssh_authorized_keys:\n  - %s\n' "$(cat "$dir/id_ed25519.pub")" > "$dir/seed/user-data"
  # nuageinit reads a config drive labelled cidata.
  if command -v genisoimage > /dev/null; then
    genisoimage -quiet -o "$dir/seed.iso" -V cidata -J -r "$dir/seed"
  else
    xorriso -as mkisofs -quiet -o "$dir/seed.iso" -V cidata -J -r "$dir/seed"
  fi
  qemu-system-x86_64 -enable-kvm -cpu host -m "${NABI_FREEBSD_MEM:-8G}" -smp "${NABI_FREEBSD_CPUS:-4}" \
    -drive file="$dir/img.qcow2",if=virtio,format=qcow2 -drive file="$dir/seed.iso",media=cdrom \
    -netdev user,id=n0,hostfwd=tcp:127.0.0.1:"$port"-:22 -device virtio-net-pci,netdev=n0 \
    -serial file:"$dir/console.log" -monitor none -display none -daemonize -pidfile "$dir/qemu.pid"
  i=0
  until vm_ssh true < /dev/null; do
    i=$((i + 1))
    [ $i -le $wait ] || {
      echo "test/freebsd.sh: the VM did not answer ssh in $((wait / 12)) minutes; console: $dir/console.log" >&2
      tail -n 20 "$dir/console.log" >&2 || true
      exit 1
    }
    sleep 5
  done
  vm_ssh uname -a < /dev/null
}

# A clean power-off: dist/freebsd.sh boots the same disk again, and a
# killed VM leaves UFS for a manual fsck. Killed after a minute.
down() {
  [ -f "$dir/qemu.pid" ] || return 0
  pid=$(cat "$dir/qemu.pid")
  vm_ssh 'su root -c "shutdown -p now"' < /dev/null > /dev/null 2>&1 || true
  i=0
  while kill -0 "$pid" 2> /dev/null && [ $i -lt 60 ]; do
    sleep 1
    i=$((i + 1))
  done
  kill "$pid" 2> /dev/null || true
  rm -f "$dir/qemu.pid"
}

# The executable goes over ssh's stdin, so the case's own stdin is null.
# The case's environment goes with it but for the host's own variables;
# its temp directory is the VM's.
run() {
  exe=$1
  shift
  python3 - "$dir" "$port" "$exe" "$@" << 'PY'
import os, shlex, sys

d, port, exe, args = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
HOST = ("PATH", "HOME", "USER", "LOGNAME", "SHELL", "PWD", "OLDPWD", "SHLVL",
        "TERM", "MAIL", "LANG", "_", "TMPDIR", "TMP", "TEMP", "SSL_CERT_FILE",
        "SSL_CERT_DIR")
PREFIX = ("LC_", "SSH_", "XDG_", "CARGO", "RUSTUP_", "RUSTC", "RUSTDOC", "LD_",
          "DBUS_", "NABI_TEST_", "NABI_FREEBSD_", "GITHUB_", "RUNNER_", "ACTIONS_",
          "INPUT_", "JAVA_", "ANDROID_", "DOTNET_", "SCCACHE", "AWS_")
env = ["TMPDIR=/tmp/nabi-t", "TMP=/tmp/nabi-t", "TEMP=/tmp/nabi-t"]
env += [f"{k}={v}" for k, v in os.environ.items() if k not in HOST and not k.startswith(PREFIX)]
r = "/tmp/r/" + os.urandom(8).hex()
# sh reports a program a signal killed ("Abort trap") on its own
# stderr: that goes to /dev/null, the program's stderr (fd 3) to ours.
cmd = (f"mkdir -p {r} /tmp/nabi-t && cat > {r}/exe && chmod +x {r}/exe && cd {r} && "
       f"{{ env {' '.join(shlex.quote(e) for e in env)} timeout -s KILL 1200 ./exe "
       f"{' '.join(shlex.quote(a) for a in args)} < /dev/null 2>&3 3>&-; }} 3>&2 2> /dev/null; "
       f"rc=$?; cd /; rm -rf {r}; exit $rc")
with open(exe, "rb") as f:
    os.dup2(f.fileno(), 0)
os.execvp("ssh", ["ssh", "-q", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                  "-o", "LogLevel=ERROR", "-i", os.path.join(d, "id_ed25519"), "-p", port,
                  "freebsd@127.0.0.1", cmd])
PY
}

case ${1:-} in
  up) up ;;
  down) down ;;
  ssh)
    shift
    vm_ssh "$@"
    ;;
  run)
    shift
    run "$@"
    ;;
  *)
    echo "usage: scripts/test/freebsd.sh up | run <exe> [args..] | ssh <cmd> | down" >&2
    exit 2
    ;;
esac
