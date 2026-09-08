//! Looking at a real machine.
//!
//! [`survey`] is the judgement; this is the eyes. It walks `/sys/block`, reads
//! what each drive says about itself, reads the first and last of what is on
//! it, and produces the [`Survey`] the judgement acts on. It is the half that
//! decides nothing.
//!
//! Two rules run through all of it, and both were learned on a Dell R230.
//!
//! **Positive evidence, never absence of a known error.** The initramfs slab
//! probe used to fall back when `stormblock slab list` said *"not a slab"*,
//! which assumes the only alternative to a slab is a disk that answers. That
//! machine's `/dev/sda` is sometimes a 2 TB WD disk and sometimes the iDRAC
//! virtual floppy, and an empty removable drive answers `ENOMEDIUM` rather
//! than "not a slab" — so the probe passed and the boot died. Every verdict
//! here rests on something that was read, and anything else is
//! [`Verdict::Foreign`] or [`Verdict::Unreadable`].
//!
//! **Where a drive is attached is part of what it is.** A stormblock slab on a
//! disk inside this chassis was written by this node in an earlier life;
//! nobody else can reach it. The same slab arriving over nvme-tcp, iSCSI or
//! Fibre Channel is somebody else's by construction — the appliance's export,
//! or a LUN shared with another node — and formatting it loses both.
//!
//! Nothing here writes a byte. Every device is opened read-only.

use crate::esp;
use crate::survey::{Drive, Survey, Verdict};

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where to look, and what with.
///
/// Real machines take [`Machine::default`]. The paths are fields rather than
/// constants so the tests can hand it a fabricated `/sys` and `/dev`: every
/// verdict below is then exercised on a laptop, without a disk to lose.
#[derive(Debug, Clone)]
pub struct Machine {
    /// Mount point of sysfs.
    pub sysfs: PathBuf,
    /// Where the device nodes are.
    pub dev: PathBuf,
    /// The stormblock binary that identifies a slab. The initramfs already
    /// carries a static one and already uses it for exactly this probe, so
    /// zeroboot borrows it rather than taking a dependency on stormblock and
    /// growing a second implementation of the superblock to drift.
    pub stormblock: Option<PathBuf>,
    /// Restrict the survey to these drives, by bare name or full path. Empty
    /// means every drive the machine has.
    pub only: Vec<String>,
    /// Who this machine is. `None` means read it where the firmware put it.
    ///
    /// It decides whether a claim written on a drive is this node's own, so
    /// getting it from the same place stormbootx claims on matters more than
    /// getting it cheaply: two implementations of "who is this machine" drift,
    /// and the one in firmware is the one proven on hardware.
    pub identity: Option<String>,
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            sysfs: PathBuf::from("/sys"),
            dev: PathBuf::from("/dev"),
            stormblock: find_stormblock(),
            only: Vec::new(),
            identity: None,
        }
    }
}

/// Placeholders firmware writes when it has nothing to say. Treating one of
/// these as an identity would make every machine of a given model claim to be
/// the same node, which is worse than having no identity at all.
const NOT_AN_IDENTITY: [&str; 7] = [
    "not specified",
    "to be filled by o.e.m.",
    "system serial number",
    "default string",
    "none",
    "0",
    "unknown",
];

/// Who this machine is, as the firmware names it: SMBIOS type 1 serial, which
/// on a Dell is the service tag. The same field stormbootx claims on and the
/// initramfs falls back to.
pub fn machine_identity(sysfs: &Path) -> Option<String> {
    read_trimmed(sysfs.join("class/dmi/id/product_serial"))
        .filter(|s| !s.is_empty())
        .filter(|s| !NOT_AN_IDENTITY.contains(&s.to_ascii_lowercase().as_str()))
}

/// The initramfs path first, since that is where this runs.
fn find_stormblock() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> =
        ["/usr/sbin/stormblock", "/sbin/stormblock", "/usr/bin/stormblock"]
            .iter()
            .map(PathBuf::from)
            .collect();
    if let Ok(path) = std::env::var("PATH") {
        candidates.extend(path.split(':').filter(|d| !d.is_empty()).map(|d| {
            Path::new(d).join("stormblock")
        }));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Look at every drive on the machine.
///
/// Never fails on a drive: a drive that cannot be read is a
/// [`Verdict::Unreadable`] in the report, because "one disk would not answer"
/// is a thing to say and not a reason to stop looking at the others. The error
/// case is not being able to see `/sys/block` at all, which means this is not
/// running where it thinks it is.
pub fn survey(m: &Machine) -> anyhow::Result<Survey> {
    let block = m.sysfs.join("block");
    let entries = fs::read_dir(&block)
        .map_err(|e| anyhow::anyhow!("cannot list drives at {}: {e}", block.display()))?;

    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !is_not_a_drive(n))
        .collect();
    names.sort();

    let me = m.identity.clone().or_else(|| machine_identity(&m.sysfs));
    let drives = names
        .into_iter()
        .filter(|n| m.wants(n))
        .map(|name| look_at(m, me.as_deref(), &name))
        .collect();

    Ok(Survey { drives })
}

impl Machine {
    fn wants(&self, name: &str) -> bool {
        if self.only.is_empty() {
            return true;
        }
        self.only.iter().any(|w| {
            w == name || Path::new(w).file_name().is_some_and(|f| f == name)
        })
    }
}

/// Names under `/sys/block` that are not a drive, or are a second view of one.
///
/// Taking any of these would be either meaningless or a way to format the same
/// disk twice through a different name.
fn is_not_a_drive(name: &str) -> bool {
    const NOT_DRIVES: [&str; 9] =
        ["loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd", "zd"];
    NOT_DRIVES.iter().any(|p| name.starts_with(p))
}

fn look_at(m: &Machine, me: Option<&str>, name: &str) -> Drive {
    let sys = m.sysfs.join("block").join(name);
    let path = m.dev.join(name);

    // /sys reports capacity in 512-byte sectors regardless of what the drive's
    // own logical block size is.
    let size_bytes = read_u64(sys.join("size")).unwrap_or(0) * 512;
    let rotational = read_u64(sys.join("queue/rotational")).unwrap_or(1) == 1;
    let removable = read_u64(sys.join("removable")).unwrap_or(0) == 1;
    let model = read_trimmed(sys.join("device/model")).filter(|s| !s.is_empty());

    // Read once and hand it to the judgement: the claim on it decides whose
    // the drive is, and the loader entry decides whether it boots.
    let found = esp::read(&path).ok().flatten();

    let verdict = judge(m, &sys, name, &path, size_bytes, removable, me, found.as_ref());

    Drive {
        path: path.to_string_lossy().into_owned(),
        size_bytes,
        rotational,
        model,
        esp: found,
        verdict,
    }
}

/// What this drive is. The order is the argument: every branch that ends in
/// "leave it alone" is taken before anything can conclude "blank".
#[allow(clippy::too_many_arguments)]
fn judge(
    m: &Machine,
    sys: &Path,
    name: &str,
    path: &Path,
    size_bytes: u64,
    removable: bool,
    me: Option<&str>,
    found: Option<&esp::Esp>,
) -> Verdict {
    // An empty drive bay is not an empty drive. The iDRAC virtual floppy is
    // present, is /dev/sdb, and has no medium; it reports zero sectors and
    // answers ENOMEDIUM to anyone who opens it.
    if size_bytes == 0 {
        return Verdict::Unreadable { why: "no medium (reports zero sectors)".into() };
    }

    // Removable media is somebody's, even when it is empty: a USB stick left
    // in the front panel is not free space, and the virtual floppy is the same
    // device class. Nothing removable is ever taken.
    if removable {
        let what = match read_trimmed(sys.join("device/model")).filter(|s| !s.is_empty()) {
            Some(model) => format!("removable media ({model})"),
            None => "removable media".to_string(),
        };
        return Verdict::Foreign { what };
    }

    // Where it is attached decides whose it is, before what is on it decides
    // what it is.
    let remote = remote_transport(sys);

    // A disk zeroboot itself laid down carries a GPT with the slab in a
    // partition, so the slab is looked for on the partitions as well as on the
    // whole device — otherwise a node booting off the disk it assimilated onto
    // sees its own work as a foreign partition table.
    let mut probes: Vec<PathBuf> = vec![path.to_path_buf()];
    let parts = partitions(sys, name);
    probes.extend(parts.iter().map(|p| path.with_file_name(p)));

    for probe in &probes {
        let sniffed = match sniff(probe) {
            Ok(s) => s,
            // Only the whole device failing to read is a verdict; a partition
            // that will not open just is not where the slab is.
            Err(e) if probe == path => {
                return Verdict::Unreadable { why: format!("{e}") };
            }
            Err(_) => continue,
        };
        if !sniffed.starts(0, SLAB_MAGIC) {
            continue;
        }

        // The magic says "a slab"; only stormblock says *which* slab. Without
        // it there is a slab here that cannot be named, and an unnameable slab
        // is not one to format.
        let Some(bin) = m.stormblock.as_deref() else {
            return Verdict::Foreign {
                what: format!(
                    "stormblock slab on {} - no stormblock binary to identify it",
                    probe.display()
                ),
            };
        };
        let Some((slab_id, role)) = identify_slab(bin, probe) else {
            return Verdict::Foreign {
                what: format!(
                    "stormblock slab magic on {} that stormblock would not identify",
                    probe.display()
                ),
            };
        };
        if let Some(via) = &remote {
            return Verdict::AnotherNode { slab_id, owner: via.clone() };
        }
        // Local. Nothing in the superblock says whose it is — it has a slab
        // uuid and a device uuid and no node identity — so the only thing that
        // can distinguish "this node's disk" from "a disk somebody moved into
        // this chassis" is a claim the owner wrote down.
        //
        // A claim naming somebody else is decisive: this is not our drive, and
        // booting it would give this node another node's hostname, which is
        // the node CA's subject CN. No claim, or a claim we cannot check
        // because this machine will not say who it is, leaves the old rule
        // standing — a drive in this chassis is this node's — which is no
        // worse than before and does not invent a new way to fail to boot.
        if let (Some(claim), Some(me)) = (found.and_then(|e| e.claim.as_ref()), me) {
            if claim.node != me {
                return Verdict::AnotherNode {
                    slab_id,
                    owner: format!("node {} (claimed on its ESP)", claim.node),
                };
            }
        }
        return Verdict::Mine {
            slab_id,
            role,
            slab: probe.to_string_lossy().into_owned(),
        };
    }

    // Not a slab. Anything that arrived over the network is still never ours
    // to take, whatever is on it.
    if let Some(via) = remote {
        return Verdict::Foreign { what: format!("attached over {via}") };
    }

    let sniffed = match sniff(path) {
        Ok(s) => s,
        Err(e) => return Verdict::Unreadable { why: format!("{e}") },
    };

    // The kernel already parsed a table here. That is the R230's /dev/sda:
    // four partitions from a previous life, very much present and not ours.
    if !parts.is_empty() {
        let table = signature(&sniffed).unwrap_or_else(|| "partition table".into());
        return Verdict::Foreign {
            what: format!("{table}, {} partition{}", parts.len(), plural(parts.len())),
        };
    }

    if let Some(what) = signature(&sniffed).or_else(|| sniffed.tail_signature()) {
        return Verdict::Foreign { what };
    }

    // Nothing recognisable in the head or the tail. This is the one branch
    // that can end in a format, so it is the one branch that pays to look at
    // the whole drive rather than the ends of it.
    match first_nonzero(path) {
        Ok(None) => Verdict::Blank,
        Ok(Some(off)) => Verdict::Foreign { what: format!("unrecognised data at offset {off}") },
        Err(e) => Verdict::Unreadable { why: format!("{e}") },
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ------------------------------------------------------------------ sysfs

fn read_trimmed(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_u64(p: impl AsRef<Path>) -> Option<u64> {
    read_trimmed(p)?.parse().ok()
}

/// The partitions the kernel found on this drive, in order.
fn partitions(sys: &Path, name: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(sys) else { return Vec::new() };
    let mut v: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("partition").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(name) && n.len() > name.len())
        .collect();
    v.sort();
    v
}

/// How the drive got here, when it did not get here by being plugged in.
///
/// `None` means local — this chassis, this node, nobody else's reach.
fn remote_transport(sys: &Path) -> Option<String> {
    // NVMe states it outright.
    if let Some(t) = read_trimmed(sys.join("device/transport"))
        .filter(|t| !t.is_empty() && t != "pcie")
    {
        return Some(format!("nvme-{t}"));
    }
    // Everything else states it in where the device hangs in the device tree.
    let link = fs::read_link(sys).ok()?.to_string_lossy().into_owned();
    for (marker, what) in [
        ("/session", "iSCSI"),
        ("iscsi", "iSCSI"),
        ("/rport-", "Fibre Channel"),
        ("fc_host", "Fibre Channel"),
        ("nvme-fabrics", "NVMe-oF"),
    ] {
        if link.contains(marker) {
            return Some(what.to_string());
        }
    }
    None
}

// ------------------------------------------------------------------- bytes

const SLAB_MAGIC: &[u8] = b"STRMSLAB";

/// The first megabyte, read on sight: every signature worth naming lives in it.
const HEAD: u64 = 1 << 20;
/// The last megabyte, read on sight: a backup GPT, and the mdraid superblocks
/// that sit at the end of a member rather than the start.
const TAIL: u64 = 1 << 20;
/// How much of a drive is read *whole* before it may be called blank. Anything
/// a drive has had done to it leaves something in the first tens of megabytes,
/// and this is one sequential read — half a second on a spinning disk.
const DEEP: u64 = 64 << 20;
/// And after that, a grain over the rest: this much is read every
/// [`STRIDE`] bytes.
const SAMPLE: u64 = 64 << 10;
const STRIDE: u64 = 1 << 30;

/// The first and last of what is on a drive. Enough to name what is there,
/// which is enough for every verdict except the one that formats something.
struct Sniff {
    head: Vec<u8>,
    tail: Vec<u8>,
}

impl Sniff {
    fn at(&self, off: usize, len: usize) -> Option<&[u8]> {
        self.head.get(off..off.checked_add(len)?)
    }

    fn starts(&self, off: usize, magic: &[u8]) -> bool {
        self.at(off, magic.len()) == Some(magic)
    }

    /// What is at the end of the drive rather than the start. Two things put
    /// themselves there and leave the front untouched: the backup GPT header
    /// in the last sector, and an mdraid v0.90 or v1.0 superblock — which is
    /// why a disk pulled out of an array can look blank from the front.
    fn tail_signature(&self) -> Option<String> {
        let t = &self.tail;
        if t.len() >= 512 && t[t.len() - 512..t.len() - 504] == *b"EFI PART" {
            return Some("GPT (backup header only)".into());
        }
        // 0xa92b4efc again, this time looked for through the tail: v1.0 sits
        // 8 KiB from the end and v0.90 on the last 64 KiB boundary, and both
        // are aligned.
        if t.chunks_exact(4096).any(|c| c[..4] == [0xfc, 0x4e, 0x2b, 0xa9]) {
            return Some("Linux md RAID member (superblock at the end)".into());
        }
        None
    }
}

fn sniff(path: &Path) -> std::io::Result<Sniff> {
    let mut f = fs::File::open(path)?;
    let len = f.seek(SeekFrom::End(0))?;

    let head = region(&mut f, 0, HEAD.min(len))?;
    if head.is_empty() {
        return Err(std::io::Error::other("read returned no bytes"));
    }
    let tail = if len > HEAD + TAIL { region(&mut f, len - TAIL, TAIL)? } else { Vec::new() };

    Ok(Sniff { head, tail })
}

/// Look at the whole drive, as far as is affordable, and say where the first
/// byte that is not zero lives.
///
/// This is the expensive one, and it is run down exactly one branch: the drive
/// carries no signature, has no partitions, arrived over nothing and is about
/// to be called [`Verdict::Blank`], which is the one verdict that leads to a
/// format. Every other verdict is reached from the head alone and costs a
/// megabyte, so a node that has nothing to take pays nothing to find out.
///
/// The first 64 MiB is read whole — one sequential read, and the region where
/// anything that has ever been done to a drive leaves a trace. After that,
/// 64 KiB every gigabyte: on a 2 TB drive that is two thousand reads, instant
/// on an SSD and tens of seconds on a spinning disk, against hours to read all
/// of it. It is a sample and not a proof, which is why it is the *last* check
/// and not the only one.
fn first_nonzero(path: &Path) -> std::io::Result<Option<u64>> {
    let mut f = fs::File::open(path)?;
    let len = f.seek(SeekFrom::End(0))?;

    // A drive smaller than the grain is simply read. It is quick, and it
    // removes the only case where reading the front and then sampling the rest
    // could leave a gap between the two.
    let whole = if len < STRIDE { len } else { DEEP };

    let mut off = 0;
    while off < whole {
        let chunk = region(&mut f, off, (1 << 20).min(whole - off))?;
        if chunk.is_empty() {
            break;
        }
        if let Some(i) = chunk.iter().position(|b| *b != 0) {
            return Ok(Some(off + i as u64));
        }
        off += chunk.len() as u64;
    }

    // The grain picks up exactly where the whole read stopped, so there is
    // nothing between them.
    let mut at = whole;
    while at + SAMPLE <= len {
        let chunk = region(&mut f, at, SAMPLE)?;
        if let Some(i) = chunk.iter().position(|b| *b != 0) {
            return Ok(Some(at + i as u64));
        }
        at += STRIDE;
    }

    if len > whole + TAIL {
        let chunk = region(&mut f, len - TAIL, TAIL)?;
        if let Some(i) = chunk.iter().position(|b| *b != 0) {
            return Ok(Some(len - TAIL + i as u64));
        }
    }

    Ok(None)
}

fn region(f: &mut fs::File, off: u64, len: u64) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(off))?;
    let mut buf = vec![0u8; len as usize];
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// Name what is on the drive, when it is something known.
///
/// This list is not what keeps a drive safe — anything unrecognised and
/// non-zero is `Foreign` regardless. It is what lets the report say *what*,
/// which is the difference between a verdict someone can check and one they
/// have to trust.
fn signature(s: &Sniff) -> Option<String> {
    let named = [
        (0usize, SLAB_MAGIC, "stormblock slab"),
        (512, b"EFI PART".as_slice(), "GPT"),
        (0, b"LUKS\xba\xbe".as_slice(), "LUKS"),
        (0, b"XFSB".as_slice(), "XFS"),
        (3, b"NTFS    ".as_slice(), "NTFS"),
        (3, b"EXFAT   ".as_slice(), "exFAT"),
        (0x10040, b"_BHRfS_M".as_slice(), "btrfs"),
        (1024, b"H+".as_slice(), "HFS+"),
        (32, b"NXSB".as_slice(), "APFS"),
        (0x8001, b"CD001".as_slice(), "ISO 9660"),
        (4086, b"SWAPSPACE2".as_slice(), "swap"),
        (4086, b"SWAP-SPACE".as_slice(), "swap"),
        (0x36, b"FAT1".as_slice(), "FAT"),
        (0x52, b"FAT32".as_slice(), "FAT32"),
    ];
    for (off, magic, what) in named {
        if s.starts(off, magic) {
            return Some(what.to_string());
        }
    }
    if s.at(0x438, 2) == Some(&[0x53, 0xef]) {
        return Some("ext2/3/4".into());
    }
    for off in [0usize, 512, 1024, 1536] {
        if s.starts(off, b"LABELONE") {
            return Some("LVM2 physical volume".into());
        }
    }
    // 0xa92b4efc, little-endian: an mdraid superblock, v1.1 at the start and
    // v1.2 a page in.
    for off in [0usize, 4096] {
        if s.at(off, 4) == Some(&[0xfc, 0x4e, 0x2b, 0xa9]) {
            return Some("Linux md RAID member".into());
        }
    }
    // A ZFS vdev label's uberblock array, 128 KiB in.
    if s.at(0x20000, 4) == Some(&[0x0c, 0xb1, 0xba, 0x00]) {
        return Some("ZFS vdev".into());
    }
    // Last, so a protective MBR is reported as the GPT it protects.
    if s.at(510, 2) == Some(&[0x55, 0xaa]) {
        return Some("MBR partition table".into());
    }
    None
}

// -------------------------------------------------------------- stormblock

/// Ask stormblock which slab this is.
///
/// Positive evidence only. A device that is a slab says so:
///
/// ```text
/// /dev/sdb: slab 7661cf8b-... (role=data, tier=hot, 4096 slots, 3900 free)
/// ```
///
/// Anything else — "not a slab", "cannot open", a drive that answers
/// `ENOMEDIUM` — is not a slab. The first version of this check in the
/// initramfs looked for the words "not a slab" and was caught out on the first
/// machine it met.
fn identify_slab(bin: &Path, dev: &Path) -> Option<(String, String)> {
    let out = Command::new(bin).arg("slab").arg("list").arg(dev).output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout.lines().find_map(parse_slab_line)
}

fn parse_slab_line(line: &str) -> Option<(String, String)> {
    let rest = line.split_once(": slab ")?.1;
    let id: String = rest.chars().take_while(|c| c.is_ascii_hexdigit() || *c == '-').collect();
    // A slab identifies itself with a UUID. Anything shorter is a line that
    // happened to contain the words.
    if id.len() != 36 {
        return None;
    }
    let role = rest
        .split_once("role=")
        .map(|(_, r)| r.split([',', ')']).next().unwrap_or("").trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "unknown".into());
    Some((id, role))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A machine built out of files: a `/sys/block` tree and a `/dev` of
    /// regular files standing in for the drives. Everything the probe reads is
    /// a file read, so a laptop can be a Dell R230 for the length of a test.
    struct FakeMachine {
        root: tempfile::TempDir,
    }

    impl FakeMachine {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir_all(root.path().join("sys/block")).unwrap();
            fs::create_dir_all(root.path().join("dev")).unwrap();
            Self { root }
        }

        /// A drive of `size` bytes whose contents are `content` at offset 0,
        /// zero elsewhere.
        fn drive(&self, name: &str, size: u64, rotational: bool, content: &[u8]) -> &Self {
            let sys = self.root.path().join("sys/block").join(name);
            fs::create_dir_all(sys.join("queue")).unwrap();
            fs::create_dir_all(sys.join("device")).unwrap();
            fs::write(sys.join("size"), format!("{}\n", size / 512)).unwrap();
            fs::write(sys.join("queue/rotational"), if rotational { "1\n" } else { "0\n" }).unwrap();
            fs::write(sys.join("removable"), "0\n").unwrap();

            let dev = self.root.path().join("dev").join(name);
            let mut f = fs::File::create(&dev).unwrap();
            f.set_len(size).unwrap();
            if !content.is_empty() {
                f.write_all(content).unwrap();
            }
            self
        }

        fn set(&self, name: &str, rel: &str, value: &str) -> &Self {
            let p = self.root.path().join("sys/block").join(name).join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, value).unwrap();
            self
        }

        /// A partition the kernel found, with its own contents.
        fn partition(&self, disk: &str, part: &str, content: &[u8]) -> &Self {
            let sys = self.root.path().join("sys/block").join(disk).join(part);
            fs::create_dir_all(&sys).unwrap();
            fs::write(sys.join("partition"), "2\n").unwrap();
            let dev = self.root.path().join("dev").join(part);
            let mut f = fs::File::create(&dev).unwrap();
            f.set_len(1 << 20).unwrap();
            f.write_all(content).unwrap();
            self
        }

        /// A stand-in for the static stormblock the initramfs carries, which
        /// answers exactly as the real one does.
        fn stormblock(&self, answer: &str) -> PathBuf {
            let p = self.root.path().join("stormblock");
            fs::write(&p, format!("#!/bin/sh\nprintf '%s\\n' \"{answer}\"\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            }
            p
        }

        fn machine(&self) -> Machine {
            Machine {
                sysfs: self.root.path().join("sys"),
                dev: self.root.path().join("dev"),
                stormblock: None,
                only: Vec::new(),
            }
        }
    }

    fn gpt_header() -> Vec<u8> {
        let mut v = vec![0u8; 1024];
        v[510] = 0x55;
        v[511] = 0xaa;
        v[512..520].copy_from_slice(b"EFI PART");
        v
    }

    /// The machine in the issue: one usable 2 TB disk carrying four partitions
    /// from a previous life, and an iDRAC virtual floppy with no medium in it.
    /// Neither is available, and the report says why for both.
    #[test]
    fn the_r230_as_it_actually_is() {
        let fake = FakeMachine::new();
        fake.drive("sda", 4 << 20, true, &gpt_header())
            .set("sda", "device/model", "WDC WD20EFAX-68F\n");
        for (i, p) in ["sda1", "sda2", "sda3", "sda4"].iter().enumerate() {
            fake.partition("sda", p, &[i as u8 + 1]);
        }
        // The virtual floppy: present, zero sectors, removable.
        fake.drive("sdb", 0, true, &[])
            .set("sdb", "removable", "1\n")
            .set("sdb", "device/model", "Virtual Floppy\n");

        let s = survey(&fake.machine()).unwrap();
        assert_eq!(s.drives.len(), 2, "{:#?}", s.drives);

        let sda = &s.drives[0];
        assert_eq!(sda.path, fake.root.path().join("dev/sda").to_string_lossy());
        assert_eq!(sda.model.as_deref(), Some("WDC WD20EFAX-68F"));
        assert_eq!(
            sda.verdict,
            Verdict::Foreign { what: "GPT, 4 partitions".into() },
            "four partitions from a previous life are not free space"
        );

        assert!(
            matches!(&s.drives[1].verdict, Verdict::Unreadable { why } if why.contains("no medium")),
            "{:?}",
            s.drives[1].verdict
        );

        assert!(s.available().is_empty(), "nothing on this machine may be taken");
        assert!(matches!(s.intent(), crate::survey::Intent::NothingToTake { .. }));
    }

    /// A drive is blank when it is zero, and one stray byte is enough to say
    /// it is not. 4 MiB in is where this was caught on a real 8 GB disk: an
    /// earlier version sampled three points from the middle, missed it, and
    /// reported "blank - available", which is the one mistake this whole file
    /// exists to avoid.
    #[test]
    fn a_zeroed_drive_is_blank_and_a_dirtied_one_is_not() {
        let size = 3 << 30;
        let fake = FakeMachine::new();
        fake.drive("sda", size, false, &[]);
        let dirty = [
            // Inside the 64 MiB that is read whole, and deliberately not on
            // any round boundary: real data does not land where you sample.
            ("sdb", (4 << 20) + 12345),
            ("sdc", DEEP - 1),
            // On the grain that covers the rest, which is anchored where the
            // whole read stops.
            ("sdd", DEEP + STRIDE),
            // And the last megabyte, which is read whole as well.
            ("sde", size - 4096),
        ];
        for (name, at) in dirty {
            fake.drive(name, size, false, &[]);
            dirty_at(&fake, name, at);
        }

        let s = survey(&fake.machine()).unwrap();
        assert_eq!(s.drives[0].verdict, Verdict::Blank, "a zeroed drive is blank");
        for (drive, (_, at)) in s.drives[1..].iter().zip(dirty.iter()) {
            assert_eq!(
                drive.verdict,
                Verdict::Foreign { what: format!("unrecognised data at offset {at}") },
                "{} has a byte at {at} and is not blank",
                drive.path
            );
        }
        assert_eq!(s.available().len(), 1, "only the zeroed drive may be taken");
    }

    /// The honest limit, written down so nobody mistakes the check for a
    /// proof: past the first 64 MiB the drive is sampled, and a single byte
    /// that falls between two samples is not seen. Reading a 2 TB disk in full
    /// on every boot is hours, so the design does not rest on this — a drive
    /// that has ever been used carries a signature at one end or the other,
    /// and that is what is checked first.
    #[test]
    fn a_single_byte_between_two_samples_is_not_seen() {
        let fake = FakeMachine::new();
        fake.drive("sda", 3 << 30, false, &[]);
        dirty_at(&fake, "sda", DEEP + STRIDE / 2);

        let s = survey(&fake.machine()).unwrap();
        assert_eq!(s.drives[0].verdict, Verdict::Blank, "a sample is not a proof");
    }

    fn dirty_at(fake: &FakeMachine, name: &str, at: u64) {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .open(fake.root.path().join("dev").join(name))
            .unwrap();
        f.seek(SeekFrom::Start(at)).unwrap();
        f.write_all(&[0x42]).unwrap();
    }

    /// A disk pulled out of a Linux array keeps its superblock at the end and
    /// nothing at the front, so the tail is read and named rather than merely
    /// counted as "something".
    #[test]
    fn a_raid_member_that_is_blank_from_the_front_is_still_a_raid_member() {
        let size = 3 << 30;
        let fake = FakeMachine::new();
        fake.drive("sda", size, true, &[]);
        let mut f = fs::OpenOptions::new()
            .write(true)
            .open(fake.root.path().join("dev/sda"))
            .unwrap();
        f.seek(SeekFrom::Start(size - 8192)).unwrap();
        f.write_all(&[0xfc, 0x4e, 0x2b, 0xa9]).unwrap();
        drop(f);

        let s = survey(&fake.machine()).unwrap();
        assert!(
            matches!(&s.drives[0].verdict, Verdict::Foreign { what } if what.contains("md RAID")),
            "{:?}",
            s.drives[0].verdict
        );
    }

    /// A node booting off the disk it assimilated onto. The slab is in a
    /// partition, so the whole device looks like a GPT — and calling that
    /// foreign would mean a node never recognises its own work.
    #[test]
    fn a_slab_in_a_partition_on_a_local_disk_is_mine() {
        let fake = FakeMachine::new();
        fake.drive("sda", 8 << 20, true, &gpt_header());
        fake.partition("sda", "sda1", b"fake esp");
        fake.partition("sda", "sda2", SLAB_MAGIC);

        let mut m = fake.machine();
        m.stormblock = Some(fake.stormblock(
            "sda2: slab 7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60 (role=data, tier=hot, 4096 slots, 3900 free)",
        ));

        let s = survey(&m).unwrap();
        let slab = fake.root.path().join("dev/sda2").to_string_lossy().into_owned();
        assert_eq!(
            s.drives[0].verdict,
            Verdict::Mine {
                slab_id: "7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60".into(),
                role: "data".into(),
                slab: slab.clone(),
            }
        );
        // The drive is /dev/sda and the thing that boots is /dev/sda2. A
        // caller handed only the drive would have to find the slab again.
        match s.intent() {
            crate::survey::Intent::AlreadyMine { drive, slab: s2, .. } => {
                assert!(drive.ends_with("sda"), "{drive}");
                assert_eq!(s2, slab);
            }
            other => panic!("expected AlreadyMine, got {other:?}"),
        }
    }

    /// The same slab, arriving over nvme-tcp. It is the appliance's export or
    /// a LUN shared with another node; it is not this node's to format, and
    /// nothing in the superblock says so — only where it is attached does.
    #[test]
    fn the_same_slab_over_the_network_belongs_to_someone_else() {
        let fake = FakeMachine::new();
        fake.drive("nvme0n1", 8 << 20, false, SLAB_MAGIC)
            .set("nvme0n1", "device/transport", "tcp\n");

        let mut m = fake.machine();
        m.stormblock = Some(fake.stormblock(
            "nvme0n1: slab 7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60 (role=data, tier=hot, 4096 slots, 3900 free)",
        ));

        let s = survey(&m).unwrap();
        assert!(
            matches!(&s.drives[0].verdict, Verdict::AnotherNode { owner, .. } if owner.contains("tcp")),
            "{:?}",
            s.drives[0].verdict
        );
        assert!(s.available().is_empty());
    }

    /// A blank drive that arrived over the network is still not ours. The
    /// appliance hands out empty namespaces; formatting one is not a mistake
    /// that shows up until something else needed it.
    #[test]
    fn a_blank_network_drive_is_never_taken() {
        let fake = FakeMachine::new();
        fake.drive("nvme0n1", 8 << 20, false, &[])
            .set("nvme0n1", "device/transport", "tcp\n");

        let s = survey(&fake.machine()).unwrap();
        assert!(
            matches!(&s.drives[0].verdict, Verdict::Foreign { what } if what.contains("nvme-tcp")),
            "{:?}",
            s.drives[0].verdict
        );
    }

    /// A local PCIe NVMe is not "attached over" anything.
    #[test]
    fn a_local_nvme_is_local() {
        let fake = FakeMachine::new();
        fake.drive("nvme0n1", 8 << 20, false, &[])
            .set("nvme0n1", "device/transport", "pcie\n");

        let s = survey(&fake.machine()).unwrap();
        assert_eq!(s.drives[0].verdict, Verdict::Blank);
        assert!(!s.drives[0].rotational);
    }

    /// Without stormblock the magic still says "a slab", and a slab that
    /// cannot be named is not one to format. This is the whole rule in one
    /// case: the drive is not judged on what is missing.
    #[test]
    fn a_slab_that_cannot_be_identified_is_left_alone() {
        let fake = FakeMachine::new();
        fake.drive("sda", 8 << 20, true, SLAB_MAGIC);

        let s = survey(&fake.machine()).unwrap();
        assert!(
            matches!(&s.drives[0].verdict, Verdict::Foreign { what } if what.contains("no stormblock")),
            "{:?}",
            s.drives[0].verdict
        );
    }

    /// stormblock saying "not a slab" about a device carrying the magic means
    /// something is wrong, not that the drive is free.
    #[test]
    fn stormblock_refusing_a_slab_is_not_permission() {
        let fake = FakeMachine::new();
        fake.drive("sda", 8 << 20, true, SLAB_MAGIC);
        let mut m = fake.machine();
        m.stormblock = Some(fake.stormblock("sda: not a slab (bad slab magic)"));

        let s = survey(&m).unwrap();
        assert!(!s.drives[0].verdict.is_available(), "{:?}", s.drives[0].verdict);
    }

    #[test]
    fn known_things_on_a_drive_are_named() {
        let cases: [(&str, Vec<u8>, &str); 4] = [
            ("sda", b"XFSB".to_vec(), "XFS"),
            ("sdb", {
                let mut v = vec![0u8; 2048];
                v[0x438] = 0x53;
                v[0x439] = 0xef;
                v
            }, "ext2/3/4"),
            ("sdc", b"LUKS\xba\xbe".to_vec(), "LUKS"),
            ("sdd", {
                let mut v = vec![0u8; 1024];
                v[512..520].copy_from_slice(b"LABELONE");
                v
            }, "LVM2 physical volume"),
        ];
        let fake = FakeMachine::new();
        for (name, content, _) in &cases {
            fake.drive(name, 8 << 20, true, content);
        }
        let s = survey(&fake.machine()).unwrap();
        for (drive, (_, _, expect)) in s.drives.iter().zip(cases.iter()) {
            assert_eq!(
                drive.verdict,
                Verdict::Foreign { what: (*expect).to_string() },
                "{}",
                drive.path
            );
        }
    }

    #[test]
    fn things_that_are_not_drives_are_not_surveyed() {
        let fake = FakeMachine::new();
        for name in ["loop0", "ram0", "dm-0", "sr0", "md0", "zram0"] {
            fake.drive(name, 8 << 20, false, &[]);
        }
        fake.drive("sda", 8 << 20, false, &[]);

        let s = survey(&fake.machine()).unwrap();
        assert_eq!(s.drives.len(), 1);
        assert!(s.drives[0].path.ends_with("sda"));
    }

    #[test]
    fn only_looks_at_the_drives_it_was_asked_about() {
        let fake = FakeMachine::new();
        fake.drive("sda", 8 << 20, true, &[]);
        fake.drive("sdb", 8 << 20, true, &[]);
        let mut m = fake.machine();
        m.only = vec!["/dev/sdb".into()];

        let s = survey(&m).unwrap();
        assert_eq!(s.drives.len(), 1);
        assert!(s.drives[0].path.ends_with("sdb"));
    }

    #[test]
    fn a_slab_line_is_parsed_and_anything_else_is_not() {
        assert_eq!(
            parse_slab_line(
                "/dev/sdb: slab 7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60 (role=data, tier=hot, 4096 slots, 3900 free)"
            ),
            Some((
                "7661cf8b-1c4f-4a2e-9f11-7d3b5a2c8e60".to_string(),
                "data".to_string()
            ))
        );
        assert_eq!(parse_slab_line("/dev/sda: not a slab (bad slab magic)"), None);
        assert_eq!(parse_slab_line("/dev/sda: cannot open (No medium found (os error 123))"), None);
        assert_eq!(parse_slab_line("/dev/sda: slab short"), None);
        assert_eq!(parse_slab_line(""), None);
    }

    #[test]
    fn a_machine_with_no_sysfs_says_so_rather_than_reporting_no_drives() {
        let m = Machine { sysfs: PathBuf::from("/nonexistent-sysfs"), ..Machine::default() };
        assert!(survey(&m).is_err(), "no drives and cannot look are different answers");
    }
}
