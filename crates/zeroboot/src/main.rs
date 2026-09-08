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

use zeroboot::{bootimage, esp, probe, survey::Intent};

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

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
        /// Who this machine is. Defaults to the SMBIOS type 1 serial — on a
        /// Dell, the service tag — which is what stormbootx claims on.
        #[arg(long)]
        node: Option<String>,
    },
    /// Write this node's claim onto a drive's ESP, so a later boot — here or
    /// in another chassis — can tell whose the drive is.
    ///
    /// Nothing in a stormblock slab records an owner, so without this a disk
    /// moved between machines is indistinguishable from one that was always
    /// there. Writes one small file into the ESP that is already on the drive:
    /// it does not format, partition, or touch the slab.
    Claim {
        /// The drive to claim (e.g. /dev/sda). The whole disk, not a partition.
        #[arg(long)]
        device: PathBuf,
        /// Who to claim it for. Defaults to this machine's SMBIOS serial.
        #[arg(long)]
        node: Option<String>,
        /// sysfs mount point, for reading this machine's identity.
        #[arg(long, default_value = "/sys")]
        sysfs: PathBuf,
        /// Take a drive another node has claimed. The claim exists to stop
        /// exactly this, so it has to be typed.
        #[arg(long)]
        force: bool,
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
        Command::Survey { json, devices, stormblock, sysfs, dev, node } => {
            let machine = probe::Machine {
                sysfs,
                dev,
                stormblock: stormblock.or_else(|| probe::Machine::default().stormblock),
                only: devices,
                identity: node,
            };
            let survey = probe::survey(&machine)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&survey.report())?);
            } else {
                print_survey(&survey);
            }
            Ok(())
        }
        Command::Claim { device, node, sysfs, force } => claim(&device, node, &sysfs, force),
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

/// Write this node's name onto a drive it owns.
///
/// The guards are the point. A claim that can be written over anything records
/// nothing, and a claim written onto a drive that is not ours is how you take
/// a disk by accident rather than on purpose.
fn claim(device: &Path, node: Option<String>, sysfs: &Path, force: bool) -> anyhow::Result<()> {
    let me = node.or_else(|| probe::machine_identity(sysfs)).ok_or_else(|| {
        anyhow::anyhow!(
            "this machine will not say who it is - no usable SMBIOS serial in \
             {}/class/dmi/id/product_serial. Pass --node.",
            sysfs.display()
        )
    })?;

    let existing = esp::read(device)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no EFI System Partition to claim in - a claim lives on the ESP, \
             so a drive carrying a bare slab has nowhere to put one",
            device.display()
        )
    })?;

    if let Some(held) = &existing.claim {
        if held.node != me && !force {
            anyhow::bail!(
                "{} is claimed by node {} (on {}). That claim is what stops a disk \
                 changing hands by accident; pass --force to take it anyway.",
                device.display(),
                held.node,
                held.claimed
            );
        }
        if held.node == me {
            println!("{} is already claimed by {me}", device.display());
            return Ok(());
        }
    }

    let claim = esp::Claim { node: me.clone(), claimed: now_rfc3339() };
    esp::write_claim(device, &claim)?;
    println!("{} claimed by {me} at {}", device.display(), claim.claimed);
    Ok(())
}

/// RFC 3339, from the clock, with no dependency to carry into an initramfs.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (mut y, mut d) = (1970i64, days as i64);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if d < len {
            break;
        }
        d -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0;
    while d >= months[m] {
        d -= months[m];
        m += 1;
    }
    format!(
        "{y:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        m + 1,
        d + 1,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
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
        if let Some(e) = &d.esp {
            if let Some(c) = &e.claim {
                println!("{:16}claimed by {} at {}", "", c.node, c.claimed);
            }
            match &e.boot {
                Some(b) => println!(
                    "{:16}boots {} from {}",
                    "",
                    b.boot_volume.as_deref().unwrap_or("(no volume named)"),
                    b.slab_device.as_deref().unwrap_or("(no slab named)"),
                ),
                None if !e.missing.is_empty() => {
                    println!("{:16}does not boot: {}", "", e.missing.join("; "));
                }
                None => {}
            }
        }
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
        // Ours, and none of it starts the node. Ask the appliance, exactly as
        // a node with no disk does.
        Intent::MineButNoneBoots { because } => {
            println!("ours, but nothing here boots - would ask the appliance:");
            for line in because {
                println!("  {line}");
            }
        }
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
