//! The agent's side of the recovery ledger (ADR 0023): the platform
//! closures the shared recovery routine needs, and the enrichment a
//! recovery draft gets from the live window before it is written.
//!
//! Both the startup pass and the clean-exit pass in `main` go through
//! [`recover_with_platform`], and so does `mosaix restore-windows`, which
//! is what keeps "which handles may be touched" one answer everywhere.

use std::path::Path;
use std::sync::{Arc, Mutex};

use mosaix_domain::recovery::{
    verdict_for, HandleVerdict, RecoveryDraft, RecoveryEntryId, RecoveryOutcome,
};
use mosaix_domain::workspace::ParkingCapability;
use mosaix_domain::{Display, WindowId};
use mosaix_persistence::{recover_parked_windows, PersistenceError, RecoveryLedger};
use mosaix_platform_windows::{ParkedAs, ParkingSite, WindowHandle};

/// The parking site validated for the current topology, shared between
/// the topology forwarder that re-validates it on every change and the
/// executor that parks against it. `None` until a site is verified, and
/// again whenever the topology refuses one.
pub type SharedParkingSite = Arc<Mutex<Option<ParkingSite>>>;

/// Validates a parking site for `displays`, publishes it to `site` for
/// the executor, and returns what the engine should be told.
pub fn report_parking_capability(
    displays: &[Display],
    site: &SharedParkingSite,
) -> ParkingCapability {
    let (capability, found) = mosaix_platform_windows::parking_capability(displays);
    match &found {
        Some(found) => tracing::info!(edge = found.edge.code(), "parking site verified"),
        None => tracing::warn!(?capability, "no recoverable parking site for this topology"),
    }
    *site.lock().expect("parking site mutex poisoned") = found;
    capability
}

/// Parks `window_id` at the shared site, or says why it could not.
///
/// A window found minimized is not parked: it occupies no screen, and
/// the engine refuses to authorise parking one, so reaching it here means
/// it minimized between authorisation and effect. Reporting that as a
/// failure keeps its ledger entry recorded but never parked, which is
/// the truth.
pub fn park(window_id: WindowId, site: &SharedParkingSite) -> Result<ParkedAs, String> {
    let site = site
        .lock()
        .expect("parking site mutex poisoned")
        .clone()
        .ok_or_else(|| "no verified parking site for the current topology".to_owned())?;
    match mosaix_platform_windows::park_window(WindowHandle(window_id.0), &site) {
        Ok(ParkedAs::LeftMinimized) => {
            Err("the window is minimized and was left as it is".to_owned())
        }
        Ok(parked_as) => Ok(parked_as),
        Err(error) => Err(error.to_string()),
    }
}

/// Puts the parked `window_id` back where ledger entry `entry_id`
/// recorded it, through the same verification recovery applies: the
/// handle must still name the recorded process instance and class. The
/// ledger is read, never written, here; the reducer records the outcome.
pub fn restore_parked_window(
    ledger_path: Option<&Path>,
    window_id: WindowId,
    entry_id: RecoveryEntryId,
) -> Result<(), String> {
    let path = ledger_path.ok_or_else(|| "this platform has no recovery ledger".to_owned())?;
    let ledger = RecoveryLedger::open(path).map_err(|error| error.to_string())?;
    let entry = ledger
        .entries()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|entry| entry.id == entry_id)
        .ok_or_else(|| format!("ledger entry {} does not exist", entry_id.0))?;
    if entry.draft.native_handle != window_id.0 {
        return Err(format!(
            "ledger entry {} records window {}, not window {}",
            entry_id.0, entry.draft.native_handle, window_id.0
        ));
    }
    let handle = WindowHandle(window_id.0);
    match verdict_for(&entry, mosaix_platform_windows::probe_handle(handle)) {
        HandleVerdict::Verified => mosaix_platform_windows::restore_window(handle, &entry)
            .map_err(|error| error.to_string()),
        verdict => Err(format!(
            "the handle no longer verifiably names the recorded window ({})",
            verdict.code()
        )),
    }
}

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
