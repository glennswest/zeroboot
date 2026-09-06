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

## Status

`survey` — the judgement — is implemented and tested. Format and volume
creation are next; see issue #2 for the design.
