//! Keeps the floating mic reliably above every other window in floating-mic mode.
//!
//! `make_topmost_noactivate` asserts the topmost z-band only at discrete moments
//! (startup, mode switch, tray show). But Windows silently demotes a topmost
//! window out of that band in routine situations — another app going
//! (borderless-)fullscreen, another `WS_EX_TOPMOST` window activating after us,
//! DWM/display reconfiguration. Floating-mic mode has no recovery path (unlike
//! side-panel/hidden, which re-assert topmost on every recording), so the mic
//! ends up below other windows a noticeable fraction of the time.
//!
//! This is a tiny background thread that re-asserts topmost roughly once a second
//! while the mic is visible. It uses the same `SWP_NOACTIVATE` / `WS_EX_NOACTIVATE`
//! path, so it never steals foreground or activation — safe to run even mid-
//! dictation. It runs only in floating-mic mode (started/stopped alongside the
//! display mode); side-panel and hidden modes don't need it.
//!
//! Limitation: over true *exclusive*-fullscreen apps (some games) Windows
//! suppresses topmost overlays by design — re-asserting can't override that.
//! Borderless/windowed-fullscreen (most modern apps, browser/video fullscreen)
//! is covered.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub struct TopmostKeeperHandle {
    shutdown: Arc<AtomicBool>,
    _thread: Option<JoinHandle<()>>,
}

impl TopmostKeeperHandle {
    /// Starts the keeper thread. Re-asserts the mic's topmost state about once a
    /// second while it's visible. Drop the handle to stop it.
    pub fn start(app: AppHandle) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let thread = thread::Builder::new()
            .name("topmost-keeper".into())
            .spawn(move || run_keeper(app, shutdown_clone))
            .ok();
        Self {
            shutdown,
            _thread: thread,
        }
    }
}

impl Drop for TopmostKeeperHandle {
    fn drop(&mut self) {
        // Signal the loop to exit; it polls the flag in ~100ms chunks and returns
        // on its own. We don't join — the thread holds no locks of ours.
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

fn run_keeper(app: AppHandle, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        if let Some(mic) = app.get_webview_window("mic") {
            // Only re-raise an already-visible mic: never un-hide one the user
            // (or a transient mode) deliberately hid.
            if mic.is_visible().unwrap_or(false) {
                crate::win_util::make_topmost_noactivate(&mic);
            }
        }
        // Sleep ~1s, but in small chunks so a Drop signal stops us within ~100ms
        // instead of leaving a zombie thread alive for up to a full second.
        for _ in 0..10 {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}
