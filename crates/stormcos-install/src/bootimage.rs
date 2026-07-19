//! Bootable disk image assembly.
//!
//! Lays out the stormcos boot artifact as a GPT disk:
//!
//! ```text
//!   p1  ESP (FAT32)  systemd-boot + vmlinuz + initramfs + loader entry
//!   p2  raw          the stormblock slab (release + per-machine volumes)
//! ```
//!
//! UEFI loads systemd-boot from the ESP, which boots the kernel with an
//! initramfs carrying the stormblock client. The initramfs runs
//! `stormblock boot-local` against the slab partition, exports the boot
//! volume as `/dev/ublkb0`, and `switch_root`s into the erofs root — the node
//! runs straight from the image, no install step on the critical path.
//!
//! Everything is written directly into the image file with `gpt` + `fatfs`:
//! no root, no loop devices, no external partitioning or format tooling, so
//! boot media builds on any host including macOS and CI.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fatfs::{FatType, FormatVolumeOptions, FsOptions};
use fscommon::StreamSlice;
use gpt::partition_types;
use serde::Serialize;

const MIB: u64 = 1024 * 1024;
/// GPT reserve at both ends (protective MBR + primary/backup headers), and
/// the alignment every partition starts on.
const RESERVE: u64 = MIB;
const LB_SIZE: u64 = 512;
/// Partition alignment, in **logical blocks** (gpt takes LBAs, not bytes):
/// 1 MiB / 512 = 2048, the conventional alignment.
const ALIGN_LBA: u64 = MIB / LB_SIZE;

pub struct BootImageSpec {
    pub kernel: PathBuf,
    pub initramfs: PathBuf,
    pub bootloader: PathBuf,
    pub slab: PathBuf,
    pub volume: String,
    pub esp_mib: u64,
    pub disk_device: String,
    pub extra_cmdline: Option<String>,
    pub out: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct BootImageReport {
    pub image: PathBuf,
    pub image_bytes: u64,
    pub esp_bytes: u64,
    pub slab_bytes: u64,
    /// Partition the initramfs attaches as the stormblock slab.
    pub slab_partition: String,
    pub boot_volume: String,
    pub cmdline: String,
}

pub fn build(spec: &BootImageSpec) -> anyhow::Result<BootImageReport> {
    for (label, p) in [
        ("kernel", &spec.kernel),
        ("initramfs", &spec.initramfs),
        ("bootloader", &spec.bootloader),
        ("slab", &spec.slab),
    ] {
        anyhow::ensure!(p.is_file(), "{label} not found: {}", p.display());
    }
    anyhow::ensure!(
        !spec.out.exists(),
        "refusing to overwrite {}",
        spec.out.display()
    );
    anyhow::ensure!(spec.esp_mib >= 64, "ESP must be at least 64 MiB for FAT32");

    let esp_bytes = spec.esp_mib * MIB;
    let slab_bytes = std::fs::metadata(&spec.slab)?.len();
    let slab_aligned = slab_bytes.div_ceil(MIB) * MIB;
    let total = RESERVE + esp_bytes + slab_aligned + RESERVE;

    // Sparse file of the full size; GPT + FAT are written into it in place.
    if let Some(parent) = spec.out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    File::create(&spec.out)?.set_len(total)?;

    // --- GPT: ESP + slab payload -------------------------------------------
    let (esp_start, slab_start) = {
        let mut disk = gpt::GptConfig::new()
            .writable(true)
            .create(&spec.out)
            .map_err(|e| anyhow::anyhow!("create GPT: {e}"))?;

        let esp_id = disk
            .add_partition("ESP", esp_bytes, partition_types::EFI, 0, Some(ALIGN_LBA))
            .map_err(|e| anyhow::anyhow!("add ESP partition: {e}"))?;
        let slab_id = disk
            .add_partition(
                "stormblock",
                slab_aligned,
                partition_types::LINUX_FS,
                0,
                Some(ALIGN_LBA),
            )
            .map_err(|e| anyhow::anyhow!("add slab partition: {e}"))?;

        let parts = disk.partitions().clone();
        let esp = parts.get(&esp_id).expect("ESP partition recorded");
        let slab = parts.get(&slab_id).expect("slab partition recorded");
        let offsets = (esp.first_lba * LB_SIZE, slab.first_lba * LB_SIZE);

        disk.write()
            .map_err(|e| anyhow::anyhow!("write GPT: {e}"))?;
        offsets
    };

    // --- ESP: FAT32 + systemd-boot + kernel + initramfs ---------------------
    let cmdline = build_cmdline(spec);
    {
        let img = OpenOptions::new().read(true).write(true).open(&spec.out)?;
        let mut esp = StreamSlice::new(img, esp_start, esp_start + esp_bytes)?;
        fatfs::format_volume(
            &mut esp,
            FormatVolumeOptions::new()
                .fat_type(FatType::Fat32)
                .volume_label(*b"STORMCOS   "),
        )?;

        let fs = fatfs::FileSystem::new(esp, FsOptions::new())?;
        let root = fs.root_dir();

        // UEFI removable-media path: /EFI/BOOT/BOOTX64.EFI
        root.create_dir("EFI")?;
        let boot_dir = root.create_dir("EFI/BOOT")?;
        copy_into(&spec.bootloader, &mut boot_dir.create_file("BOOTX64.EFI")?)?;

        copy_into(&spec.kernel, &mut root.create_file("vmlinuz")?)?;
        copy_into(&spec.initramfs, &mut root.create_file("initramfs.img")?)?;

        // systemd-boot config
        let loader = root.create_dir("loader")?;
        loader
            .create_file("loader.conf")?
            .write_all(b"default stormcos\ntimeout 0\nconsole-mode max\n")?;
        let entries = root.create_dir("loader/entries")?;
        entries.create_file("stormcos.conf")?.write_all(
            format!(
                "title   Storm CoreOS\nlinux   /vmlinuz\ninitrd  /initramfs.img\noptions {cmdline}\n"
            )
            .as_bytes(),
        )?;
        // Dropping the filesystem flushes it.
    }

    // --- slab payload -------------------------------------------------------
    {
        let mut img = OpenOptions::new().read(true).write(true).open(&spec.out)?;
        img.seek(SeekFrom::Start(slab_start))?;
        let mut slab = File::open(&spec.slab)?;
        let copied = std::io::copy(&mut slab, &mut img)?;
        anyhow::ensure!(
            copied == slab_bytes,
            "slab copy short: {copied} of {slab_bytes}"
        );
        img.flush()?;
    }

    Ok(BootImageReport {
        image: spec.out.clone(),
        image_bytes: total,
        esp_bytes,
        slab_bytes,
        slab_partition: format!("{}2", spec.disk_device),
        boot_volume: spec.volume.clone(),
        cmdline,
    })
}

fn build_cmdline(spec: &BootImageSpec) -> String {
    let mut c = format!(
        "console=ttyS0 root=/dev/ublkb0 rd.stormblock.slab={}2 \
         rd.stormblock.meta=/etc/stormblock/meta stormblock.volume={}",
        spec.disk_device, spec.volume
    );
    if let Some(extra) = &spec.extra_cmdline {
        c.push(' ');
        c.push_str(extra);
    }
    c
}

fn copy_into<W: Write>(src: &Path, dst: &mut W) -> anyhow::Result<()> {
    let mut f = File::open(src)?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(dir: &Path, name: &str, len: usize) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, vec![0xABu8; len]).unwrap();
        p
    }

    fn spec_in(dir: &Path) -> BootImageSpec {
        BootImageSpec {
            kernel: fake(dir, "vmlinuz", 4096),
            initramfs: fake(dir, "initramfs.img", 8192),
            bootloader: fake(dir, "systemd-bootx64.efi", 2048),
            slab: fake(dir, "root.slab", 3 * 1024 * 1024),
            volume: "boot-cp-01".into(),
            esp_mib: 64,
            disk_device: "/dev/vda".into(),
            extra_cmdline: None,
            out: dir.join("stormcos.img"),
        }
    }

    #[test]
    fn builds_bootable_layout_with_esp_and_slab() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = spec_in(tmp.path());
        let report = build(&spec).unwrap();

        assert_eq!(report.slab_partition, "/dev/vda2");
        assert!(report.cmdline.contains("root=/dev/ublkb0"));
        assert!(report.cmdline.contains("stormblock.volume=boot-cp-01"));
        assert!(report.cmdline.contains("rd.stormblock.slab=/dev/vda2"));
        assert!(spec.out.is_file());

        // The image must carry a real GPT with our two partitions.
        let disk = gpt::GptConfig::new()
            .writable(false)
            .open(&spec.out)
            .expect("valid GPT");
        let parts = disk.partitions();
        assert_eq!(parts.len(), 2, "ESP + slab");
        let names: Vec<_> = parts.values().map(|p| p.name.clone()).collect();
        assert!(names.iter().any(|n| n == "ESP"));
        assert!(names.iter().any(|n| n == "stormblock"));
    }

    #[test]
    fn esp_is_fat_with_bootloader_kernel_and_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = spec_in(tmp.path());
        build(&spec).unwrap();

        let disk = gpt::GptConfig::new()
            .writable(false)
            .open(&spec.out)
            .unwrap();
        let esp = disk
            .partitions()
            .values()
            .find(|p| p.name == "ESP")
            .unwrap()
            .clone();
        let start = esp.first_lba * LB_SIZE;
        let end = (esp.last_lba + 1) * LB_SIZE;

        let img = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&spec.out)
            .unwrap();
        let slice = StreamSlice::new(img, start, end).unwrap();
        let fs = fatfs::FileSystem::new(slice, FsOptions::new()).expect("FAT on ESP");
        let root = fs.root_dir();

        // UEFI boot path + kernel + initramfs + loader entry all present.
        assert!(root.open_file("EFI/BOOT/BOOTX64.EFI").is_ok());
        assert!(root.open_file("vmlinuz").is_ok());
        assert!(root.open_file("initramfs.img").is_ok());
        let mut entry = String::new();
        root.open_file("loader/entries/stormcos.conf")
            .unwrap()
            .read_to_string(&mut entry)
            .unwrap();
        assert!(entry.contains("linux   /vmlinuz"));
        assert!(entry.contains("stormblock.volume=boot-cp-01"));
    }

    #[test]
    fn slab_payload_lands_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let mut spec = spec_in(tmp.path());
        // Recognizable payload so we can prove it landed at the partition.
        let payload: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&spec.slab, &payload).unwrap();
        spec.out = tmp.path().join("payload.img");
        build(&spec).unwrap();

        let disk = gpt::GptConfig::new()
            .writable(false)
            .open(&spec.out)
            .unwrap();
        let part = disk
            .partitions()
            .values()
            .find(|p| p.name == "stormblock")
            .unwrap()
            .clone();
        let mut img = File::open(&spec.out).unwrap();
        img.seek(SeekFrom::Start(part.first_lba * LB_SIZE)).unwrap();
        let mut got = vec![0u8; payload.len()];
        img.read_exact(&mut got).unwrap();
        assert_eq!(got, payload, "slab must land byte-for-byte");
    }

    #[test]
    fn refuses_to_overwrite_existing_image() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = spec_in(tmp.path());
        std::fs::write(&spec.out, b"existing").unwrap();
        assert!(build(&spec).unwrap_err().to_string().contains("refusing"));
    }
}
