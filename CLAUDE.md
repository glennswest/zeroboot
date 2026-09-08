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
| `esp.rs` | reading and writing the ESP of a drive zeroboot laid out — loader entry, and the owner record. |
| `bootimage.rs` | building a bootable disk image, in pure Rust. Also the writer whose output `esp.rs` reads back. |

`survey` decides nothing about how to boot; it says what is there. Everything
destructive is opt-in and named.

## Work plan

- [x] `survey` — the judgement (#3)
- [x] `probe` — drive enumeration, verdicts, `zeroboot survey` (#3)
- [x] `Intent::AlreadyMine` carries the device that actually boots
- [x] `Mine` is not the same as bootable — `esp.rs` reads the loader entry, and
      `Intent::MineButNoneBoots` is the third answer (issue #2 comment)
- [x] whose slab is it — a claim in the ESP, checked against the SMBIOS service
      tag, so a disk moved between chassis does not change hands silently
      (issue #2 comment)
- [ ] format and volume creation — assimilation proper (#2)

## Known limits, deliberately

- **A slab's volumes cannot be listed offline.** `stormblock slab list` gives
  the uuid, role, tier and slot counts; there is no `volume list` that reads a
  slab from a file without attaching it over ublk. So zeroboot can check that a
  disk carries a bootloader and that its cmdline points at the slab on the same
  disk, but not that the slab holds the boot volume the cmdline names. Filed as stormblock#108.
- **Ownership is claimed in the ESP**, so it covers the boot drive. A data slab
  with no ESP still falls back to "local, therefore this node's". That is the
  right scope: the identity that matters is the hostname in `stormcos-state`,
  and that lives on the disk that boots.
