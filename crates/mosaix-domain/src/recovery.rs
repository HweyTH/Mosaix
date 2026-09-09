//! The native recovery ledger: what must be durable before a window may
//! leave visible geometry, and how a later session decides whether a
//! recorded native handle still means the window it did.
//!
//! The ledger is deliberately separate from cross-session identity. The
//! state database never treats a native handle as identity, because a
//! handle is reused freely once its window is gone; the ledger records
//! handles precisely because a parked window has to be found again by
//! the same session -- or by the out-of-process restore command after a
//! crash -- without going through the scored matcher, which could pick
//! the wrong window. Every entry therefore carries enough process and
//! window evidence to reject a reused handle, and a handle that fails
//! that check is left alone rather than moved.

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;
use crate::id::{ApplicationId, WindowId};
use crate::window::{Window, WindowLifecycle};

/// The identity of one running process instance. A process id alone is
/// reused after the process exits; paired with the kernel's creation
/// time for that id, it names one instance and no other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInstance {
    pub process_id: u32,
    /// The platform's creation timestamp for `process_id`, in whatever
    /// unit the platform reports (100-nanosecond intervals on Windows).
    /// Zero when the platform could not read it, in which case the
    /// process id is the only evidence and a reused id is treated as a
    /// reused handle.
    pub creation_time: u64,
}

/// The show state a parked window had, so restoration can give it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShowState {
    Normal,
    Maximized,
    Minimized,
}

impl ShowState {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Maximized => "maximized",
            Self::Minimized => "minimized",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "normal" => Some(Self::Normal),
            "maximized" => Some(Self::Maximized),
            "minimized" => Some(Self::Minimized),
            _ => None,
        }
    }

    pub const fn of(lifecycle: WindowLifecycle) -> Self {
        match lifecycle {
            WindowLifecycle::Maximized => Self::Maximized,
            WindowLifecycle::Minimized => Self::Minimized,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RecoveryEntryId(pub i64);

/// What the ledger records before any parking effect: everything needed
/// to put the window back exactly, and everything needed to refuse to
/// touch a handle that no longer means this window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryDraft {
    /// The agent session recording the entry, so a later session can
    /// tell its own live entries from a previous session's leftovers.
    pub session_id: String,
    /// The native handle, stored as the domain window id carries it.
    pub native_handle: isize,
    pub process: ProcessInstance,
    pub application_id: ApplicationId,
    pub executable_path: Option<String>,
    pub native_class: Option<String>,
    pub original_display_fingerprint: String,
    /// Where the window was on screen when recorded.
    pub visible_bounds: Rect,
    /// The bounds the window returns to when not maximized or minimized.
    pub normal_bounds: Rect,
    pub show_state: ShowState,
    pub recorded_at_unix: i64,
}

impl RecoveryDraft {
    /// The draft for `window` as the inventory describes it. The process
    /// creation time is not part of a `Window`, so the caller with
    /// platform access fills it in before the draft is written.
    pub fn capture(
        window: &Window,
        session_id: &str,
        display_fingerprint: &str,
        normal_bounds: Rect,
        recorded_at_unix: i64,
    ) -> Self {
        Self {
            session_id: session_id.to_owned(),
            native_handle: window.id.0,
            process: ProcessInstance {
                process_id: window.process_id,
                creation_time: 0,
            },
            application_id: window.application_id.clone(),
            executable_path: window
                .executable_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            native_class: window.native_class.clone(),
            original_display_fingerprint: display_fingerprint.to_owned(),
            visible_bounds: window.bounds,
            normal_bounds,
            show_state: ShowState::of(window.lifecycle),
            recorded_at_unix,
        }
    }
}

/// One stored ledger entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryEntry {
    pub id: RecoveryEntryId,
    pub draft: RecoveryDraft,
    /// Whether the native parking effect this entry authorised was
    /// carried out. An entry recorded but never parked describes a window
    /// that never left visible geometry, and restoration skips it.
    pub parked: bool,
    /// Whether the window has since been restored, by the session that
    /// parked it or by recovery. A restored entry is history.
    pub restored: bool,
}

impl RecoveryEntry {
    pub const fn window_id(&self) -> WindowId {
        WindowId(self.draft.native_handle)
    }

    /// Whether this entry still describes a window that may be parked.
    pub const fn is_open(&self) -> bool {
        self.parked && !self.restored
    }
}

/// What a live probe of a native handle reports, for comparison against
/// the evidence an entry recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveHandleEvidence {
    pub process: ProcessInstance,
    pub native_class: Option<String>,
}

/// What recovery concluded about one entry's handle. Only `Verified`
/// authorises touching the window; every other verdict is reported with
/// the evidence that produced it and moves nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandleVerdict {
    /// The handle is live and its process instance and class match.
    Verified,
    /// The handle no longer names a window.
    Stale,
    /// The handle names a window, but not the one recorded: the process
    /// instance or the window class differs.
    Reused {
        recorded: ProcessInstance,
        live: ProcessInstance,
        recorded_class: Option<String>,
        live_class: Option<String>,
    },
    /// More than one open entry claims the handle, so neither can be
    /// trusted to describe the window it names now.
    Ambiguous { claimants: usize },
}

impl HandleVerdict {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Stale => "stale",
            Self::Reused { .. } => "reused",
            Self::Ambiguous { .. } => "ambiguous",
        }
    }
}

/// Decides which open entries may be restored, without restoring any.
///
/// `probe` answers what the handle names right now, or `None` when it
/// names nothing. A handle two open entries claim is ambiguous for both:
/// one of them is a leftover from a session that recorded the same
/// handle for a different window, and there is no safe way to say which.
pub fn plan_recovery(
    entries: &[RecoveryEntry],
    probe: impl Fn(isize) -> Option<LiveHandleEvidence>,
) -> Vec<(RecoveryEntryId, HandleVerdict)> {
    let open: Vec<&RecoveryEntry> = entries.iter().filter(|entry| entry.is_open()).collect();
    open.iter()
        .map(|entry| {
            let claimants = open
                .iter()
                .filter(|other| other.draft.native_handle == entry.draft.native_handle)
                .count();
            if claimants > 1 {
                return (entry.id, HandleVerdict::Ambiguous { claimants });
            }
            (
                entry.id,
                verdict_for(entry, probe(entry.draft.native_handle)),
            )
        })
        .collect()
}

/// What one entry's handle means now, given what a live probe reported
/// for it: the same process instance and class verify it, anything else
/// is a reused handle, and no window at all is stale. Ambiguity between
/// entries is [`plan_recovery`]'s to decide; this looks at one entry.
pub fn verdict_for(entry: &RecoveryEntry, live: Option<LiveHandleEvidence>) -> HandleVerdict {
    let Some(live) = live else {
        return HandleVerdict::Stale;
    };
    let recorded = entry.draft.process;
    let same_process = live.process.process_id == recorded.process_id
        && (recorded.creation_time == 0 || live.process.creation_time == recorded.creation_time);
    let same_class = match (&entry.draft.native_class, &live.native_class) {
        (Some(recorded), Some(live)) => recorded == live,
        _ => true,
    };
    if same_process && same_class {
        HandleVerdict::Verified
    } else {
        HandleVerdict::Reused {
            recorded,
            live: live.process,
            recorded_class: entry.draft.native_class.clone(),
            live_class: live.native_class,
        }
    }
}

/// What recovery did about one entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryOutcome {
    pub entry_id: RecoveryEntryId,
    pub native_handle: isize,
    pub application_id: ApplicationId,
    pub verdict: HandleVerdict,
    /// Whether the window was put back. Only ever true for a verified
    /// handle whose restore call succeeded.
    pub restored: bool,
    /// The platform's reason when a verified restore failed.
    pub failure: Option<String>,
}

/// Which native step of parking failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParkingStage {
    /// Moving the window out of visible geometry.
    Park,
    /// Putting a parked window back.
    Restore,
}

impl ParkingStage {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Park => "park",
            Self::Restore => "restore",
        }
    }
}

/// A native parking step the adapter could not carry out, with the
/// platform's reason. A failed park leaves the window where it was and
/// its entry recorded but never parked; a failed restore leaves the
/// window parked with its entry open, so recovery can still find it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkingFailure {
    pub window_id: WindowId,
    pub entry_id: RecoveryEntryId,
    pub stage: ParkingStage,
    pub reason: String,
}

/// Why the engine will not authorise parking a window right now. Every
/// refusal is reached before any ledger write or native effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParkingRefusal {
    /// The window is not managed, so there is nothing to record.
    NotManaged { window_id: WindowId },
    /// Committed state is not durable, so recovery data could not be
    /// promised.
    PersistenceDegraded,
    /// No adapter has verified a recoverable parking site.
    ParkingCapabilityUnverified,
    /// The adapter refused a parking site for this topology.
    ParkingRefused { reason: String },
    /// A full-screen window is never forced out of full-screen.
    Fullscreen { window_id: WindowId },
    /// A minimized window occupies no screen and is left minimized; it is
    /// never restored in order to be parked.
    Minimized { window_id: WindowId },
    /// The window's display is not in the topology, so there is no
    /// original display to record.
    UnknownDisplay { window_id: WindowId },
    /// The window is already awaiting authorisation.
    AlreadyPending { window_id: WindowId },
}

impl ParkingRefusal {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotManaged { .. } => "not_managed",
            Self::PersistenceDegraded => "persistence_degraded",
            Self::ParkingCapabilityUnverified => "parking_capability_unverified",
            Self::ParkingRefused { .. } => "parking_refused",
            Self::Fullscreen { .. } => "fullscreen",
            Self::Minimized { .. } => "minimized",
            Self::UnknownDisplay { .. } => "unknown_display",
            Self::AlreadyPending { .. } => "already_pending",
        }
    }
}

impl std::fmt::Display for ParkingRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotManaged { window_id } => {
                write!(formatter, "window {} is not managed", window_id.0)
            }
            Self::PersistenceDegraded => {
                formatter.write_str("the state database is not durable, so no window may be parked")
            }
            Self::ParkingCapabilityUnverified => {
                formatter.write_str("no recoverable parking site has been verified")
            }
            Self::ParkingRefused { reason } => {
                write!(formatter, "no recoverable parking site: {reason}")
            }
            Self::Fullscreen { window_id } => write!(
                formatter,
                "window {} is full-screen and is never forced out of it",
                window_id.0
            ),
            Self::Minimized { window_id } => write!(
                formatter,
                "window {} is minimized and occupies no screen, so it is left as it is",
                window_id.0
            ),
            Self::UnknownDisplay { window_id } => write!(
                formatter,
                "window {} is on a display that is not connected",
                window_id.0
            ),
            Self::AlreadyPending { window_id } => write!(
                formatter,
                "window {} is already awaiting recovery data",
                window_id.0
            ),
        }
    }
}

/// The typed answer to an explicit request to park one window: the
/// request was accepted and recovery data is being recorded, or it was
/// refused before anything was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParkWindowResult {
    Requested { window_id: WindowId },
    Refused(ParkingRefusal),
}

impl ParkWindowResult {
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Requested { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(handle: isize, pid: u32, created: u64, class: Option<&str>) -> RecoveryDraft {
        RecoveryDraft {
            session_id: "s1".to_owned(),
            native_handle: handle,
            process: ProcessInstance {
                process_id: pid,
                creation_time: created,
            },
            application_id: ApplicationId("code.exe".to_owned()),
            executable_path: None,
            native_class: class.map(str::to_owned),
            original_display_fingerprint: "D1".to_owned(),
            visible_bounds: Rect::new(0, 0, 800, 600),
            normal_bounds: Rect::new(0, 0, 800, 600),
            show_state: ShowState::Normal,
            recorded_at_unix: 1,
        }
    }

    fn entry(id: i64, draft: RecoveryDraft, parked: bool, restored: bool) -> RecoveryEntry {
        RecoveryEntry {
            id: RecoveryEntryId(id),
            draft,
            parked,
            restored,
        }
    }

    fn live(pid: u32, created: u64, class: Option<&str>) -> LiveHandleEvidence {
        LiveHandleEvidence {
            process: ProcessInstance {
                process_id: pid,
                creation_time: created,
            },
            native_class: class.map(str::to_owned),
        }
    }

    #[test]
    fn a_matching_process_instance_and_class_verifies_the_handle() {
        let entries = vec![entry(1, draft(100, 7, 500, Some("Chrome")), true, false)];

        let plan = plan_recovery(&entries, |_| Some(live(7, 500, Some("Chrome"))));

        assert_eq!(plan, vec![(RecoveryEntryId(1), HandleVerdict::Verified)]);
    }

    #[test]
    fn a_handle_that_names_nothing_is_stale() {
        let entries = vec![entry(1, draft(100, 7, 500, None), true, false)];

        let plan = plan_recovery(&entries, |_| None);

        assert_eq!(plan, vec![(RecoveryEntryId(1), HandleVerdict::Stale)]);
    }

    #[test]
    fn a_different_process_instance_or_class_is_a_reused_handle() {
        let entries = vec![entry(1, draft(100, 7, 500, Some("Chrome")), true, false)];

        let other_instance = plan_recovery(&entries, |_| Some(live(7, 999, Some("Chrome"))));
        assert!(matches!(other_instance[0].1, HandleVerdict::Reused { .. }));

        let other_class = plan_recovery(&entries, |_| Some(live(7, 500, Some("Notepad"))));
        assert!(matches!(other_class[0].1, HandleVerdict::Reused { .. }));
    }

    #[test]
    fn an_unknown_creation_time_falls_back_to_the_process_id_alone() {
        let entries = vec![entry(1, draft(100, 7, 0, None), true, false)];

        let plan = plan_recovery(&entries, |_| Some(live(7, 12345, None)));

        assert_eq!(plan[0].1, HandleVerdict::Verified);
    }

    #[test]
    fn two_open_entries_for_one_handle_are_ambiguous_and_neither_is_verified() {
        let entries = vec![
            entry(1, draft(100, 7, 500, None), true, false),
            entry(2, draft(100, 7, 500, None), true, false),
        ];

        let plan = plan_recovery(&entries, |_| Some(live(7, 500, None)));

        assert_eq!(
            plan,
            vec![
                (
                    RecoveryEntryId(1),
                    HandleVerdict::Ambiguous { claimants: 2 }
                ),
                (
                    RecoveryEntryId(2),
                    HandleVerdict::Ambiguous { claimants: 2 }
                ),
            ]
        );
    }

    #[test]
    fn entries_never_parked_or_already_restored_are_not_recovered() {
        let entries = vec![
            entry(1, draft(100, 7, 500, None), false, false),
            entry(2, draft(101, 7, 500, None), true, true),
        ];

        assert!(plan_recovery(&entries, |_| Some(live(7, 500, None))).is_empty());
    }

    #[test]
    fn a_draft_captured_from_a_window_records_its_show_state_and_no_title() {
        let window = Window {
            id: WindowId(42),
            process_id: 9,
            application_id: ApplicationId("code.exe".to_owned()),
            executable_path: Some(std::path::PathBuf::from("C:/code.exe")),
            title: "secret.docx".to_owned(),
            native_class: Some("Chrome".to_owned()),
            role: crate::WindowRole::Normal,
            bounds: Rect::new(0, 0, 1920, 1080),
            display_id: crate::DisplayId(1),
            capabilities: crate::WindowCapabilities {
                can_move: true,
                can_resize: true,
                can_minimize: true,
                can_maximize: true,
            },
            elevated: false,
            lifecycle: WindowLifecycle::Maximized,
            minimum_size: None,
        };

        let draft = RecoveryDraft::capture(&window, "s1", "D1", Rect::new(10, 10, 800, 600), 5);

        assert_eq!(draft.show_state, ShowState::Maximized);
        assert_eq!(draft.normal_bounds, Rect::new(10, 10, 800, 600));
        assert_eq!(draft.visible_bounds, Rect::new(0, 0, 1920, 1080));
        assert!(!serde_json::to_string(&draft).unwrap().contains("secret"));
    }

    #[test]
    fn verdict_and_refusal_codes_are_distinct() {
        let verdicts = [
            HandleVerdict::Verified.code(),
            HandleVerdict::Stale.code(),
            HandleVerdict::Reused {
                recorded: ProcessInstance {
                    process_id: 1,
                    creation_time: 1,
                },
                live: ProcessInstance {
                    process_id: 1,
                    creation_time: 2,
                },
                recorded_class: None,
                live_class: None,
            }
            .code(),
            HandleVerdict::Ambiguous { claimants: 2 }.code(),
        ];
        let mut unique = verdicts.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), verdicts.len());

        let refusals = [
            ParkingRefusal::NotManaged {
                window_id: WindowId(1),
            }
            .code(),
            ParkingRefusal::PersistenceDegraded.code(),
            ParkingRefusal::ParkingCapabilityUnverified.code(),
            ParkingRefusal::ParkingRefused {
                reason: String::new(),
            }
            .code(),
            ParkingRefusal::Fullscreen {
                window_id: WindowId(1),
            }
            .code(),
            ParkingRefusal::Minimized {
                window_id: WindowId(1),
            }
            .code(),
            ParkingRefusal::UnknownDisplay {
                window_id: WindowId(1),
            }
            .code(),
            ParkingRefusal::AlreadyPending {
                window_id: WindowId(1),
            }
            .code(),
        ];
        let mut unique = refusals.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), refusals.len());
    }
}
