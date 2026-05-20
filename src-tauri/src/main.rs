// Prevent additional console window on Windows in release; do not remove.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Auto-grant microphone access for our embedded WebView2 so the hidden
    // speech window can run webkitSpeechRecognition without a permission
    // dialog blocking the flow on first use.
    #[cfg(target_os = "windows")]
    {
        std::env::set_var(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "--use-fake-ui-for-media-stream --autoplay-policy=no-user-gesture-required",
        );
    }

    timluli_lib::run();
}
