#![cfg(target_os = "windows")]

use std::thread::sleep;
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_V,
};

use crate::win_util;

/// Focuses the target window, then injects the given text as real keyboard
/// input via SendInput (KEYEVENTF_UNICODE) or clipboard paste for long text.
/// Returns an error if the target window is gone or injection fails.
pub fn inject(target_hwnd: isize, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }

    // Bug #3 fix: check HWND validity before trying to inject.
    if !win_util::is_window(target_hwnd) {
        return Err("חלון היעד נסגר".into());
    }

    // Only pay the settle delay when focus actually had to swap. When the
    // target is already foreground (the common case for repeated finals in
    // one utterance) we can inject immediately.
    let focus_changed = win_util::focus_hwnd(target_hwnd);
    if focus_changed {
        sleep(Duration::from_millis(10));
    }

    if text.chars().count() > 30 {
        inject_via_paste(text)?;
    } else {
        inject_unicode_string(text)?;
    }

    Ok(())
}

fn inject_unicode_string(text: &str) -> Result<(), String> {
    // Each UTF-16 code unit needs a key-down + key-up pair.
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() {
        return Ok(());
    }
    let mut inputs: Vec<INPUT> = Vec::with_capacity(units.len() * 2);
    for unit in units {
        inputs.push(make_input(unit, false));
        inputs.push(make_input(unit, true));
    }
    // Bug #1 fix: check how many events were actually delivered.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if (sent as usize) < inputs.len() {
        return Err(format!(
            "SendInput נכשל: נשלחו {}/{} אירועים (ייתכן חסימת UIPI)",
            sent,
            inputs.len()
        ));
    }
    Ok(())
}

fn inject_via_paste(text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0); // null terminator
    let bytes = units.len() * std::mem::size_of::<u16>();

    unsafe {
        OpenClipboard(None).map_err(|e| format!("OpenClipboard: {e}"))?;
        // From here on we MUST CloseClipboard before returning, even on error.
        let result: Result<(), String> = (|| {
            EmptyClipboard().map_err(|e| format!("EmptyClipboard: {e}"))?;
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|e| format!("GlobalAlloc: {e}"))?;
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                return Err("GlobalLock החזיר null".into());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
            let _ = GlobalUnlock(h);
            // SetClipboardData takes ownership of the HGLOBAL on success.
            SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h.0))
                .map_err(|e| format!("SetClipboardData: {e}"))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result?;
    }

    send_ctrl_v()
}

fn send_ctrl_v() -> Result<(), String> {
    let inputs = [
        make_vk_input(VK_CONTROL.0, false),
        make_vk_input(VK_V.0, false),
        make_vk_input(VK_V.0, true),
        make_vk_input(VK_CONTROL.0, true),
    ];
    // Bug #1 fix: check SendInput return value for Ctrl+V too.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if (sent as usize) < inputs.len() {
        return Err(format!(
            "SendInput (Ctrl+V) נכשל: נשלחו {}/{} אירועים",
            sent,
            inputs.len()
        ));
    }
    Ok(())
}

fn make_vk_input(vk: u16, key_up: bool) -> INPUT {
    let mut flags: KEYBD_EVENT_FLAGS = KEYBD_EVENT_FLAGS(0);
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn make_input(scancode: u16, key_up: bool) -> INPUT {
    let mut flags: KEYBD_EVENT_FLAGS = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scancode,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
