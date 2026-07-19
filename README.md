# stormcos-installer

**The Storm CoreOS installer — our `openshift-install`.** Takes a stormcos
release artifact and turns it into something that boots and becomes a cluster:
boot media, infrastructure, and cluster bootstrap.

Governing principle (inherited from stormcos): **OpenShift shape always wins.**
This project is deliberately the analog of `openshift/installer` — same job,
same phases, storm-native implementations underneath.

| openshift-install | stormcos-install |
|---|---|
| `create image` / coreos-installer (RHCOS ISO, PXE) | `boot-image` — bootable disk/ISO carrying kernel + initramfs + the **stormblock slab payload** |
| `install-config.yaml` | the stormcos **TOML manifest** + CloudID metadata |
| Terraform/CAPI infra provisioning | Proxmox provisioning via the shared terragrunt modules |
| bootstrap node → control-plane static pods → pivot | bootstrap → rustkube static pods on kubelet → destroy bootstrap |

## Why a separate repo

`stormcos` builds the **image** (compose → image-store → release volume). The
installer *consumes* that artifact to produce boot media and stand up a
cluster. OpenShift keeps the same split (`openshift/os` vs
`openshift/installer`), and the installer grows infra providers and bootstrap
logic that have no business inside an image composer.

## Boot artifact model

A stormcos node boots: **kernel + initramfs + a stormblock release volume**.
The initramfs runs `stormblock boot-local`, which attaches the slab, exports
the per-machine COW snapshot as `/dev/ublkb0`, and `switch_root`s into the
erofs root. Install is not a copy step — it's a background RAID1 flow-over
(`--local-disk`) while the node already runs.

So `boot-image` lays out:

```
GPT disk
  p1  ESP (FAT32)   systemd-boot + vmlinuz + initramfs + loader entry
                    options: rd.stormblock.slab=<p2> stormblock.volume=<name>
  p2  raw           the stormblock slab (root + image-store volumes)
```

Written in **pure Rust** (`gpt` + `fatfs`) directly into the image file — no
root, no loop devices, no external partitioning/format tooling — so it builds
on any host, including macOS and CI.

### Overlay root (immutable OS, writable where it counts)

The erofs root is read-only, but a running node must write (sshd host keys,
systemd-logind, kubelet state). The initramfs composes the OpenShift/RHCOS
shape: a **read-only erofs lower + a writable upper** (overlayfs). Upper is
tmpfs today (ephemeral); a persistent per-machine stormblock volume is the
follow-on. `scripts/stormcos-initramfs.sh` adds this plus the RHEL10 runtime
fixes below to a stormblock initramfs.

### Proven boot (2026-07-19)

A stormcos node boots end to end on Proxmox (real hardware, no nesting):
UEFI → systemd-boot → Rocky 6.12 → initramfs → `stormblock boot-local` → ublk
root → overlay(erofs) → `switch_root` → **systemd multi-user.target with
kubelet, kube-proxy, cadvisor, sshd, systemd-logind all started, zero
failures**. Four fixes were required, each a real live-boot failure first:

1. boot-image must write a **protective MBR** (else the disk reads as raw data).
2. `gpt` alignment is in **LBAs, not bytes**.
3. initramfs needs **virtio_scsi / sd_mod / erofs / overlay decompressed**
   (they're modules on RHEL10; busybox has no xzcat).
4. **`kernel.io_uring_disabled=2`** on RHEL10 — ublk needs it re-enabled.

Proxmox-specific: the VM must be **UEFI (OVMF)**, and the module attaches the
disk as **scsi0 → /dev/sda**, so build with `--disk-device /dev/sda`.

Open upstream: per-machine COW snapshot boot needs stormblock to persist
extent maps (stormblock#13) — until then boot the template volume.

## Status

Early. `boot-image` works and boots a real node. Next: persistent writable
state (stormblock /var volume), infra provisioning, cluster bootstrap.
