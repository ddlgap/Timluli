#![cfg(target_os = "windows")]

use tauri::WebviewWindow;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, SetForegroundWindow, SetWindowLongW, SetWindowPos,
    ShowWindow, GWL_EXSTYLE, HWND_BOTTOM, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_HIDE, SW_SHOWNOACTIVATE, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST,
};

pub fn is_window(hwnd_raw: isize) -> bool {
    if hwnd_raw == 0 {
        return false;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe { IsWindow(hwnd).as_bool() }
}

pub fn get_foreground_hwnd() -> isize {
    unsafe {
        let hwnd = GetForegroundWindow();
        hwnd.0 as isize
    }
}

/// Focuses the target window, returning `true` if a focus swap actually
/// happened. Returns `false` when the target was already foreground (or the
/// HWND is null), which lets callers skip the post-focus settle delay.
pub fn focus_hwnd(hwnd_raw: isize) -> bool {
    if hwnd_raw == 0 {
        return false;
    }
    let target = HWND(hwnd_raw as *mut _);
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.0 == target.0 {
            return false;
        }

        let current_thread = GetCurrentThreadId();
        let foreground_thread = GetWindowThreadProcessId(foreground, None);
        let target_thread = GetWindowThreadProcessId(target, None);

        let attach_fg = !foreground.is_invalid() && foreground_thread != current_thread;
        let attach_tg = target_thread != current_thread && target_thread != foreground_thread;

        if attach_fg {
            let _ = AttachThreadInput(current_thread, foreground_thread, true);
        }
        if attach_tg {
            let _ = AttachThreadInput(current_thread, target_thread, true);
        }

        let _ = SetForegroundWindow(target);

        if attach_fg {
            let _ = AttachThreadInput(current_thread, foreground_thread, false);
        }
        if attach_tg {
            let _ = AttachThreadInput(current_thread, target_thread, false);
        }
        true
    }
}

struct FindByTitle {
    needle: String,
    found: isize,
}

unsafe extern "system" fn enum_find_title(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut FindByTitle);
    let len = GetWindowTextLengthW(hwnd);
    if len > 0 {
        let mut buf = vec![0u16; (len + 1) as usize];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n > 0 {
            let title = String::from_utf16_lossy(&buf[..n as usize]);
            if title.contains(&ctx.needle) {
                ctx.found = hwnd.0 as isize;
                return BOOL(0); // stop enumeration
            }
        }
    }
    BOOL(1)
}

/// Find a top-level window whose title contains `needle`, strip it from the
/// taskbar (tool window, no app-window), and shove it off-screen. Best-effort:
/// returns `true` once the window was found and adjusted.
pub fn hide_offscreen_by_title(needle: &str) -> bool {
    let mut ctx = FindByTitle {
        needle: needle.to_string(),
        found: 0,
    };
    unsafe {
        let _ = EnumWindows(Some(enum_find_title), LPARAM(&mut ctx as *mut _ as isize));
    }
    if ctx.found == 0 {
        return false;
    }
    let hwnd = HWND(ctx.found as *mut _);
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let new = (ex | WS_EX_TOOLWINDOW.0 as i32 | WS_EX_NOACTIVATE.0 as i32)
            & !(WS_EX_APPWINDOW.0 as i32);
        SetWindowLongW(hwnd, GWL_EXSTYLE, new);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(hwnd, HWND_BOTTOM, -32000, -32000, 1, 1, SWP_NOACTIVATE);
    }
    true
}

pub fn make_topmost_noactivate(window: &WebviewWindow) {
    let raw = match window.hwnd() {
        Ok(h) => h,
        Err(_) => return,
    };
    let hwnd = HWND(raw.0 as *mut _);
    unsafe {
        let cur = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let new = cur
            | WS_EX_NOACTIVATE.0 as i32
            | WS_EX_TOOLWINDOW.0 as i32
            | WS_EX_TOPMOST.0 as i32
            | WS_EX_LAYERED.0 as i32;
        SetWindowLongW(hwnd, GWL_EXSTYLE, new);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}
