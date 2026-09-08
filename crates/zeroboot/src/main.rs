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
    /// Decide, as a step in the boot. This is the entry point the initramfs
    /// calls; the others are for a person.
    ///
    /// Prints `KEY=value` lines for a shell to `eval`, and sets an exit code
    /// so `init` can branch without parsing anything:
    ///
    ///   0  boot-local     a slab of ours that boots; ZB_SLAB says which device
    ///   2  ask-appliance  nothing of ours starts this node
    ///   1  error          could not look at the machine at all
    ///
    /// Human-readable progress goes to stderr and /dev/kmsg, so it lands in the
    /// boot log without polluting what the shell evaluates.
    Boot {
        /// Where to write the drive inventory for whatever comes after
        /// (stormdrive reads this). Empty to write none.
        #[arg(long, default_value = "/run/zeroboot/survey.json")]
        report: String,
        /// Do not record this node's claim on a drive it decides to boot.
        #[arg(long)]
        no_claim: bool,
        #[arg(long, default_value = "/sys")]
        sysfs: PathBuf,
        #[arg(long, default_value = "/dev")]
        dev: PathBuf,
        #[arg(long)]
        stormblock: Option<PathBuf>,
        #[arg(long)]
        node: Option<String>,
    },
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
        Command::Boot { report, no_claim, sysfs, dev, stormblock, node } => {
            let machine = probe::Machine {
                sysfs,
                dev,
                stormblock: stormblock.or_else(|| probe::Machine::default().stormblock),
                only: vec![],
                identity: node,
            };
            std::process::exit(boot(&machine, &report, !no_claim));
        }
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

/// The boot decision, as `init` consumes it.
///
/// Never returns an error to the caller: a boot step that fails with a
/// backtrace has told `init` nothing it can act on. Everything ends in an
/// action and an exit code, and the reason is carried in `ZB_REASON`.
fn boot(machine: &probe::Machine, report: &str, claim_it: bool) -> i32 {
    let survey = match probe::survey(machine) {
        Ok(s) => s,
        Err(e) => {
            say(&format!("cannot look at this machine: {e}"));
            emit(&[("ZB_ACTION", "error"), ("ZB_REASON", &e.to_string())]);
            return 1;
        }
    };

    // Hand the inventory on before deciding anything: whatever this node does
    // next, what it saw is worth having, and a boot that ends in the appliance
    // is exactly when someone wants to know what the drives looked like.
    if !report.is_empty() {
        if let Err(e) = write_report(&survey, report) {
            say(&format!("could not write {report}: {e}"));
        } else {
            say(&format!("drive inventory written to {report}"));
        }
    }

    match survey.intent() {
        Intent::AlreadyMine { drive, slab, slab_id } => {
            let d = survey.drives.iter().find(|d| d.path == drive);
            let volume = d
                .and_then(|d| d.esp.as_ref())
                .and_then(|e| e.boot.as_ref())
                .and_then(|b| b.boot_volume.clone())
                .unwrap_or_default();

            // The node is about to boot off this drive, which is the moment it
            // takes ownership in fact. Recording it is what makes the disk
            // recognisable if it ever turns up in another chassis — and it has
            // to happen here, because there is no operator in a boot.
            if claim_it {
                claim_on_boot(machine, d);
            }

            say(&format!("booting local slab {slab_id} on {slab}"));
            emit(&[
                ("ZB_ACTION", "boot-local"),
                ("ZB_DRIVE", &drive),
                ("ZB_SLAB", &slab),
                ("ZB_SLAB_ID", &slab_id),
                ("ZB_VOLUME", &volume),
            ]);
            0
        }
        // Everything else ends the same way for `init` — ask the appliance —
        // and differs only in what it says about why.
        Intent::MineButNoneBoots { because } => ask(&because.join("; "), ""),
        Intent::NothingToTake { because } => ask(&because.join("; "), ""),
        Intent::TakeOver { path } => ask(
            &format!("{path} is free, and zeroboot cannot yet format it"),
            &path,
        ),
    }
}

fn ask(reason: &str, takeable: &str) -> i32 {
    say(&format!("asking the appliance: {reason}"));
    emit(&[("ZB_ACTION", "ask-appliance"), ("ZB_REASON", reason), ("ZB_TAKEABLE", takeable)]);
    2
}

/// Claim a drive the node is about to boot, when nobody has claimed it yet.
fn claim_on_boot(machine: &probe::Machine, drive: Option<&zeroboot::survey::Drive>) {
    let Some(d) = drive else { return };
    if d.esp.as_ref().is_some_and(|e| e.claim.is_some()) {
        return;
    }
    let Some(me) = machine.identity.clone().or_else(|| probe::machine_identity(&machine.sysfs))
    else {
        say("not claiming: this machine will not say who it is");
        return;
    };
    let claim = esp::Claim { node: me.clone(), claimed: now_rfc3339() };
    match esp::write_claim(Path::new(&d.path), &claim) {
        Ok(()) => say(&format!("claimed {} for {me}", d.path)),
        // A drive that will not take a claim still boots. This is a record for
        // next time, not a precondition for this time.
        Err(e) => say(&format!("could not claim {}: {e}", d.path)),
    }
}

/// `KEY=value` for a shell to `eval`, single-quoted so a reason containing a
/// space, a `$` or a `;` cannot become something the shell runs.
fn emit(pairs: &[(&str, &str)]) {
    for (k, v) in pairs {
        println!("{k}='{}'", v.replace('\n', " ").replace('\'', "'\\''"));
    }
}

/// Progress, to stderr and to the kernel log — the initramfs console reads
/// kmsg, and stdout is the shell's to evaluate.
fn say(line: &str) {
    eprintln!("zeroboot: {line}");
    if let Ok(mut k) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        use std::io::Write;
        let _ = writeln!(k, "zeroboot: {line}");
    }
}

fn write_report(survey: &zeroboot::survey::Survey, path: &str) -> anyhow::Result<()> {
    if let Some(dir) = Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&survey.report())?)?;
    Ok(())
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
