//! zeroboot — a node is functional the moment it boots.
//!
//! Not an installer. There is no step where a machine is "installed" and then
//! becomes useful: it boots, looks at its drives, and if any of them are
//! nobody's it takes one and puts its writable state there — before anything
//! has written a byte. A node with nowhere to put that state still boots, on
//! whatever the appliance is serving. Assimilation is an improvement, not a
//! precondition.
//!
//! Running early is the whole design. The writable volumes a node mounts —
//! `stormcos-state`, `fastetcd-data`, and the rest — are copy-on-write clones
//! served over the network until they are local. Leave assimilation until
//! after `switch_root` and every one of those has to be migrated, or staged in
//! a ramdisk and moved, with a window where the node's own identity exists
//! only in RAM. Doing it in the initramfs costs seconds and removes both.
//!
//! [`survey`] is the judgement: what is on each drive and whose it is, and
//! [`probe`] is the eyes — it walks `/sys/block`, reads what is on each drive
//! and hands the judgement something to judge. Both write nothing; the caller
//! decides.

//! Boot-artifact assembly lives here too, reusable by stormcos_builder and
//! other tooling; the binary is a thin CLI over it.

pub mod probe;
pub mod survey;

pub mod bootimage;
