# zeroboot

**A node is functional the moment it boots.** Not an installer — there is no
step where a machine is installed and *then* becomes useful.

It boots, looks at its drives, and if any of them are nobody's it takes one and
puts its writable state there. A node with nowhere to put that state still
boots, on whatever the appliance is serving. Assimilation is an improvement,
not a precondition.

## Why it runs in the initramfs

The writable volumes a stormcos node mounts — `stormcos-state`,
`stormcert-data`, `fastetcd-data` and the rest — are copy-on-write clones
served from the appliance over NVMe/TCP until they are local. They are
writable, and they are not durable: a fresh clone is minted on every boot, so
nothing written survives.

`stormcos-state` makes it concrete. PID 1 reads `/state/config/stormcos.toml`
for the hostname, and that hostname is the node CA's subject CN — so a node
whose state is remote and ephemeral cannot keep its own identity across a
reboot.

Leave assimilation until after `switch_root` and every one of those volumes has
to be migrated, or staged in a ramdisk and moved later, with a window where the
node's own identity exists only in RAM. Running in the initramfs, before
anything has written a byte, costs a few seconds and removes both.

```
boot ─► look at the drives ─► take one, if it is nobody's ─► format ─►
        create the writable volumes there ─► switch_root
```

## Looking comes before taking

Taking a drive over is easy; it is a format. Deciding a drive is *nobody's* is
the part that destroys data when it is wrong, so `survey` is written to be
read. Every verdict names its evidence, and anything unrecognised is `Foreign`
— never "probably free":

| verdict | meaning | taken? |
|---|---|---|
| `Mine` | a stormblock slab this node owns | no — already done |
| `AnotherNode` | a slab belonging to someone else | never |
| `Foreign` | a table, a filesystem, a signature we do not know | never |
| `Unreadable` | would not answer | never |
| `Blank` | nothing recognisable | **yes** |

Two lessons are baked into that table, both learned on a Dell R230. Its
`/dev/sda` is a 2 TB disk carrying four partitions from a previous life — not
blank, not ours, leave it. And on some boots `/dev/sda` is instead the iDRAC
virtual floppy, which answers `ENOMEDIUM`: absence of a known error is not
evidence of emptiness.

## Two images

- **boot** — netboot, or an attached drive
- **ISO** — the same content as virtual media, for a machine that has nothing
  else

Both carry the same zeroboot; only how they arrive differs.

## Looking at a machine

```
zeroboot survey
```

reads `/sys/block`, reads the first and last of what is on each drive, and
prints the table above with the evidence for every line:

```
/dev/sda        2.00 TB  spinning  WDC WD20EFAX-68F   not ours - GPT, 4 partitions
/dev/sdb            0 B  spinning  Virtual Floppy     unreadable - no medium (reports zero sectors)

nothing to take:
  /dev/sda not ours - GPT, 4 partitions
  /dev/sdb unreadable - no medium (reports zero sectors)
```

`--json` gives the same thing to a machine. It writes nothing: every device is
opened read-only, and the subcommand has no way to format anything.

A slab is identified by asking `stormblock slab list`, the same static binary
the initramfs already carries and already uses for exactly this probe — so
there is one implementation of the superblock rather than two that drift.
Without it a slab is still recognised by its magic but cannot be named, and an
unnameable slab is `Foreign`.

Where a drive is attached is part of what it is. A slab on a disk inside this
chassis was written by this node in an earlier life and is `Mine`; the same
slab arriving over nvme-tcp, iSCSI or Fibre Channel is the appliance's export
or a LUN shared with another node, and is `AnotherNode`. Nothing removable is
ever taken — a USB stick in the front panel is not free space.

`Blank` is reached one way only: nothing removable, nothing over the network,
no partitions, no signature, and every byte read comes back zero. What is read
is the first and last megabyte in full, then 64 KiB every megabyte through the
first 64 MiB and 64 KiB every gigabyte after that — about 130 MiB and two
thousand reads on a 2 TB disk, which is seconds, where reading all of it is
hours in an initramfs on every boot. It is a sample and not a proof, and it
does not have to be one: a drive that has ever been used carries a signature
the head names, and one byte found anywhere in the grain makes the drive
`Foreign`.

## Status

`survey` — the judgement and the machine it looks at — is implemented and
tested, and is a subcommand. Format and volume creation are next; see issue #2
for the design.
