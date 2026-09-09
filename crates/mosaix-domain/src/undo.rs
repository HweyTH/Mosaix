//! Persistent undo records.
//!
//! One explicit placement-changing command produces one [`UndoTransaction`]
//! holding every window it moved. Undo is **refusal-first**: it examines
//! only the newest transaction, requires an exact topology match, and
//! preflights every member before a single window moves. A transaction it
//! refuses is kept for retry rather than skipped, so undo never silently
//! reverses an older action the user did not ask for.
//!
//! There is deliberately no force or best-guess variant of any of this.

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;
use crate::id::{DisplayId, WindowId};
use crate::identity::{MatchOutcome, WindowEvidence};
use crate::tree::PersistedTree;
use crate::workspace::WorkspaceRefusal;

/// Seconds since the Unix epoch, as retention bounds measure time.
///
/// A clock that has gone backwards past the epoch yields zero rather than
/// panicking. Retention then treats such an entry as ancient, which errs
/// toward pruning history rather than hoarding it.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// A stored transaction's identity. Assigned by the state database, so a
/// caller can name the transaction a refusal was about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UndoTransactionId(pub i64);

impl std::fmt::Display for UndoTransactionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One window's share of a transaction: where it was before the command,
/// and the evidence needed to find it again in a later session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoMember {
    /// Position within the transaction, stable across restarts so refusal
    /// reports name the same target every time.
    pub ordinal: u32,
    /// Where the window sat before the command moved it. This is what undo
    /// restores.
    pub prior_placement: Rect,
    /// The display the prior placement is expressed on.
    pub prior_display_fingerprint: String,
    /// How to recognise this window after a restart.
    pub evidence: WindowEvidence,
}

/// One display's container tree as it stood before the command, so undo
/// can put the structure back and not only the windows.
///
/// Without this, undoing a swap or a resize would restore every window's
/// rectangle and leave the tree describing the new arrangement -- and the
/// next reflow would quietly redo the command. Stored in the durable form,
/// so it is restored through the same confident matching a saved
/// arrangement is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UndoTreeSnapshot {
    pub display_fingerprint: String,
    pub tree: PersistedTree,
}

/// Which logical workspace one display showed before the command, so
/// undoing a workspace switch puts the assignment back and not only the
/// windows.
///
/// Recorded by fingerprint for the same reason a tree is: a display id is
/// a native handle and means nothing in a later session. `workspace` is
/// `None` for a display that showed no workspace at all -- an unfilled
/// display is a state a switch can leave and undo has to restore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoAssignment {
    pub display_fingerprint: String,
    pub workspace: Option<String>,
}

/// Everything one explicit command changed, reversed as one operation.
///
/// Not `Eq`: a container tree carries float weights.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UndoTransaction {
    pub id: UndoTransactionId,
    /// The command that created it, for display in refusal messages.
    pub command: String,
    /// Seconds since the Unix epoch, used by retention pruning.
    pub recorded_at_unix: i64,
    /// The exact topology this transaction's geometry means something on.
    pub topology_fingerprint: String,
    /// The reducer revision this transaction was committed against.
    pub durable_revision: u64,
    pub members: Vec<UndoMember>,
    /// The container trees the command reshaped, as they were before it.
    /// Empty for a command that changed no tree.
    #[serde(default)]
    pub prior_trees: Vec<UndoTreeSnapshot>,
    /// The displayed workspace assignments the command changed, as they
    /// were before it. Empty for every command but a workspace switch.
    #[serde(default)]
    pub prior_assignments: Vec<UndoAssignment>,
}

impl UndoTransaction {
    /// Whether reversing this transaction means switching a display back
    /// to another workspace, rather than only moving windows.
    pub fn changes_workspace_assignment(&self) -> bool {
        !self.prior_assignments.is_empty()
    }
}

/// A transaction that has not been stored yet, and so has no id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UndoTransactionDraft {
    pub command: String,
    pub recorded_at_unix: i64,
    pub topology_fingerprint: String,
    pub durable_revision: u64,
    pub members: Vec<UndoMember>,
    #[serde(default)]
    pub prior_trees: Vec<UndoTreeSnapshot>,
    #[serde(default)]
    pub prior_assignments: Vec<UndoAssignment>,
}

/// What the matcher concluded about one member of a transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoTargetOutcome {
    pub ordinal: u32,
    /// The application the member belonged to, so a refusal can say which
    /// window it means without naming a document.
    pub application: String,
    pub outcome: MatchOutcome,
}

impl UndoTargetOutcome {
    pub fn is_resolved(&self) -> bool {
        self.outcome.confident_window().is_some()
    }
}

/// Why undo did nothing. Every variant leaves the transaction in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoRefusal {
    /// No transaction is available to undo.
    NothingToUndo,
    /// The displays are not the ones this transaction's geometry was
    /// recorded against, so its rectangles would mean something else now.
    TopologyChanged {
        transaction_id: UndoTransactionId,
        recorded_fingerprint: String,
        current_fingerprint: String,
    },
    /// At least one member could not be resolved confidently. Carries every
    /// member's outcome, not only the failing ones, so the report shows what
    /// was checked.
    TargetsUnresolved {
        transaction_id: UndoTransactionId,
        targets: Vec<UndoTargetOutcome>,
    },
    /// Two members resolved to the same live window, so at least one of
    /// them is wrong. Distinct from ambiguity, which is one member with
    /// several candidates.
    TargetsCollide {
        transaction_id: UndoTransactionId,
        window_id: WindowId,
        ordinals: Vec<u32>,
    },
    /// The state database cannot currently be trusted to consume the
    /// transaction, so undoing it would risk applying it twice.
    PersistenceDegraded {
        transaction_id: UndoTransactionId,
        reason: String,
    },
    /// Reversing this transaction means switching a display back to
    /// another workspace, and that switch would itself be refused. It
    /// carries the switch's own typed refusal, so a caller can tell a
    /// blocked degraded condition from an unauthorised profile without
    /// reading prose.
    WorkspaceSwitchRefused {
        transaction_id: UndoTransactionId,
        reason: WorkspaceRefusal,
    },
}

impl UndoRefusal {
    /// A stable machine-readable reason code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NothingToUndo => "nothing_to_undo",
            Self::TopologyChanged { .. } => "topology_changed",
            Self::TargetsUnresolved { .. } => "targets_unresolved",
            Self::TargetsCollide { .. } => "targets_collide",
            Self::PersistenceDegraded { .. } => "persistence_degraded",
            Self::WorkspaceSwitchRefused { .. } => "workspace_switch_refused",
        }
    }

    /// The transaction that was left in place, if there was one.
    pub const fn transaction_id(&self) -> Option<UndoTransactionId> {
        match self {
            Self::NothingToUndo => None,
            Self::TopologyChanged { transaction_id, .. }
            | Self::TargetsUnresolved { transaction_id, .. }
            | Self::TargetsCollide { transaction_id, .. }
            | Self::PersistenceDegraded { transaction_id, .. }
            | Self::WorkspaceSwitchRefused { transaction_id, .. } => Some(*transaction_id),
        }
    }
}

impl std::fmt::Display for UndoRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingToUndo => formatter.write_str("there is nothing to undo"),
            Self::TopologyChanged { .. } => formatter.write_str(
                "your displays have changed since that command, so undo would place windows \
                 using geometry that no longer means the same thing",
            ),
            Self::TargetsUnresolved { targets, .. } => {
                let unresolved = targets
                    .iter()
                    .filter(|target| !target.is_resolved())
                    .count();
                write!(
                    formatter,
                    "{unresolved} of {} windows from that command could not be identified \
                     confidently, so nothing was moved",
                    targets.len()
                )
            }
            Self::TargetsCollide { ordinals, .. } => write!(
                formatter,
                "{} windows from that command matched the same live window, \
                 so at least one match is wrong and nothing was moved",
                ordinals.len()
            ),
            Self::PersistenceDegraded { .. } => formatter.write_str(
                "the state database is degraded, so undo cannot record that it happened",
            ),
            Self::WorkspaceSwitchRefused { reason, .. } => write!(
                formatter,
                "undoing that command means switching a display back to another workspace, and that switch was refused: {reason}"
            ),
        }
    }
}

/// What undo restored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoApplied {
    pub transaction_id: UndoTransactionId,
    pub command: String,
    /// The windows moved back, in the transaction's own member order.
    pub restored: Vec<UndoRestoredWindow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoRestoredWindow {
    pub ordinal: u32,
    pub window_id: WindowId,
    /// The display the prior placement belongs to, resolved from the
    /// member's recorded display fingerprint. Bounds alone are not enough:
    /// a command can move a window between monitors.
    pub display_id: DisplayId,
    pub placement: Rect,
}

/// The result of asking to undo. Typed rather than a string, so IPC and
/// the CLI can both report the same facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoResult {
    Applied(UndoApplied),
    Refused(UndoRefusal),
}

impl UndoResult {
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ScoredCandidate;

    fn unresolved_target(ordinal: u32) -> UndoTargetOutcome {
        UndoTargetOutcome {
            ordinal,
            application: "Code.exe".to_owned(),
            outcome: MatchOutcome::NoMatch {
                considered: Vec::new(),
            },
        }
    }

    fn resolved_target(ordinal: u32) -> UndoTargetOutcome {
        UndoTargetOutcome {
            ordinal,
            application: "Code.exe".to_owned(),
            outcome: MatchOutcome::Confident(ScoredCandidate {
                window_id: WindowId(1),
                score: 100,
                contributions: Vec::new(),
            }),
        }
    }

    #[test]
    fn refusal_codes_are_stable_and_distinct() {
        let codes = [
            UndoRefusal::NothingToUndo.code(),
            UndoRefusal::TopologyChanged {
                transaction_id: UndoTransactionId(1),
                recorded_fingerprint: String::new(),
                current_fingerprint: String::new(),
            }
            .code(),
            UndoRefusal::TargetsUnresolved {
                transaction_id: UndoTransactionId(1),
                targets: Vec::new(),
            }
            .code(),
            UndoRefusal::PersistenceDegraded {
                transaction_id: UndoTransactionId(1),
                reason: String::new(),
            }
            .code(),
        ];
        let mut unique = codes.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len());
    }

    #[test]
    fn every_refusal_except_an_empty_history_names_the_transaction_it_kept() {
        assert_eq!(UndoRefusal::NothingToUndo.transaction_id(), None);
        assert_eq!(
            UndoRefusal::TopologyChanged {
                transaction_id: UndoTransactionId(7),
                recorded_fingerprint: String::new(),
                current_fingerprint: String::new(),
            }
            .transaction_id(),
            Some(UndoTransactionId(7))
        );
    }

    #[test]
    fn an_unresolved_refusal_counts_only_the_targets_that_failed() {
        let refusal = UndoRefusal::TargetsUnresolved {
            transaction_id: UndoTransactionId(3),
            targets: vec![resolved_target(0), unresolved_target(1), resolved_target(2)],
        };

        assert_eq!(
            refusal.to_string(),
            "1 of 3 windows from that command could not be identified confidently, \
             so nothing was moved"
        );
    }
}
