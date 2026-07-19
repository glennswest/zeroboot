#!/bin/bash
# stormcos-initramfs.sh — post-process a stormblock initramfs into a stormcos
# boot initramfs.
#
# stormblock's build-stormblock-initramfs.sh produces the base (busybox +
# static stormblock + boot-local + switch_root). This adds the four things a
# real stormcos node boot needs on the RHEL10 kernel — every one of which was
# a live-boot failure before it was added:
#
#   1. Storage + fs drivers, DECOMPRESSED. virtio_scsi / sd_mod / erofs are
#      modules on RHEL10, and busybox has no xzcat, so shipping .ko.xz fails
#      silently. Without them the disk never appears and stormblock opens a
#      nonexistent /dev/sdX2 (creating an empty file -> "bad slab magic").
#   2. io_uring re-enable. RHEL10 ships kernel.io_uring_disabled=2 (hardening)
#      even with CONFIG_IO_URING=y, so ublk's root export fails EPERM.
#   3. A wait for the slab block device before starting stormblock.
#   4. Overlay root: read-only erofs lower + writable upper (tmpfs for now).
#      The immutable-OS (OpenShift/RHCOS) shape — without it sshd-keygen,
#      systemd-logind, and kubelet can't write and fail.
#
# This is interim: the generic fixes (1-3) belong in stormblock's initramfs
# script, and the overlay upper should become a persistent per-machine
# stormblock volume rather than tmpfs (see repo TODOs / stormblock issues).
#
# Usage: ./scripts/stormcos-initramfs.sh <src-initramfs> <kver> <modules-dir> <out>

set -euo pipefail

SRC="${1:?src initramfs}"
KV="${2:?kernel version}"
MODDIR="${3:?/lib/modules/<kver> dir}"
OUT="${4:?output path}"

D="$(mktemp -d)"
cd "$D"
zstd -dc "$SRC" | cpio -idm --quiet
mkdir -p lib/modules

for m in ublk_drv virtio_scsi sd_mod erofs overlay; do
    f="$(find "$MODDIR" -name "$m.ko.xz" | head -1)"
    xz -dc "$f" > "lib/modules/$m.ko"
    echo "  + $m.ko"
done
rm -f lib/modules/ublk_drv.ko.xz

python3 - "$D/init" <<'PYEOF'
import sys
p = sys.argv[1]
s = open(p).read()

drivers = '''
# Storage + fs drivers first: virtio_scsi/sd_mod/erofs/overlay are modules on
# RHEL10, so without these the disk never appears and the erofs root can't mount.
for m in sd_mod virtio_scsi erofs overlay; do
    [ -f /lib/modules/$m.ko ] && insmod /lib/modules/$m.ko 2>/dev/null
done
'''
anchor = "# Load ublk driver"
assert anchor in s, "driver anchor missing"
s = s.replace(anchor, drivers + "\n" + anchor, 1)

pre = '''
# RHEL10 ships kernel.io_uring_disabled=2 (hardening) even with CONFIG_IO_URING=y;
# ublk is an io_uring interface, so the root export fails EPERM without this.
if [ -w /proc/sys/kernel/io_uring_disabled ]; then
    echo 0 > /proc/sys/kernel/io_uring_disabled
fi

if [ "$BOOT_MODE" = "local" ]; then
    echo "Waiting for slab device $SLAB..."
    n=0
    while [ ! -b "$SLAB" ] && [ $n -lt 15 ]; do sleep 1; n=$((n+1)); done
    [ -b "$SLAB" ] && echo "Slab device present: $SLAB" || echo "WARNING: $SLAB is not a block device"
fi
'''
a2 = 'echo "Starting StormBlock..."'
assert a2 in s, "stormblock start anchor missing"
s = s.replace(a2, pre + "\n" + a2, 1)

mount_block = '''mount -t erofs -o ro "$ROOTDEV" /sysroot 2>/dev/null \\
    || mount -t ext4 "$ROOTDEV" /sysroot 2>/dev/null \\
    || mount "$ROOTDEV" /sysroot \\
    || { echo "FATAL: Failed to mount root"; exec /bin/sh; }'''
overlay_block = '''echo "Composing overlay root (read-only erofs + writable tmpfs upper)..."
mkdir -p /erofs-lower /ovl
mount -t erofs -o ro "$ROOTDEV" /erofs-lower 2>/dev/null \\
    || { echo "FATAL: erofs lower mount failed"; exec /bin/sh; }
mount -t tmpfs -o mode=0755 tmpfs /ovl
mkdir -p /ovl/upper /ovl/work
mount -t overlay overlay \\
    -o lowerdir=/erofs-lower,upperdir=/ovl/upper,workdir=/ovl/work /sysroot \\
    || { echo "FATAL: overlay root mount failed"; exec /bin/sh; }'''
assert mount_block in s, "mount block not found (stormblock initramfs changed?)"
s = s.replace(mount_block, overlay_block, 1)

open(p, "w").write(s)
print("init patched: drivers + io_uring + slab-wait + overlay root")
PYEOF

find . | cpio -o -H newc --quiet | zstd -19 -T0 -q > "$OUT"
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
