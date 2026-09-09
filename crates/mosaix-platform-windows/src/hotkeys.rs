//! Global hotkeys via `RegisterHotKey`/`WM_HOTKEY`.
//!
//! Same shape as `events.rs`: a dedicated thread owns the registrations
//! and pumps the message loop `WM_HOTKEY` delivery requires. Registration
//! is per-binding partial-success, not all-or-nothing -- a combination
//! already owned by the OS or another app is reported as a failure for
//! just that binding, while the rest still register.

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_HOTKEY, WM_QUIT,
};

use crate::{Result, WindowError};

/// A global hotkey to register: a modifier set plus a virtual-key code,
/// under a caller-chosen `id` used to identify the binding when it fires
/// and when reporting its registration outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub id: i32,
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

/// A normalized firing of a successfully registered hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeyFired {
    pub id: i32,
}

/// The outcome of registering one [`HotkeyBinding`].
#[derive(Debug)]
pub struct HotkeyRegistrationResult {
    pub id: i32,
    pub outcome: std::result::Result<(), WindowError>,
}

thread_local! {
    static HOTKEY_SENDER: RefCell<Option<Sender<HotkeyFired>>> = const { RefCell::new(None) };
}

fn register_bindings(bindings: &[HotkeyBinding]) -> Vec<HotkeyRegistrationResult> {
    bindings
        .iter()
        .map(|binding| {
            let outcome =
                unsafe { RegisterHotKey(None, binding.id, binding.modifiers, binding.vk) }
                    .map_err(WindowError::HotkeyRegistrationFailed);
            HotkeyRegistrationResult {
                id: binding.id,
                outcome,
            }
        })
        .collect()
}

/// Whether a combination can be registered right now.
///
/// Deliberately two answers, not three. The probe cannot tell a
/// combination the operating system reserves from one another application
/// registered first -- both come back as a refusal -- and it cannot see
/// `Win+L` or `Ctrl+Alt+Del` at all, because those are never registered
/// hotkeys. Naming the owner is the caller's job: it knows Mosaix's own
/// bindings, and it holds the short reserved list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAvailability {
    /// `RegisterHotKey` accepted it, and the registration was released
    /// again immediately.
    Available,
    /// `RegisterHotKey` refused it. Something already owns it.
    Taken,
}

/// Asks the operating system whether `modifiers`+`vk` is free, by
/// registering it and releasing it again.
///
/// The registration is undone before this returns, whether it succeeded
/// or not, so probing never leaves a combination held -- which matters
/// because the whole point of probing is to answer a question while the
/// user still has the dialog open.
///
/// `RegisterHotKey` associates a hotkey with the calling thread and does
/// not need a message pump to accept one, so unlike [`start_hotkeys`]
/// this needs no thread of its own. The id is a fixed one outside the
/// range the registry hands out, and it is unregistered on the same
/// thread that took it.
pub fn probe_hotkey(modifiers: HOT_KEY_MODIFIERS, vk: u32) -> HotkeyAvailability {
    match unsafe { RegisterHotKey(None, PROBE_HOTKEY_ID, modifiers, vk) } {
        Ok(()) => {
            unsafe {
                let _ = UnregisterHotKey(None, PROBE_HOTKEY_ID);
            }
            HotkeyAvailability::Available
        }
        Err(error) => {
            tracing::debug!(%error, "combination refused by RegisterHotKey; reporting it as taken");
            HotkeyAvailability::Taken
        }
    }
}

/// The id [`probe_hotkey`] borrows. Negative, so it can never collide
/// with an id the binding registry allocated for a real registration.
const PROBE_HOTKEY_ID: i32 = -1;

/// A running set of hotkey registrations, owned by a dedicated thread.
///
/// `results` reports the per-binding outcome of registration, in the same
/// order the bindings were passed to [`start_hotkeys`]. Dropping (or
/// explicitly calling [`stop`](Self::stop)) unregisters whichever bindings
/// succeeded, exits the message loop, and joins the thread.
pub struct HotkeyRegistrations {
    pub results: Vec<HotkeyRegistrationResult>,
    thread_id: u32,
    join_handle: Option<JoinHandle<()>>,
}

impl HotkeyRegistrations {
    /// Stops the registration thread and waits for it to exit.
    pub fn stop(mut self) {
        self.request_stop();
    }

    fn request_stop(&mut self) {
        if let Some(join_handle) = self.join_handle.take() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = join_handle.join();
        }
    }
}

impl Drop for HotkeyRegistrations {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Starts a dedicated thread that registers `RegisterHotKey` bindings and
/// pumps the message loop `WM_HOTKEY` delivery requires.
///
/// Registration is partial-success: a binding that fails (e.g. already
/// owned by the OS or another app) is reported in the returned
/// [`HotkeyRegistrations::results`] without preventing the rest from
/// registering. The outer `Result` only reports failure to start the
/// registration thread itself.
pub fn start_hotkeys(
    bindings: Vec<HotkeyBinding>,
) -> Result<(HotkeyRegistrations, Receiver<HotkeyFired>)> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel::<(u32, Vec<HotkeyRegistrationResult>)>();

    let join_handle = thread::spawn(move || {
        HOTKEY_SENDER.with(|sender| *sender.borrow_mut() = Some(tx));

        let results = register_bindings(&bindings);
        let registered_ids: Vec<i32> = results
            .iter()
            .filter(|result| result.outcome.is_ok())
            .map(|result| result.id)
            .collect();

        let thread_id = unsafe { GetCurrentThreadId() };
        let _ = ready_tx.send((thread_id, results));

        let mut msg = MSG::default();
        loop {
            let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if result.0 <= 0 {
                break;
            }
            if msg.message == WM_HOTKEY {
                let fired = HotkeyFired {
                    id: msg.wParam.0 as i32,
                };
                HOTKEY_SENDER.with(|sender| {
                    if let Some(tx) = sender.borrow().as_ref() {
                        let _ = tx.send(fired);
                    }
                });
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        for id in registered_ids {
            unsafe {
                let _ = UnregisterHotKey(None, id);
            }
        }
    });

    match ready_rx.recv() {
        Ok((thread_id, results)) => Ok((
            HotkeyRegistrations {
                results,
                thread_id,
                join_handle: Some(join_handle),
            },
            rx,
        )),
        Err(_) => {
            let _ = join_handle.join();
            Err(WindowError::HotkeyThreadStartFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::wait_for;
    use std::time::Duration;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F13, VK_F14, VK_F15, VK_F16, VK_F17};

    #[test]
    fn registers_and_delivers_a_hotkey_message() {
        let binding = HotkeyBinding {
            id: 1,
            modifiers: HOT_KEY_MODIFIERS(0),
            vk: VK_F13.0 as u32,
        };
        let (registrations, rx) = start_hotkeys(vec![binding]).expect("thread should start");

        assert_eq!(registrations.results.len(), 1);
        assert!(
            registrations.results[0].outcome.is_ok(),
            "expected VK_F13 to register cleanly: {:?}",
            registrations.results[0].outcome
        );

        // SendInput is denied on non-interactive Windows desktops used by CI.
        // Posting the native message directly keeps this message-pump test
        // deterministic; registration success is asserted independently above.
        unsafe {
            PostThreadMessageW(
                registrations.thread_id,
                WM_HOTKEY,
                WPARAM(binding.id as usize),
                LPARAM(0),
            )
        }
        .expect("WM_HOTKEY should reach the registration thread");

        assert!(
            wait_for(&rx, |fired| fired.id == 1, Duration::from_secs(2)),
            "expected a HotkeyFired for id 1 after pressing the registered key"
        );

        registrations.stop();
    }

    #[test]
    fn conflicting_binding_fails_without_blocking_the_rest() {
        let bindings = vec![
            HotkeyBinding {
                id: 10,
                modifiers: HOT_KEY_MODIFIERS(0),
                vk: VK_F16.0 as u32,
            },
            HotkeyBinding {
                id: 11,
                modifiers: HOT_KEY_MODIFIERS(0),
                vk: VK_F16.0 as u32,
            },
        ];
        let (registrations, _rx) = start_hotkeys(bindings).expect("thread should start");

        assert_eq!(registrations.results.len(), 2);
        assert!(
            registrations.results[0].outcome.is_ok(),
            "first registration of VK_F16 should succeed: {:?}",
            registrations.results[0].outcome
        );
        assert!(
            matches!(
                registrations.results[1].outcome,
                Err(WindowError::HotkeyRegistrationFailed(_))
            ),
            "duplicate registration of the same combination should fail: {:?}",
            registrations.results[1].outcome
        );

        registrations.stop();
    }

    #[test]
    fn a_free_combination_probes_as_available_and_is_left_free() {
        let probe = probe_hotkey(HOT_KEY_MODIFIERS(0), VK_F14.0 as u32);

        assert_eq!(probe, HotkeyAvailability::Available);
        // Released, or this would come back Taken by the probe itself --
        // and a dialog that made a combination unavailable by asking
        // about it would be worse than not asking.
        assert_eq!(
            probe_hotkey(HOT_KEY_MODIFIERS(0), VK_F14.0 as u32),
            HotkeyAvailability::Available,
            "probing must release whatever it registered"
        );
    }

    #[test]
    fn a_combination_something_else_owns_probes_as_taken() {
        let binding = HotkeyBinding {
            id: 20,
            modifiers: HOT_KEY_MODIFIERS(0),
            vk: VK_F15.0 as u32,
        };
        let (registrations, _rx) = start_hotkeys(vec![binding]).expect("thread should start");
        assert!(registrations.results[0].outcome.is_ok());

        assert_eq!(
            probe_hotkey(HOT_KEY_MODIFIERS(0), VK_F15.0 as u32),
            HotkeyAvailability::Taken
        );

        registrations.stop();
    }

    #[test]
    fn a_combination_comes_back_free_once_its_owner_releases_it() {
        let binding = HotkeyBinding {
            id: 21,
            modifiers: HOT_KEY_MODIFIERS(0),
            vk: VK_F17.0 as u32,
        };
        let (registrations, _rx) = start_hotkeys(vec![binding]).expect("thread should start");
        registrations.stop();

        assert!(
            wait_until(
                || probe_hotkey(HOT_KEY_MODIFIERS(0), VK_F17.0 as u32)
                    == HotkeyAvailability::Available,
                Duration::from_secs(2)
            ),
            "a combination Mosaix gave up has to read as free, or capture would \
             report every binding as conflicting with itself"
        );
    }

    /// Polls `condition` until it holds or `timeout` elapses.
    ///
    /// `UnregisterHotKey` takes effect on the owning thread, which
    /// [`HotkeyRegistrations::stop`] has to reach through the message
    /// loop, so a probe issued immediately afterwards can still see the
    /// old registration.
    fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        condition()
    }
}
