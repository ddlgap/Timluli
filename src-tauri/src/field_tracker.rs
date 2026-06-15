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
use tauri::{AppHandle, Manager, PhysicalPosition, Position, WebviewWindow};
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
pub struct FieldRect {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
}

/// Computes the mic window's top-left so its centered disc sits horizontally
/// centered over `rect` and snug just above the field's top edge (flipping to
/// just below when the field hugs the screen top). The disc is centered in the
/// 240px window, so placing the *window centre* at the desired disc spot is
/// independent of the disc diameter. Shared by the floating-mic follow tracker
/// (`apply_rect`) and the hidden-mic docking path (`commands::sync_side_panel_mic`).
/// UIA rects are physical screen px; the window half-size and clearance are
/// scaled by the mic window's DPI to match.
pub fn dock_position(mic: &WebviewWindow, rect: FieldRect) -> (i32, i32) {
    let scale = mic.scale_factor().unwrap_or(1.0);
    let half = (120.0 * scale).round() as i32; // half the 240px window (centre offset)
    let clear = (44.0 * scale).round() as i32; // disc-centre clearance above the top edge
    let cx = (rect.left + rect.right) / 2;
    let mut cy = rect.top - clear;
    if cy < clear {
        cy = rect.bottom + clear;
    }
    (cx - half, cy - half)
}

/// True when `rect` overlaps the visible area of at least one connected monitor.
///
/// Some editors don't expose a normal on-screen text field to UI Automation: Google
/// Docs renders the page on a `<canvas>` and routes keystrokes through a hidden,
/// off-screen contenteditable, so the "focused field" UIA hands us is positioned far
/// outside every display. Docking the mic to such a rect flings it off-screen, where
/// it reads to the user as "the mic disappeared." Callers treat an off-screen (or
/// degenerate) rect as "no usable field" and fall back to a sane on-screen position.
///
/// Conservative on uncertainty: if monitors can't be enumerated we return `true`
/// (don't reject), so a probing failure never *causes* the mic to vanish.
fn rect_on_screen(app: &AppHandle, rect: FieldRect) -> bool {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return false; // zero/negative area — never a real field box
    }
    let Some(mic) = app.get_webview_window("mic") else {
        return true;
    };
    let monitors = match mic.available_monitors() {
        Ok(m) if !m.is_empty() => m,
        _ => return true,
    };
    monitors.iter().any(|m| {
        let p = m.position();
        let s = m.size();
        let (ml, mt) = (p.x, p.y);
        let (mr, mb) = (p.x + s.width as i32, p.y + s.height as i32);
        // Axis-aligned overlap: the field must share some area with this monitor.
        rect.left < mr && rect.right > ml && rect.top < mb && rect.bottom > mt
    })
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
            bottom: rect.get_bottom(),
            left: rect.get_left(),
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
/// returning its full `FieldRect` in physical screen coords. Returns `None` when
/// focus isn't on an editable field, when it's one of our own windows, or when
/// UIA can't resolve it. Callers pick the anchor: side-panel keeps a top-right
/// anchor; hidden-mic feeds it through `dock_position` (centered-above).
///
/// Used by the transient display modes to dock the mic next to the dictation area
/// *only while recording* (no background follow tracker). Runs UIA on a short-lived
/// thread so it owns its own COM apartment and never disturbs the caller's. The
/// lookup is bounded by a timeout (via a channel, not `join`) so an unresponsive
/// target app can't stall the caller — which is an async command worker — for more
/// than a blink; on timeout we return `None` and the caller falls back to a default.
pub fn focused_field_rect(app: &AppHandle) -> Option<FieldRect> {
    let own: Vec<isize> = ["mic", "panel", "settings", "onboarding", "speech"]
        .iter()
        .filter_map(|label| app.get_webview_window(label))
        .filter_map(|w| w.hwnd().ok().map(|h| h.0 as isize))
        .collect();

    let (tx, rx) = channel::<Option<FieldRect>>();
    thread::Builder::new()
        .name("focused-field".into())
        .spawn(move || {
            let rect = (|| -> Option<FieldRect> {
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
                Some(FieldRect {
                    top: rect.get_top(),
                    bottom: rect.get_bottom(),
                    left: rect.get_left(),
                    right: rect.get_right(),
                })
            })();
            // If we already timed out, the receiver is gone — that's fine, the
            // worker thread just exits (it never holds any of our locks).
            let _ = tx.send(rect);
        })
        .ok()?;

    let rect = rx.recv_timeout(Duration::from_millis(400)).ok().flatten()?;
    // Reject off-screen fields (e.g. Google Docs' hidden input) so callers fall back
    // to a visible default instead of docking the mic where it can't be seen.
    if rect_on_screen(app, rect) {
        Some(rect)
    } else {
        None
    }
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
    // An off-screen "field" (e.g. Google Docs' hidden input) would dock the mic
    // where it can't be seen; leave it at its current visible spot instead.
    if !rect_on_screen(app, rect) {
        return;
    }
    // Dock the disc horizontally centered over the field, snug above its top edge.
    let (x, y) = dock_position(&mic, rect);
    let _ = mic.set_position(Position::Physical(PhysicalPosition::new(x, y)));
    // In side-panel (auto-hide) mode the mic starts hidden; reveal it on the field
    // it just docked to. In floating mode it's already visible. Either way,
    // re-assert topmost after the move so docking never leaves the mic demoted —
    // otherwise the periodic keeper would take up to ~1s to bring it back on top.
    if auto_hide {
        let _ = mic.show();
    }
    crate::win_util::make_topmost_noactivate(&mic);
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
