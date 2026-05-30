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
    pub fn start(app: AppHandle) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let app_clone = app.clone();
        let thread = thread::Builder::new()
            .name("field-tracker".into())
            .spawn(move || run_tracker(app_clone, shutdown_clone))
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

struct FocusHandler {
    tx: Mutex<Sender<FieldRect>>,
    own_hwnds: Vec<isize>,
    shutdown: Arc<AtomicBool>,
}

impl CustomFocusChangedEventHandler for FocusHandler {
    fn handle(&self, sender: &UIElement) -> uiautomation::Result<()> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Don't dock to our own windows (clicking the mic or settings UI).
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
            return Ok(());
        }
        let rect = match sender.get_bounding_rectangle() {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let _ = self.tx.lock().send(FieldRect {
            top: rect.get_top(),
            right: rect.get_right(),
        });
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

fn run_tracker(app: AppHandle, shutdown: Arc<AtomicBool>) {
    let mut own_hwnds = Vec::new();
    for label in &["mic", "settings", "onboarding", "speech"] {
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

    let (tx, rx): (Sender<FieldRect>, Receiver<FieldRect>) = channel();
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
            Ok(rect) => apply_rect(&app, rect),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = automation.remove_focus_changed_event_handler(&handler);
}

fn apply_rect(app: &AppHandle, rect: FieldRect) {
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
}
