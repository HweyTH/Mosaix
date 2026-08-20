//! Global hotkeys via `RegisterHotKey`/`WM_HOTKEY` (ADR 0002).
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
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
        VK_F13, VK_F16,
    };

    fn send_key_press(vk: VIRTUAL_KEY) {
        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: Default::default(),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let mut up = down;
        up.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;

        let inputs = [down, up];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        assert_eq!(
            sent,
            inputs.len() as u32,
            "SendInput should submit both events"
        );
    }

    #[test]
    fn registers_and_delivers_a_real_hotkey_firing() {
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

        send_key_press(VK_F13);

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
}
