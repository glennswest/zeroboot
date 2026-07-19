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

## Status

Early. `boot-image` first; infra provisioning and bootstrap follow.
