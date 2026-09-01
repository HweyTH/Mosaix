//! Persistent click-through focus border for automatic tiling.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use mosaix_domain::Rect;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, FrameRect, InvalidateRect,
    HBRUSH, HGDIOBJ, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    PostMessageW, PostThreadMessageW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, LWA_ALPHA, LWA_COLORKEY, MSG, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_APP, WM_DESTROY, WM_PAINT, WM_QUIT, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::{Result, WindowError};

const WM_SHOW_BORDER: u32 = WM_APP + 60;
const WM_HIDE_BORDER: u32 = WM_APP + 61;
const TRANSPARENT_KEY: COLORREF = COLORREF(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusBorderStyle {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
    pub thickness: u16,
}

#[derive(Debug, Clone, Copy)]
struct PendingBorder {
    bounds: Rect,
    style: FocusBorderStyle,
}

thread_local! {
    static PENDING_BORDER: RefCell<Option<Arc<Mutex<Option<PendingBorder>>>>> = const { RefCell::new(None) };
}

fn colorref(style: FocusBorderStyle) -> COLORREF {
    COLORREF(u32::from(style.red) | (u32::from(style.green) << 8) | (u32::from(style.blue) << 16))
}

unsafe extern "system" fn border_wndproc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    _lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let pending = PENDING_BORDER.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|shared| shared.lock().ok().and_then(|guard| *guard))
            });
            let mut paint = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
            let mut client = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            let background = unsafe { CreateSolidBrush(TRANSPARENT_KEY) };
            unsafe {
                let _ = FillRect(hdc, &client, background);
            }
            if let Some(pending) = pending {
                let brush = unsafe { CreateSolidBrush(colorref(pending.style)) };
                let thickness = i32::from(pending.style.thickness)
                    .min((client.right - client.left).max(0) / 2)
                    .min((client.bottom - client.top).max(0) / 2);
                for inset in 0..thickness {
                    let frame = RECT {
                        left: client.left + inset,
                        top: client.top + inset,
                        right: client.right - inset,
                        bottom: client.bottom - inset,
                    };
                    unsafe {
                        let _ = FrameRect(hdc, &frame, brush);
                    }
                }
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(brush.0));
                }
            }
            unsafe {
                let _ = DeleteObject(HGDIOBJ(background.0));
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_SHOW_BORDER => {
            let pending = PENDING_BORDER.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|shared| shared.lock().ok().and_then(|guard| *guard))
            });
            if let Some(pending) = pending {
                unsafe {
                    let _ = SetLayeredWindowAttributes(
                        hwnd,
                        TRANSPARENT_KEY,
                        pending.style.alpha,
                        LWA_COLORKEY | LWA_ALPHA,
                    );
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        pending.bounds.x,
                        pending.bounds.y,
                        pending.bounds.width,
                        pending.bounds.height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    let _ = InvalidateRect(hwnd, None, true);
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
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
        _ => unsafe { DefWindowProcW(hwnd, msg, _wparam, _lparam) },
    }
}

fn create_border_window() -> Result<HWND> {
    let class_name = w!("MosaixFocusBorderWindow");
    let instance = unsafe { GetModuleHandleW(None) }.map_err(WindowError::from)?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(border_wndproc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        ..Default::default()
    };
    unsafe { RegisterClassW(&class) };

    let extended =
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(extended.0),
            class_name,
            w!("mosaix focus border"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )
    }
    .map_err(WindowError::from)
}

pub struct FocusBorder {
    hwnd: isize,
    thread_id: u32,
    pending: Arc<Mutex<Option<PendingBorder>>>,
    join_handle: Option<JoinHandle<()>>,
    stopped: AtomicBool,
}

impl FocusBorder {
    pub fn show(&self, bounds: Rect, style: FocusBorderStyle) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut pending) = self.pending.lock() {
            *pending = Some(PendingBorder { bounds, style });
        }
        unsafe {
            let _ = PostMessageW(
                HWND(self.hwnd as *mut _),
                WM_SHOW_BORDER,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    pub fn hide(&self) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(mut pending) = self.pending.lock() {
            *pending = None;
        }
        unsafe {
            let _ = PostMessageW(
                HWND(self.hwnd as *mut _),
                WM_HIDE_BORDER,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

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
    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }
}

impl Drop for FocusBorder {
    fn drop(&mut self) {
        self.request_stop();
    }
}

pub fn start_focus_border() -> Result<FocusBorder> {
    let pending = Arc::new(Mutex::new(None));
    let pending_for_thread = Arc::clone(&pending);
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, isize)>>();
    let join_handle = thread::spawn(move || {
        PENDING_BORDER.with(|cell| *cell.borrow_mut() = Some(pending_for_thread));
        let hwnd = match create_border_window() {
            Ok(hwnd) => hwnd,
            Err(error) => {
                let _ = ready_tx.send(Err(error));
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
        Ok(Ok((thread_id, hwnd))) => Ok(FocusBorder {
            hwnd,
            thread_id,
            pending,
            join_handle: Some(join_handle),
            stopped: AtomicBool::new(false),
        }),
        Ok(Err(error)) => {
            let _ = join_handle.join();
            Err(error)
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
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, GetWindowRect, IsWindowVisible, GWL_EXSTYLE,
    };

    #[test]
    fn border_is_click_through_tracks_geometry_and_hides() {
        ensure_dpi_awareness();
        let border = start_focus_border().expect("focus border should start");
        let bounds = Rect::new(-420, 75, 401, 299);
        border.show(
            bounds,
            FocusBorderStyle {
                red: 0,
                green: 120,
                blue: 215,
                alpha: 255,
                thickness: 2,
            },
        );
        std::thread::sleep(Duration::from_millis(100));

        let hwnd = border.hwnd();
        assert!(unsafe { IsWindowVisible(hwnd) }.as_bool());
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        assert_ne!(style & WS_EX_TRANSPARENT.0, 0);
        assert_ne!(style & WS_EX_NOACTIVATE.0, 0);
        assert_ne!(style & WS_EX_TOOLWINDOW.0, 0);
        let mut actual = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut actual) }.expect("border bounds should be readable");
        assert_eq!(actual.left, bounds.x);
        assert_eq!(actual.top, bounds.y);
        assert_eq!(actual.right - actual.left, bounds.width);
        assert_eq!(actual.bottom - actual.top, bounds.height);

        border.hide();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool());
        border.stop();
    }
}
