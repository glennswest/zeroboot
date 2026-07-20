#!/bin/bash
# stormcos-initramfs.sh — inject the RHEL10 storage/fs modules into a stormblock
# initramfs.
#
# stormblock's build-stormblock-initramfs.sh produces the base initramfs
# (busybox + static stormblock + the LinuxBoot /init). That init already handles
# everything stormcos used to patch in — decompressed-module loading, the RHEL10
# io_uring re-enable, the bounded slab-device wait, overlay root
# (rd.stormblock.overlay), and the writable thin volumes (rd.stormblock.writable).
# The one thing it can't do on THIS build host is bundle the storage modules:
# the base build resolves them with `modprobe -S`, which needs a depmod'd
# /lib/modules/<kver> — and the stock tree only ships .ko.xz with no modules.dep.
#
# So all this script does now is pull virtio_scsi / sd_mod / erofs / overlay /
# ublk_drv out of a real (depmod'd) modules dir, DECOMPRESSED (busybox insmod
# can't read .ko.xz), so the disk and erofs root actually appear at boot. Without
# them stormblock opens a nonexistent /dev/sdX2 and dies with "bad slab magic".
#
# The overlay root and writable volumes are selected on the kernel cmdline
# (rd.stormblock.overlay=tmpfs[:SIZE], rd.stormblock.writable=name:mount,...),
# not here — see stormcos-install boot-image / build_cmdline.
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
    f="$(find "$MODDIR" -name "$m.ko.xz" -o -name "$m.ko" | head -1)"
    if [ -z "$f" ]; then
        echo "  WARNING: module $m not found under $MODDIR" >&2
        continue
    fi
    case "$f" in
        *.xz) xz -dc "$f" > "lib/modules/$m.ko" ;;
        *)    cp "$f" "lib/modules/$m.ko" ;;
    esac
    echo "  + $m.ko"
done

find . | cpio -o -H newc --quiet | zstd -19 -T0 -q > "$OUT"
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
