import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Channel } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { getVersion } from '@tauri-apps/api/app';
import { getCurrentWindow } from '@tauri-apps/api/window';

const $ = (id) => document.getElementById(id);

// ---- Custom title-bar controls (decorations:false) ----
// Close hides the window (lib.rs intercepts CloseRequested for settings).
const appWindow = getCurrentWindow();
$('tb-min')?.addEventListener('click', () => appWindow.minimize());
$('tb-close')?.addEventListener('click', () => appWindow.close());

// ---- Maximize / restore (custom titlebar button) ----
async function refreshMaxIcon() {
  try {
    const max = await appWindow.isMaximized();
    $('tb-max')?.classList.toggle('is-max', !!max);
  } catch (_) {}
}
$('tb-max')?.addEventListener('click', async () => {
  try { await appWindow.toggleMaximize(); } catch (_) {}
  refreshMaxIcon();
});
appWindow.onResized?.(() => refreshMaxIcon());
refreshMaxIcon();

// ---- Light / dark mode (settings window only; persisted locally) ----
const THEME_KEY = 'timluli_settings_theme';
function applyTheme(t) {
  document.documentElement.classList.toggle('light', t === 'light');
  const btn = $('tb-theme');
  if (btn) btn.setAttribute('aria-label', t === 'light' ? 'עבור למצב כהה' : 'עבור למצב בהיר');
}
let uiTheme = (() => { try { return localStorage.getItem(THEME_KEY) || 'light'; } catch (_) { return 'light'; } })();
applyTheme(uiTheme);
$('tb-theme')?.addEventListener('click', () => {
  uiTheme = uiTheme === 'light' ? 'dark' : 'light';
  try { localStorage.setItem(THEME_KEY, uiTheme); } catch (_) {}
  applyTheme(uiTheme);
});

// ---- Tabs (ARIA tablist with roving tabindex + arrow-key navigation) ----
const tabs = Array.from(document.querySelectorAll('nav.tabs [role="tab"]'));
const sections = document.querySelectorAll('main .section');

function activateTab(tab, focus = false) {
  if (!tab) return;
  tabs.forEach((t) => {
    const on = t === tab;
    t.classList.toggle('active', on);
    t.setAttribute('aria-selected', on ? 'true' : 'false');
    t.tabIndex = on ? 0 : -1;
  });
  sections.forEach((s) => s.classList.remove('active'));
  document.getElementById(tab.dataset.tab)?.classList.add('active');
  if (focus) tab.focus();
}

tabs.forEach((tab) => {
  tab.addEventListener('click', () => activateTab(tab));
});

// RTL tablist: ArrowLeft moves to the next tab, ArrowRight to the previous.
document.querySelector('nav.tabs')?.addEventListener('keydown', (e) => {
  const i = tabs.indexOf(document.activeElement);
  if (i === -1) return;
  let next = null;
  if (e.key === 'ArrowLeft') next = tabs[(i + 1) % tabs.length];
  else if (e.key === 'ArrowRight') next = tabs[(i - 1 + tabs.length) % tabs.length];
  else if (e.key === 'Home') next = tabs[0];
  else if (e.key === 'End') next = tabs[tabs.length - 1];
  if (next) { e.preventDefault(); activateTab(next, true); }
});

const hashTab = location.hash.replace('#', '');
if (hashTab) activateTab(document.querySelector(`[data-tab="${hashTab}"]`));

// Home quick-jump cards → switch to the named settings tab.
document.querySelectorAll('.jump-card[data-jump]').forEach((card) => {
  card.addEventListener('click', () => {
    const tab = document.querySelector(`nav.tabs [data-tab="${card.dataset.jump}"]`);
    if (tab) activateTab(tab, true);
  });
});

// ---- Auto-save ----
// Every staged control persists itself shortly after it changes (debounced), so
// there is no "Save" button and no "unsaved" limbo. Instant controls (theme,
// engine, display mode, punctuation, video subtitles) still self-save via their
// own handlers and early-return from the change listener below. Keeping the
// setDirty(true/false) name means every existing call site now schedules a save.
let autosaveTimer = null;
function setDirty(v) {
  clearTimeout(autosaveTimer);
  if (!v) return;                 // false = nothing pending (during load / after a save)
  autosaveTimer = setTimeout(() => saveSettings(), 350);
}

// Controls that persist on change (so they must NOT mark the form dirty).
const INSTANT_IDS = new Set(['display_mode', 'punctuation_enabled', 'punctuation_newline', 'video_subtitles_enabled']);
const INSTANT_NAMES = new Set(['engine_id', 'audio_file_engine']);
document.querySelector('main')?.addEventListener('change', (e) => {
  const t = e.target;
  if (!t || t.disabled) return;
  if (INSTANT_IDS.has(t.id) || INSTANT_NAMES.has(t.name)) return;
  if (t.closest('dialog')) return; // wizard inputs are saved on their own
  setDirty(true);
});

// External links open in the system browser (the webview swallows target="_blank").
document.addEventListener('click', (e) => {
  const a = e.target.closest('a[href]');
  if (!a) return;
  const href = a.getAttribute('href');
  if (href && /^https?:\/\//i.test(href)) {
    e.preventDefault();
    invoke('open_external', { url: href }).catch((err) =>
      console.warn('open_external failed:', err)
    );
  }
});

// ---- Shortcut recorder ----
const shortcutInput = $('shortcut-key');
const shortcutStatus = $('shortcut-status');
let recording = false;
let originalShortcut = '';

async function startRecording() {
  recording = true;
  originalShortcut = shortcutInput.textContent;
  shortcutInput.classList.add('recording');
  shortcutInput.textContent = 'הקש קומבינציה (Esc לביטול)...';
  shortcutStatus.textContent = '';
  try { await invoke('pause_global_shortcut'); } catch (e) { console.warn(e); }
}

async function endRecording(restore = true) {
  recording = false;
  shortcutInput.classList.remove('recording');
  if (restore) {
    shortcutInput.textContent = originalShortcut;
  }
  try { await invoke('resume_global_shortcut'); } catch (e) { console.warn(e); }
}

shortcutInput.addEventListener('click', () => {
  if (!recording) startRecording();
});

window.addEventListener('keydown', (e) => {
  if (!recording) return;
  e.preventDefault();
  e.stopPropagation();

  if (e.key === 'Escape') {
    endRecording(true);
    return;
  }

  const parts = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Super');

  const key = e.key;
  const onlyMods = ['Control', 'Alt', 'Shift', 'Meta', 'OS'].includes(key);
  if (onlyMods) return;

  let mainKey = key;
  if (key === ' ') mainKey = 'Space';
  else if (mainKey.length === 1) mainKey = mainKey.toUpperCase();
  parts.push(mainKey);

  if (parts.length < 2) {
    shortcutStatus.textContent = 'נדרש מודיפייר לפחות (Ctrl/Alt/Shift/Win) + מקש.';
    return;
  }
  shortcutInput.textContent = parts.join('+');
  setDirty(true);
  endRecording(false);
});

// ---- Shortcut type: double-tap (e.g. Ctrl+Ctrl) vs custom combo ----
const shortcutType = $('shortcut_type');
const doubleTapModifier = $('double_tap_modifier');
const doubleTapField = $('double_tap_field');
const comboField = $('combo_field');

const DT_MODS = ['Ctrl', 'Alt', 'Shift', 'Win'];

// "Ctrl+Ctrl" / "Alt+Alt" → the modifier name; any real chord → null.
function doubleTapModifierOf(combo) {
  if (!combo) return null;
  const toks = combo.split('+').map((t) => t.trim()).filter(Boolean);
  if (toks.length < 2) return null;
  const norm = toks.map((t) => {
    const l = t.toLowerCase();
    if (l === 'super' || l === 'meta' || l === 'win' || l === 'cmd') return 'Win';
    if (l === 'control') return 'Ctrl';
    return t;
  });
  const first = norm[0];
  if (!DT_MODS.includes(first)) return null;
  return norm.every((t) => t === first) ? first : null;
}

function applyShortcutType(type) {
  const isDouble = type === 'double';
  doubleTapField.style.display = isDouble ? '' : 'none';
  comboField.style.display = isDouble ? 'none' : '';
}

// The shortcut string currently configured in the UI.
function currentShortcutString() {
  if (shortcutType.value === 'double') {
    const mod = doubleTapModifier.value || 'Ctrl';
    return `${mod}+${mod}`;
  }
  return shortcutInput.textContent;
}

shortcutType.addEventListener('change', () => applyShortcutType(shortcutType.value));

// ---- Toggle widgets ----
// The checkbox is the source of truth (focusable for keyboard/Space + screen
// readers); clicking the pill or its label drives it, and a single change
// listener keeps the visual `.on` state and the dirty flag in sync.
// Mirror a single-purpose toggle's on/off onto its wrapping .s-card so the card
// gets the green "active" tint (Dribbble reference). Cards that hold more than
// one toggle are skipped, so a sub-option can't flip the whole card's tint.
function syncCardOn(toggleEl) {
  const card = toggleEl.closest('.s-card');
  if (!card) return;
  if (card.querySelectorAll('.toggle').length !== 1) return;
  card.classList.toggle('on', toggleEl.classList.contains('on'));
}

document.querySelectorAll('.toggle').forEach((el) => {
  const checkbox = el.querySelector('input');
  if (!checkbox) return;
  el.addEventListener('click', (e) => {
    if (checkbox.disabled) return;
    if (e.target === checkbox) return; // native toggle already handled it
    checkbox.checked = !checkbox.checked;
    checkbox.dispatchEvent(new Event('change', { bubbles: true }));
  });
  checkbox.addEventListener('change', () => {
    el.classList.toggle('on', checkbox.checked);
    syncCardOn(el);
  });
});

function setToggle(key, value) {
  const el = document.querySelector(`.toggle[data-key="${key}"]`);
  if (!el) return;
  el.classList.toggle('on', !!value);
  const cb = el.querySelector('input');
  if (cb) cb.checked = !!value;
  syncCardOn(el);
}

function getToggle(key) {
  const el = document.querySelector(`.toggle[data-key="${key}"]`);
  return el ? el.classList.contains('on') : false;
}

// ---- Opacity slider ----
const opacity = $('mic_opacity');
const opacityValue = $('opacity_value');
opacity.addEventListener('input', () => {
  opacityValue.textContent = opacity.value;
});

// ---- Silence timeout slider ----
const silence = $('silence_timeout_ms');
const silenceValue = $('silence_value');
silence.addEventListener('input', () => {
  silenceValue.textContent = silence.value;
});

// ---- Load settings into UI ----
async function loadSettings() {
  const stg = await invoke('get_settings');
  $('language').value = stg.language || 'he-IL';
  setToggle('start_with_windows', stg.start_with_windows);
  setToggle('show_mic_on_startup', stg.show_mic_on_startup);
  // mute_during_fullscreen / mute_during_calls are disabled (feature not live yet) —
  // they stay visibly off rather than showing a stored value behind a dead control.
  setToggle('field_docking_enabled', stg.field_docking_enabled);
  setToggle('punctuation_newline', stg.punctuation_newline);
  setToggle('video_subtitles_enabled', stg.video_subtitles_enabled !== false);
  const sc = stg.shortcut || 'Ctrl+Ctrl';
  // Home quick-guide shortcut chip — show the live shortcut (Super → Win for users).
  const homeKbd = $('home-shortcut-kbd');
  if (homeKbd) homeKbd.textContent = sc.split('+').map((t) => (t === 'Super' ? 'Win' : t)).join(' + ');
  const dtMod = doubleTapModifierOf(sc);
  if (dtMod) {
    shortcutType.value = 'double';
    doubleTapModifier.value = dtMod;
  } else {
    shortcutType.value = 'combo';
    shortcutInput.textContent = sc;
  }
  applyShortcutType(shortcutType.value);
  $('activation_mode').value = stg.activation_mode || 'toggle';
  $('display_mode').value = stg.display_mode || 'side-panel';
  applyDisplayModeVisibility($('display_mode').value);
  $('mic_size').value = stg.mic_size || 'medium';
  const op = Math.round((stg.mic_opacity ?? 0.95) * 100);
  opacity.value = String(op);
  opacityValue.textContent = String(op);
  const sil = stg.silence_timeout_ms ?? 1500;
  silence.value = String(sil);
  silenceValue.textContent = String(sil);

  // Engine tab
  const engineId = stg.engine_id || 'web-speech';
  setEngineRadio(engineId);
  setAudioFileEngineRadio(stg.audio_file_engine || 'groq');

  // Theme swatches
  setSelectedTheme(stg.mic_theme || 'graphite');

  // Subtitle burn-in style cards
  setSelectedBurnStyle(stg.burn_style || 'classic');

  // Translation tab
  $('translate_target_language').value = stg.translate_target_language || 'Hebrew';
  $('pdf_rtl_layout').value = stg.pdf_rtl_layout || 'same-box';
  // Each provider's tier maps to its own paid flag (independent free/paid).
  setSpeed('groq', stg.groq_paid ? 'fast' : 'normal');
  setSpeed('cerebras', stg.cerebras_paid ? 'fast' : 'normal');
  await refreshKeyStatus();
  await populateModels('groq', stg.groq_model);
  await populateModels('cerebras', stg.cerebras_model);

  setDirty(false);
  return stg;
}

// ---- Translation speed segmented controls (one per provider/key) ----
// Each provider has its own free/paid tier, so a free main service can sit next
// to a paid backup (or vice versa). 'groq' = main service, 'cerebras' = backup.
function setSpeed(provider, speed) {
  document.querySelectorAll(`#${provider}_speed_segmented .seg`).forEach((b) => {
    const on = b.dataset.speed === speed;
    b.classList.toggle('on', on);
    if (b.getAttribute('role') === 'radio') b.setAttribute('aria-checked', on ? 'true' : 'false');
  });
}
function getSpeed(provider) {
  const on = document.querySelector(`#${provider}_speed_segmented .seg.on`);
  return on ? on.dataset.speed : 'normal';
}
['groq', 'cerebras'].forEach((provider) => {
  document.querySelectorAll(`#${provider}_speed_segmented .seg`).forEach((btn) => {
    btn.addEventListener('click', () => { setSpeed(provider, btn.dataset.speed); setDirty(true); });
  });
});

// Updates the status banner from the current key state.
function updateTranslateBanner(hasKey) {
  const banner = $('translate_banner');
  if (!banner) return;
  const icon = banner.querySelector('.tb-icon');
  const text = banner.querySelector('.tb-text');
  banner.classList.toggle('ready', hasKey);
  banner.classList.toggle('setup', !hasKey);
  if (hasKey) {
    icon.textContent = '✓';
    text.textContent = 'הכול מוכן — גרור מסמך על אייקון המיקרופון כדי לתרגם.';
  } else {
    icon.textContent = '↓';
    text.textContent = "כדי להתחיל, לחץ על „חבר שירות תרגום” למטה.";
  }
}

// ---- Connect wizard (guided key setup + live validation) ----
const connectDialog = $('connect_dialog');
let wizValidatedKey = null;
let wizTimer = null;

function setWizFeedback(text, kind) {
  const f = $('wiz_feedback');
  if (!f) return;
  f.textContent = text;
  f.className = 'wiz-feedback' + (kind ? ' ' + kind : '');
}

function resetWizard() {
  if ($('groq_api_key')) $('groq_api_key').value = '';
  setWizFeedback('', '');
  if ($('wiz_save')) $('wiz_save').disabled = true;
  wizValidatedKey = null;
  clearTimeout(wizTimer);
}

$('conn_open')?.addEventListener('click', () => {
  resetWizard();
  if (connectDialog?.showModal) connectDialog.showModal();
});
$('wiz_close')?.addEventListener('click', () => connectDialog?.close());
$('wiz_cancel')?.addEventListener('click', () => connectDialog?.close());
connectDialog?.addEventListener('close', resetWizard);

$('wiz_open_site')?.addEventListener('click', () => {
  invoke('open_external', { url: 'https://console.groq.com/keys' }).catch(() => {});
});

// Validate the pasted key live (debounced) before allowing save.
$('groq_api_key')?.addEventListener('input', () => {
  const key = $('groq_api_key').value.trim();
  $('wiz_save').disabled = true;
  wizValidatedKey = null;
  clearTimeout(wizTimer);
  if (!key) {
    setWizFeedback('', '');
    return;
  }
  setWizFeedback('בודק את המפתח…', 'checking');
  wizTimer = setTimeout(async () => {
    try {
      const models = await invoke('list_provider_models', { provider: 'groq', key });
      if ($('groq_api_key').value.trim() !== key) return; // stale
      setWizFeedback(`✓ המפתח תקין — נמצאו ${models.length} מודלים`, 'ok');
      wizValidatedKey = key;
      $('wiz_save').disabled = false;
    } catch (e) {
      if ($('groq_api_key').value.trim() !== key) return; // stale
      setWizFeedback('✗ המפתח לא תקין או שאין חיבור לאינטרנט', 'err');
      wizValidatedKey = null;
      $('wiz_save').disabled = true;
    }
  }, 600);
});

$('wiz_save')?.addEventListener('click', async () => {
  const key = wizValidatedKey || $('groq_api_key').value.trim();
  if (!key) return;
  $('wiz_save').disabled = true;
  try {
    await invoke('save_translation_keys', { groq: key, cerebras: null });
    connectDialog?.close();
    await refreshKeyStatus();
    await populateModels('groq', $('groq_model').value);
    showToast('שירות התרגום חובר בהצלחה', 'ok');
  } catch (err) {
    setWizFeedback(`שגיאה בשמירה: ${err}`, 'err');
    $('wiz_save').disabled = false;
  }
});

/// Fills a provider's model <select> from its live /models endpoint (requires a
/// saved key). Keeps the leading "automatic" option and preserves `selected`.
async function populateModels(provider, selected) {
  const sel = $(`${provider}_model`);
  if (!sel) return;
  // Reset to just the "automatic" option (the first child).
  while (sel.options.length > 1) sel.remove(1);
  let models = [];
  try {
    models = await invoke('list_provider_models', { provider, key: null });
  } catch (e) {
    /* no key yet, or fetch failed — leave only "automatic" */
  }
  // Models arrive quality-ranked from the backend (best first), so the first entry
  // is the recommended pick — flag it in the label (value stays the bare id).
  models.forEach((m, i) => {
    const opt = document.createElement('option');
    opt.value = m.id;
    opt.textContent = i === 0 ? `${m.id} — מומלץ` : m.id;
    sel.appendChild(opt);
  });
  const want = selected || '';
  // If a previously-saved model isn't in the fetched list, keep it selectable.
  if (want && !Array.from(sel.options).some((o) => o.value === want)) {
    const opt = document.createElement('option');
    opt.value = want;
    opt.textContent = want;
    sel.appendChild(opt);
  }
  sel.value = want;
}

async function refreshKeyStatus() {
  try {
    const status = await invoke('get_translation_keys_status');
    // Primary connection (Groq) — surfaced via the connect row.
    const cs = $('conn_status');
    const cb = $('conn_open');
    if (cs) {
      cs.textContent = status.groq_set ? 'מחובר ✓' : 'לא מחובר';
      cs.classList.toggle('ok', status.groq_set);
    }
    if (cb) cb.textContent = status.groq_set ? 'החלף מפתח' : 'חבר שירות תרגום';
    // Backup service (Cerebras) lives in the advanced section.
    const c = $('cerebras_saved');
    if (c) c.style.display = status.cerebras_set ? 'inline' : 'none';
    if (status.cerebras_set) $('cerebras_api_key').placeholder = '•••••••••• (מחובר)';
    updateTranslateBanner(status.groq_set || status.cerebras_set);
  } catch (e) {
    updateTranslateBanner(false);
  }
}

function setSelectedTheme(theme) {
  document.querySelectorAll('.theme-swatch').forEach((el) => {
    const on = el.dataset.theme === theme;
    el.classList.toggle('selected', on);
    el.setAttribute('aria-pressed', on ? 'true' : 'false');
  });
}

document.querySelectorAll('.theme-swatch').forEach((swatch) => {
  swatch.addEventListener('click', async () => {
    const theme = swatch.dataset.theme;
    setSelectedTheme(theme);
    try {
      const previous = await invoke('get_settings');
      await invoke('save_settings', {
        newSettings: { ...previous, mic_theme: theme },
      });
      showToast('ערכת הצבע עודכנה', 'ok');
    } catch (err) {
      showToast(`שגיאה: ${err}`, 'err');
    }
  });
});

// ---- Subtitle burn-in style chips (instant self-save, like theme swatches) ----
// The chips are compact (preview + name only); the selected style's description
// shows in one shared line under the grid.
function setSelectedBurnStyle(style) {
  let desc = '';
  document.querySelectorAll('.burn-card').forEach((el) => {
    const on = el.dataset.style === style;
    el.classList.toggle('selected', on);
    el.setAttribute('aria-pressed', on ? 'true' : 'false');
    if (on) desc = el.dataset.desc || '';
  });
  const descEl = document.getElementById('burn-style-desc');
  if (descEl) descEl.textContent = desc;
}

document.querySelectorAll('.burn-card:not(.disabled)').forEach((card) => {
  card.addEventListener('click', async () => {
    const style = card.dataset.style;
    setSelectedBurnStyle(style);
    try {
      const previous = await invoke('get_settings');
      await invoke('save_settings', {
        newSettings: { ...previous, burn_style: style },
      });
      showToast('סגנון הצריבה עודכן', 'ok');
    } catch (err) {
      showToast(`שגיאה: ${err}`, 'err');
    }
  });
});

// ---- Save ----
let saving = false;
async function saveSettings() {
  if (saving) { setDirty(true); return; }  // a save is in flight — re-queue, don't overlap
  saving = true;

  const previous = await invoke('get_settings');
  const newSettings = {
    ...previous,
    language: $('language').value,
    shortcut: currentShortcutString(),
    activation_mode: $('activation_mode').value,
    mic_size: $('mic_size').value,
    mic_opacity: Number(opacity.value) / 100,
    start_with_windows: getToggle('start_with_windows'),
    show_mic_on_startup: getToggle('show_mic_on_startup'),
    mute_during_fullscreen: getToggle('mute_during_fullscreen'),
    mute_during_calls: getToggle('mute_during_calls'),
    field_docking_enabled: getToggle('field_docking_enabled'),
    silence_timeout_ms: Number(silence.value),
    translate_target_language: $('translate_target_language').value,
    pdf_rtl_layout: $('pdf_rtl_layout').value,
    groq_model: $('groq_model').value || null,
    cerebras_model: $('cerebras_model').value || null,
    // Independent per-provider tiers: the backend uses each model's own flag for
    // its output cap / pacing, and the primary's for the job-level path.
    groq_paid: getSpeed('groq') === 'fast',
    cerebras_paid: getSpeed('cerebras') === 'fast',
    // engine_id is saved immediately on radio change, not on Save button
  };

  try {
    if (newSettings.shortcut !== previous.shortcut) {
      await invoke('update_shortcut', { combo: newSettings.shortcut });
    }
    if (newSettings.start_with_windows !== previous.start_with_windows) {
      await invoke('set_autostart_enabled', { enabled: newSettings.start_with_windows });
    }
    if (newSettings.field_docking_enabled !== previous.field_docking_enabled) {
      await invoke('set_field_docking', { enabled: newSettings.field_docking_enabled });
    }
    await invoke('save_settings', { newSettings });

    // Persist API keys only when the user typed something new (blank = unchanged).
    const groqVal = $('groq_api_key').value.trim();
    const cerebrasVal = $('cerebras_api_key').value.trim();
    if (groqVal || cerebrasVal) {
      await invoke('save_translation_keys', {
        groq: groqVal || null,
        cerebras: cerebrasVal || null,
      });
      $('groq_api_key').value = '';
      $('cerebras_api_key').value = '';
      await refreshKeyStatus();
      // A freshly-saved key unlocks the live model list.
      if (groqVal) await populateModels('groq', $('groq_model').value);
      if (cerebrasVal) await populateModels('cerebras', $('cerebras_model').value);
    }

    setDirty(false);
    showToast('נשמר ✓', 'ok');
  } catch (err) {
    showToast(`שגיאה בשמירה: ${err}`, 'err');
  } finally {
    saving = false;
  }
}

function showToast(text, kind = 'ok') {
  const toast = $('toast');
  if (!toast) return;
  const icon = toast.querySelector('.icon');
  const label = toast.querySelector('.text');
  icon.textContent = kind === 'ok' ? '✓' : '⚠';
  label.textContent = text;
  toast.classList.remove('ok', 'err', 'show');
  void toast.offsetWidth;
  toast.classList.add(kind, 'show');
  clearTimeout(showToast._t);
  showToast._t = setTimeout(() => toast.classList.remove('show'), 2400);
}

function setStatus(text, kind = '') {
  const el = $('status');
  el.textContent = text;
  el.className = 'status-msg' + (kind ? ' ' + kind : '');
  if (text) setTimeout(() => { el.textContent = ''; el.className = 'status-msg'; }, 2500);
}

$('reset')?.addEventListener('click', async () => {
  if (!confirm('לשחזר הגדרות ברירת מחדל?')) return;
  await invoke('set_field_docking', { enabled: false });
  await invoke('set_display_mode', { mode: 'side-panel' }).catch(() => {});
  await invoke('update_shortcut', { combo: 'Ctrl+Ctrl' }).catch(() => {});
  await invoke('save_settings', {
    newSettings: {
      language: 'he-IL',
      shortcut: 'Ctrl+Ctrl',
      activation_mode: 'toggle',
      mic_size: 'medium',
      mic_opacity: 0.95,
      start_with_windows: false,
      show_mic_on_startup: true,
      mute_during_fullscreen: false,
      mute_during_calls: false,
      silence_timeout_ms: 1500,
      mic_position: null,
      engine_id: 'web-speech',
      local_model_id: null,
      field_docking_enabled: true,
    },
  });
  await loadSettings();
  showToast('שוחזרו ברירות מחדל', 'ok');
});

await listen('speakly://shortcut-conflict', (e) => {
  shortcutStatus.textContent = `שים לב: הקיצור "${e.payload}" כבר תפוס במערכת.`;
});

await listen('speakly://error', (e) => setStatus(String(e.payload), 'err'));

// ════════════════════════════════════════════════════════════════════════════
// Engine tab logic
// ════════════════════════════════════════════════════════════════════════════

let currentEngineId = 'web-speech';
let modelsData = []; // last fetched ModelView[]

function setEngineRadio(engineId) {
  currentEngineId = engineId;
  document.querySelectorAll('input[name="engine_id"]').forEach((r) => {
    r.checked = r.value === engineId;
    r.closest('.engine-option').classList.toggle('selected', r.checked);
  });
  updateNoModelGuidance();
}

function updateNoModelGuidance() {
  const guidance = $('no-model-guidance');
  if (!guidance) return;
  const hasInstalled = modelsData.some(
    (m) => m.status === 'installed' || m.status === 'manually_imported'
  );
  // A local model is needed by either the live offline engine OR the file
  // transcription engine (audio + video share `audio_file_engine`). Show the
  // "download a model" hint whenever any local path is selected but none exists.
  const audioLocal =
    document.querySelector('input[name="audio_file_engine"]:checked')?.value === 'whisper-local';
  const needsLocal = currentEngineId === 'whisper-local' || audioLocal;
  guidance.style.display = needsLocal && !hasInstalled ? 'flex' : 'none';
}

// Audio-file transcription backend selection
function setAudioFileEngineRadio(engineId) {
  document.querySelectorAll('input[name="audio_file_engine"]').forEach((r) => {
    r.checked = r.value === engineId;
    r.closest('.engine-option').classList.toggle('selected', r.checked);
  });
}

document.querySelectorAll('input[name="audio_file_engine"]').forEach((radio) => {
  radio.addEventListener('change', async () => {
    const engineId = radio.value;
    setAudioFileEngineRadio(engineId);
    updateNoModelGuidance();
    try {
      await invoke('set_audio_file_engine', { engineId });
      showToast(
        engineId === 'groq'
          ? 'תמלול קבצים: ענן (Groq)'
          : 'תמלול קבצים: מקומי',
        'ok'
      );
    } catch (e) {
      showToast(`שגיאה: ${e}`, 'err');
    }
  });
});

// Engine radio change handler
document.querySelectorAll('input[name="engine_id"]').forEach((radio) => {
  radio.addEventListener('change', async () => {
    const engineId = radio.value;
    setEngineRadio(engineId);
    try {
      await invoke('set_active_engine', { engineId });
      if (engineId === 'web-speech') {
        // Free memory when switching away from local
        await invoke('unload_local_model').catch(() => {});
      }
      showToast(
        engineId === 'web-speech'
          ? 'עברת למנוע Web Speech'
          : 'עברת למנוע מקומי',
        'ok'
      );
    } catch (e) {
      showToast(`שגיאה: ${e}`, 'err');
    }
  });
});

// Listen for engine-changed from other windows / shortcut
await listen('speakly://engine-changed', (e) => {
  setEngineRadio(e?.payload?.engineId || 'web-speech');
});

// ── Hebrew auto-punctuation ───────────────────────────────────────────────────
// Applies immediately (like the engine picker). Enabling requires the ~283 MB model
// to be downloaded first; if it isn't, the toggle bounces back and offers to fetch it.
const punctToggle = $('punctuation_enabled');
const punctStatusLabel = $('punct-status-label');
const punctProgress = $('punct-progress');
const punctFill = $('punct-fill');
const punctPlabel = $('punct-plabel');
const punctDownloadBtn = $('punct-download-btn');
const punctCancelBtn = $('punct-cancel-btn');
let punctDownloading = false;

async function refreshPunctStatus() {
  try {
    const s = await invoke('get_punctuation_status');
    setToggle('punctuation_enabled', s.enabled);
    punctDownloading = s.downloading;
    if (s.installed) {
      punctStatusLabel.textContent = s.loaded ? 'מודל הפיסוק מותקן ומוכן ✓' : 'מודל הפיסוק מותקן';
    } else {
      punctStatusLabel.textContent = 'מודל הפיסוק אינו מותקן';
    }
    punctDownloadBtn.style.display = !s.installed && !punctDownloading ? '' : 'none';
    punctCancelBtn.style.display = punctDownloading ? '' : 'none';
    if (!punctDownloading) punctProgress.style.display = 'none';
  } catch (_) {}
}

async function startPunctDownload() {
  if (punctDownloading) return;
  punctDownloading = true;
  punctProgress.style.display = 'flex';
  punctDownloadBtn.style.display = 'none';
  punctCancelBtn.style.display = '';
  punctPlabel.textContent = 'מתחיל הורדה…';
  const channel = new Channel();
  channel.onmessage = (p) => {
    const pct = p.totalBytes > 0 ? Math.round((p.downloadedBytes / p.totalBytes) * 100) : 0;
    if (punctFill) punctFill.style.width = `${pct}%`;
    const mbDone = Math.round(p.downloadedBytes / 1_000_000);
    const mbTotal = Math.round(p.totalBytes / 1_000_000);
    const kbps = Math.round(p.speedBps / 1000);
    if (punctPlabel) punctPlabel.textContent = `${pct}% · ${mbDone}/${mbTotal} MB · ${kbps} KB/s`;
  };
  try {
    await invoke('download_punctuation_model', { onProgress: channel });
  } catch (e) {
    punctDownloading = false;
    showToast(`שגיאה בהורדת מודל הפיסוק: ${e}`, 'err');
    refreshPunctStatus();
  }
}

punctToggle?.addEventListener('change', async () => {
  const enabled = punctToggle.checked;
  if (enabled) {
    const s = await invoke('get_punctuation_status').catch(() => null);
    if (s && !s.installed) {
      setToggle('punctuation_enabled', false);
      showToast('יש להוריד תחילה את מודל הפיסוק', 'err');
      startPunctDownload();
      return;
    }
  }
  try {
    await invoke('set_punctuation_enabled', { enabled });
    showToast(enabled ? 'פיסוק אוטומטי הופעל' : 'פיסוק אוטומטי כובה', 'ok');
  } catch (e) {
    setToggle('punctuation_enabled', !enabled);
    showToast(`שגיאה: ${e}`, 'err');
  }
  refreshPunctStatus();
});

// "New line after each sentence" — applies immediately (no Rust command; just a flag
// read at injection time).
const punctNewline = $('punctuation_newline');
punctNewline?.addEventListener('change', async () => {
  try {
    const previous = await invoke('get_settings');
    await invoke('save_settings', {
      newSettings: { ...previous, punctuation_newline: punctNewline.checked },
    });
    showToast(punctNewline.checked ? 'שורה חדשה אחרי כל משפט: פעיל' : 'שורה חדשה: כבוי', 'ok');
  } catch (e) {
    setToggle('punctuation_newline', !punctNewline.checked);
    showToast(`שגיאה: ${e}`, 'err');
  }
});

punctDownloadBtn?.addEventListener('click', startPunctDownload);
punctCancelBtn?.addEventListener('click', async () => {
  await invoke('cancel_punctuation_download').catch(() => {});
  punctDownloading = false;
  refreshPunctStatus();
});

await listen('speakly://punct-model-installed', () => {
  punctDownloading = false;
  showToast('מודל הפיסוק הותקן ✓', 'ok');
  refreshPunctStatus();
});

refreshPunctStatus();

// ── Video subtitles (video → SRT) ─────────────────────────────────────────────
// The on/off flag applies immediately (saved via save_settings, like the newline
// toggle). ffmpeg is fetched on demand — its status/download row mirrors the
// punctuation model's exactly (same DownloadProgress channel shape).
const videoToggle = $('video_subtitles_enabled');
const ffStatusLabel = $('ffmpeg-status-label');
const ffProgress = $('ffmpeg-progress');
const ffFill = $('ffmpeg-fill');
const ffPlabel = $('ffmpeg-plabel');
const ffDownloadBtn = $('ffmpeg-download-btn');
const ffCancelBtn = $('ffmpeg-cancel-btn');
let ffDownloading = false;

async function refreshFfmpegStatus() {
  try {
    const s = await invoke('get_ffmpeg_status');
    ffDownloading = s.downloading;
    ffStatusLabel.textContent = s.installed ? 'ffmpeg מותקן ומוכן ✓' : 'ffmpeg אינו מותקן';
    ffDownloadBtn.style.display = !s.installed && !ffDownloading ? '' : 'none';
    ffCancelBtn.style.display = ffDownloading ? '' : 'none';
    if (!ffDownloading) ffProgress.style.display = 'none';
  } catch (_) {}
}

async function startFfmpegDownload() {
  if (ffDownloading) return;
  ffDownloading = true;
  ffProgress.style.display = 'flex';
  ffDownloadBtn.style.display = 'none';
  ffCancelBtn.style.display = '';
  ffPlabel.textContent = 'מתחיל הורדה…';
  const channel = new Channel();
  channel.onmessage = (p) => {
    const pct = p.totalBytes > 0 ? Math.round((p.downloadedBytes / p.totalBytes) * 100) : 0;
    if (ffFill) ffFill.style.width = `${pct}%`;
    const mbDone = Math.round(p.downloadedBytes / 1_000_000);
    const mbTotal = Math.round(p.totalBytes / 1_000_000);
    const kbps = Math.round(p.speedBps / 1000);
    if (ffPlabel) ffPlabel.textContent = `${pct}% · ${mbDone}/${mbTotal} MB · ${kbps} KB/s`;
  };
  try {
    await invoke('download_ffmpeg', { onProgress: channel });
  } catch (e) {
    ffDownloading = false;
    showToast(`שגיאה בהורדת ffmpeg: ${e}`, 'err');
    refreshFfmpegStatus();
  }
}

videoToggle?.addEventListener('change', async () => {
  const enabled = videoToggle.checked;
  try {
    const previous = await invoke('get_settings');
    await invoke('save_settings', {
      newSettings: { ...previous, video_subtitles_enabled: enabled },
    });
    showToast(enabled ? 'כתוביות לווידאו: פעיל' : 'כתוביות לווידאו: כבוי', 'ok');
    if (enabled) refreshFfmpegStatus();
  } catch (e) {
    setToggle('video_subtitles_enabled', !enabled);
    showToast(`שגיאה: ${e}`, 'err');
  }
});

ffDownloadBtn?.addEventListener('click', startFfmpegDownload);
ffCancelBtn?.addEventListener('click', async () => {
  await invoke('cancel_ffmpeg_download').catch(() => {});
  ffDownloading = false;
  refreshFfmpegStatus();
});

await listen('speakly://ffmpeg-installed', () => {
  ffDownloading = false;
  showToast('ffmpeg הותקן ✓', 'ok');
  refreshFfmpegStatus();
});

refreshFfmpegStatus();

// ── Display mode (floating mic ⇄ side panel) ──────────────────────────────────
// Mirrors the engine picker: apply immediately on change, no Save needed.

// Mic-appearance settings (size/opacity/theme/field-docking) only affect the
// floating-mic mode — in side-panel mode the mic is hidden at rest, so changing
// them appears to do nothing. Hide that group in side-panel mode to avoid the
// confusion (mirrors the onboarding, which only reveals the theme strip for the
// floating mic). Called on load and whenever the mode changes.
function applyDisplayModeVisibility(mode) {
  const box = $('mic-only-settings');
  // The mic appears — and its size/opacity/theme apply — in both floating-mic and
  // hidden modes; only side-panel hides the mic entirely, so hide the group there.
  if (box) box.style.display = mode === 'side-panel' ? 'none' : 'flex';
}

$('display_mode')?.addEventListener('change', async () => {
  const mode = $('display_mode').value;
  applyDisplayModeVisibility(mode);
  try {
    await invoke('set_display_mode', { mode });
    showToast(
      mode === 'side-panel' ? 'עברת לתפריט צד'
        : mode === 'hidden-mic' ? 'עברת למצב מוסתר'
        : 'עברת למיקרופון מרחף',
      'ok'
    );
  } catch (e) {
    showToast(`שגיאה: ${e}`, 'err');
  }
});

await listen('speakly://display-mode-changed', (e) => {
  const mode = typeof e?.payload === 'string' ? e.payload : 'floating-mic';
  if ($('display_mode')) $('display_mode').value = mode;
  applyDisplayModeVisibility(mode);
});

// Listen for model install / delete to refresh list
await listen('speakly://model-installed', (e) => {
  const id = e?.payload?.id;
  if (id) delete activeDownloads[id];
  refreshModelList();
});
await listen('speakly://model-deleted', () => refreshModelList());

// Guidance scroll button
$('guidance-scroll-btn')?.addEventListener('click', () => {
  $('model-list')?.scrollIntoView({ behavior: 'smooth' });
});

// Manual import button — native file picker (tauri-plugin-dialog).
$('btn-import-manual')?.addEventListener('click', async () => {
  let filePath;
  try {
    filePath = await open({
      multiple: false,
      directory: false,
      title: 'בחר קובץ מודל',
      filters: [{ name: 'מודל Whisper (.bin, .gguf)', extensions: ['bin', 'gguf'] }],
    });
  } catch (e) {
    showToast(`שגיאה בבחירת הקובץ: ${e}`, 'err');
    return;
  }
  if (!filePath) return; // user cancelled
  const displayName = prompt('שם מותאם אישית למודל:', 'מודל ידני') || 'מודל ידני';
  try {
    showToast('מייבא מודל…', 'ok');
    const view = await invoke('import_model_manual', {
      filePath,
      displayName,
    });
    await refreshModelList();
    showToast(`המודל "${view.displayName}" יובא בהצלחה`, 'ok');
  } catch (e) {
    showToast(`שגיאה בייבוא: ${e}`, 'err');
  }
});

// Active download state per model id
const activeDownloads = {}; // id → { channel, progressEl }

async function refreshModelList() {
  try {
    modelsData = await invoke('list_models');
    renderModelList(modelsData);
    updateNoModelGuidance();
  } catch (e) {
    const list = $('model-list');
    if (list) list.innerHTML = `<p class="model-error">שגיאה בטעינת מודלים: ${e}</p>`;
  }
}

function renderModelList(models) {
  const list = $('model-list');
  if (!list) return;
  if (!models || models.length === 0) {
    list.innerHTML = '<p class="muted">אין מודלים זמינים.</p>';
    return;
  }
  list.innerHTML = '';
  for (const m of models) {
    list.appendChild(buildModelCard(m));
  }
}

function buildModelCard(m) {
  const card = document.createElement('div');
  card.className = 'model-card';
  card.dataset.id = m.id;
  if (m.isActive) card.classList.add('active');

  const sizeMB = m.sizeBytes ? `${Math.round(m.sizeBytes / 1_000_000)} MB` : '';
  const badge = m.source === 'manual' ? '<span class="model-badge">ידני</span>' : '';
  // Quality comes from the backend in English; show it in Hebrew.
  const QUALITY_HE = { high: 'איכות גבוהה', medium: 'איכות בינונית', low: 'איכות בסיסית' };
  const qualityHe = m.quality ? (QUALITY_HE[m.quality] || m.quality) : '';
  // Keep the LTR size+unit ("1620 MB") from being reversed inside the RTL line.
  const metaParts = [];
  if (sizeMB) metaParts.push(`<span dir="ltr">${sizeMB}</span>`);
  if (qualityHe) metaParts.push(qualityHe);

  card.innerHTML = `
    <div class="model-card-info">
      <div class="model-card-name">${m.displayName}${badge}</div>
      <div class="model-card-meta">${metaParts.join(' · ')}</div>
    </div>
    <div class="model-card-actions" id="actions-${m.id}"></div>
    <div class="model-card-progress" id="progress-${m.id}" style="display:none">
      <div class="progress-bar"><div class="progress-fill" id="fill-${m.id}"></div></div>
      <span class="progress-label" id="plabel-${m.id}">0%</span>
    </div>
  `;

  renderModelActions(card, m);
  return card;
}

function renderModelActions(card, m) {
  const actionsEl = card.querySelector(`#actions-${m.id}`);
  if (!actionsEl) return;
  actionsEl.innerHTML = '';

  if (activeDownloads[m.id]) {
    // Already downloading — cancel button
    const cancelBtn = btn('ביטול', 'danger-btn', () => cancelDownload(m.id));
    actionsEl.appendChild(cancelBtn);
    return;
  }

  switch (m.status) {
    case 'not_installed':
      actionsEl.appendChild(btn('הורד', 'primary-sm', () => startDownload(m)));
      break;

    case 'installed':
      if (m.isActive) {
        actionsEl.appendChild(disabledBtn('פעיל ✓'));
        actionsEl.appendChild(btn('בטל פעילות', 'secondary-sm', () => deactivateModel(m.id)));
      } else {
        actionsEl.appendChild(btn('הפעל', 'primary-sm', () => activateModel(m.id)));
        actionsEl.appendChild(btn('מחק', 'danger-sm', () => deleteModel(m.id)));
      }
      break;

    case 'manually_imported':
      if (m.isActive) {
        actionsEl.appendChild(disabledBtn('פעיל ✓'));
        actionsEl.appendChild(btn('בטל פעילות', 'secondary-sm', () => deactivateModel(m.id)));
      } else {
        actionsEl.appendChild(btn('הפעל', 'primary-sm', () => activateModel(m.id)));
        actionsEl.appendChild(btn('מחק', 'danger-sm', () => deleteModel(m.id)));
      }
      break;

    case 'corrupt':
      actionsEl.appendChild(btn('פגום — הורד מחדש', 'danger-sm', () => startDownload(m)));
      break;

    default:
      break;
  }
}

function btn(label, cls, onClick) {
  const b = document.createElement('button');
  b.textContent = label;
  b.className = `model-btn ${cls}`;
  b.addEventListener('click', onClick);
  return b;
}

function disabledBtn(label) {
  const b = document.createElement('button');
  b.textContent = label;
  b.className = 'model-btn active-btn';
  b.disabled = true;
  return b;
}

async function startDownload(m) {
  const progressEl = $(`progress-${m.id}`);
  const fillEl = $(`fill-${m.id}`);
  const labelEl = $(`plabel-${m.id}`);

  if (progressEl) progressEl.style.display = 'flex';
  if (labelEl) labelEl.textContent = 'מתחיל הורדה…';

  const channel = new Channel();
  activeDownloads[m.id] = { channel };

  // Re-render actions to show Cancel
  const card = document.querySelector(`.model-card[data-id="${m.id}"]`);
  if (card) renderModelActions(card, m);

  channel.onmessage = (progress) => {
    const pct =
      progress.totalBytes > 0
        ? Math.round((progress.downloadedBytes / progress.totalBytes) * 100)
        : 0;
    if (fillEl) fillEl.style.width = `${pct}%`;
    if (labelEl) {
      const mbDone = Math.round(progress.downloadedBytes / 1_000_000);
      const mbTotal = Math.round(progress.totalBytes / 1_000_000);
      const kbps = Math.round(progress.speedBps / 1000);
      labelEl.textContent = `${pct}% · ${mbDone}/${mbTotal} MB · ${kbps} KB/s`;
    }
  };

  try {
    await invoke('download_model', { id: m.id, onProgress: channel });
    // download runs in background; model-installed event triggers refresh
  } catch (e) {
    delete activeDownloads[m.id];
    if (progressEl) progressEl.style.display = 'none';
    showToast(`שגיאה בהורדה: ${e}`, 'err');
    await refreshModelList();
  }
}

async function cancelDownload(id) {
  try {
    await invoke('cancel_download', { id });
  } catch (_) {}
  delete activeDownloads[id];
  await refreshModelList();
  const progressEl = $(`progress-${id}`);
  if (progressEl) progressEl.style.display = 'none';
}

async function activateModel(id) {
  try {
    showToast('טוען מודל…', 'ok');
    await invoke('load_local_model', { id });
    await refreshModelList();
    showToast('המודל פעיל ✓', 'ok');
  } catch (e) {
    showToast(`שגיאה בטעינת מודל: ${e}`, 'err');
  }
}

async function deactivateModel(_id) {
  try {
    await invoke('unload_local_model');
    await refreshModelList();
    showToast('המודל הוסר מהזיכרון', 'ok');
  } catch (e) {
    showToast(`שגיאה: ${e}`, 'err');
  }
}

async function deleteModel(id) {
  if (!confirm('למחוק את המודל מהדיסק?')) return;
  try {
    await invoke('delete_model', { id });
    await refreshModelList();
    showToast('המודל נמחק', 'ok');
  } catch (e) {
    showToast(`שגיאה במחיקה: ${e}`, 'err');
  }
}

// Show the real app version everywhere from a single source (tauri.conf.json),
// so the header and the About tab can never drift apart.
getVersion()
  .then((v) => {
    const hv = $('version');
    if (hv) hv.textContent = `v${v}`;
    const av = document.querySelector('.about-version');
    if (av) av.textContent = `גרסה ${v} · Oliel Studio`;
  })
  .catch(() => {});

// ── Initial load ─────────────────────────────────────────────────────────────
await loadSettings();
await refreshModelList();
