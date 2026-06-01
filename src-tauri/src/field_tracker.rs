//! Experimental: dynamically dock the mic window next to whatever text field the
//! user focuses anywhere on the system. Off by default; toggled in settings.
//!
//! Approach: register a UI Automation focus-changed event handler on a dedicated
//! thread. When focus lands on an editable element (TextPattern available or
//! ControlType::Edit/Document), grab its BoundingRectangle and anchor the mic to
//! the top-right corner of the field. Skip while `is_listening` so the mic does
//! not chase the caret during dictation.
//!
//! Coverage caveats: Java/Swing without Access Bridge, custom-drawn apps, and
//! games will silently not dock (handler simply does nothing for those).

#![cfg(target_os = "windows")]

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{AppHandle, Manager, PhysicalPosition, Position};
use uiautomation::events::{CustomFocusChangedEventHandler, UIFocusChangedEventHandler};
use uiautomation::patterns::UITextPattern;
use uiautomation::types::ControlType;
use uiautomation::{UIAutomation, UIElement};

use crate::AppState;

pub struct FieldTrackerHandle {
    shutdown: Arc<AtomicBool>,
    _thread: Option<JoinHandle<()>>,
}

impl FieldTrackerHandle {
    /// Starts the focus tracker. `auto_hide` selects the behavior:
    /// - `false` (floating-mic / classic field-docking): the always-visible mic
    ///   is *repositioned* next to the focused field; focusing a non-editable
    ///   element is ignored (the mic stays where it is).
    /// - `true` (side-panel mode): the mic is hidden by default and only *shown*
    ///   (docked) when a text field gains focus; focusing any non-editable
    ///   element immediately *hides* it again.
    pub fn start(app: AppHandle, auto_hide: bool) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let app_clone = app.clone();
        let thread = thread::Builder::new()
            .name("field-tracker".into())
            .spawn(move || run_tracker(app_clone, shutdown_clone, auto_hide))
            .ok();
        Self {
            shutdown,
            _thread: thread,
        }
    }

    fn signal_stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for FieldTrackerHandle {
    fn drop(&mut self) {
        // The consumer loop polls the flag on a 500ms timeout and unregisters the
        // UIA handler before exiting; we don't join because UIA cleanup happens
        // inside the thread before it returns.
        self.signal_stop();
    }
}

#[derive(Debug, Clone, Copy)]
struct FieldRect {
    top: i32,
    right: i32,
}

/// What the focus landed on. `Other` (a non-editable, non-own element) is only
/// acted on in auto-hide mode, where it triggers hiding the mic.
#[derive(Debug, Clone, Copy)]
enum FocusEvent {
    Field(FieldRect),
    Other,
}

struct FocusHandler {
    tx: Mutex<Sender<FocusEvent>>,
    own_hwnds: Vec<isize>,
    shutdown: Arc<AtomicBool>,
}

impl CustomFocusChangedEventHandler for FocusHandler {
    fn handle(&self, sender: &UIElement) -> uiautomation::Result<()> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Don't react to focus on our own windows (clicking the mic, the side
        // panel, or settings UI) — that must not move or hide the mic.
        // Handle exposes the underlying HANDLE via AsRef; HANDLE.0 is the raw
        // *mut c_void pointer, which we cast to isize to compare against the
        // HWND values we recorded for our own webview windows.
        if let Ok(handle) = sender.get_native_window_handle() {
            let raw = handle.as_ref().0 as isize;
            if raw != 0 && self.own_hwnds.contains(&raw) {
                return Ok(());
            }
        }
        if !is_editable(sender) {
            // Focus moved to something that isn't a text field.
            let _ = self.tx.lock().send(FocusEvent::Other);
            return Ok(());
        }
        let rect = match sender.get_bounding_rectangle() {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let _ = self.tx.lock().send(FocusEvent::Field(FieldRect {
            top: rect.get_top(),
            right: rect.get_right(),
        }));
        Ok(())
    }
}

fn is_editable(elem: &UIElement) -> bool {
    if elem.get_pattern::<UITextPattern>().is_ok() {
        return true;
    }
    matches!(
        elem.get_control_type(),
        Ok(ControlType::Edit) | Ok(ControlType::Document)
    )
}

/// One-shot lookup of the currently focused editable element's bounding box,
/// returning `(top, right)` in physical screen coords — the same anchor the
/// auto-hide tracker used in `apply_rect`. Returns `None` when focus isn't on an
/// editable field, when it's one of our own windows, or when UIA can't resolve it.
///
/// Used by side-panel mode to dock the mic next to the dictation area *only while
/// recording* (no background follow tracker). Runs UIA on a short-lived thread so
/// it owns its own COM apartment and never disturbs the caller's. The lookup is
/// bounded by a timeout (via a channel, not `join`) so an unresponsive target app
/// can't stall the caller — which is an async command worker — for more than a
/// blink; on timeout we return `None` and the caller falls back to a default spot.
pub fn focused_field_rect(app: &AppHandle) -> Option<(i32, i32)> {
    let own: Vec<isize> = ["mic", "panel", "settings", "onboarding", "speech"]
        .iter()
        .filter_map(|label| app.get_webview_window(label))
        .filter_map(|w| w.hwnd().ok().map(|h| h.0 as isize))
        .collect();

    let (tx, rx) = channel::<Option<(i32, i32)>>();
    thread::Builder::new()
        .name("focused-field".into())
        .spawn(move || {
            let rect = (|| -> Option<(i32, i32)> {
                let automation = UIAutomation::new().ok()?;
                let el = automation.get_focused_element().ok()?;
                // Never anchor to our own (NOACTIVATE) windows.
                if let Ok(handle) = el.get_native_window_handle() {
                    let raw = handle.as_ref().0 as isize;
                    if raw != 0 && own.contains(&raw) {
                        return None;
                    }
                }
                if !is_editable(&el) {
                    return None;
                }
                let rect = el.get_bounding_rectangle().ok()?;
                Some((rect.get_top(), rect.get_right()))
            })();
            // If we already timed out, the receiver is gone — that's fine, the
            // worker thread just exits (it never holds any of our locks).
            let _ = tx.send(rect);
        })
        .ok()?;

    rx.recv_timeout(Duration::from_millis(400)).ok().flatten()
}

fn run_tracker(app: AppHandle, shutdown: Arc<AtomicBool>, auto_hide: bool) {
    let mut own_hwnds = Vec::new();
    for label in &["mic", "panel", "settings", "onboarding", "speech"] {
        if let Some(w) = app.get_webview_window(label) {
            if let Ok(h) = w.hwnd() {
                own_hwnds.push(h.0 as isize);
            }
        }
    }

    let automation = match UIAutomation::new() {
        Ok(a) => a,
        Err(e) => {
            log::error!("field-tracker: UIAutomation init failed: {e:?}");
            return;
        }
    };

    let (tx, rx): (Sender<FocusEvent>, Receiver<FocusEvent>) = channel();
    let focus_handler = FocusHandler {
        tx: Mutex::new(tx),
        own_hwnds,
        shutdown: shutdown.clone(),
    };
    let handler = UIFocusChangedEventHandler::from(focus_handler);
    if let Err(e) = automation.add_focus_changed_event_handler(None, &handler) {
        log::error!("field-tracker: register handler failed: {e:?}");
        return;
    }

    while !shutdown.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(FocusEvent::Field(rect)) => apply_rect(&app, rect, auto_hide),
            // In auto-hide mode, focusing a non-field hides the mic (unless we're
            // mid-dictation). In classic docking mode we ignore it.
            Ok(FocusEvent::Other) => {
                if auto_hide {
                    hide_mic(&app);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = automation.remove_focus_changed_event_handler(&handler);
}

fn apply_rect(app: &AppHandle, rect: FieldRect, auto_hide: bool) {
    let state = app.state::<AppState>();
    if *state.is_listening.lock() {
        return;
    }
    let Some(mic) = app.get_webview_window("mic") else {
        return;
    };
    const PAD: i32 = 8;
    let x = rect.right + PAD;
    let y = rect.top;
    let _ = mic.set_position(Position::Physical(PhysicalPosition::new(x, y)));
    // In side-panel (auto-hide) mode the mic starts hidden; reveal it on the
    // field it just docked to, and keep it on top without stealing focus.
    if auto_hide {
        let _ = mic.show();
        crate::win_util::make_topmost_noactivate(&mic);
    }
}

/// Hides the mic unless dictation is in progress (don't yank it away mid-speech).
fn hide_mic(app: &AppHandle) {
    let state = app.state::<AppState>();
    if *state.is_listening.lock() {
        return;
    }
    if let Some(mic) = app.get_webview_window("mic") {
        let _ = mic.hide();
    }
}
