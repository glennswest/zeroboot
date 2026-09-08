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

Whose a slab is, and whether a drive boots, are different questions with
different evidence, and the survey keeps them apart. A slab formatted an hour
ago with nothing in it and no bootloader anywhere is as much `Mine` as a
working boot disk; a data slab is `Mine` and is not supposed to boot at all. So
a third answer sits beside "boot" and "take one":

| intent | meaning |
|---|---|
| `AlreadyMine` | a slab of ours that boots — and *which device* to boot |
| `MineButNoneBoots` | ours, and none of it starts the node: ask the appliance |
| `TakeOver` | nothing here is anyone's; take it |
| `NothingToTake` | nowhere to go; boot on the appliance's clone and say why |

Collapsing the middle one into `AlreadyMine` is how a node with a dead system
disk declines to fetch the image it cannot come up without.

## Whose drive is it?

Nothing in a stormblock slab records an owner — the superblock carries a slab
uuid and a device uuid and no node identity — so a disk moved from one chassis
to another is, on the evidence in it, indistinguishable from one that was
always here. That matters more than it sounds: `stormcos-state` holds the
hostname, PID 1 reads it, and it is the node CA's subject CN. A node that
adopts a moved disk boots as somebody else.

Two things answer it, in order:

- **Where the drive is attached.** A slab that arrived over nvme-tcp, iSCSI or
  Fibre Channel is the appliance's export or a LUN shared with another node.
  Never taken, never booted.
- **A claim on the ESP.** `zeroboot claim --device /dev/sda` writes this
  machine's SMBIOS serial — the service tag, the same field stormbootx claims
  on — into `stormcos/claim` on the drive's own ESP. A later boot compares it.
  A claim naming somebody else makes the drive `AnotherNode`, however local it
  is.

A drive with no claim, or a claim this machine cannot check because its
firmware will not say who it is, keeps the old rule: a drive in this chassis is
this node's. That is no worse than before, and refusing to boot there would
invent a new way to fail on a machine whose only fault is an empty DMI field —
which is also why a placeholder serial (`Not Specified`, `To Be Filled By
O.E.M.`, `0`) is treated as no identity rather than as one every machine of
that model shares.

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
no partitions, no signature, and every byte read comes back zero.

Every other verdict is reached from the first and last megabyte, so a boot that
finds nothing to take costs a megabyte a drive — the whole of a four-drive
machine, one of them a 2 TB spinning disk, surveys in 0.2 s. `Blank` is the one
verdict that leads to a format, so it is the one that pays to look properly:
the first 64 MiB whole — one sequential read, and where anything ever done to a
drive leaves a trace — then 64 KiB every gigabyte to the end, and the last
megabyte. On a 2 TB drive that is around two thousand reads: instant on an SSD,
tens of seconds on a spinning disk, against hours to read all of it. It is a
sample and not a proof, which is exactly why it is the last check and not the
only one.

## Status

`survey` — the judgement and the machine it looks at — is implemented and
tested, and is a subcommand, as is `claim`. Format and volume creation are
next; see issue #2 for the design.

One check is missing and is not zeroboot's to make: the loader entry names a
boot volume (`stormblock.volume=boot-cp-01`), and there is no way to list the
volumes inside a slab without attaching it over ublk — `stormblock slab list`
gives the uuid, role, tier and slot counts and stops there. So zeroboot can say
a disk carries a bootloader and that its command line points at the slab on the
same disk, and cannot yet say the slab holds the volume it asks for. Filed as stormblock#108; until it lands, `boot_volume` is reported and not verified.
