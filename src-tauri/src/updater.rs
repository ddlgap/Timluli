//! Auto-update via `tauri-plugin-updater`.
//!
//! Checks GitHub Releases for a newer **signed** installer and, with the
//! user's consent, downloads and installs it. All UI strings are Hebrew to
//! match the rest of the app (see CLAUDE.md "User-facing strings are Hebrew").
//!
//! The whole flow lives in Rust (no frontend changes) and reuses the
//! already-registered `tauri-plugin-dialog` for the consent / progress
//! prompts, mirroring the existing pattern elsewhere in the app.

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

/// Kick off a non-blocking update check.
///
/// `manual = true` means the user explicitly asked (from the tray), so we also
/// surface "you're up to date" and error messages. On the silent startup check
/// (`manual = false`) we stay quiet unless an update is actually available, so
/// offline launches and rate-limited checks never nag the user.
pub fn check(app: AppHandle, manual: bool) {
    tauri::async_runtime::spawn(async move {
        run_check(app, manual).await;
    });
}

async fn run_check(app: AppHandle, manual: bool) {
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            if manual {
                notify(&app, "שגיאה", &format!("בדיקת העדכונים נכשלה: {e}"));
            }
            return;
        }
    };

    match updater.check().await {
        Ok(Some(update)) => prompt_and_install(&app, update).await,
        Ok(None) => {
            if manual {
                notify(&app, "Timluli מעודכן", "אתה כבר משתמש בגרסה האחרונה.");
            }
        }
        Err(e) => {
            if manual {
                notify(&app, "שגיאה", &format!("בדיקת העדכונים נכשלה: {e}"));
            }
        }
    }
}

async fn prompt_and_install(app: &AppHandle, update: tauri_plugin_updater::Update) {
    let version = update.version.clone();
    let notes = update.body.clone().unwrap_or_default();
    let message = if notes.trim().is_empty() {
        format!("גרסה {version} זמינה. להוריד ולהתקין עכשיו?")
    } else {
        format!("גרסה {version} זמינה.\n\n{}\n\nלהוריד ולהתקין עכשיו?", notes.trim())
    };

    let confirmed = app
        .dialog()
        .message(message)
        .title("עדכון זמין ל-Timluli")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "עדכן עכשיו".into(),
            "אחר כך".into(),
        ))
        .blocking_show();

    if !confirmed {
        return;
    }

    // Download + install the signed bundle. We don't surface byte-level
    // progress (the NSIS "passive" installer shows its own progress bar);
    // the closures are required by the API but intentionally no-ops.
    match update
        .download_and_install(|_downloaded, _total| {}, || {})
        .await
    {
        Ok(()) => {
            let restart = app
                .dialog()
                .message("העדכון הותקן בהצלחה. להפעיל מחדש את Timluli עכשיו?")
                .title("העדכון הושלם")
                .kind(MessageDialogKind::Info)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "הפעל מחדש".into(),
                    "אחר כך".into(),
                ))
                .blocking_show();
            if restart {
                app.restart();
            }
        }
        Err(e) => {
            notify(app, "שגיאה בעדכון", &format!("התקנת העדכון נכשלה: {e}"));
        }
    }
}

fn notify(app: &AppHandle, title: &str, message: &str) {
    app.dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Info)
        .blocking_show();
}
