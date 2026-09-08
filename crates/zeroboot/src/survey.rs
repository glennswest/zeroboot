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
    pub verdict: Verdict,
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
        if let Some(already) = self.mine().iter().find_map(|d| match &d.verdict {
            Verdict::Mine { slab_id, slab, .. } => Some(Intent::AlreadyMine {
                drive: d.path.clone(),
                slab: slab.clone(),
                slab_id: slab_id.clone(),
            }),
            _ => None,
        }) {
            return already;
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

    fn drive(path: &str, size: u64, rotational: bool, verdict: Verdict) -> Drive {
        Drive { path: path.into(), size_bytes: size, rotational, model: None, verdict }
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
