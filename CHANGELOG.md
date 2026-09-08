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
- **feat(survey):** `Intent::AlreadyMine` says which device to boot. It carried
  no path at all — only "something here is ours" — so a caller could not act on
  it without searching for the slab again, with a second implementation of the
  judgement to drift from the first. `Verdict::Mine` now records the device the
  slab is actually on, which is a partition (`/dev/sda2`) on a disk laid out
  with an ESP and the whole drive when the slab was written to one, and
  `AlreadyMine` carries the drive, that device and the slab id.
- **feat(survey):** when a node owns both a system slab and a data slab, the
  system one is the one handed to the caller. Otherwise which disk a node boots
  from depends on which port it is plugged into.
- **feat(survey):** the end of a drive is named, not just counted. A backup GPT
  header and an mdraid v0.90/v1.0 superblock both live there and leave the
  front untouched, so a disk pulled out of a Linux array reads as blank from
  the front; it was never going to be taken, but it is now reported as the
  array member it is.
- **feat(survey):** nothing removable is ever taken, empty or not — a USB stick
  in the front panel is not free space, and the iDRAC virtual floppy is the
  same device class.
- **feat(survey):** `Blank` is reached one way only: nothing removable, nothing
  over the network, no partitions, no known signature, and every byte read
  comes back zero. Every other verdict comes from the first and last megabyte,
  so a boot with nothing to take costs a megabyte a drive; `Blank` is the one
  verdict that leads to a format, so it alone pays for the first 64 MiB whole,
  64 KiB every gigabyte to the end, and the last megabyte — around two thousand
  reads on a 2 TB drive, against hours to read all of it. A disk whose first
  megabyte was once zeroed does not read as empty. The first version sampled
  three points from the middle and called a real 8 GB disk with a byte written
  4 MiB in "blank - available", which is the one mistake this file exists to
  avoid.
- **feat(survey):** `Drive` carries the drive's model string, and `Survey`,
  `Drive`, `Verdict` and `Intent` serialise, so `--json` reports the evidence
  and the conclusion separately.

Verified on a Linux host against real artifacts rather than fixtures alone: a
slab formatted by the real `stormblock` reads as `Mine` with its UUID and role,
a real ext4 filesystem and a real GPT-with-four-partitions disk read as
`Foreign`, a live nvme-tcp namespace reads as attached over the network, a
200 GB zeroed disk reads as `Blank`, and an mdraid superblock 8 KiB from the
end reads as a RAID member. A four-drive machine including a 2 TB spinning disk
surveys in 0.2 s.
