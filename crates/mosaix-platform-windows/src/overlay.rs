//! Snap preview overlay: a single layered, click-through popup that shows
//! where a window will land.
//!
//! Owned by a dedicated thread with a message loop (same shape as
//! `display.rs` / `hotkeys.rs`). Callers post show/hide requests; the
//! owning thread alone creates, paints, moves, and destroys the window.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mosaix_domain::Rect;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, FrameRect, HBRUSH, HGDIOBJ,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    PostMessageW, PostThreadMessageW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, LWA_ALPHA, MSG, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_APP, WM_DESTROY, WM_PAINT, WM_QUIT, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::{Result, WindowError};

const WM_SHOW_PREVIEW: u32 = WM_APP + 40;
const WM_HIDE_PREVIEW: u32 = WM_APP + 41;

/// Semi-transparent alpha for the whole layered window (~30% opacity).
const OVERLAY_ALPHA: u8 = 76;

/// Accent fill color (Windows blue, BGR).
const OVERLAY_FILL: COLORREF = COLORREF(0x00_D7_78_00);
/// Slightly darker border (BGR).
const OVERLAY_BORDER: COLORREF = COLORREF(0x00_9E_5A_00);

thread_local! {
    static PENDING_BOUNDS: RefCell<Option<Arc<Mutex<Option<Rect>>>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            let fill = unsafe { CreateSolidBrush(OVERLAY_FILL) };
            let border = unsafe { CreateSolidBrush(OVERLAY_BORDER) };
            let mut client = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            unsafe {
                let _ = FillRect(hdc, &client, fill);
                let _ = FrameRect(hdc, &client, border);
                let _ = DeleteObject(HGDIOBJ(fill.0));
                let _ = DeleteObject(HGDIOBJ(border.0));
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_SHOW_PREVIEW => {
            let bounds = PENDING_BOUNDS.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|shared| shared.lock().ok().and_then(|g| *g))
            });
            if let Some(bounds) = bounds {
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        bounds.x,
                        bounds.y,
                        bounds.width,
                        bounds.height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }
            LRESULT(0)
        }
        WM_HIDE_PREVIEW => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_overlay_window() -> Result<HWND> {
    let class_name = w!("MosaixSnapPreviewWindow");
    let hinstance = unsafe { GetModuleHandleW(None) }.map_err(WindowError::from)?;

    let class = WNDCLASSW {
        lpfnWndProc: Some(overlay_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: class_name,
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        ..Default::default()
    };
    // Ignore failure: a prior overlay in the same process may have already
    // registered this class.
    unsafe { RegisterClassW(&class) };

    let ex_style =
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(ex_style.0),
            class_name,
            w!("mosaix snap preview"),
            WS_POPUP,
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
    .map_err(WindowError::from)?;

    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), OVERLAY_ALPHA, LWA_ALPHA) }
        .map_err(WindowError::from)?;

    Ok(hwnd)
}

/// A running snap-preview overlay window owned by a dedicated thread.
///
/// Dropping (or [`stop`](Self::stop)) hides the window, exits the message
/// loop, and joins the thread.
pub struct PreviewOverlay {
    hwnd: isize,
    thread_id: u32,
    pending: Arc<Mutex<Option<Rect>>>,
    join_handle: Option<JoinHandle<()>>,
    stopped: AtomicBool,
}

impl PreviewOverlay {
    /// Shows (or repositions) the overlay at `bounds` in physical-pixel
    /// screen coordinates. Cross-thread-safe: posts to the owner thread.
    pub fn show(&self, bounds: Rect) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut guard) = self.pending.lock() {
            *guard = Some(bounds);
        }
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            let _ = PostMessageW(hwnd, WM_SHOW_PREVIEW, WPARAM(0), LPARAM(0));
        }
    }

    /// Hides the overlay. Cross-thread-safe.
    pub fn hide(&self) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut guard) = self.pending.lock() {
            *guard = None;
        }
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            let _ = PostMessageW(hwnd, WM_HIDE_PREVIEW, WPARAM(0), LPARAM(0));
        }
    }

    /// Stops the overlay thread and waits for it to exit.
    pub fn stop(mut self) {
        self.request_stop();
    }

    fn request_stop(&mut self) {
        if self.stopped.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Some(join_handle) = self.join_handle.take() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = join_handle.join();
        }
    }

    #[cfg(test)]
    pub(crate) fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }
}

impl Drop for PreviewOverlay {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Starts the snap-preview overlay on a dedicated thread.
pub fn start_preview_overlay() -> Result<PreviewOverlay> {
    let pending = Arc::new(Mutex::new(None));
    let pending_for_thread = Arc::clone(&pending);
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, isize)>>();

    let join_handle = thread::spawn(move || {
        PENDING_BOUNDS.with(|cell| *cell.borrow_mut() = Some(pending_for_thread));

        let hwnd = match create_overlay_window() {
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

        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    });

    match ready_rx.recv() {
        Ok(Ok((thread_id, hwnd))) => Ok(PreviewOverlay {
            hwnd,
            thread_id,
            pending,
            join_handle: Some(join_handle),
            stopped: AtomicBool::new(false),
        }),
        Ok(Err(err)) => {
            let _ = join_handle.join();
            Err(err)
        }
        Err(_) => {
            let _ = join_handle.join();
            Err(WindowError::HotkeyThreadStartFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ensure_dpi_awareness;
    use std::time::Duration;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindowVisible};

    #[test]
    fn show_positions_the_overlay_and_hide_conceals_it() {
        ensure_dpi_awareness();
        let overlay = start_preview_overlay().expect("overlay thread should start");
        let target = Rect::new(80, 60, 400, 300);

        overlay.show(target);
        // Give the owner thread a moment to process the posted message.
        std::thread::sleep(Duration::from_millis(100));

        let hwnd = overlay.hwnd();
        assert!(
            unsafe { IsWindowVisible(hwnd) }.as_bool(),
            "overlay should be visible after show"
        );
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rect) }.expect("GetWindowRect should succeed");
        assert_eq!(rect.left, target.x);
        assert_eq!(rect.top, target.y);
        assert_eq!(rect.right - rect.left, target.width);
        assert_eq!(rect.bottom - rect.top, target.height);

        overlay.hide();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !unsafe { IsWindowVisible(hwnd) }.as_bool(),
            "overlay should be hidden after hide"
        );

        overlay.stop();
    }
}
