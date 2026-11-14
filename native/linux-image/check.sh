#!/usr/bin/env bash
# Checks a built Linux system image before it is bundled: mounts system.erofs
# read-only and looks for what compile VMs rely on.
#
#   sudo native/linux-image/check.sh <image dir>
set -euo pipefail

dir=$(cd "${1:?usage: check.sh <image dir>}" && pwd)
for file in system.erofs Image image.json packages.manifest; do
  [[ -s $dir/$file ]] || {
    echo "Missing $file" >&2
    exit 1
  }
done
if [[ $(id -u) != 0 ]]; then
  echo "check.sh must run as root" >&2
  exit 1
fi
mount=$(mktemp -d)
# shellcheck disable=SC2329 # called by the EXIT trap
cleanup() {
  if mountpoint -q "$mount"; then umount "$mount"; fi
  rmdir "$mount"
}
trap cleanup EXIT
modprobe erofs 2>/dev/null || true
mount -t erofs -o loop,ro "$dir/system.erofs" "$mount"

failed=0
expect() {
  if ! test "$@"; then
    echo "missing: test $*" >&2
    failed=1
  fi
}
# The guest agent formats work disks and packs packages with these.
for tool in usr/sbin/mke2fs usr/bin/mkfs.erofs usr/bin/tar usr/bin/bash; do expect -x "$mount/$tool"; done
# Build tools from packages.txt and Bazelisk.
for tool in usr/bin/gcc usr/bin/make usr/bin/cmake usr/bin/ninja usr/bin/git usr/bin/python3 usr/bin/ccache usr/bin/curl; do
  expect -x "$mount/$tool"
done
expect -x "$mount/usr/local/bin/bazelisk"
expect -L "$mount/usr/local/bin/bazel"
expect -d "$mount/usr/lib/ccache"
expect -f "$mount/etc/gizmoth-image.json"
if ! grep -q '^builder:x:1000:1000:' "$mount/etc/passwd"; then
  echo "missing: builder (uid 1000)" >&2
  failed=1
fi
if [[ $(stat -c %u:%g "$mount/home/builder") != 1000:1000 ]]; then
  echo "/home/builder is not owned by builder" >&2
  failed=1
fi
if ! cmp -s "$mount/etc/gizmoth-image.json" "$dir/image.json"; then
  echo "image.json differs from /etc/gizmoth-image.json" >&2
  failed=1
fi
# The kernel is an arm64 Linux Image ("ARM\x64" magic at offset 56).
if [[ $(dd if="$dir/Image" bs=1 skip=56 count=4 status=none | od -An -tx1 | tr -d ' \n') != 41524d64 ]]; then
  echo "Image is not an arm64 Linux kernel" >&2
  failed=1
fi
if [[ $failed == 0 ]]; then
  echo "Image checks passed: $(du -h "$dir/system.erofs" | cut -f1) system.erofs, $(wc -l <"$dir/packages.manifest") Debian packages"
fi
exit "$failed"
