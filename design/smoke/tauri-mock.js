// ─────────────────────────────────────────────────────────────────────────────
// Tauri-pipeline mock for the settings-window SMOKE HARNESS.
//
// Purpose: run the REAL src/settings.html + src/scripts/settings.js in a plain
// browser (no Rust backend), record every `invoke` the frontend makes, and detect
// JS errors — so a UI/design migration can be verified, step by step, to fire the
// EXACT same backend contract as the current (working) code.
//
// HOW TO USE (with the chrome-devtools MCP, Vite already serving on :1420):
//   navigate_page({ type:'url', url:'http://127.0.0.1:1420/settings.html',
//                   initScript: <contents of this file> })
// initScript runs BEFORE the page's own scripts, so window.__TAURI_INTERNALS__ is
// in place by the time @tauri-apps/api resolves. Then drive controls via
// evaluate_script and read window.__SMOKE__.
//
// This mocks ONLY the JS↔Rust boundary. It proves the frontend calls the right
// commands with the right shape and renders; it does NOT replace a final
// `npm run tauri:dev` manual pass against the real app.
// ─────────────────────────────────────────────────────────────────────────────
(() => {
  const calls = [];
  let cbId = 0;

  // Mock settings — mirrors src-tauri/src/settings.rs defaults (snake_case fields).
  const SETTINGS = {
    language: 'he-IL', shortcut: 'Ctrl+Ctrl', activation_mode: 'toggle',
    mic_size: 'medium', mic_opacity: 0.95, silence_timeout_ms: 1500,
    start_with_windows: false, show_mic_on_startup: true,
    mute_during_fullscreen: false, mute_during_calls: false, mic_position: null,
    engine_id: 'web-speech', local_model_id: null, mic_theme: 'graphite',
    onboarding_done: true, translate_target_language: 'Hebrew',
    pdf_rtl_layout: 'same-box', field_docking_enabled: true,
    groq_model: null, cerebras_model: null, groq_paid: false, cerebras_paid: false,
    audio_file_engine: 'groq', display_mode: 'side-panel', panel_offset_y: null,
    mic_window_v2: true, punctuation_enabled: false, punctuation_newline: false,
  };
  // Mock ModelView[] — mirrors commands_local.rs list_models output.
  const MODELS = [
    { id: 'ivrit-q5', displayName: 'ivrit.ai Turbo Q5', sizeBytes: 704000000, quality: 'high', source: 'catalog', status: 'installed', isActive: true },
    { id: 'ivrit-fp16', displayName: 'ivrit.ai Turbo FP16', sizeBytes: 1620000000, quality: 'high', source: 'catalog', status: 'not_installed', isActive: false },
  ];

  function handle(cmd, p) {
    switch (cmd) {
      case 'get_settings': return Promise.resolve({ ...SETTINGS });
      case 'get_translation_keys_status': return Promise.resolve({ groq_set: false, cerebras_set: false });
      case 'list_provider_models': return Promise.resolve((p && p.key) ? [{ id: 'gpt-oss-120b' }, { id: 'llama-4-scout' }] : []);
      case 'list_models': return Promise.resolve(MODELS.map(m => ({ ...m })));
      case 'get_punctuation_status': return Promise.resolve({ enabled: false, installed: true, loaded: false, downloading: false });
      case 'get_ffmpeg_status': return Promise.resolve({ installed: true });
    }
    if (cmd.indexOf('plugin:app') === 0) return Promise.resolve('1.12.0'); // getVersion
    return Promise.resolve(null); // generic OK for set_*/update_*/load_*/plugin:*
  }

  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'settings' }, currentWebview: { label: 'settings', windowLabel: 'settings' } },
    transformCallback(cb) { return ++cbId; },
    invoke(cmd, payload) {
      calls.push({ cmd, payload: payload ?? null });
      if (cmd === 'plugin:event|listen') return Promise.resolve(++cbId);
      if (cmd === 'plugin:event|unlisten') return Promise.resolve();
      return handle(cmd, payload);
    },
    convertFileSrc(x) { return x; },
  };

  window.__SMOKE__ = {
    calls,
    reset() { calls.length = 0; },
    commands() { return [...new Set(calls.map(c => c.cmd))]; },
    callsFor(cmd) { return calls.filter(c => c.cmd === cmd); },
    errors: [],
  };
  window.addEventListener('error', e => window.__SMOKE__.errors.push(String(e.message)));
  window.addEventListener('unhandledrejection', e => window.__SMOKE__.errors.push('promise: ' + String(e.reason)));
})();
