#![cfg(target_os = "windows")]

use tauri::WebviewWindow;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongW, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
    SetWindowLongW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
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
