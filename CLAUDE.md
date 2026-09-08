# CLAUDE.md — zeroboot

**A node is functional the moment it boots.** Not an installer. See `README.md`
for the design and `CHANGELOG.md` for what has happened.

## Build and test

Cross-project rules apply: **every build and test runs on `root@dev.g8.lo`**,
never on the Mac. The storage path is Linux-only, so a macOS build skips the
code most likely to be wrong.

```
commit → push → pull on dev → build and test on dev
export CARGO_TARGET_DIR=/build/cargo/zeroboot
```

Nothing persists on dev's SSD root; `/build` is the 2 TB drive. Never write a
disk image into `/tmp` on dev — it is a tmpfs at half of RAM.

## Version

`0.1.0`, in the workspace `Cargo.toml` (`workspace.package.version`). One
location.

## Shape

| module | what it is |
|---|---|
| `survey.rs` | the judgement: what is on each drive, whose it is, what follows. Pure; no I/O. |
| `probe.rs` | the eyes: `/sys/block`, the bytes on each drive, `stormblock slab list`. Read-only. |
| `esp.rs` | reading and writing the ESP of a drive zeroboot laid out — loader entry, partition GUIDs, and the owner record. |
| `bootimage.rs` | building a bootable disk image, in pure Rust. Also the writer whose output `esp.rs` reads back. |

`survey` decides nothing about how to boot; it says what is there. Everything
destructive is opt-in and named.

**zeroboot is a step in the boot, not a command anyone runs.** `zeroboot boot`
is the entry point: the initramfs carries the binary at `/sbin/zeroboot` and
`/init` calls it, evaluating `KEY='value'` off stdout and branching on the exit
code. That makes stdout an interface with a shell on the other end — every
diagnostic goes to stderr, every value is single-quoted, and
`tests/boot_contract.rs` runs the real binary through `/bin/sh` to keep it so.
`survey` and `claim` are for a person with a machine in front of them.

## Work plan

- [x] `survey` — the judgement (#3)
- [x] `probe` — drive enumeration, verdicts, `zeroboot survey` (#3)
- [x] `Intent::AlreadyMine` carries the device that actually boots
- [x] `Mine` is not the same as bootable — `esp.rs` reads the loader entry, and
      `Intent::MineButNoneBoots` is the third answer (issue #2 comment)
- [x] whose slab is it — a claim in the ESP, checked against the SMBIOS service
      tag, so a disk moved between chassis does not change hands silently
      (issue #2 comment)
- [x] `zeroboot boot` — the entry point `/init` calls, and the binary actually
      in the initramfs
- [x] a drive inventory for stormdrive, keyed on wwid/serial/GPT GUIDs
- [x] name the flow-over target so stormblock can assimilate (#2) — zeroboot
      does not format; `boot-local --local-disk` already does, and what was
      missing was which drive
- [ ] `/init` calling the hook, and passing `--local-disk` — stormblock#109
- [ ] two drives rather than one: `available()` returns a list and only the
      first is offered (#2, and the R230 has one drive so it cannot be tried
      here)

## Known limits, deliberately

- **A slab's volumes cannot be listed offline.** `stormblock slab list` gives
  the uuid, role, tier and slot counts; there is no `volume list` that reads a
  slab from a file without attaching it over ublk. So zeroboot can check that a
  disk carries a bootloader and that its cmdline points at the slab on the same
  disk, but not that the slab holds the boot volume the cmdline names. Filed as stormblock#108.
- **zeroboot never formats.** `stormblock boot-local --local-disk` lays the
  data and system slabs and migrates the extents; zeroboot decides which drive
  is safe to hand it. Reimplementing the format here would duplicate a careful
  implementation, its identity guard included.
- **A flow-over drive has no ESP**, so no claim can be written on one and it
  falls back to "local, therefore this node's". Only a disk laid out by
  `boot-image` carries a claim.
- **`/init` does not call zeroboot yet** (stormblock#109). Until it does, the
  binary ships in the initramfs and nothing invokes it, and the boot falls back
  to stormblock's own slab probe — which is the behaviour that exists today.
- **Ownership is claimed in the ESP**, so it covers the boot drive. A data slab
  with no ESP still falls back to "local, therefore this node's". That is the
  right scope: the identity that matters is the hostname in `stormcos-state`,
  and that lives on the disk that boots.
