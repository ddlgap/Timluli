import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Channel } from '@tauri-apps/api/core';

const $ = (id) => document.getElementById(id);

const tabs = document.querySelectorAll('nav.tabs button');
const sections = document.querySelectorAll('main .section');

tabs.forEach((tab) => {
  tab.addEventListener('click', () => {
    tabs.forEach((t) => t.classList.remove('active'));
    sections.forEach((s) => s.classList.remove('active'));
    tab.classList.add('active');
    document.getElementById(tab.dataset.tab).classList.add('active');
  });
});

if (location.hash === '#about') document.querySelector('[data-tab="about"]').click();
if (location.hash === '#shortcut') document.querySelector('[data-tab="shortcut"]').click();
if (location.hash === '#engine') document.querySelector('[data-tab="engine"]').click();
if (location.hash === '#translation') document.querySelector('[data-tab="translation"]').click();

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
  endRecording(false);
});

// ---- Toggle widgets ----
document.querySelectorAll('.toggle').forEach((el) => {
  const checkbox = el.querySelector('input');
  el.addEventListener('click', () => {
    checkbox.checked = !checkbox.checked;
    el.classList.toggle('on', checkbox.checked);
  });
});

function setToggle(key, value) {
  const el = document.querySelector(`.toggle[data-key="${key}"]`);
  if (!el) return;
  el.classList.toggle('on', !!value);
  const cb = el.querySelector('input');
  if (cb) cb.checked = !!value;
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
  setToggle('mute_during_fullscreen', stg.mute_during_fullscreen);
  setToggle('mute_during_calls', stg.mute_during_calls);
  shortcutInput.textContent = stg.shortcut || 'Ctrl+Super+Space';
  $('activation_mode').value = stg.activation_mode || 'toggle';
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

  // Theme swatches
  setSelectedTheme(stg.mic_theme || 'graphite');

  // Translation tab
  $('translate_target_language').value = stg.translate_target_language || 'Hebrew';
  await refreshKeyStatus();

  return stg;
}

async function refreshKeyStatus() {
  try {
    const status = await invoke('get_translation_keys_status');
    const g = $('groq_saved');
    const c = $('cerebras_saved');
    if (g) g.style.display = status.groq_set ? 'inline' : 'none';
    if (c) c.style.display = status.cerebras_set ? 'inline' : 'none';
    if (status.groq_set) $('groq_api_key').placeholder = '•••••••••• (מפתח שמור)';
    if (status.cerebras_set) $('cerebras_api_key').placeholder = '•••••••••• (מפתח שמור)';
  } catch (e) {
    /* ignore */
  }
}

function setSelectedTheme(theme) {
  document.querySelectorAll('.theme-swatch').forEach((el) => {
    el.classList.toggle('selected', el.dataset.theme === theme);
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

// ---- Save ----
async function saveSettings() {
  const saveBtn = $('save');
  const originalLabel = saveBtn.textContent;
  saveBtn.disabled = true;
  saveBtn.textContent = 'שומר...';

  const previous = await invoke('get_settings');
  const newSettings = {
    ...previous,
    language: $('language').value,
    shortcut: shortcutInput.textContent,
    activation_mode: $('activation_mode').value,
    mic_size: $('mic_size').value,
    mic_opacity: Number(opacity.value) / 100,
    start_with_windows: getToggle('start_with_windows'),
    show_mic_on_startup: getToggle('show_mic_on_startup'),
    mute_during_fullscreen: getToggle('mute_during_fullscreen'),
    mute_during_calls: getToggle('mute_during_calls'),
    silence_timeout_ms: Number(silence.value),
    translate_target_language: $('translate_target_language').value,
    // engine_id is saved immediately on radio change, not on Save button
  };

  try {
    if (newSettings.shortcut !== previous.shortcut) {
      await invoke('update_shortcut', { combo: newSettings.shortcut });
    }
    if (newSettings.start_with_windows !== previous.start_with_windows) {
      await invoke('set_autostart_enabled', { enabled: newSettings.start_with_windows });
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
    }

    saveBtn.textContent = '✓ נשמר';
    saveBtn.classList.add('saved');
    showToast('הגדרות נשמרו בהצלחה', 'ok');
    setTimeout(() => {
      saveBtn.textContent = originalLabel;
      saveBtn.classList.remove('saved');
      saveBtn.disabled = false;
    }, 1500);
  } catch (err) {
    saveBtn.textContent = originalLabel;
    saveBtn.disabled = false;
    showToast(`שגיאה בשמירה: ${err}`, 'err');
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

$('save').addEventListener('click', saveSettings);
$('reset').addEventListener('click', async () => {
  if (!confirm('לשחזר הגדרות ברירת מחדל?')) return;
  await invoke('save_settings', {
    newSettings: {
      language: 'he-IL',
      shortcut: 'Ctrl+Super+Space',
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
  guidance.style.display =
    currentEngineId === 'whisper-local' && !hasInstalled ? 'flex' : 'none';
}

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

// Manual import button — uses prompt() for file path input.
// A full file-picker dialog requires tauri-plugin-dialog (future enhancement).
$('btn-import-manual')?.addEventListener('click', async () => {
  const filePath = prompt(
    'הכנס נתיב מלא לקובץ המודל (.bin או .gguf):\nדוגמה: C:\\Users\\User\\Downloads\\ggml-model-q5_0.bin'
  );
  if (!filePath || !filePath.trim()) return;
  const displayName = prompt('שם מותאם אישית למודל:', 'מודל ידני') || 'מודל ידני';
  try {
    showToast('מייבא מודל…', 'ok');
    const view = await invoke('import_model_manual', {
      filePath: filePath.trim(),
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

  card.innerHTML = `
    <div class="model-card-info">
      <div class="model-card-name">${m.displayName}${badge}</div>
      <div class="model-card-meta">${sizeMB}${m.quality ? ` · ${m.quality}` : ''}</div>
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

// ── Initial load ─────────────────────────────────────────────────────────────
await loadSettings();
await refreshModelList();
