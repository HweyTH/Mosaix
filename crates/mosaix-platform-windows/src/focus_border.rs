//! Focus border overlay: a persistent, click-through outline around the
//! focused window (CONTEXT.md "Focus border").
//!
//! Deliberately a *second* overlay window rather than a fourth mode of the
//! snap preview's controller. The preview is transient and its modes
//! preempt each other -- a drag hides an in-flight flash -- whereas the
//! border must stay up across all of that. Two windows also means the two
//! can be visible at once, which is what a snap flash inside the focused
//! window should look like.
//!
//! The outline is hollow because the window's *region* excludes its middle
//! ([`SetWindowRgn`]), not because of a color key: what shows through is
//! exactly the untouched pixels of the window underneath, at any thickness.
//! The border is drawn *inside* the target bounds, so it never paints into
//! a gap or over a neighbour no matter how gaps are configured.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mosaix_domain::Rect;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CombineRgn, CreateRectRgn, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
    InvalidateRect, SetWindowRgn, HBRUSH, HGDIOBJ, PAINTSTRUCT, RGN_DIFF, RGN_ERROR,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    PostMessageW, PostThreadMessageW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, HWND_TOPMOST, LWA_ALPHA, MSG, SWP_NOACTIVATE, SW_HIDE,
    SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_APP, WM_DESTROY, WM_PAINT, WM_QUIT, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::{Result, WindowError};

const WM_SHOW_BORDER: u32 = WM_APP + 50;
const WM_HIDE_BORDER: u32 = WM_APP + 51;

/// Fully opaque. Unlike the snap preview -- which is a translucent wash
/// over where a window *will* go -- the border marks a window that is
/// already there, and reads better crisp.
const BORDER_ALPHA: u8 = 255;

/// The bounds, color, and thickness of one border placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderPlacement {
    pub bounds: Rect,
    /// 24-bit RGB, in the order a human writes `#RRGGBB`.
    ///
    /// A plain triple rather than `mosaix_config::Rgb` on purpose: this
    /// crate depends only on `mosaix-domain` and `mosaix-platform-api`, and
    /// a platform adapter has no business knowing the config crate exists.
    pub color: (u8, u8, u8),
    pub thickness: i32,
}

/// Win32 `COLORREF` is `0x00BBGGRR` -- byte-reversed from `#RRGGBB`.
fn colorref(color: (u8, u8, u8)) -> COLORREF {
    let (r, g, b) = color;
    COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16))
}

/// The rectangle punched out of the middle, in client coordinates, or
/// `None` when the window is too small to hold a hole -- in which case the
/// border is painted solid rather than vanishing, so a tiny window still
/// reads as focused.
pub(crate) fn hole_rect(width: i32, height: i32, thickness: i32) -> Option<Rect> {
    if thickness <= 0 || width <= 2 * thickness || height <= 2 * thickness {
        return None;
    }
    Some(Rect::new(
        thickness,
        thickness,
        width - 2 * thickness,
        height - 2 * thickness,
    ))
}

thread_local! {
    static PENDING_BORDER: RefCell<Option<Arc<Mutex<Option<BorderPlacement>>>>> =
        const { RefCell::new(None) };
}

/// Shapes the window so only the outline is part of it, leaving the middle
/// genuinely outside the window rather than painted over.
///
/// A window too small to hollow out keeps a solid region, which paints as a
/// filled rectangle -- see [`hole_rect`]. Every GDI call here is checked:
/// on failure the region is left alone rather than being set to null, since
/// `SetWindowRgn(hwnd, null, TRUE)` *removes* the region and would leave a
/// solid opaque rectangle sitting over the focused window.
fn apply_region(hwnd: HWND, width: i32, height: i32, thickness: i32) {
    let outer = unsafe { CreateRectRgn(0, 0, width, height) };
    if outer.is_invalid() {
        tracing::warn!("could not create the focus border region; leaving the last shape");
        return;
    }
    if let Some(hole) = hole_rect(width, height, thickness) {
        let inner = unsafe { CreateRectRgn(hole.x, hole.y, hole.right(), hole.bottom()) };
        if inner.is_invalid() {
            tracing::warn!("could not create the focus border hole; leaving the last shape");
            unsafe {
                let _ = DeleteObject(HGDIOBJ(outer.0));
            }
            return;
        }
        let combined = unsafe { CombineRgn(outer, outer, inner, RGN_DIFF) };
        unsafe {
            let _ = DeleteObject(HGDIOBJ(inner.0));
        }
        if combined == RGN_ERROR {
            tracing::warn!("could not subtract the focus border hole; leaving the last shape");
            unsafe {
                let _ = DeleteObject(HGDIOBJ(outer.0));
            }
            return;
        }
    }
    // The window owns the region after a successful `SetWindowRgn`, so it
    // must not be deleted here.
    if unsafe { SetWindowRgn(hwnd, outer, true) } == 0 {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(outer.0));
        }
    }
}

unsafe extern "system" fn border_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let placement = PENDING_BORDER.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|shared| shared.lock().ok().and_then(|g| *g))
            });
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if let Some(placement) = placement {
                let brush = unsafe { CreateSolidBrush(colorref(placement.color)) };
                let mut client = RECT::default();
                let _ = unsafe { GetClientRect(hwnd, &mut client) };
                unsafe {
                    // The region already limits this to the outline.
                    let _ = FillRect(hdc, &client, brush);
                    let _ = DeleteObject(HGDIOBJ(brush.0));
                }
            }
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_SHOW_BORDER => {
            let placement = PENDING_BORDER.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|shared| shared.lock().ok().and_then(|g| *g))
            });
            if let Some(placement) = placement {
                let bounds = placement.bounds;
                unsafe {
                    // Unlike the snap preview (`overlay.rs`, which passes
                    // `SWP_NOZORDER`), the border re-asserts `HWND_TOPMOST`
                    // on every show: it has to stay above the window it
                    // just followed focus onto.
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        bounds.x,
                        bounds.y,
                        bounds.width,
                        bounds.height,
                        SWP_NOACTIVATE,
                    );
                }
                apply_region(hwnd, bounds.width, bounds.height, placement.thickness);
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = InvalidateRect(hwnd, None, true);
                }
            }
            LRESULT(0)
        }
        WM_HIDE_BORDER => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_border_window() -> Result<HWND> {
    let class_name = w!("MosaixFocusBorderWindow");
    let hinstance = unsafe { GetModuleHandleW(None) }.map_err(WindowError::from)?;

    let class = WNDCLASSW {
        lpfnWndProc: Some(border_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: class_name,
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        ..Default::default()
    };
    // Ignore failure: a prior border overlay in the same process may have
    // already registered this class.
    unsafe { RegisterClassW(&class) };

    let ex_style =
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(ex_style.0),
            class_name,
            w!("mosaix focus border"),
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

    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), BORDER_ALPHA, LWA_ALPHA) }
        .map_err(WindowError::from)?;

    Ok(hwnd)
}

/// A running focus-border overlay owned by a dedicated thread.
///
/// Dropping (or [`stop`](Self::stop)) hides the window, exits the message
/// loop, and joins the thread -- the same lifecycle as
/// [`crate::PreviewOverlay`].
pub struct FocusBorderOverlay {
    hwnd: isize,
    thread_id: u32,
    pending: Arc<Mutex<Option<BorderPlacement>>>,
    join_handle: Option<JoinHandle<()>>,
    stopped: AtomicBool,
}

impl FocusBorderOverlay {
    /// Shows (or moves, recolors, or resizes) the border. Cross-thread-safe:
    /// posts to the owner thread.
    pub fn show(&self, placement: BorderPlacement) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut guard) = self.pending.lock() {
            *guard = Some(placement);
        }
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            let _ = PostMessageW(hwnd, WM_SHOW_BORDER, WPARAM(0), LPARAM(0));
        }
    }

    /// Hides the border. Cross-thread-safe.
    pub fn hide(&self) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut guard) = self.pending.lock() {
            *guard = None;
        }
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            let _ = PostMessageW(hwnd, WM_HIDE_BORDER, WPARAM(0), LPARAM(0));
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

impl Drop for FocusBorderOverlay {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Starts the focus-border overlay on a dedicated thread.
pub fn start_focus_border_overlay() -> Result<FocusBorderOverlay> {
    let pending = Arc::new(Mutex::new(None));
    let pending_for_thread = Arc::clone(&pending);
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, isize)>>();

    let join_handle = thread::spawn(move || {
        PENDING_BORDER.with(|cell| *cell.borrow_mut() = Some(pending_for_thread));

        let hwnd = match create_border_window() {
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
        Ok(Ok((thread_id, hwnd))) => Ok(FocusBorderOverlay {
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
    fn colorref_reverses_rgb_into_win32_bgr_order() {
        assert_eq!(colorref((0x00, 0x78, 0xD7)), COLORREF(0x00_D7_78_00));
        assert_eq!(colorref((0xFF, 0x00, 0x00)), COLORREF(0x00_00_00_FF));
    }

    #[test]
    fn hole_rect_insets_by_the_thickness_on_every_edge() {
        let hole = hole_rect(100, 80, 3).expect("a 100x80 window has room for a hole");
        assert_eq!((hole.x, hole.y), (3, 3));
        assert_eq!((hole.right(), hole.bottom()), (97, 77));
    }

    #[test]
    fn hole_rect_is_none_when_the_window_cannot_hold_one() {
        // Exactly twice the thickness leaves nothing in the middle.
        assert_eq!(hole_rect(6, 80, 3), None);
        assert_eq!(hole_rect(100, 6, 3), None);
        assert_eq!(hole_rect(4, 4, 3), None);
        assert_eq!(hole_rect(100, 80, 0), None);
    }

    #[test]
    fn show_positions_the_border_and_hide_conceals_it() {
        ensure_dpi_awareness();
        let overlay = start_focus_border_overlay().expect("border thread should start");
        let target = Rect::new(120, 90, 500, 400);

        overlay.show(BorderPlacement {
            bounds: target,
            color: (0x00, 0x78, 0xD7),
            thickness: 3,
        });
        // Give the owner thread a moment to process the posted message.
        std::thread::sleep(Duration::from_millis(100));

        let hwnd = overlay.hwnd();
        assert!(
            unsafe { IsWindowVisible(hwnd) }.as_bool(),
            "border should be visible after show"
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
            "border should be hidden after hide"
        );

        overlay.stop();
    }

    #[test]
    fn a_later_show_moves_the_existing_border_rather_than_needing_a_new_overlay() {
        ensure_dpi_awareness();
        let overlay = start_focus_border_overlay().expect("border thread should start");

        for target in [Rect::new(10, 10, 300, 200), Rect::new(400, 300, 640, 480)] {
            overlay.show(BorderPlacement {
                bounds: target,
                color: (0xFF, 0x88, 0x00),
                thickness: 5,
            });
            std::thread::sleep(Duration::from_millis(100));

            let mut rect = RECT::default();
            unsafe { GetWindowRect(overlay.hwnd(), &mut rect) }
                .expect("GetWindowRect should succeed");
            assert_eq!(rect.left, target.x);
            assert_eq!(rect.top, target.y);
            assert_eq!(rect.right - rect.left, target.width);
        }

        overlay.stop();
    }
}
