#![cfg(target_os = "windows")]

//! System-wide *double-tap-a-modifier* detector (e.g. tap **Ctrl** twice quickly
//! to toggle dictation, like a double-click).
//!
//! The Win32 global-shortcut API (`RegisterHotKey`, used by
//! `tauri-plugin-global-shortcut`) can only bind a modifier+key *chord* — it can
//! never bind a lone modifier. So to support "Ctrl+Ctrl" / "Alt+Alt" we observe
//! the raw key stream with a `WH_KEYBOARD_LL` low-level keyboard hook and detect
//! the gesture ourselves.
//!
//! We never swallow the keys (we always call the next hook), so Ctrl/Alt keep
//! working completely normally — a real chord like Ctrl+C is recognised as
//! "polluted" and ignored by the detector.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use tauri::AppHandle;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, HC_ACTION,
    HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

// Encoding of the watched modifier. 0 = disabled (the hook is a pure passthrough).
const DT_NONE: u8 = 0;
const DT_CTRL: u8 = 1;
const DT_ALT: u8 = 2;
const DT_SHIFT: u8 = 3;
const DT_WIN: u8 = 4;

/// Max pause between the first tap's release and the second tap's press for the
/// two to count as a double-tap. Matches the feel of the Windows double-click time.
const DOUBLE_TAP_GAP_MS: u32 = 400;
/// A press held longer than this is a deliberate hold (e.g. Ctrl while typing a
/// chord), not a quick tap — so it can never start or complete a double-tap.
const MAX_HOLD_MS: u32 = 300;

/// Which modifier the detector currently watches. Written from the UI thread,
/// read on every keystroke by the hook thread — a single relaxed atomic.
static TARGET: AtomicU8 = AtomicU8::new(DT_NONE);
/// Set once; the hook thread sends `()` here on each detected double-tap.
static TRIGGER_TX: OnceLock<Sender<()>> = OnceLock::new();
/// Guards one-time spawning of the worker + hook threads.
static STARTED: AtomicBool = AtomicBool::new(false);

/// Per-(hook-)thread tap-detection state. Only ever touched by the hook thread,
/// so it needs no synchronisation.
#[derive(Clone, Copy)]
struct TapState {
    /// Tick (`KBDLLHOOKSTRUCT.time`) when the target key last went down.
    down_time: u32,
    /// Tick of the last completed *clean* tap (key up). Valid iff `has_prev_tap`.
    last_tap_up: u32,
    /// Target key is currently physically held (used to swallow auto-repeat).
    held: bool,
    /// A non-target key was seen while the target was held → the press is not a
    /// clean tap (e.g. Ctrl+C).
    polluted: bool,
    /// We have one armed clean tap awaiting a partner.
    has_prev_tap: bool,
}

impl TapState {
    const fn new() -> Self {
        Self {
            down_time: 0,
            last_tap_up: 0,
            held: false,
            polluted: false,
            has_prev_tap: false,
        }
    }
}

thread_local! {
    static TAP: RefCell<TapState> = const { RefCell::new(TapState::new()) };
}

/// Parse a stored shortcut string into the double-tap modifier it represents, if
/// any. Returns `Some(code)` for `"Ctrl+Ctrl"`, `"Alt+Alt"`, … — i.e. when every
/// `+`-separated token is the *same* modifier and there is no main key. Returns
/// `None` for ordinary chords like `"Ctrl+Super+Space"`.
pub fn parse_target(combo: &str) -> Option<u8> {
    let mut seen: Option<u8> = None;
    let mut count = 0u32;
    for raw in combo.split('+') {
        let tok = raw.trim();
        if tok.is_empty() {
            continue;
        }
        let code = match tok.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => DT_CTRL,
            "alt" | "option" => DT_ALT,
            "shift" => DT_SHIFT,
            "win" | "super" | "meta" | "cmd" | "command" => DT_WIN,
            // Any non-modifier token means this is a regular chord, not a gesture.
            _ => return None,
        };
        match seen {
            Some(s) if s != code => return None, // mixed modifiers → chord
            _ => seen = Some(code),
        }
        count += 1;
    }
    match seen {
        Some(code) if count >= 2 => Some(code),
        _ => None,
    }
}

/// True when `combo` denotes a double-tap gesture rather than a key chord.
pub fn is_double_tap(combo: &str) -> bool {
    parse_target(combo).is_some()
}

/// Point the detector at a modifier (`DT_CTRL`, …). Cheap — just an atomic store.
pub fn set_target(code: u8) {
    TARGET.store(code, Ordering::Relaxed);
}

/// Turn the detector off (pure passthrough). Used when a chord shortcut is active
/// or while the shortcut recorder is capturing keys.
pub fn disable() {
    TARGET.store(DT_NONE, Ordering::Relaxed);
}

/// Install the detector backing the double-tap gesture. Spawns the hook thread
/// (which installs `WH_KEYBOARD_LL` and pumps messages) and a worker thread that
/// turns each detected gesture into the same toggle the global shortcut performs.
///
/// Idempotent: only the first call does work. The actual watched modifier is set
/// separately via [`set_target`] from the shortcut-registration path.
pub fn init(app: AppHandle) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    let (tx, rx) = channel::<()>();
    let _ = TRIGGER_TX.set(tx);

    // Worker thread: kept separate from the hook thread so the hook callback
    // never blocks — Windows silently unhooks a low-level hook whose callback is
    // too slow (the `LowLevelHooksTimeout`, ~300 ms).
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = crate::shortcut::handle_pressed(app).await {
                    log::warn!("double-tap toggle failed: {err}");
                }
            });
        }
    });

    // Hook thread: a low-level keyboard hook only fires on the installing thread,
    // and that thread must pump a message loop. This thread lives for the whole
    // process lifetime.
    std::thread::spawn(|| unsafe {
        let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), HINSTANCE::default(), 0) {
            Ok(h) => h,
            Err(e) => {
                log::error!("double-tap: SetWindowsHookExW failed: {e}");
                return;
            }
        };
        let _ = hook; // kept installed for the process lifetime
        let mut msg = MSG::default();
        // GetMessageW returns >0 for a message, 0 for WM_QUIT, -1 on error.
        while GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

/// Map a raw virtual-key code to its modifier class, or `0xFF` for any other key.
fn classify_vk(vk: u32) -> u8 {
    let vk = vk as u16;
    if vk == VK_CONTROL.0 || vk == VK_LCONTROL.0 || vk == VK_RCONTROL.0 {
        DT_CTRL
    } else if vk == VK_MENU.0 || vk == VK_LMENU.0 || vk == VK_RMENU.0 {
        DT_ALT
    } else if vk == VK_SHIFT.0 || vk == VK_LSHIFT.0 || vk == VK_RSHIFT.0 {
        DT_SHIFT
    } else if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
        DT_WIN
    } else {
        0xFF
    }
}

/// Feed one keyboard event into the per-thread state machine. Returns `true` when
/// this event *completes* a double-tap of the watched modifier.
fn process(target: u8, msg: u32, kbd: &KBDLLHOOKSTRUCT) -> bool {
    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    if !is_down && !is_up {
        return false;
    }

    let cls = classify_vk(kbd.vkCode);
    let time = kbd.time;

    TAP.with(|cell| {
        let mut s = cell.borrow_mut();

        if cls != target {
            // Activity on any other key breaks the gesture: it pollutes the
            // current target press (Ctrl+C) and cancels a pending first tap
            // (Ctrl, X, Ctrl must not count).
            if is_down {
                s.polluted = true;
                s.has_prev_tap = false;
            }
            return false;
        }

        // From here on the event concerns the watched modifier itself.
        if is_down {
            if !s.held {
                s.held = true;
                s.down_time = time;
                s.polluted = false;
            }
            // Auto-repeat key-downs while held are ignored.
            return false;
        }

        // Key up → a press just completed.
        s.held = false;
        let held_ms = time.wrapping_sub(s.down_time);
        let clean = !s.polluted && held_ms <= MAX_HOLD_MS;
        s.polluted = false;

        if !clean {
            s.has_prev_tap = false;
            return false;
        }

        // A clean tap. If it lands soon after a previously armed tap → fire.
        if s.has_prev_tap && s.down_time.wrapping_sub(s.last_tap_up) <= DOUBLE_TAP_GAP_MS {
            s.has_prev_tap = false;
            return true;
        }

        // Otherwise treat this as the (new) first tap and arm it.
        s.has_prev_tap = true;
        s.last_tap_up = time;
        false
    })
}

/// The low-level keyboard hook callback. Runs on the hook thread for *every*
/// system keystroke, so it must stay fast and must always forward via
/// `CallNextHookEx`.
unsafe extern "system" fn ll_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ncode == HC_ACTION as i32 {
        let target = TARGET.load(Ordering::Relaxed);
        if target != DT_NONE && lparam.0 != 0 {
            let kbd = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if process(target, wparam.0 as u32, kbd) {
                if let Some(tx) = TRIGGER_TX.get() {
                    let _ = tx.send(());
                }
            }
        }
    }
    CallNextHookEx(HHOOK::default(), ncode, wparam, lparam)
}
