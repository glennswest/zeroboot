//! What is on this machine's drives, and whose it is.
//!
//! This is the whole of zeroboot's judgement. Taking a drive over is easy —
//! it is a format. Deciding that a drive is *nobody's* is the part that can
//! destroy someone's data if it is wrong, so it is written to be read: every
//! verdict names the evidence it rests on, and anything unrecognised is
//! [`Verdict::Foreign`], never "probably free".
//!
//! Deliberately read-only. Nothing here writes a byte; `survey` produces a
//! report and the caller decides.

use crate::esp;

use serde::Serialize;
use std::fmt;

/// What a drive turned out to be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// A stormblock slab this node already owns. Nothing to do — this is a
    /// node that has already assimilated and is booting again.
    Mine {
        slab_id: String,
        role: String,
        /// The device the slab actually is: the drive itself when the slab was
        /// written to the whole disk, and a partition of it when the disk was
        /// laid out with an ESP beside it. This is the device that boots, and
        /// it is not always the drive it was found on.
        slab: String,
    },
    /// A stormblock slab belonging to a different node. Never taken: two
    /// nodes writing one slab is how you lose both.
    AnotherNode { slab_id: String, owner: String },
    /// Something is on it that is not ours — a partition table, a filesystem,
    /// a signature we do not recognise. Left alone.
    Foreign { what: String },
    /// Nothing recognisable. Available to take.
    Blank,
    /// Could not be read well enough to judge. Treated as foreign: a drive
    /// that will not answer is not a drive to format.
    Unreadable { why: String },
}

impl Verdict {
    /// Whether zeroboot may format this drive.
    ///
    /// Only [`Verdict::Blank`]. Not "not mine", not "unreadable", not
    /// "foreign but it looked empty" — the one case where the evidence says
    /// there is nothing to lose.
    pub fn is_available(&self) -> bool {
        matches!(self, Verdict::Blank)
    }

    /// Whether this node is already set up here.
    pub fn is_mine(&self) -> bool {
        matches!(self, Verdict::Mine { .. })
    }

    /// What the slab is for, when this drive carries one of ours.
    fn slab_role(&self) -> Option<&str> {
        match self {
            Verdict::Mine { role, .. } => Some(role),
            _ => None,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Mine { slab_id, role, slab } => {
                write!(f, "mine - stormblock slab {slab_id} on {slab} ({role})")
            }
            Verdict::AnotherNode { slab_id, owner } => {
                write!(f, "another node's - slab {slab_id} owned by {owner}")
            }
            Verdict::Foreign { what } => write!(f, "not ours - {what}"),
            Verdict::Blank => write!(f, "blank - available"),
            Verdict::Unreadable { why } => write!(f, "unreadable - {why}"),
        }
    }
}

/// One drive, judged.
#[derive(Debug, Clone, Serialize)]
pub struct Drive {
    pub path: String,
    pub size_bytes: u64,
    pub rotational: bool,
    /// What the drive says it is, when it says anything — the SCSI/NVMe model
    /// string. A survey is meant to be read by someone deciding whether the
    /// verdict is right, and `/dev/sda` alone does not tell them which disk
    /// that is.
    pub model: Option<String>,
    /// The drive's own name for itself, independent of where it is plugged in.
    ///
    /// A device path is not an identity — this machine's `/dev/sda` is
    /// sometimes a 2 TB disk and sometimes an iDRAC virtual floppy. These are
    /// what anything downstream should key on, and stormdrive's first line is
    /// exactly that: identity that survives reboots and path changes.
    pub wwid: Option<String>,
    pub serial: Option<String>,
    /// The GPT disk GUID and every partition's own GUID, when the drive has a
    /// partition table. Written into the table itself, so they travel with the
    /// disk between chassis and controllers.
    pub table: Option<esp::Table>,
    /// What the drive's ESP says, when it has one: whether it can boot, what
    /// is missing if it cannot, and who has claimed it.
    ///
    /// Kept apart from the verdict on purpose. "Whose is this?" and "will it
    /// boot?" are different questions with different evidence, and a node that
    /// runs them together declines to ask the appliance for an image it needs.
    pub esp: Option<esp::Esp>,
    pub verdict: Verdict,
}

impl Drive {
    /// Whether this drive can start the node.
    ///
    /// Two layouts reach this, and only one of them boots itself from
    /// firmware:
    ///
    /// - **A disk zeroboot laid out** (`boot-image`, or the ISO) has an ESP
    ///   carrying a bootloader, a kernel, an initramfs and a loader entry. It
    ///   claims to boot on its own, and the claim is checkable: if the kernel
    ///   the entry names is not there, the drive does not boot, and saying so
    ///   is the whole point of looking.
    /// - **A disk assimilated by the flow-over** has no ESP at all. stormblock
    ///   lays a data slab and a system slab and no boot partition, because the
    ///   node netboots its kernel and only its *slab* is local. There is
    ///   nothing here to check and nothing missing.
    ///
    /// So the absence of an ESP is not evidence of anything, and treating it
    /// as a defect sends a node that assimilated perfectly well back to the
    /// appliance on every boot afterwards — which is the failure this whole
    /// distinction exists to avoid, pointed the other way.
    ///
    /// What is left unverified is whether the system slab actually holds the
    /// boot volume. That needs listing the volumes inside a slab, which cannot
    /// be done offline (stormblock#108). Until it can, a system slab is taken
    /// at its word and a data slab is not: a data slab is not supposed to boot.
    pub fn boots(&self) -> bool {
        match &self.esp {
            Some(e) => e.boot.is_some(),
            None => self.verdict.slab_role() == Some("system"),
        }
    }

    /// Why it does not, in the drive's own words.
    fn why_not(&self) -> String {
        match &self.esp {
            None => match self.verdict.slab_role() {
                Some(role) => format!("a {role} slab, which does not boot a node"),
                None => "nothing on it that boots".into(),
            },
            Some(e) if e.missing.is_empty() => "nothing bootable on its ESP".into(),
            Some(e) => e.missing.join("; "),
        }
    }
}

/// Every drive on the machine, and what zeroboot intends to do.
#[derive(Debug, Clone, Serialize)]
pub struct Survey {
    pub drives: Vec<Drive>,
}

impl Survey {
    /// Drives this node already owns, the one that boots first.
    ///
    /// A node can own more than one — a system slab and a data slab is the
    /// ordinary case, and the split is the point of having roles at all. Only
    /// the system slab boots, so it is the one a caller is handed; ties keep
    /// the order the drives were seen in.
    pub fn mine(&self) -> Vec<&Drive> {
        let mut v: Vec<&Drive> = self.drives.iter().filter(|d| d.verdict.is_mine()).collect();
        v.sort_by_key(|d| if d.verdict.slab_role() == Some("system") { 0 } else { 1 });
        v
    }

    /// Drives this node owns *and* can boot from.
    ///
    /// Owning a slab and being able to boot are not the same thing and the
    /// difference is not academic: a freshly formatted slab with no volumes in
    /// it and no bootloader anywhere is as much "ours" as a working boot disk.
    /// A data slab is ours and is not supposed to boot at all.
    pub fn bootable(&self) -> Vec<&Drive> {
        self.mine().into_iter().filter(|d| d.boots()).collect()
    }

    /// Drives that may be taken, largest first — and among equals, solid
    /// state before spinning, since the writable end is small and hot.
    pub fn available(&self) -> Vec<&Drive> {
        let mut v: Vec<&Drive> =
            self.drives.iter().filter(|d| d.verdict.is_available()).collect();
        v.sort_by(|a, b| {
            a.rotational
                .cmp(&b.rotational)
                .then(b.size_bytes.cmp(&a.size_bytes))
        });
        v
    }

    /// Is there anything to do, and may it be done?
    ///
    /// The three answers a caller acts on, and nothing in between: a node that
    /// is already set up boots; a node with somewhere to go takes it; a node
    /// with nowhere to go boots anyway on whatever the appliance is serving,
    /// because assimilation is an improvement, not a precondition.
    pub fn intent(&self) -> Intent {
        if let Some(already) = self.bootable().iter().find_map(|d| match &d.verdict {
            Verdict::Mine { slab_id, slab, .. } => Some(Intent::AlreadyMine {
                drive: d.path.clone(),
                slab: slab.clone(),
                slab_id: slab_id.clone(),
            }),
            _ => None,
        }) {
            return already;
        }
        // Ours, and none of it boots. Not a drive to take — it is already this
        // node's — and not a node that can start on its own either, so it asks
        // the appliance exactly as a node with no disk at all does.
        let mine = self.mine();
        if !mine.is_empty() {
            return Intent::MineButNoneBoots {
                because: mine
                    .iter()
                    .map(|d| format!("{} {} - {}", d.path, d.verdict, d.why_not()))
                    .collect(),
            };
        }
        match self.available().first() {
            Some(d) => Intent::TakeOver { path: d.path.clone() },
            None => Intent::NothingToTake {
                because: self
                    .drives
                    .iter()
                    .map(|d| format!("{} {}", d.path, d.verdict))
                    .collect(),
            },
        }
    }
}

/// What the survey concluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum Intent {
    /// This node has already assimilated. Boot — off `slab`, which is the
    /// device the slab is on and not necessarily the drive it was found on.
    /// Saying which costs nothing here and saves the caller from searching for
    /// it a second time, with a second implementation of the same judgement.
    AlreadyMine { drive: String, slab: String, slab_id: String },
    /// This node owns a slab here and none of them boots — a system disk with
    /// no bootloader on it, a slab formatted and never filled, or a data slab
    /// on its own, which is not supposed to boot.
    ///
    /// Distinct from both of the others on purpose. Nothing here may be taken,
    /// because it is already ours; and nothing here can start the node, so it
    /// asks the appliance. Reporting this as `AlreadyMine` is how a node
    /// declines to fetch an image it cannot start without.
    MineButNoneBoots { because: Vec<String> },
    /// Nothing here is anyone's. Take it.
    TakeOver { path: String },
    /// Nowhere to go. Boot on the appliance's clone and say why — a node that
    /// silently declines to assimilate looks identical to one that tried and
    /// failed.
    NothingToTake { because: Vec<String> },
}

/// A survey as it leaves the process: what was seen, and what follows from
/// it. The two are separate on purpose — the drives are evidence and the
/// intent is the conclusion drawn from it, and anyone checking the second
/// needs the first in front of them.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub drives: Vec<Drive>,
    pub intent: Intent,
}

impl Survey {
    pub fn report(&self) -> Report {
        Report { drives: self.drives.clone(), intent: self.intent() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drive that boots, unless a test says otherwise: most of these are
    /// about ownership, and having to spell out an ESP in each would bury it.
    fn drive(path: &str, size: u64, rotational: bool, verdict: Verdict) -> Drive {
        Drive {
            path: path.into(),
            size_bytes: size,
            rotational,
            model: None,
            wwid: None,
            serial: None,
            table: None,
            esp: Some(esp::Esp {
                boot: Some(esp::Boot {
                    cmdline: String::new(),
                    slab_device: None,
                    boot_volume: None,
                }),
                claim: None,
                missing: vec![],
            }),
            verdict,
        }
    }

    fn without_an_esp(mut d: Drive) -> Drive {
        d.esp = None;
        d
    }

    trait BrokenEsp {
        fn with_broken_esp(self, missing: &str) -> Drive;
    }
    impl BrokenEsp for Drive {
        /// An ESP that is there and does not have what it names — the one case
        /// where a drive positively says it cannot boot.
        fn with_broken_esp(mut self, missing: &str) -> Drive {
            self.esp =
                Some(esp::Esp { boot: None, claim: None, missing: vec![missing.to_string()] });
            self
        }
    }

    #[test]
    fn only_a_blank_drive_may_be_taken() {
        assert!(Verdict::Blank.is_available());
        assert!(!Verdict::Foreign { what: "GPT with 4 partitions".into() }.is_available());
        assert!(!Verdict::Unreadable { why: "no medium".into() }.is_available());
        assert!(!Verdict::AnotherNode {
            slab_id: "abc".into(),
            owner: "node-2".into()
        }
        .is_available());
        assert!(!Verdict::Mine {
            slab_id: "abc".into(),
            role: "data".into(),
            slab: "/dev/sda".into()
        }
        .is_available());
    }

    /// The R230 that prompted this: a 2 TB disk carrying four partitions from
    /// a previous life. It is not blank and it is not ours, and the only safe
    /// reading of that is "leave it".
    #[test]
    fn a_disk_with_someone_elses_partitions_is_never_taken() {
        let s = Survey {
            drives: vec![drive(
                "/dev/sda",
                2_000_398_934_016,
                true,
                Verdict::Foreign { what: "GPT, 4 partitions".into() },
            )],
        };
        assert!(s.available().is_empty());
        assert!(matches!(s.intent(), Intent::NothingToTake { .. }));
    }

    /// An empty removable drive answers ENOMEDIUM, and that is not an
    /// invitation. Same lesson as the slab probe: absence of a known error is
    /// not evidence of emptiness.
    #[test]
    fn an_unreadable_drive_is_not_treated_as_empty() {
        let s = Survey {
            drives: vec![drive(
                "/dev/sda",
                0,
                false,
                Verdict::Unreadable { why: "no medium found (os error 123)".into() },
            )],
        };
        assert!(s.available().is_empty());
    }

    #[test]
    fn a_node_that_already_owns_a_slab_just_boots() {
        let s = Survey {
            drives: vec![
                drive("/dev/sda", 1 << 40, true, Verdict::Mine {
                    slab_id: "7661cf8b".into(),
                    role: "data".into(),
                    slab: "/dev/sda2".into(),
                }),
                drive("/dev/sdb", 1 << 40, false, Verdict::Blank),
            ],
        };
        assert_eq!(
            s.intent(),
            Intent::AlreadyMine {
                drive: "/dev/sda".into(),
                slab: "/dev/sda2".into(),
                slab_id: "7661cf8b".into(),
            },
            "owning a slab settles it, and says which device to boot"
        );
    }

    /// A node with both a system slab and a data slab boots off the system
    /// one. The drives are seen in name order and the roles decide, not the
    /// order — otherwise which disk a node boots from depends on which SATA
    /// port it happens to be in.
    #[test]
    fn the_system_slab_is_the_one_that_boots() {
        let s = Survey {
            drives: vec![
                drive("/dev/sda", 4 << 40, true, Verdict::Mine {
                    slab_id: "data-slab".into(),
                    role: "data".into(),
                    slab: "/dev/sda".into(),
                }),
                drive("/dev/sdb", 1 << 40, false, Verdict::Mine {
                    slab_id: "system-slab".into(),
                    role: "system".into(),
                    slab: "/dev/sdb2".into(),
                }),
            ],
        };
        match s.intent() {
            Intent::AlreadyMine { slab, slab_id, .. } => {
                assert_eq!(slab, "/dev/sdb2");
                assert_eq!(slab_id, "system-slab");
            }
            other => panic!("expected AlreadyMine, got {other:?}"),
        }
    }

    #[test]
    fn solid_state_is_preferred_and_then_the_larger_drive() {
        let s = Survey {
            drives: vec![
                drive("/dev/sda", 4 << 40, true, Verdict::Blank),
                drive("/dev/sdb", 1 << 40, false, Verdict::Blank),
                drive("/dev/sdc", 2 << 40, false, Verdict::Blank),
            ],
        };
        let order: Vec<&str> = s.available().iter().map(|d| d.path.as_str()).collect();
        assert_eq!(order, vec!["/dev/sdc", "/dev/sdb", "/dev/sda"]);
        assert_eq!(s.intent(), Intent::TakeOver { path: "/dev/sdc".into() });
    }

    /// Owning a slab is not the same as being able to start from it. A node
    /// whose system disk carries a slab but no bootloader — a slab formatted
    /// and never filled, or a boot disk whose ESP was wiped — must ask the
    /// appliance, exactly as a node with no disk does. Calling that
    /// `AlreadyMine` is how a node declines to fetch the image it cannot start
    /// without.
    #[test]
    fn a_slab_that_boots_nothing_does_not_settle_the_boot() {
        let s = Survey {
            drives: vec![drive("/dev/sda", 1 << 40, true, Verdict::Mine {
                slab_id: "7661cf8b".into(),
                role: "system".into(),
                slab: "/dev/sda2".into(),
            })
            .with_broken_esp("kernel /vmlinuz named by the loader entry is not on the ESP")],
        };
        match s.intent() {
            Intent::MineButNoneBoots { because } => {
                assert_eq!(because.len(), 1);
                assert!(because[0].contains("/dev/sda"), "{because:?}");
                assert!(because[0].contains("vmlinuz"), "{because:?}");
            }
            other => panic!("expected MineButNoneBoots, got {other:?}"),
        }
        // And it is still not a drive to take: it is already ours.
        assert!(s.available().is_empty());
    }

    /// A drive the flow-over assimilated: two slabs, no ESP, because the node
    /// netboots its kernel and only the slab is local. There is nothing here
    /// to check and nothing missing — and calling that "does not boot" would
    /// send a node that assimilated perfectly well back to the appliance on
    /// every boot for the rest of its life.
    #[test]
    fn a_flow_over_drive_has_no_esp_and_boots_anyway() {
        let s = Survey {
            drives: vec![
                without_an_esp(drive("/dev/sda", 4 << 40, true, Verdict::Mine {
                    slab_id: "data-slab".into(),
                    role: "data".into(),
                    slab: "/dev/sda1".into(),
                })),
                without_an_esp(drive("/dev/sda", 4 << 40, true, Verdict::Mine {
                    slab_id: "system-slab".into(),
                    role: "system".into(),
                    slab: "/dev/sda2".into(),
                })),
            ],
        };
        match s.intent() {
            Intent::AlreadyMine { slab, slab_id, .. } => {
                assert_eq!(slab_id, "system-slab", "the system half is the one that boots");
                assert_eq!(slab, "/dev/sda2");
            }
            other => panic!("expected AlreadyMine, got {other:?}"),
        }
    }

    /// The case that makes the distinction earn its keep: the system disk died
    /// and the data disk survived. The node owns a slab, the slab is fine, and
    /// there is nothing on it to boot.
    #[test]
    fn a_surviving_data_slab_does_not_pretend_to_be_a_boot_disk() {
        let s = Survey {
            drives: vec![without_an_esp(drive("/dev/sdb", 4 << 40, true, Verdict::Mine {
                slab_id: "data-slab".into(),
                role: "data".into(),
                slab: "/dev/sdb".into(),
            }))],
        };
        assert!(matches!(s.intent(), Intent::MineButNoneBoots { .. }));
    }

    /// With one of each, the bootable one settles it — and it is picked
    /// because it boots, not because of where it sits in the list.
    #[test]
    fn the_drive_that_boots_is_the_one_chosen() {
        let s = Survey {
            drives: vec![
                drive("/dev/sda", 4 << 40, true, Verdict::Mine {
                    slab_id: "data-slab".into(),
                    role: "system".into(),
                    slab: "/dev/sda".into(),
                })
                .with_broken_esp("no /EFI/BOOT/BOOTX64.EFI"),
                drive("/dev/sdb", 1 << 40, false, Verdict::Mine {
                    slab_id: "boot-slab".into(),
                    role: "data".into(),
                    slab: "/dev/sdb2".into(),
                }),
            ],
        };
        match s.intent() {
            Intent::AlreadyMine { slab_id, slab, .. } => {
                assert_eq!(slab_id, "boot-slab");
                assert_eq!(slab, "/dev/sdb2");
            }
            other => panic!("expected AlreadyMine, got {other:?}"),
        }
    }

    /// Nowhere to go is not a failure. The node still boots on what the
    /// appliance is serving — but it says which drives it looked at and what
    /// each one was, because "did not assimilate" and "could not" look the
    /// same from outside.
    #[test]
    fn nowhere_to_go_still_reports_what_it_saw() {
        let s = Survey {
            drives: vec![
                drive("/dev/sda", 1 << 40, true, Verdict::Foreign { what: "ext4".into() }),
                drive("/dev/sdb", 1 << 40, true, Verdict::AnotherNode {
                    slab_id: "aaaa".into(),
                    owner: "node-7".into(),
                }),
            ],
        };
        match s.intent() {
            Intent::NothingToTake { because } => {
                assert_eq!(because.len(), 2);
                assert!(because[0].contains("/dev/sda"), "{because:?}");
                assert!(because[1].contains("node-7"), "{because:?}");
            }
            other => panic!("expected NothingToTake, got {other:?}"),
        }
    }
}
