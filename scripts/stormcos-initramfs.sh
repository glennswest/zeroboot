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
# So this script pulls virtio_scsi / sd_mod / erofs / overlay / ublk_drv out of
# a real (depmod'd) modules dir, DECOMPRESSED (busybox insmod can't read
# .ko.xz), so the disk and erofs root actually appear at boot. Without them
# stormblock opens a nonexistent /dev/sdX2 and dies with "bad slab magic".
#
# The overlay root and writable volumes are selected on the kernel cmdline
# (rd.stormblock.overlay=tmpfs[:SIZE], rd.stormblock.writable=name:mount,...),
# not here — see zeroboot boot-image / build_cmdline.
#
# It also installs the zeroboot binary at /sbin/zeroboot when one is given.
# zeroboot is a step in the boot, not something anybody runs: the initramfs
# calls `zeroboot boot` before the local-slab probe, and it answers with
# KEY=value on stdout and an exit code. Without the binary in here, none of
# zeroboot's judgement executes on a real machine — which was true until now.
#
# This script does NOT patch /init. That was retired in 81b7922 because the
# python anchor patches went stale and crashed the build, and re-adding them
# would repeat the mistake. The base stormblock init calls the hook itself.
#
# Usage: ./scripts/stormcos-initramfs.sh <src-initramfs> <kver> <modules-dir> <out> [zeroboot-binary]
#   or set ZEROBOOT_BIN=/path/to/zeroboot

set -euo pipefail

SRC="${1:?src initramfs}"
KV="${2:?kernel version}"
MODDIR="${3:?/lib/modules/<kver> dir}"
OUT="${4:?output path}"
ZB="${5:-${ZEROBOOT_BIN:-}}"

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

if [ -n "$ZB" ]; then
    [ -f "$ZB" ] || { echo "ERROR: zeroboot binary $ZB not found" >&2; exit 1; }
    # The initramfs is busybox and a static stormblock; there is no dynamic
    # loader and no libc in it. A dynamically linked binary here does not fail
    # at build time, it fails at boot as "not found" on a file that is plainly
    # there, which is a bad hour to spend.
    if readelf -l "$ZB" 2>/dev/null | grep -q INTERP; then
        echo "ERROR: $ZB is dynamically linked; build it for a musl target:" >&2
        echo "  cargo build --release --target x86_64-unknown-linux-musl" >&2
        exit 1
    fi
    mkdir -p sbin
    install -m 0755 "$ZB" sbin/zeroboot
    echo "  + /sbin/zeroboot ($(du -h sbin/zeroboot | cut -f1))"
else
    echo "  ! no zeroboot binary given - this initramfs cannot run zeroboot's" >&2
    echo "    judgement; the boot falls back to stormblock's own slab probe" >&2
fi

find . | cpio -o -H newc --quiet | zstd -19 -T0 -q > "$OUT"
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
