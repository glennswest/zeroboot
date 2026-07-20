//! stormcos-install — the Storm CoreOS installer (our `openshift-install`).
//!
//! Consumes a stormcos release artifact and produces something that boots and
//! becomes a cluster. Phase 1 is `boot-image`: lay a bootable GPT disk with an
//! ESP (systemd-boot + kernel + initramfs) and the stormblock slab payload,
//! written in pure Rust straight into the image file — no root, no loop
//! devices, no external partitioning/format tooling.

use stormcos_install::bootimage;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "stormcos-install", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a bootable disk image: ESP (systemd-boot + kernel + initramfs)
    /// plus the stormblock slab payload.
    BootImage {
        /// Kernel image (vmlinuz) for the pinned release kernel.
        #[arg(long)]
        kernel: PathBuf,
        /// Initramfs carrying the stormblock client + boot handoff.
        #[arg(long)]
        initramfs: PathBuf,
        /// EFI bootloader binary (systemd-bootx64.efi).
        #[arg(long)]
        bootloader: PathBuf,
        /// stormblock slab holding the release volumes (root.slab).
        #[arg(long)]
        slab: PathBuf,
        /// Boot volume to export as root, by name or UUID (e.g. boot-cp-01).
        #[arg(long)]
        volume: String,
        /// ESP size in MiB.
        #[arg(long, default_value = "256")]
        esp_mib: u64,
        /// Preloaded image-store volume to export at boot, by name (e.g.
        /// image-store-stormcos-0.1.0). Without it the store is never exported
        /// and CRI-O cannot see any preloaded image.
        #[arg(long)]
        image_store: Option<String>,
        /// Writable thin volume to export + mount, as volume:mount (e.g.
        /// var-stormcos-0.1.0:/var). Repeatable; empty = none.
        #[arg(long = "writable")]
        writable: Vec<String>,
        /// Guest device the disk appears as; the slab partition becomes
        /// <disk>2 on the kernel cmdline.
        #[arg(long, default_value = "/dev/vda")]
        disk_device: String,
        /// Extra kernel cmdline arguments.
        #[arg(long)]
        cmdline: Option<String>,
        /// Output image path.
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let cli = Cli::parse();

    match cli.command {
        Command::BootImage {
            kernel,
            initramfs,
            bootloader,
            slab,
            volume,
            esp_mib,
            image_store,
            writable,
            disk_device,
            cmdline,
            out,
        } => {
            let writable = writable
                .iter()
                .map(|w| {
                    let (vol, mnt) = w
                        .split_once(':')
                        .ok_or_else(|| anyhow::anyhow!("--writable must be volume:mount, got {w}"))?;
                    Ok(bootimage::WritableMount {
                        volume: vol.to_string(),
                        mount: mnt.to_string(),
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let report = bootimage::build(&bootimage::BootImageSpec {
                kernel,
                initramfs,
                bootloader,
                slab,
                volume,
                esp_mib,
                image_store,
                writable,
                disk_device,
                extra_cmdline: cmdline,
                out,
            })?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
    }
}
