//! OS-level window event hooks via `SetWinEventHook`.
//!
//! Same "raw spike" scope as `move_resize_window`: no dependency on
//! `mosaix-domain` or `mosaix-platform-api`. Covers exactly the events the
//! architecture doc calls out for the Windows adapter -- window lifecycle,
//! focus, and move/resize -- via a dedicated thread that owns the hooks and
//! pumps the message loop `WINEVENT_OUTOFCONTEXT` delivery requires.
//!
//! `EVENT_SYSTEM_MOVESIZESTART`/`END` only fire for an interactive
//! (mouse/keyboard driven) move or resize, not for a programmatic
//! `SetWindowPos` call, so they aren't exercised by the automated test here
//! -- see the architecture doc's platform integration matrix for that
//! tier of testing.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, EVENT_OBJECT_CREATE,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, MSG, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    WM_QUIT,
};

use crate::{Result, WindowError};

/// A window handle carried across threads.
///
/// `HWND` wraps a raw pointer and so is not `Send`. Handles are stable,
/// process-local identifiers rather than pointers we dereference ourselves,
/// so this newtype stores the same value as an `isize` purely to move
/// events across the channel to the caller's thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowHandle(pub isize);

impl From<HWND> for WindowHandle {
    fn from(hwnd: HWND) -> Self {
        WindowHandle(hwnd.0 as isize)
    }
}

impl From<WindowHandle> for HWND {
    fn from(handle: WindowHandle) -> Self {
        HWND(handle.0 as *mut c_void)
    }
}

/// A normalized OS window event, covering the subset the architecture doc
/// calls out for the Windows adapter: lifecycle, focus, and move/resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RawEvent {
    WindowCreated(WindowHandle),
    WindowDestroyed(WindowHandle),
    Focused(WindowHandle),
    LocationChanged(WindowHandle),
    MoveResizeStart(WindowHandle),
    MoveResizeEnd(WindowHandle),
}

thread_local! {
    static EVENT_SENDER: RefCell<Option<Sender<RawEvent>>> = const { RefCell::new(None) };
}

const CHILDID_SELF: i32 = 0;

unsafe extern "system" fn win_event_proc(
    _hwineventhook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    idobject: i32,
    idchild: i32,
    _ideventthread: u32,
    _dwmseventtime: u32,
) {
    if hwnd.0.is_null() || idobject != OBJID_WINDOW.0 || idchild != CHILDID_SELF {
        return;
    }

    let handle = WindowHandle::from(hwnd);
    let raw = match event {
        EVENT_OBJECT_CREATE => RawEvent::WindowCreated(handle),
        EVENT_OBJECT_DESTROY => RawEvent::WindowDestroyed(handle),
        EVENT_OBJECT_LOCATIONCHANGE => RawEvent::LocationChanged(handle),
        EVENT_SYSTEM_FOREGROUND => RawEvent::Focused(handle),
        EVENT_SYSTEM_MOVESIZESTART => RawEvent::MoveResizeStart(handle),
        EVENT_SYSTEM_MOVESIZEEND => RawEvent::MoveResizeEnd(handle),
        _ => return,
    };

    EVENT_SENDER.with(|sender| {
        if let Some(tx) = sender.borrow().as_ref() {
            let _ = tx.send(raw);
        }
    });
}

/// Registers all hooks, or unhooks whatever was already registered and
/// returns an error if any registration fails partway through.
unsafe fn register_hooks() -> Result<Vec<HWINEVENTHOOK>> {
    let ranges: [(u32, u32); 4] = [
        (EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY),
        (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
    ];

    let mut hooks = Vec::with_capacity(ranges.len());
    for (min, max) in ranges {
        let hook = unsafe {
            SetWinEventHook(
                min,
                max,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        if hook.is_invalid() {
            for hook in hooks {
                unsafe {
                    let _ = UnhookWinEvent(hook);
                }
            }
            return Err(WindowError::EventHookRegistrationFailed);
        }
        hooks.push(hook);
    }
    Ok(hooks)
}

/// A running set of OS event hooks, owned by a dedicated thread.
///
/// Dropping (or explicitly calling [`stop`](Self::stop)) unhooks and joins
/// the thread.
pub struct EventHooks {
    thread_id: u32,
    join_handle: Option<JoinHandle<()>>,
}

impl EventHooks {
    /// Stops the hook thread and waits for it to exit.
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

impl Drop for EventHooks {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Starts a dedicated thread that registers `SetWinEventHook` hooks for
/// window lifecycle, focus, and move/resize events, and pumps the message
/// loop those hooks require.
///
/// Returns a handle controlling the thread's lifetime and a channel of
/// normalized events.
pub fn start_event_hooks() -> Result<(EventHooks, Receiver<RawEvent>)> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32>>();

    let join_handle = thread::spawn(move || {
        EVENT_SENDER.with(|sender| *sender.borrow_mut() = Some(tx));

        let hooks = match unsafe { register_hooks() } {
            Ok(hooks) => hooks,
            Err(err) => {
                let _ = ready_tx.send(Err(err));
                return;
            }
        };

        let thread_id = unsafe { GetCurrentThreadId() };
        let _ = ready_tx.send(Ok(thread_id));

        let mut msg = MSG::default();
        loop {
            let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if result.0 <= 0 {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        for hook in hooks {
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
    });

    match ready_rx.recv() {
        Ok(Ok(thread_id)) => Ok((
            EventHooks {
                thread_id,
                join_handle: Some(join_handle),
            },
            rx,
        )),
        Ok(Err(err)) => {
            let _ = join_handle.join();
            Err(err)
        }
        Err(_) => {
            let _ = join_handle.join();
            Err(WindowError::EventHookRegistrationFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::move_resize_window;
    use crate::test_support::{create_test_window, wait_for};
    use mosaix_domain::Rect;
    use std::time::Duration;
    use windows::Win32::UI::Accessibility::NotifyWinEvent;
    use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

    #[test]
    fn observes_create_location_change_and_destroy() {
        let (hooks, rx) = start_event_hooks().expect("hooks should register");

        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);

        assert!(
            wait_for(
                &rx,
                |e| matches!(e, RawEvent::WindowCreated(h) if *h == handle),
                Duration::from_secs(2),
            ),
            "expected WindowCreated for the freshly created window"
        );

        move_resize_window(hwnd, Rect::new(10, 10, 300, 200)).expect("move/resize should succeed");

        assert!(
            wait_for(
                &rx,
                |e| matches!(e, RawEvent::LocationChanged(h) if *h == handle),
                Duration::from_secs(2),
            ),
            "expected LocationChanged after SetWindowPos"
        );

        unsafe { DestroyWindow(hwnd) }.expect("cleanup should succeed");

        assert!(
            wait_for(
                &rx,
                |e| matches!(e, RawEvent::WindowDestroyed(h) if *h == handle),
                Duration::from_secs(2),
            ),
            "expected WindowDestroyed after DestroyWindow"
        );

        hooks.stop();
    }

    #[test]
    fn accepts_foreground_system_events_without_object_identifiers() {
        let (tx, rx) = mpsc::channel();
        EVENT_SENDER.with(|sender| *sender.borrow_mut() = Some(tx));
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);

        unsafe {
            win_event_proc(
                HWINEVENTHOOK::default(),
                EVENT_SYSTEM_FOREGROUND,
                hwnd,
                0,
                0,
                0,
                0,
            );
        }

        assert!(
            wait_for(
                &rx,
                |event| matches!(event, RawEvent::Focused(found) if *found == handle),
                Duration::from_secs(2),
            ),
            "expected a foreground system event without object identifiers to be accepted"
        );

        unsafe { DestroyWindow(hwnd) }.expect("cleanup should succeed");
        EVENT_SENDER.with(|sender| *sender.borrow_mut() = None);
    }

    #[test]
    fn observes_native_move_resize_start_and_end_events() {
        let (hooks, rx) = start_event_hooks().expect("hooks should register");
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);

        unsafe {
            NotifyWinEvent(
                EVENT_SYSTEM_MOVESIZESTART,
                hwnd,
                OBJID_WINDOW.0,
                CHILDID_SELF,
            );
            NotifyWinEvent(EVENT_SYSTEM_MOVESIZEEND, hwnd, OBJID_WINDOW.0, CHILDID_SELF);
        }

        assert!(
            wait_for(
                &rx,
                |event| matches!(event, RawEvent::MoveResizeStart(found) if *found == handle),
                Duration::from_secs(2),
            ),
            "expected MoveResizeStart from the native accessibility event"
        );
        assert!(
            wait_for(
                &rx,
                |event| matches!(event, RawEvent::MoveResizeEnd(found) if *found == handle),
                Duration::from_secs(2),
            ),
            "expected MoveResizeEnd from the native accessibility event"
        );

        unsafe { DestroyWindow(hwnd) }.expect("cleanup should succeed");
        hooks.stop();
    }
}
