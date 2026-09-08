# Changelog

## [Unreleased]

### 2026-09-08
- **feat(survey):** `zeroboot survey` — the judgement can now look at a real
  machine. `probe` walks `/sys/block` for every drive's size, rotational flag,
  model, partitions and transport, reads the head, the tail and three samples
  from the middle of each one, and produces the `Survey` that `survey.rs`
  judges. Read-only throughout; every device is opened read-only and the
  subcommand has no way to format anything. Closes #3.
- **feat(survey):** a slab is identified by shelling out to `stormblock slab
  list` — the static binary the initramfs already carries and already uses for
  this probe — so there is one implementation of the superblock rather than two
  that drift. Positive evidence only (`: slab <uuid>`): "not a slab", "cannot
  open" and an `ENOMEDIUM` from an empty removable drive are all *not a slab*.
  A slab whose magic is present but which cannot be named is `Foreign`.
- **feat(survey):** where a drive is attached decides whose it is. A slab on a
  local disk is `Mine`; the same slab over nvme-tcp, iSCSI or Fibre Channel is
  `AnotherNode`, because it is the appliance's export or a LUN shared with
  another node. A slab in a partition of a local disk is `Mine` too, so a node
  booting off the disk it assimilated onto recognises its own work rather than
  reading it as a foreign partition table.
- **feat(survey):** nothing removable is ever taken, empty or not — a USB stick
  in the front panel is not free space, and the iDRAC virtual floppy is the
  same device class.
- **feat(survey):** `Blank` is reached one way only: nothing removable, nothing
  over the network, no partitions, no known signature, and the head, the tail
  and three 64 KiB samples from the middle all zero. A disk whose first
  megabyte was once zeroed does not read as empty.
- **feat(survey):** `Drive` carries the drive's model string, and `Survey`,
  `Drive`, `Verdict` and `Intent` serialise, so `--json` reports the evidence
  and the conclusion separately.
