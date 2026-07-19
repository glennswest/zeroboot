# Unit: stormcos-boot — throwaway VM that boots a stormcos boot-image.
#
# Boots the artifact end to end on real hardware (no nested virt): UEFI ->
# systemd-boot on the ESP -> Rocky kernel + stormblock initramfs ->
# `stormblock boot-local` attaches the slab partition, exports the boot volume
# as /dev/ublkb0 -> mount erofs -> switch_root into systemd.
#
# Throwaway by design: apply, watch it boot, destroy, release the vmid
# (../free-vmid.sh --release <id>).
#
# Build + stage the image first:
#   stormcos-install boot-image --kernel <vmlinuz> --initramfs <initramfs> \
#     --bootloader systemd-bootx64.efi --slab <root.slab> --volume boot-cp-01 \
#     --disk-device /dev/sda --out stormcos-boot.img
#   scp stormcos-boot.img root@pve.g8.lo:/var/lib/vz/import/stormcos-boot.raw
#   ssh root@pve.g8.lo 'qemu-img convert -f raw -O qcow2 \
#     /var/lib/vz/import/stormcos-boot.raw /var/lib/vz/import/stormcos-boot.qcow2'
#
# Then:
#   VMID=$(../free-vmid.sh) STORMCOS_BOOT_VMID=$VMID terragrunt apply
#
# NOTE: the module attaches the disk as scsi0 (virtio-scsi), so the guest sees
# /dev/sda — the image must be built with --disk-device /dev/sda so the baked
# cmdline points at /dev/sda2 for the slab.

include "root" {
  path = find_in_parent_folders("root.hcl")
}

terraform {
  source = "git::ssh://git@github.com/glennswest/terraform-modules.git//modules/proxmox-fedora-vm?ref=v0.3.0"
}

locals {
  ssh_key = trimspace(file(pathexpand("~/.ssh/id_rsa.pub")))
  # Fixed MAC -> reserved IP outside the g8 DHCP pool (.100-.200).
  # .60 = ublktest (stormblock), .61 = irondirectory; .66 free per scan 2026-07-19.
  node = {
    vm_id = tonumber(get_env("STORMCOS_BOOT_VMID", "0"))
    mac   = "BC:24:11:08:00:66"
    ip    = "192.168.8.66"
  }
}

inputs = {
  dns_zone_id        = "9bed60c8-1664-4183-88f9-a1a21b927edc" # g8.lo
  ci_ssh_public_keys = [local.ssh_key]
  tags               = ["terraform", "stormcos", "boot-test", "throwaway"]

  # Boot our image instead of the Fedora cloud image. The cloud-init drive the
  # module attaches on ide2 is inert here — stormcos boots from its own ESP.
  fedora_image = "local:import/stormcos-boot.qcow2"

  vm_datastore      = "test-lvm-thin"
  snippet_datastore = "terraform-snippets"

  vms = {
    stormcos-boot = {
      vm_id = local.node.vm_id
      mac   = local.node.mac
      ip    = local.node.ip
      cores = 2
      # The composed rootfs + image store need headroom; the image itself is
      # ~1.4 GiB and the disk is grown to this size on import.
      memory    = 4096
      disk_size = 4
    }
  }
}
