#!/usr/bin/env bash
# Builds GizMoth's Linux system image for compile VMs: the Ubuntu base with
# the packages in packages.txt, the `builder` user (uid 1000) and Bazelisk,
# packed as a compressed read-only erofs filesystem, plus the guest kernel.
# The release workflow builds it on ubuntu-24.04-arm and bundles it with the
# macOS app; `npm run linux-image:build` runs this script in Docker elsewhere.
#
#   sudo native/linux-image/build.sh <output dir>
#
# Needs root on Linux arm64 with curl, sha256sum, tar and mkfs.erofs
# (erofs-utils). Writes system.erofs, Image, image.json and packages.manifest
# (installed Debian package versions, for source and license references).
#
# Environment: APT_MIRROR replaces the apt source URI during the build only;
# GIZMOTH_IMAGE_CACHE keeps verified downloads between runs;
# GIZMOTH_IMAGE_COMMIT is recorded in image.json; OUTPUT_OWNER (uid:gid)
# receives the outputs.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
output=${1:?usage: build.sh <output dir>}
# shellcheck source-path=SCRIPTDIR source=sources.env
. "$here/sources.env"

if [[ $(id -u) != 0 ]]; then
  echo "build.sh must run as root" >&2
  exit 1
fi
if [[ $(uname -s) != Linux || $(uname -m) != aarch64 ]]; then
  echo "build.sh runs on Linux arm64 (found $(uname -s) $(uname -m))" >&2
  exit 1
fi
for tool in curl sha256sum tar chroot mount mkfs.erofs; do
  command -v "$tool" >/dev/null || {
    echo "build.sh needs $tool" >&2
    exit 1
  }
done

mkdir -p "$output"
output=$(cd "$output" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/gizmoth-image.XXXXXX")
cache=${GIZMOTH_IMAGE_CACHE:-$work/downloads}
mkdir -p "$cache"
rootfs=$work/rootfs

unmount_all() {
  for path in run dev/pts dev sys proc; do
    if mountpoint -q "$rootfs/$path" 2>/dev/null; then umount "$rootfs/$path"; fi
  done
}
cleanup() {
  unmount_all || true
  # Never delete through a mount that is still attached (such as /dev).
  if grep -q " $rootfs/" /proc/mounts; then
    echo "Leaving $work: it still has mounts" >&2
  else
    rm -rf --one-file-system "$work"
  fi
}
trap cleanup EXIT

step() { printf '\n== %s\n' "$*"; }

# Downloads into the cache once and checks the SHA-256 every time.
fetch() {
  local url=$1 sha256=$2 path=$cache/$2
  if [[ ! -f $path ]] || ! echo "$sha256  $path" | sha256sum -c --status -; then
    curl -fL --retry 3 --retry-delay 5 --silent --show-error -o "$path.partial" "$url"
    if ! echo "$sha256  $path.partial" | sha256sum -c --status -; then
      echo "$url does not match its pinned SHA-256" >&2
      rm -f "$path.partial"
      exit 1
    fi
    mv "$path.partial" "$path"
  fi
  printf '%s\n' "$path"
}

in_root() {
  chroot "$rootfs" /usr/bin/env -i \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    HOME=/root LANG=C.UTF-8 TERM=dumb DEBIAN_FRONTEND=noninteractive "$@"
}

step "Downloading pinned inputs"
base=$(fetch "$UBUNTU_BASE_URL" "$UBUNTU_BASE_SHA256")
kernel=$(fetch "$KERNEL_URL" "$KERNEL_SHA256")
bazelisk=$(fetch "$BAZELISK_URL" "$BAZELISK_SHA256")

step "Extracting the kernel"
member=$(tar -tzf "$kernel" | grep -E '(^|/)Image$' | head -n 1)
[[ -n $member ]] || {
  echo "The kernel archive has no Image" >&2
  exit 1
}
tar -xzf "$kernel" -O "$member" >"$output/Image.partial"
mv "$output/Image.partial" "$output/Image"

step "Unpacking $UBUNTU_BASE_NAME"
mkdir "$rootfs"
tar --numeric-owner -xpzf "$base" -C "$rootfs"
mount -t proc proc "$rootfs/proc"
mount -t sysfs sysfs "$rootfs/sys"
mount --bind /dev "$rootfs/dev"
mount --bind /dev/pts "$rootfs/dev/pts"
mount -t tmpfs tmpfs "$rootfs/run"
rm -f "$rootfs/etc/resolv.conf"
cp -L /etc/resolv.conf "$rootfs/etc/resolv.conf"
# Package scripts must not start services in the chroot.
printf '#!/bin/sh\nexit 101\n' >"$rootfs/usr/sbin/policy-rc.d"
chmod 755 "$rootfs/usr/sbin/policy-rc.d"
sources=$rootfs/etc/apt/sources.list.d/ubuntu.sources
cp "$sources" "$work/ubuntu.sources"
if [[ -n ${APT_MIRROR:-} ]]; then
  sed -E -i "s#^URIs:.*#URIs: ${APT_MIRROR}#" "$sources"
fi

mapfile -t packages < <(sed -e 's/#.*//' -e 's/[[:space:]]//g' "$here/packages.txt" | grep -v '^$')
step "Installing ${#packages[@]} packages"
in_root apt-get update
in_root apt-get install -y --no-install-recommends "${packages[@]}"

step "Creating the build user"
if ! grep -q '^builder:' "$rootfs/etc/passwd"; then
  in_root useradd -m -u 1000 -U -s /bin/bash builder
fi

step "Installing $BAZELISK_NAME"
install -m 755 "$bazelisk" "$rootfs/usr/local/bin/bazelisk"
ln -sfn bazelisk "$rootfs/usr/local/bin/bazel"

step "Cleaning up"
# shellcheck disable=SC2016 # dpkg-query's own format
in_root dpkg-query -W -f '${Package}\t${Version}\n' >"$output/packages.manifest"
in_root apt-get clean
rm -rf "$rootfs"/var/lib/apt/lists/* "$rootfs"/var/cache/apt/*.bin "$rootfs"/var/log/apt/*
cp "$work/ubuntu.sources" "$sources"
# Worker VMs write their own resolv.conf at boot.
rm -f "$rootfs/usr/sbin/policy-rc.d" "$rootfs/etc/resolv.conf"
unmount_all

agent=$(sed -n 's/^pub const AGENT_VERSION: u32 = \([0-9]*\);.*/\1/p' "$here/../compile-agent/src/protocol.rs")
commit=${GIZMOTH_IMAGE_COMMIT:-$(git -C "$here" rev-parse HEAD 2>/dev/null || true)}
commit_json=null
if [[ -n $commit ]]; then commit_json="\"$commit\""; fi
list=$(printf '"%s",' "${packages[@]}")
info=$(printf '{\n  "format": "erofs",\n  "builtAt": %s,\n  "commit": %s,\n  "base": "%s",\n  "kernel": "%s",\n  "bazelisk": "%s",\n  "agentVersion": %s,\n  "packages": [%s]\n}\n' \
  "$(($(date +%s) * 1000))" "$commit_json" \
  "$UBUNTU_BASE_NAME" "$KERNEL_NAME" "$BAZELISK_NAME" "${agent:-0}" "${list%,}")
printf '%s' "$info" >"$rootfs/etc/gizmoth-image.json"

step "Packing system.erofs"
mkfs.erofs --quiet -zlz4hc "$output/system.erofs.partial" "$rootfs"
mv "$output/system.erofs.partial" "$output/system.erofs"
printf '%s' "$info" >"$output/image.json"
if [[ -n ${OUTPUT_OWNER:-} ]]; then
  chown "$OUTPUT_OWNER" "$output" "$output/system.erofs" "$output/Image" "$output/image.json" "$output/packages.manifest"
fi
ls -l "$output"
