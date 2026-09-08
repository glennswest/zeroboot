//! The ESP of a drive zeroboot laid out, read back.
//!
//! `bootimage` writes a disk: a GPT with an EFI System Partition holding
//! systemd-boot, a kernel, an initramfs and a loader entry, and a stormblock
//! slab in the partition beside it. This reads that back, and it is the
//! difference between two questions the survey has to keep apart:
//!
//! - *is this slab ours?* — the superblock and where the drive is attached
//! - *will this drive boot?* — a bootloader, a kernel, an initramfs, and a
//!   loader entry whose command line points at the slab on this same disk
//!
//! A slab with nothing in it and no bootloader anywhere is `Mine` by the first
//! question and boots nothing by the second, and a node that conflates them
//! declines to ask the appliance for an image it very much needs.
//!
//! It is also where a node writes down that a drive is **its**. Nothing in a
//! stormblock slab records an owner — the superblock has a slab uuid and a
//! device uuid and no node identity — so without a claim written somewhere, a
//! disk moved from one chassis to another is indistinguishable from a disk
//! that was always here. The claim goes in the ESP because that is zeroboot's
//! own territory on the disk, and because the identity that matters (the
//! hostname in `stormcos-state`, which is the node CA's subject CN) lives on
//! the drive that boots, which is the drive that has an ESP.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use fatfs::{FsOptions, Read as _, Write as _};
use fscommon::StreamSlice;
use serde::Serialize;

const LB_SIZE: u64 = 512;

/// Where a node writes down that a drive is its own. Under a directory of our
/// own name so it is obvious to anyone who mounts the ESP looking for why a
/// machine boots the way it does.
pub const CLAIM_PATH: &str = "stormcos/claim";

/// What the drive says about how it boots.
#[derive(Debug, Clone, Serialize)]
pub struct Boot {
    /// The loader entry's `options` line: the kernel command line this disk
    /// boots itself with.
    pub cmdline: String,
    /// `rd.stormblock.slab=` from it — the device the disk expects its slab
    /// on.
    pub slab_device: Option<String>,
    /// `stormblock.volume=` from it — the volume it expects to boot.
    ///
    /// Recorded, not verified. Checking it means listing the volumes inside
    /// the slab, and `stormblock` can only do that by attaching the slab over
    /// ublk; there is no offline `volume list`. So zeroboot can say the disk
    /// asks for `boot-cp-01` and cannot yet say the slab has one.
    pub boot_volume: Option<String>,
}

/// A claim: this drive is this node's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Claim {
    /// The machine, as the firmware names it. Same field stormbootx claims on
    /// and the initramfs falls back to — SMBIOS type 1 serial, which on a Dell
    /// is the service tag.
    pub node: String,
    /// When, so a disk with two histories can be read in order.
    pub claimed: String,
}

impl Claim {
    fn parse(text: &str) -> Option<Claim> {
        let mut node = None;
        let mut claimed = String::new();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "node" => node = Some(v.trim().to_string()).filter(|s: &String| !s.is_empty()),
                "claimed" => claimed = v.trim().to_string(),
                _ => {}
            }
        }
        Some(Claim { node: node?, claimed })
    }

    fn render(&self) -> String {
        format!("node={}\nclaimed={}\n", self.node, self.claimed)
    }
}

/// What was found on a drive's ESP.
#[derive(Debug, Clone, Serialize)]
pub struct Esp {
    /// How this drive boots, when it has everything it takes to.
    pub boot: Option<Boot>,
    /// Who says the drive is theirs, when anyone has said so.
    pub claim: Option<Claim>,
    /// What is missing, when `boot` is `None`. A drive that will not boot
    /// should say which part of it is absent rather than just "no".
    pub missing: Vec<String>,
}

/// Read the ESP of a drive, if it has one.
///
/// `Ok(None)` means the drive has no GPT or no EFI System Partition — an
/// ordinary answer for a drive carrying a bare slab, not an error.
pub fn read(drive: &Path) -> anyhow::Result<Option<Esp>> {
    let Some((start, end)) = esp_extent(drive)? else { return Ok(None) };

    let img = OpenOptions::new().read(true).open(drive)?;
    let slice = StreamSlice::new(img, start, end)?;
    let fs = fatfs::FileSystem::new(slice, FsOptions::new())?;
    let root = fs.root_dir();

    let claim = read_file(&root, CLAIM_PATH).and_then(|t| Claim::parse(&t));

    // systemd-boot's own layout, which is what bootimage writes: an entry per
    // file under /loader/entries, each naming a kernel, an initramfs and a
    // command line.
    let mut missing = Vec::new();
    let entry = loader_entry(&root);
    let Some(entry) = entry else {
        missing.push("no loader entry under /loader/entries".into());
        return Ok(Some(Esp { boot: None, claim, missing }));
    };

    // An entry naming a kernel that is not there boots nothing, and is the
    // shape a half-finished write leaves behind.
    for (what, path) in [("kernel", &entry.linux), ("initramfs", &entry.initrd)] {
        match path {
            Some(p) if read_file(&root, p.trim_start_matches('/')).is_some() => {}
            Some(p) => missing.push(format!("{what} {p} named by the loader entry is not on the ESP")),
            None => missing.push(format!("loader entry names no {what}")),
        }
    }
    if root.open_file("EFI/BOOT/BOOTX64.EFI").is_err() {
        missing.push("no /EFI/BOOT/BOOTX64.EFI".into());
    }
    if !missing.is_empty() {
        return Ok(Some(Esp { boot: None, claim, missing }));
    }

    let cmdline = entry.options.unwrap_or_default();
    Ok(Some(Esp {
        boot: Some(Boot {
            slab_device: cmdline_value(&cmdline, "rd.stormblock.slab="),
            boot_volume: cmdline_value(&cmdline, "stormblock.volume="),
            cmdline,
        }),
        claim,
        missing,
    }))
}

/// Write a claim onto a drive's ESP.
///
/// The one thing in zeroboot that writes to a drive that already exists, and
/// it writes a file into a filesystem that is already there — it does not
/// format, partition, or touch the slab. The caller decides whether claiming
/// is allowed; this only does it.
pub fn write_claim(drive: &Path, claim: &Claim) -> anyhow::Result<()> {
    let (start, end) = esp_extent(drive)?
        .ok_or_else(|| anyhow::anyhow!("{} has no EFI System Partition to claim in", drive.display()))?;

    let img = OpenOptions::new().read(true).write(true).open(drive)?;
    let slice = StreamSlice::new(img, start, end)?;
    let fs = fatfs::FileSystem::new(slice, FsOptions::new())?;
    let root = fs.root_dir();

    if let Some((dir, _)) = CLAIM_PATH.rsplit_once('/') {
        // create_dir on one that exists is an error, and an existing directory
        // is the ordinary case on a re-claim.
        let _ = root.create_dir(dir);
    }
    let mut f = root.create_file(CLAIM_PATH)?;
    f.truncate()?;
    f.write_all(claim.render().as_bytes())?;
    f.flush()?;
    Ok(())
}

/// The byte range of the drive's EFI System Partition.
fn esp_extent(drive: &Path) -> anyhow::Result<Option<(u64, u64)>> {
    let disk = match gpt::GptConfig::new().writable(false).open(drive) {
        Ok(d) => d,
        // No GPT at all. Not an error: a drive carrying a bare slab has none.
        Err(_) => return Ok(None),
    };
    let esp = disk
        .partitions()
        .values()
        .find(|p| p.part_type_guid == gpt::partition_types::EFI)
        .cloned();
    Ok(esp.map(|p| (p.first_lba * LB_SIZE, (p.last_lba + 1) * LB_SIZE)))
}

struct Entry {
    linux: Option<String>,
    initrd: Option<String>,
    options: Option<String>,
}

fn loader_entry<IO: fatfs::ReadWriteSeek, TP: fatfs::TimeProvider, OCC: fatfs::OemCpConverter>(
    root: &fatfs::Dir<'_, IO, TP, OCC>,
) -> Option<Entry> {
    let dir = root.open_dir("loader/entries").ok()?;
    for e in dir.iter().flatten() {
        let name = e.file_name();
        if e.is_dir() || !name.to_ascii_lowercase().ends_with(".conf") {
            continue;
        }
        let Some(text) = read_file(root, &format!("loader/entries/{name}")) else { continue };
        let field = |key: &str| -> Option<String> {
            text.lines()
                .find_map(|l| l.strip_prefix(key))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        return Some(Entry {
            linux: field("linux"),
            initrd: field("initrd"),
            options: field("options"),
        });
    }
    None
}

fn read_file<IO: fatfs::ReadWriteSeek, TP: fatfs::TimeProvider, OCC: fatfs::OemCpConverter>(
    root: &fatfs::Dir<'_, IO, TP, OCC>,
    path: &str,
) -> Option<String> {
    let mut f = root.open_file(path).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

/// Pull `key=value` out of a kernel command line.
fn cmdline_value(cmdline: &str, key: &str) -> Option<String> {
    cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key))
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootimage::{self, BootImageSpec};
    use std::path::PathBuf;

    /// Build a real disk with `bootimage` and read it back. The writer and the
    /// reader are the same repo and drift apart silently otherwise — this is
    /// the only test that makes one prove the other.
    fn a_real_disk(dir: &Path) -> PathBuf {
        for (name, content) in
            [("vmlinuz", "kernel"), ("initramfs.img", "initramfs"), ("boot.efi", "MZ")]
        {
            std::fs::write(dir.join(name), content).unwrap();
        }
        // A slab payload only has to be bytes for this: nothing here opens it.
        std::fs::write(dir.join("root.slab"), vec![0u8; 4 << 20]).unwrap();
        let out = dir.join("disk.img");
        bootimage::build(&BootImageSpec {
            kernel: dir.join("vmlinuz"),
            initramfs: dir.join("initramfs.img"),
            bootloader: dir.join("boot.efi"),
            slab: dir.join("root.slab"),
            volume: "boot-cp-01".into(),
            esp_mib: 64,
            image_store: None,
            writable: vec![],
            disk_device: "/dev/sda".into(),
            extra_cmdline: None,
            out: out.clone(),
        })
        .unwrap();
        out
    }

    #[test]
    fn a_disk_bootimage_built_reads_back_as_bootable() {
        let tmp = tempfile::tempdir().unwrap();
        let disk = a_real_disk(tmp.path());

        let esp = read(&disk).unwrap().expect("the disk has an ESP");
        assert!(esp.missing.is_empty(), "{:?}", esp.missing);
        let boot = esp.boot.expect("it boots");
        assert_eq!(boot.slab_device.as_deref(), Some("/dev/sda2"));
        assert_eq!(boot.boot_volume.as_deref(), Some("boot-cp-01"));
        assert!(boot.cmdline.contains("root=/dev/ublkb0"), "{}", boot.cmdline);
        assert!(esp.claim.is_none(), "nobody has claimed it yet");
    }

    /// The failure that matters: everything is there except the kernel the
    /// entry names. The drive has an ESP, a bootloader and a loader entry, and
    /// boots nothing.
    #[test]
    fn an_entry_naming_a_kernel_that_is_not_there_does_not_boot() {
        let tmp = tempfile::tempdir().unwrap();
        let disk = a_real_disk(tmp.path());

        let (start, end) = esp_extent(&disk).unwrap().unwrap();
        let img = OpenOptions::new().read(true).write(true).open(&disk).unwrap();
        let fs = fatfs::FileSystem::new(StreamSlice::new(img, start, end).unwrap(), FsOptions::new())
            .unwrap();
        fs.root_dir().remove("vmlinuz").unwrap();
        drop(fs);

        let esp = read(&disk).unwrap().unwrap();
        assert!(esp.boot.is_none());
        assert!(
            esp.missing.iter().any(|m| m.contains("kernel /vmlinuz")),
            "{:?}",
            esp.missing
        );
    }

    #[test]
    fn a_claim_survives_being_written_and_read() {
        let tmp = tempfile::tempdir().unwrap();
        let disk = a_real_disk(tmp.path());

        let claim = Claim { node: "4F2XYZ1".into(), claimed: "2026-09-08T12:00:00Z".into() };
        write_claim(&disk, &claim).unwrap();
        assert_eq!(read(&disk).unwrap().unwrap().claim, Some(claim));

        // And claiming again replaces it rather than appending to it — a disk
        // that changed hands legitimately must not read as two owners.
        let second = Claim { node: "9ABCDE2".into(), claimed: "2026-09-09T00:00:00Z".into() };
        write_claim(&disk, &second).unwrap();
        assert_eq!(read(&disk).unwrap().unwrap().claim, Some(second));

        // Still bootable: claiming writes a file, it does not disturb the disk.
        assert!(read(&disk).unwrap().unwrap().boot.is_some());
    }

    #[test]
    fn a_drive_with_no_gpt_has_no_esp_and_that_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("bare.img");
        std::fs::write(&bare, vec![0u8; 4 << 20]).unwrap();
        assert!(read(&bare).unwrap().is_none());
    }

    #[test]
    fn a_claim_is_parsed_and_junk_is_not() {
        assert_eq!(
            Claim::parse("node=4F2XYZ1\nclaimed=2026-09-08T12:00:00Z\n"),
            Some(Claim { node: "4F2XYZ1".into(), claimed: "2026-09-08T12:00:00Z".into() })
        );
        // A claim with no node names nobody and is not a claim.
        assert_eq!(Claim::parse("claimed=2026-09-08T12:00:00Z\n"), None);
        assert_eq!(Claim::parse("node=\n"), None);
        assert_eq!(Claim::parse(""), None);
    }

    #[test]
    fn cmdline_values_are_pulled_out_whole() {
        let c = "console=ttyS0 rd.stormblock.slab=/dev/sda2 stormblock.volume=boot-cp-01 quiet";
        assert_eq!(cmdline_value(c, "rd.stormblock.slab="), Some("/dev/sda2".into()));
        assert_eq!(cmdline_value(c, "stormblock.volume="), Some("boot-cp-01".into()));
        assert_eq!(cmdline_value(c, "rd.stormblock.boothost="), None);
    }
}
