//! Side-panel display mode: a glowing neon radial menu docked to the right edge
//! of the screen (closed = a slim vertical tab with a half-circle trigger;
//! open = a half-circle radial menu with three wedge sectors). An alternative to
//! the floating mic, selected in settings. This module owns the `panel` window's
//! geometry and lifecycle; all three sectors are *new entry points into existing
//! pipelines* (`toggle_listening`, `translate_file`, `transcribe_audio_file`) —
//! no new transcription/translation logic lives here.
//!
//! The menu is vertically centered and spans most of the screen height (matching
//! the mockup), so the window is sized to the full work-area height in both
//! states; only the width changes (collapsed → a narrow strip, expanded → wide
//! enough for the radial menu). X is always anchored to the right work-area edge.

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Position, Size};

/// Collapsed width (logical px): a narrow strip on the right edge holding the
/// vertical tab + half-circle trigger, so clicks elsewhere are mostly free.
const COLLAPSED_W: f64 = 92.0;
/// Expanded width (logical px): wide enough for the radial half-circle menu plus
/// its outer neon bloom. The CSS caps the visual size, so extra width is just
/// transparent glow headroom.
const EXPANDED_W: f64 = 360.0;

/// Sizes and positions the `panel` window for the given expand state. Height is
/// always the full work-area height (the menu is centered vertically in CSS); X
/// is anchored to the right work-area edge; width depends on the expand state.
pub fn position_panel(app: &AppHandle, expanded: bool) {
    let Some(win) = app.get_webview_window("panel") else {
        return;
    };

    // Work area (excludes the taskbar) of the panel's current monitor.
    let monitor = match win.current_monitor() {
        Ok(Some(m)) => m,
        _ => return,
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let area_x = area.position.x;
    let area_y = area.position.y;
    let area_w = area.size.width as i32;
    let area_h = area.size.height as i32;

    let logical_w = if expanded { EXPANDED_W } else { COLLAPSED_W };
    let phys_w = (logical_w * scale).round() as i32;

    // Full-height window, pinned to the right edge.
    let _ = win.set_size(Size::Physical(PhysicalSize::new(
        phys_w.max(1) as u32,
        area_h.max(1) as u32,
    )));
    let x = area_x + area_w - phys_w;
    let _ = win.set_position(Position::Physical(PhysicalPosition::new(x, area_y)));

    // Clip the (otherwise full-height, transparent) window to just the visible
    // shape so clicks around it pass through to the desktop. A circle centered on
    // the window's right edge yields the left-facing semicircle: the small
    // collapsed trigger, or the larger radial menu when expanded. Done in Rust
    // (right after the resize, which resets any prior region) so it's reliable —
    // no JS/resize-event timing races.
    #[cfg(target_os = "windows")]
    {
        let win_h_logical = (area_h as f64 / scale).max(1.0);
        // Radii mirror the CSS: collapsed trigger is 38px; the open menu radius is
        // min(24vh, 280) logical (its container is half the window height tall).
        let r_logical = if expanded {
            (0.24 * win_h_logical).min(280.0) + 16.0
        } else {
            38.0 + 12.0
        };
        let r = (r_logical * scale).round() as i32;
        let cx = phys_w; // window-relative right edge
        let cy = area_h / 2; // window-relative vertical middle
        crate::win_util::set_ellipse_region(&win, cx - r, cy - r, cx + r, cy + r);
    }
}

/// Shows the panel (collapsed), positions it, and re-applies the no-activate
/// topmost style. Used when switching into side-panel mode and on startup.
pub fn show_panel(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.show();
        position_panel(app, false);
        #[cfg(target_os = "windows")]
        crate::win_util::make_topmost_noactivate(&win);
    }
}

/// Hides the panel window.
pub fn hide_panel(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.hide();
    }
}
