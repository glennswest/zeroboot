//! The contract `init` depends on.
//!
//! `zeroboot boot` is not run by a person: the initramfs calls it, evaluates
//! its stdout in a busybox shell, and branches on its exit code. That makes
//! stdout an interface with a shell on the other end of it, and the failure
//! mode is not a wrong answer — it is PID 1 executing a line that was meant to
//! be a log message. This runs the real binary and checks what comes out.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use zeroboot::bootimage::{self, BootImageSpec};

const SLAB_MAGIC: &[u8] = b"STRMSLAB";

/// A machine with one disk that zeroboot itself laid out, and a stormblock
/// that answers about it the way the real one does.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let f = Fixture { root };
        fs::create_dir_all(f.at("sys/block")).unwrap();
        fs::create_dir_all(f.at("dev")).unwrap();
        f.build_disk();
        f
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn build_disk(&self) {
        let work = self.at("build");
        fs::create_dir_all(&work).unwrap();
        for (n, c) in [("vmlinuz", "k"), ("initramfs.img", "i"), ("boot.efi", "MZ")] {
            fs::write(work.join(n), c).unwrap();
        }
        let mut slab = vec![0u8; 4 << 20];
        slab[..SLAB_MAGIC.len()].copy_from_slice(SLAB_MAGIC);
        fs::write(work.join("root.slab"), &slab).unwrap();

        let dev = self.at("dev/sda");
        bootimage::build(&BootImageSpec {
            kernel: work.join("vmlinuz"),
            initramfs: work.join("initramfs.img"),
            bootloader: work.join("boot.efi"),
            slab: work.join("root.slab"),
            volume: "boot-cp-01".into(),
            esp_mib: 64,
            image_store: None,
            writable: vec![],
            disk_device: "/dev/sda".into(),
            extra_cmdline: None,
            out: dev.clone(),
        })
        .unwrap();

        let sys = self.at("sys/block/sda");
        fs::create_dir_all(sys.join("queue")).unwrap();
        fs::create_dir_all(sys.join("device")).unwrap();
        fs::write(sys.join("size"), format!("{}\n", fs::metadata(&dev).unwrap().len() / 512))
            .unwrap();
        fs::write(sys.join("queue/rotational"), "1\n").unwrap();
        fs::write(sys.join("removable"), "0\n").unwrap();
        // A model with a quote and a space in it: this ends up in a shell.
        fs::write(sys.join("device/model"), "WDC WD20 'EFAX'\n").unwrap();
        fs::write(sys.join("wwid"), "naa.5000c500a1b2c3d4\n").unwrap();
        fs::write(sys.join("device/serial"), "WD-WCC4N1234567\n").unwrap();
        fs::create_dir_all(self.at("sys/class/dmi/id")).unwrap();
        fs::write(self.at("sys/class/dmi/id/product_serial"), "R230-SVCTAG\n").unwrap();

        let disk = gpt::GptConfig::new().writable(false).open(&dev).unwrap();
        let parts: Vec<_> = disk.partitions().iter().map(|(i, p)| (*i, p.clone())).collect();
        drop(disk);
        for (i, p) in parts {
            let child = format!("sda{i}");
            fs::create_dir_all(sys.join(&child)).unwrap();
            fs::write(sys.join(&child).join("partition"), format!("{i}\n")).unwrap();
            let mut src = fs::File::open(&dev).unwrap();
            src.seek(SeekFrom::Start(p.first_lba * 512)).unwrap();
            let mut buf = vec![0u8; ((p.last_lba + 1 - p.first_lba) * 512) as usize];
            src.read_exact(&mut buf).unwrap();
            fs::write(self.at("dev").join(&child), &buf).unwrap();
        }
    }

    fn stormblock(&self, answer: &str) -> PathBuf {
        let p = self.at("stormblock");
        fs::write(&p, format!("#!/bin/sh\nprintf '%s\\n' \"{answer}\"\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn boot(&self, sb: &Path) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_zeroboot"))
            .args(["boot", "--sysfs"])
            .arg(self.at("sys"))
            .arg("--dev")
            .arg(self.at("dev"))
            .arg("--stormblock")
            .arg(sb)
            .arg("--report")
            .arg(self.at("run/survey.json"))
            .output()
            .unwrap()
    }
}

/// Every line of stdout must be a `KEY='value'` a shell can evaluate, and
/// nothing else. A log line here is a line PID 1 tries to run.
#[test]
fn stdout_is_only_shell_assignments() {
    let f = Fixture::new();
    let sb = f.stormblock(
        "sda2: slab 7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60 (role=system, tier=hot, 40 slots, 3 free)",
    );
    let out = f.boot(&sb);
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(!stdout.trim().is_empty(), "it must say something");
    for line in stdout.lines() {
        let (k, v) = line.split_once('=').unwrap_or_else(|| panic!("not an assignment: {line:?}"));
        assert!(
            k.starts_with("ZB_") && k.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
            "not a key: {k:?}"
        );
        assert!(v.starts_with('\'') && v.ends_with('\''), "not quoted: {line:?}");
    }
    assert_eq!(out.status.code(), Some(0), "a disk of ours that boots is exit 0");
    assert!(stdout.contains("ZB_ACTION='boot-local'"), "{stdout}");
    assert!(stdout.contains("ZB_VOLUME='boot-cp-01'"), "{stdout}");
    assert!(stdout.contains("/dev/sda2'"), "the slab device init boots: {stdout}");
}

/// And the shell agrees: evaluating it sets the variables and runs nothing.
#[test]
fn a_real_shell_can_evaluate_it() {
    let f = Fixture::new();
    let sb = f.stormblock(
        "sda2: slab 7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60 (role=system, tier=hot, 40 slots, 3 free)",
    );
    let stdout = String::from_utf8(f.boot(&sb).stdout).unwrap();

    let script = format!("{stdout}\nprintf '%s|%s\\n' \"$ZB_ACTION\" \"$ZB_VOLUME\"\n");
    let out = Command::new("/bin/sh").arg("-c").arg(&script).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "boot-local|boot-cp-01");
}

/// Nothing of ours boots: exit 2, and a reason carried whole through the
/// shell even though it is full of spaces, commas and slashes.
#[test]
fn nothing_of_ours_asks_the_appliance() {
    let f = Fixture::new();
    let sb = f.stormblock("sda2: not a slab (bad slab magic)");
    let out = f.boot(&sb);
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert!(stdout.contains("ZB_ACTION='ask-appliance'"), "{stdout}");

    // The reason is a sentence with spaces, slashes and punctuation in it.
    // What matters is not which sentence — that is the verdict's business —
    // but that the shell hands back exactly what zeroboot wrote.
    let quoted = stdout
        .lines()
        .find_map(|l| l.strip_prefix("ZB_REASON="))
        .expect("a reason is given");
    let written = quoted.trim_matches('\'').replace("'\\''", "'");
    assert!(written.contains(' '), "a reason worth quoting: {written:?}");

    let script = format!("{stdout}\nprintf '%s' \"$ZB_REASON\"\n");
    let sh = Command::new("/bin/sh").arg("-c").arg(&script).output().unwrap();
    assert!(sh.status.success(), "{}", String::from_utf8_lossy(&sh.stderr));
    assert_eq!(
        String::from_utf8(sh.stdout).unwrap(),
        written,
        "the reason survives the shell whole"
    );
}

/// The inventory stormdrive reads: stable identity, not device paths.
#[test]
fn the_report_carries_identity_that_outlives_a_device_path() {
    let f = Fixture::new();
    let sb = f.stormblock(
        "sda2: slab 7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60 (role=system, tier=hot, 40 slots, 3 free)",
    );
    f.boot(&sb);

    let text = fs::read_to_string(f.at("run/survey.json")).expect("the report is written");
    let v: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let d = &v["drives"][0];

    assert_eq!(d["wwid"], "naa.5000c500a1b2c3d4");
    assert_eq!(d["serial"], "WD-WCC4N1234567");
    assert_eq!(d["model"], "WDC WD20 'EFAX'");
    let table = &d["table"];
    assert_eq!(table["disk_guid"].as_str().unwrap().len(), 36, "a real GPT disk GUID");
    let parts = table["partitions"].as_array().unwrap();
    assert_eq!(parts.len(), 2, "ESP + slab");
    assert_eq!(parts[0]["name"], "ESP");
    assert_eq!(parts[0]["index"], 1);
    assert_eq!(parts[1]["name"], "stormblock");
    // Every partition names itself, and no two the same.
    assert_ne!(parts[0]["guid"], parts[1]["guid"]);
    assert_eq!(parts[1]["guid"].as_str().unwrap().len(), 36);
    assert_eq!(v["intent"]["intent"], "already_mine");
}

/// A boot claims the drive it boots, because there is no operator in a boot to
/// type `zeroboot claim`. Without this the claim is never written on a real
/// machine and the ownership check has nothing to check.
#[test]
fn booting_a_drive_claims_it() {
    let f = Fixture::new();
    let sb = f.stormblock(
        "sda2: slab 7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60 (role=system, tier=hot, 40 slots, 3 free)",
    );
    assert!(
        zeroboot::esp::read(&f.at("dev/sda")).unwrap().unwrap().claim.is_none(),
        "nobody has claimed it yet"
    );

    f.boot(&sb);

    let claim = zeroboot::esp::read(&f.at("dev/sda")).unwrap().unwrap().claim;
    assert_eq!(claim.map(|c| c.node), Some("R230-SVCTAG".to_string()));
}
