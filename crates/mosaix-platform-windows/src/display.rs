//! Display topology detection and connect/disconnect watching via Win32.
//!
//! `enumerate_displays` walks the current monitor set with
//! `EnumDisplayMonitors`/`GetMonitorInfoW`, per-monitor DPI
//! (`GetDpiForMonitor`), and current display settings
//! (`EnumDisplaySettingsW`) for rotation, producing `mosaix_domain::Display`
//! values directly -- unlike the move/resize and event-hook spikes, display
//! topology is squarely a domain concept (architecture doc 7.1), so this
//! module depends on `mosaix-domain` rather than staying standalone.
//!
//! `watch_display_topology` owns a hidden top-level window (message-only
//! windows are excluded from the `WM_DISPLAYCHANGE` broadcast, so a real
//! one is required) on a dedicated thread, and re-enumerates on every
//! `WM_DISPLAYCHANGE` -- which Windows sends for monitor connect/disconnect
//! as well as resolution/arrangement changes. Like the OS event hooks, this
//! is a hint, not a diff: callers compare `mosaix_domain::topology_fingerprint`
//! across calls to decide whether anything meaningful actually changed.
//!
//! `stable_fingerprint` here is the architecture doc's documented fallback
//! tier (device name + geometry + scale), not the strongest EDID-based
//! identifier from `QueryDisplayConfig`/`DisplayConfigGetDeviceInfo` --
//! upgrading to that is follow-up work.

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use mosaix_domain::{Display, DisplayId, Rect, Rotation};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, DMDO_180, DMDO_270,
    DMDO_90, ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFOEXW,
};

use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostThreadMessageW,
    RegisterClassW, TranslateMessage, MONITORINFOF_PRIMARY, MSG, WINDOW_EX_STYLE, WM_DISPLAYCHANGE,
    WM_POWERBROADCAST, WM_QUIT, WNDCLASSW, WS_OVERLAPPED,
};

use crate::{Result, WindowError};

fn rect_from_win32(rect: RECT) -> Rect {
    Rect::new(
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    )
}

fn wchar_array_to_string(chars: &[u16]) -> String {
    let len = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..len])
}

fn monitor_scale_factor(hmonitor: HMONITOR) -> Option<f64> {
    let mut dpi_x = 0u32;
    let mut dpi_y = 0u32;
    unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.ok()?;
    Some(dpi_x as f64 / 96.0)
}

fn monitor_rotation(device_name: &str) -> Rotation {
    let wide: Vec<u16> = device_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut devmode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    let ok =
        unsafe { EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut devmode) };
    if !ok.as_bool() {
        return Rotation::Landscape;
    }

    match unsafe { devmode.Anonymous1.Anonymous2.dmDisplayOrientation } {
        DMDO_90 => Rotation::Portrait,
        DMDO_180 => Rotation::LandscapeFlipped,
        DMDO_270 => Rotation::PortraitFlipped,
        _ => Rotation::Landscape,
    }
}

fn describe_monitor(hmonitor: HMONITOR) -> Option<Display> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo) };
    if !ok.as_bool() {
        return None;
    }

    let full_bounds = rect_from_win32(info.monitorInfo.rcMonitor);
    let work_area = rect_from_win32(info.monitorInfo.rcWork);
    let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;
    let device_name = wchar_array_to_string(&info.szDevice);
    let scale_factor = monitor_scale_factor(hmonitor).unwrap_or(1.0);
    let rotation = monitor_rotation(&device_name);

    let stable_fingerprint = format!(
        "{device_name}|{}x{}|scale={scale_factor}",
        full_bounds.width, full_bounds.height
    );

    Some(Display {
        id: DisplayId(hmonitor.0 as isize),
        stable_fingerprint,
        full_bounds,
        work_area,
        scale_factor,
        rotation,
        is_primary,
    })
}

unsafe extern "system" fn enum_monitor_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let displays = unsafe { &mut *(lparam.0 as *mut Vec<Display>) };
    if let Some(display) = describe_monitor(hmonitor) {
        displays.push(display);
    }
    BOOL(1)
}

/// Enumerates all currently active monitors: geometry, work area (taskbar
/// excluded), DPI scale, rotation, and arrangement (via each display's
/// `full_bounds` position on the shared virtual desktop).
pub fn enumerate_displays() -> Result<Vec<Display>> {
    let mut displays: Vec<Display> = Vec::new();
    let lparam = LPARAM(std::ptr::addr_of_mut!(displays) as isize);

    let ok = unsafe { EnumDisplayMonitors(None, None, Some(enum_monitor_proc), lparam) };
    if !ok.as_bool() {
        return Err(WindowError::EnumerateDisplaysFailed);
    }

    Ok(displays)
}

/// A topology-change notification. Carries a fresh enumeration rather than
/// a diff -- like the OS event hooks, this is a hint prompting the caller
/// to re-check, not an authoritative delta.
#[derive(Debug, Clone)]
pub enum TopologyEvent {
    /// The display configuration changed (monitor connect/disconnect,
    /// resolution, DPI, or arrangement change via `WM_DISPLAYCHANGE`).
    Changed(Vec<Display>),
    /// The system just resumed from sleep or hibernation
    /// (`WM_POWERBROADCAST` / `PBT_APMRESUMEAUTOMATIC`).  Carries a fresh
    /// display enumeration taken after a brief settling delay (monitors need
    /// time to re-initialize after wake).  The agent should use this to
    /// send [`mosaix_engine::Event::WakeReconciliation`] rather than the
    /// ordinary [`mosaix_engine::Event::DisplayTopologyChanged`], because
    /// wake recovery also needs to re-enumerate windows.
    WakeFromSleep(Vec<Display>),
}

thread_local! {
    static TOPOLOGY_SENDER: RefCell<Option<Sender<TopologyEvent>>> = const { RefCell::new(None) };
}

/// Settling delay after a wake event before re-enumerating displays.
///
/// Monitors need time to re-initialize after the system wakes from sleep.
/// Querying the display list too quickly can return an empty or stale
/// topology.  Two seconds is a conservative but safe budget that avoids
/// returning a stale (possibly empty) topology.
const WAKE_SETTLE_MILLIS: u64 = 2000;

unsafe extern "system" fn topology_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_DISPLAYCHANGE {
        let displays = enumerate_displays().unwrap_or_default();
        TOPOLOGY_SENDER.with(|sender| {
            if let Some(tx) = sender.borrow().as_ref() {
                let _ = tx.send(TopologyEvent::Changed(displays));
            }
        });
        return LRESULT(0);
    }

    // Feature 29 — sleep/wake recovery.
    //
    // `PBT_APMRESUMEAUTOMATIC` (0x0012) fires when the system wakes (both
    // from user action and automatic wake).  We wait for the display
    // subsystem to settle before re-enumerating, then emit `WakeFromSleep`
    // so the agent can send `Event::WakeReconciliation` and also
    // re-enumerate windows.
    //
    // The `windows` crate v0.58 does not expose this particular constant
    // through its generated API, so we use the raw value from the SDK docs:
    // https://learn.microsoft.com/en-us/windows/win32/power/pbt-apmresumeautomatic
    if msg == WM_POWERBROADCAST && wparam.0 as u32 == 0x0012u32 {
        tracing::info!("system wake detected; waiting for display subsystem to settle");
        std::thread::sleep(std::time::Duration::from_millis(WAKE_SETTLE_MILLIS));
        let displays = enumerate_displays().unwrap_or_default();
        tracing::info!(
            display_count = displays.len(),
            "display enumeration complete after wake"
        );
        TOPOLOGY_SENDER.with(|sender| {
            if let Some(tx) = sender.borrow().as_ref() {
                let _ = tx.send(TopologyEvent::WakeFromSleep(displays));
            }
        });
        return LRESULT(0);
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// A running display-topology watcher, owned by a dedicated thread with a
/// hidden top-level window. Dropping (or [`stop`](Self::stop)) tears the
/// window down and joins the thread.
pub struct DisplayWatcher {
    thread_id: u32,
    join_handle: Option<JoinHandle<()>>,
    #[allow(dead_code)]
    hwnd: HWND,
}

impl DisplayWatcher {
    /// Stops the watcher thread and waits for it to exit.
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

    #[cfg(test)]
    pub(crate) fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for DisplayWatcher {
    fn drop(&mut self) {
        self.request_stop();
    }
}

fn create_topology_window() -> Result<HWND> {
    let class_name = w!("MosaixDisplayTopologyWatcherWindow");
    let hinstance = unsafe { GetModuleHandleW(None) }.map_err(WindowError::from)?;

    let class = WNDCLASSW {
        lpfnWndProc: Some(topology_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    // Ignore failure: a prior watcher in the same process may have already
    // registered this class.
    unsafe { RegisterClassW(&class) };

    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("mosaix-platform-windows display topology watcher"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            hinstance,
            None,
        )
    }
    .map_err(WindowError::from)
}

/// Starts watching for display topology changes (monitor connect/disconnect,
/// resolution, and arrangement changes) via `WM_DISPLAYCHANGE`.
///
/// Returns a handle controlling the watcher's lifetime and a channel of
/// topology-change notifications.
pub fn watch_display_topology() -> Result<(DisplayWatcher, Receiver<TopologyEvent>)> {
    let (tx, rx) = mpsc::channel();
    // `HWND` wraps a raw pointer and so is not `Send`; carry it across this
    // handshake channel as the same value stored in an `isize` instead.
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, isize)>>();

    let join_handle = thread::spawn(move || {
        TOPOLOGY_SENDER.with(|sender| *sender.borrow_mut() = Some(tx));

        let hwnd = match create_topology_window() {
            Ok(hwnd) => hwnd,
            Err(err) => {
                let _ = ready_tx.send(Err(err));
                return;
            }
        };

        let thread_id = unsafe { GetCurrentThreadId() };
        let _ = ready_tx.send(Ok((thread_id, hwnd.0 as isize)));

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
    });

    match ready_rx.recv() {
        Ok(Ok((thread_id, hwnd))) => Ok((
            DisplayWatcher {
                thread_id,
                join_handle: Some(join_handle),
                hwnd: HWND(hwnd as *mut std::ffi::c_void),
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
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

    #[test]
    fn enumerate_displays_returns_a_sane_primary_display() {
        let displays = enumerate_displays().expect("should enumerate displays");
        assert!(!displays.is_empty(), "expected at least one display");

        let primary_count = displays.iter().filter(|d| d.is_primary).count();
        assert_eq!(primary_count, 1, "expected exactly one primary display");

        for d in &displays {
            assert!(
                d.scale_factor > 0.0,
                "scale factor should be positive: {d:?}"
            );
            assert!(
                d.full_bounds.contains(&d.work_area),
                "work area should fit inside full bounds: {d:?}"
            );
        }
    }

    #[test]
    fn topology_watcher_reacts_to_wm_displaychange() {
        let (watcher, rx) = watch_display_topology().expect("watcher should start");

        unsafe {
            let _ = PostMessageW(watcher.hwnd(), WM_DISPLAYCHANGE, WPARAM(0), LPARAM(0));
        }

        let event = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("expected a TopologyEvent after WM_DISPLAYCHANGE");

        match event {
            TopologyEvent::Changed(displays) => {
                assert!(
                    !displays.is_empty(),
                    "expected the fresh enumeration to be non-empty"
                );
            }
            TopologyEvent::WakeFromSleep(_) => {
                panic!("expected TopologyEvent::Changed after WM_DISPLAYCHANGE, got WakeFromSleep");
            }
        }

        watcher.stop();
    }
}
