//! zeroboot — a node is functional the moment it boots.
//!
//! Not an installer. Nothing here "installs" a machine and then hands it over
//! working; it boots, looks at its drives, and takes one if it is nobody's.
//!
//! Consumes a stormcos release artifact and produces something that boots and
//! becomes a cluster. Phase 1 is `boot-image`: lay a bootable GPT disk with an
//! ESP (systemd-boot + kernel + initramfs) and the stormblock slab payload,
//! written in pure Rust straight into the image file — no root, no loop
//! devices, no external partitioning/format tooling.

use zeroboot::{bootimage, probe, survey::Intent};

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "zeroboot", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Look at this machine's drives and say what is on each one, and what
    /// would be taken. Reads only — nothing here writes a byte.
    Survey {
        /// Report as JSON rather than a table.
        #[arg(long)]
        json: bool,
        /// Only look at these drives, by name or path (e.g. sda, /dev/sda).
        /// Repeatable; empty = every drive on the machine.
        #[arg(long = "device")]
        devices: Vec<String>,
        /// The stormblock binary that identifies a slab. Defaults to the
        /// static one the initramfs carries, then $PATH. Without it a slab is
        /// still recognised by its magic, but cannot be named — and an
        /// unnameable slab is never taken.
        #[arg(long)]
        stormblock: Option<PathBuf>,
        /// sysfs mount point.
        #[arg(long, default_value = "/sys")]
        sysfs: PathBuf,
        /// Where the device nodes are.
        #[arg(long, default_value = "/dev")]
        dev: PathBuf,
    },
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
        Command::Survey { json, devices, stormblock, sysfs, dev } => {
            let machine = probe::Machine {
                sysfs,
                dev,
                stormblock: stormblock.or_else(|| probe::Machine::default().stormblock),
                only: devices,
            };
            let survey = probe::survey(&machine)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&survey.report())?);
            } else {
                print_survey(&survey);
            }
            Ok(())
        }
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

/// The survey as something to read. A verdict nobody looks at is a verdict
/// nobody checks, and this one decides whether a disk gets formatted.
fn print_survey(survey: &zeroboot::survey::Survey) {
    for d in &survey.drives {
        println!(
            "{:<14} {:>9}  {:<9} {:<18} {}",
            d.path,
            human_size(d.size_bytes),
            if d.rotational { "spinning" } else { "solid" },
            d.model.as_deref().unwrap_or("-"),
            d.verdict,
        );
    }
    println!();
    match survey.intent() {
        Intent::AlreadyMine { drive, slab, slab_id } => {
            println!("already assimilated - would boot slab {slab_id} on {slab} ({drive})");
        }
        Intent::TakeOver { path } => println!("would take {path}"),
        // Nowhere to go is not a failure: the node boots on what the appliance
        // is serving. It still says what it looked at, because "did not
        // assimilate" and "could not" look identical from outside.
        Intent::NothingToTake { because } => {
            println!("nothing to take:");
            for line in because {
                println!("  {line}");
            }
        }
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.2} {}", UNITS[u]) }
}
