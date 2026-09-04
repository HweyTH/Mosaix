//! The agent's side of the recovery ledger (ADR 0023): the platform
//! closures the shared recovery routine needs, and the enrichment a
//! recovery draft gets from the live window before it is written.
//!
//! Both the startup pass and the clean-exit pass in `main` go through
//! [`recover_with_platform`], and so does `mosaix restore-windows`, which
//! is what keeps "which handles may be touched" one answer everywhere.

use mosaix_domain::recovery::{RecoveryDraft, RecoveryOutcome};
use mosaix_persistence::{recover_parked_windows, PersistenceError, RecoveryLedger};
use mosaix_platform_windows::WindowHandle;

/// Restores every open entry whose handle still verifiably names the
/// window it recorded, using the real Windows probe and placement calls.
pub fn recover_with_platform(
    ledger: &mut RecoveryLedger,
) -> Result<Vec<RecoveryOutcome>, PersistenceError> {
    recover_parked_windows(
        ledger,
        |handle| mosaix_platform_windows::probe_handle(WindowHandle(handle)),
        |entry| {
            mosaix_platform_windows::restore_window(WindowHandle(entry.draft.native_handle), entry)
                .map_err(|error| error.to_string())
        },
    )
}

/// Fills in what the reducer cannot know: the owning process instance,
/// and the normal bounds and show state as the window reports them now.
pub fn enrich_draft(draft: &mut RecoveryDraft) {
    let handle = WindowHandle(draft.native_handle);
    if let Some(creation_time) =
        mosaix_platform_windows::process_creation_time(draft.process.process_id)
    {
        draft.process.creation_time = creation_time;
    }
    if let Some((normal_bounds, show_state)) = mosaix_platform_windows::window_placement(handle) {
        draft.normal_bounds = normal_bounds;
        draft.show_state = show_state;
    }
}
