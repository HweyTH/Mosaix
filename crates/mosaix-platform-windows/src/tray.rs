//! System tray icon with pause / settings / quit menu (Feature 33).
//!
//! Owned by a dedicated thread with a hidden top-level window (same shape
//! as `display.rs`). Right-click opens a popup menu; pause state is pushed
//! in via [`TrayHandle::set_paused`] so the tooltip and icon track
//! `EngineState::paused` regardless of who toggled it.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, FillRect, GetDC, ReleaseDC, SelectObject, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, NOTIFY_ICON_MESSAGE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, LoadIconW,
    PostMessageW, PostThreadMessageW, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
    SetMenuDefaultItem, TrackPopupMenu, TranslateMessage, CreateWindowExW, HICON, ICONINFO, MSG,
    SM_CXSMICON, SM_CYSMICON, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_APP, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_QUIT, WM_RBUTTONUP, WNDCLASSW,
    WS_OVERLAPPED, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, TPM_RETURNCMD,
};

use crate::{Result, WindowError};

const WM_TRAYICON: u32 = WM_APP + 50;
const WM_SET_PAUSED: u32 = WM_APP + 51;

const TRAY_UID: u32 = 1;
const IDM_TOGGLE_PAUSE: usize = 1001;
const IDM_OPEN_CONFIG: usize = 1002;
const IDM_QUIT: usize = 1003;

/// A user action from the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    TogglePause,
    OpenConfig,
    Quit,
}

thread_local! {
    static TRAY_SENDER: RefCell<Option<Sender<TrayEvent>>> = const { RefCell::new(None) };
    static TRAY_PAUSED: RefCell<bool> = const { RefCell::new(false) };
    static TRAY_ICON_RUNNING: RefCell<Option<HICON>> = const { RefCell::new(None) };
    static TRAY_ICON_PAUSED: RefCell<Option<HICON>> = const { RefCell::new(None) };
    static TRAY_HWND: RefCell<Option<HWND>> = const { RefCell::new(None) };
    static TASKBAR_CREATED_MSG: RefCell<u32> = const { RefCell::new(0) };
}

fn tip_for(paused: bool) -> [u16; 128] {
    let text = if paused {
        "Mosaix — Paused"
    } else {
        "Mosaix — Running"
    };
    let mut buf = [0u16; 128];
    for (i, c) in text.encode_utf16().take(127).enumerate() {
        buf[i] = c;
    }
    buf
}

fn current_icon(paused: bool) -> HICON {
    if paused {
        TRAY_ICON_PAUSED.with(|c| c.borrow().unwrap_or_else(stock_icon))
    } else {
        TRAY_ICON_RUNNING.with(|c| c.borrow().unwrap_or_else(stock_icon))
    }
}

fn stock_icon() -> HICON {
    unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or(HICON(std::ptr::null_mut()))
}

fn notify_data(hwnd: HWND, paused: bool) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_UID;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAYICON;
    data.hIcon = current_icon(paused);
    data.szTip = tip_for(paused);
    data
}

fn add_or_modify(hwnd: HWND, paused: bool, message: NOTIFY_ICON_MESSAGE) {
    let mut data = notify_data(hwnd, paused);
    let _ = unsafe { Shell_NotifyIconW(message, &mut data) };
}

fn delete_icon(hwnd: HWND) {
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_UID;
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &mut data) };
}

/// Builds a 16×16 (or SM_CXSMICON) solid-color icon. Returns `None` on any
/// GDI failure so the caller can fall back to the stock application icon.
fn create_status_icon(color: COLORREF) -> Option<HICON> {
    let cx = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let cy = unsafe { GetSystemMetrics(SM_CYSMICON) }.max(16);

    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        return None;
    }
    let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
    if mem_dc.is_invalid() {
        unsafe { ReleaseDC(None, screen_dc) };
        return None;
    }
    let color_bm = unsafe { CreateCompatibleBitmap(screen_dc, cx, cy) };
    if color_bm.is_invalid() {
        unsafe {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
        }
        return None;
    }
    let old = unsafe { SelectObject(mem_dc, HGDIOBJ(color_bm.0)) };
    let brush = unsafe { CreateSolidBrush(color) };
    let rect = windows::Win32::Foundation::RECT {
        left: 0,
        top: 0,
        right: cx,
        bottom: cy,
    };
    unsafe {
        let _ = FillRect(mem_dc, &rect, brush);
        // Inner mosaic: a slightly inset square so the icon reads as a tile.
        let inset = (cx / 4).max(2);
        let inner = windows::Win32::Foundation::RECT {
            left: inset,
            top: inset,
            right: cx - inset,
            bottom: cy - inset,
        };
        let inner_brush = CreateSolidBrush(COLORREF(color.0.wrapping_add(0x00202020) & 0x00FFFFFF));
        let _ = FillRect(mem_dc, &inner, inner_brush);
        let _ = DeleteObject(HGDIOBJ(inner_brush.0));
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = SelectObject(mem_dc, old);
    }

    // AND mask: all zeros = fully opaque.
    let mask_bm = unsafe { CreateBitmap(cx, cy, 1, 1, None) };
    if mask_bm.is_invalid() {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(color_bm.0));
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
        }
        return None;
    }

    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask_bm,
        hbmColor: color_bm,
    };
    let icon = unsafe { CreateIconIndirect(&info) }.ok();

    unsafe {
        let _ = DeleteObject(HGDIOBJ(mask_bm.0));
        let _ = DeleteObject(HGDIOBJ(color_bm.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
    }

    icon
}

fn show_context_menu(hwnd: HWND) {
    let paused = TRAY_PAUSED.with(|c| *c.borrow());
    let menu = match unsafe { CreatePopupMenu() } {
        Ok(m) => m,
        Err(_) => return,
    };

    let pause_label: Vec<u16> = if paused {
        "&Resume\0".encode_utf16().collect()
    } else {
        "&Pause\0".encode_utf16().collect()
    };
    let config_label: Vec<u16> = "Open &config folder\0".encode_utf16().collect();
    let quit_label: Vec<u16> = "&Quit\0".encode_utf16().collect();

    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, IDM_TOGGLE_PAUSE, PCWSTR(pause_label.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, IDM_OPEN_CONFIG, PCWSTR(config_label.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, IDM_QUIT, PCWSTR(quit_label.as_ptr()));
        let _ = SetMenuDefaultItem(menu, IDM_TOGGLE_PAUSE as u32, 0);
    }

    let mut pt = POINT::default();
    let _ = unsafe { GetCursorPos(&mut pt) };
    // Required so the menu dismisses correctly when the user clicks away.
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RETURNCMD,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        )
    };
    unsafe {
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }

    let event = match cmd.0 as usize {
        IDM_TOGGLE_PAUSE => Some(TrayEvent::TogglePause),
        IDM_OPEN_CONFIG => Some(TrayEvent::OpenConfig),
        IDM_QUIT => Some(TrayEvent::Quit),
        _ => None,
    };
    if let Some(event) = event {
        TRAY_SENDER.with(|sender| {
            if let Some(tx) = sender.borrow().as_ref() {
                let _ = tx.send(event);
            }
        });
    }
}

unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED_MSG.with(|c| *c.borrow());
    if taskbar_created != 0 && msg == taskbar_created {
        let paused = TRAY_PAUSED.with(|c| *c.borrow());
        add_or_modify(hwnd, paused, NIM_ADD);
        return LRESULT(0);
    }

    match msg {
        WM_TRAYICON => {
            let mouse = lparam.0 as u32;
            if mouse == WM_RBUTTONUP || mouse == WM_LBUTTONUP {
                show_context_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_SET_PAUSED => {
            let paused = wparam.0 != 0;
            TRAY_PAUSED.with(|c| *c.borrow_mut() = paused);
            add_or_modify(hwnd, paused, NIM_MODIFY);
            LRESULT(0)
        }
        WM_COMMAND => {
            // Defensive: some paths deliver menu commands via WM_COMMAND.
            let id = wparam.0 as usize & 0xFFFF;
            let event = match id {
                IDM_TOGGLE_PAUSE => Some(TrayEvent::TogglePause),
                IDM_OPEN_CONFIG => Some(TrayEvent::OpenConfig),
                IDM_QUIT => Some(TrayEvent::Quit),
                _ => None,
            };
            if let Some(event) = event {
                TRAY_SENDER.with(|sender| {
                    if let Some(tx) = sender.borrow().as_ref() {
                        let _ = tx.send(event);
                    }
                });
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            delete_icon(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_tray_window() -> Result<HWND> {
    let class_name = w!("MosaixTrayWindow");
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map_err(WindowError::from)?
        .into();

    let class = WNDCLASSW {
        lpfnWndProc: Some(tray_wndproc),
        hInstance: hinstance,
        lpszClassName: class_name,
        ..Default::default()
    };
    unsafe { RegisterClassW(&class) };

    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("mosaix tray"),
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

/// Handle for a running tray icon. Dropping (or [`stop`](Self::stop)) removes
/// the icon and joins the owner thread.
pub struct TrayHandle {
    hwnd: isize,
    thread_id: u32,
    join_handle: Option<JoinHandle<()>>,
    stopped: AtomicBool,
}

impl TrayHandle {
    /// Updates the tray icon and tooltip to reflect `paused`.
    pub fn set_paused(&self, paused: bool) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            let _ = PostMessageW(
                hwnd,
                WM_SET_PAUSED,
                WPARAM(if paused { 1 } else { 0 }),
                LPARAM(0),
            );
        }
    }

    /// Stops the tray thread and waits for it to exit.
    pub fn stop(mut self) {
        self.request_stop();
    }

    fn request_stop(&mut self) {
        if self.stopped.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Some(join_handle) = self.join_handle.take() {
            let hwnd = HWND(self.hwnd as *mut _);
            delete_icon(hwnd);
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = join_handle.join();
        }
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Starts the system tray icon on a dedicated thread.
pub fn start_tray() -> Result<(TrayHandle, Receiver<TrayEvent>)> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, isize)>>();

    let join_handle = thread::spawn(move || {
        TRAY_SENDER.with(|s| *s.borrow_mut() = Some(tx));

        // Accent blue (BGR) for running; muted gray for paused.
        let running = create_status_icon(COLORREF(0x00_D7_78_00));
        let paused_icon = create_status_icon(COLORREF(0x00_80_80_80));
        TRAY_ICON_RUNNING.with(|c| *c.borrow_mut() = running);
        TRAY_ICON_PAUSED.with(|c| *c.borrow_mut() = paused_icon);

        let taskbar_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        TASKBAR_CREATED_MSG.with(|c| *c.borrow_mut() = taskbar_msg);

        let hwnd = match create_tray_window() {
            Ok(hwnd) => hwnd,
            Err(err) => {
                let _ = ready_tx.send(Err(err));
                return;
            }
        };
        TRAY_HWND.with(|c| *c.borrow_mut() = Some(hwnd));
        add_or_modify(hwnd, false, NIM_ADD);

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

        delete_icon(hwnd);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        TRAY_ICON_RUNNING.with(|c| {
            if let Some(icon) = c.borrow_mut().take() {
                if !icon.is_invalid() {
                    let _ = unsafe { DestroyIcon(icon) };
                }
            }
        });
        TRAY_ICON_PAUSED.with(|c| {
            if let Some(icon) = c.borrow_mut().take() {
                if !icon.is_invalid() {
                    let _ = unsafe { DestroyIcon(icon) };
                }
            }
        });
    });

    match ready_rx.recv() {
        Ok(Ok((thread_id, hwnd))) => Ok((
            TrayHandle {
                hwnd,
                thread_id,
                join_handle: Some(join_handle),
                stopped: AtomicBool::new(false),
            },
            rx,
        )),
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

    #[test]
    fn tray_lifecycle_start_set_paused_stop() {
        ensure_dpi_awareness();
        let (tray, _rx) = start_tray().expect("tray thread should start");
        tray.set_paused(true);
        std::thread::sleep(Duration::from_millis(50));
        tray.set_paused(false);
        std::thread::sleep(Duration::from_millis(50));
        tray.stop();
    }
}
