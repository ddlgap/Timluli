import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

// Major target languages for the "transcribe + translate" mode. Value = the
// English language NAME the translation prompt interpolates ("…into Hebrew");
// label = Hebrew display. Hebrew is the default (gender-aware translation).
const LANGS = [
  ['Hebrew', 'עברית'],
  ['English', 'אנגלית'],
  ['Arabic', 'ערבית'],
  ['Russian', 'רוסית'],
  ['Spanish', 'ספרדית'],
  ['French', 'צרפתית'],
  ['German', 'גרמנית'],
  ['Portuguese', 'פורטוגזית'],
  ['Italian', 'איטלקית'],
  ['Hindi', 'הינדי'],
  ['Chinese', 'סינית'],
  ['Japanese', 'יפנית'],
  ['Korean', 'קוריאנית'],
  ['Turkish', 'טורקית'],
  ['Ukrainian', 'אוקראינית'],
  ['Polish', 'פולנית'],
  ['Dutch', 'הולנדית'],
];

const sel = document.getElementById('lang');
for (const [val, label] of LANGS) {
  const o = document.createElement('option');
  o.value = val;
  o.textContent = label;
  if (val === 'Hebrew') o.selected = true;
  sel.appendChild(o);
}

const fileEl = document.getElementById('file');
const statusEl = document.getElementById('status');
const startBtn = document.getElementById('start');
const cancelBtn = document.getElementById('cancel');

const baseName = (p) => p.split(/[\\/]/).pop();
let paths = [];

async function loadPending() {
  try {
    paths = (await invoke('get_pending_video')) || [];
  } catch {
    paths = []; // running outside Tauri (visual preview) — keep the UI usable
  }
  if (paths.length === 1) {
    fileEl.textContent = baseName(paths[0]);
    fileEl.title = paths[0];
  } else if (paths.length > 1) {
    fileEl.textContent = `${paths.length} סרטונים`;
    fileEl.title = paths.map(baseName).join('\n');
  } else {
    fileEl.textContent = '';
  }
}
loadPending();

// Re-pull pending paths every time the window is shown (it is reused across drops).
// Guarded: outside Tauri (visual preview) this API is absent — must not abort the
// rest of the script (the option/button listeners below).
try {
  getCurrentWindow().onFocusChanged(({ payload }) => {
    if (payload) {
      resetUi();
      loadPending();
    }
  });
} catch { /* not running inside Tauri */ }

// Card selection visuals follow the radio (the <label> toggles the radio natively).
document.querySelectorAll('input[name="mode"]').forEach((r) => {
  r.addEventListener('change', () => {
    document
      .querySelectorAll('.opt')
      .forEach((o) => o.classList.toggle('selected', o.dataset.mode === r.value));
  });
});

function chosenMode() {
  return document.querySelector('input[name="mode"]:checked').value;
}

function resetUi() {
  document.body.dataset.working = 'false';
  startBtn.disabled = false;
  cancelBtn.disabled = false;
  statusEl.textContent = '';
  statusEl.className = 'status';
}

async function close() {
  resetUi();
  try {
    await invoke('close_video_options');
  } catch {
    try { await getCurrentWindow().hide(); } catch { /* preview */ }
  }
}

cancelBtn.addEventListener('click', close);
document.getElementById('close').addEventListener('click', close);
window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') close();
  if (e.key === 'Enter' && !startBtn.disabled) startBtn.click();
});

startBtn.addEventListener('click', async () => {
  // This window is reused across drops and re-pulls paths on focus; if that event
  // was missed, paths may be empty. Re-pull once before giving up — otherwise we'd
  // close without invoking anything, leaving the mic frozen on "בחר פעולה".
  if (!paths.length) {
    await loadPending();
  }
  if (!paths.length) {
    close();
    return;
  }
  const mode = chosenMode();
  const target = sel.value;

  document.body.dataset.working = 'true';
  startBtn.disabled = true;
  cancelBtn.disabled = true;
  statusEl.className = 'status';
  statusEl.textContent =
    mode === 'translate' ? 'מתמלל ומתרגם… זה עשוי לקחת זמן' : 'יוצר כתוביות… זה עשוי לקחת זמן';

  let ok = 0;
  let fail = 0;
  for (const p of paths) {
    try {
      if (mode === 'translate') {
        await invoke('transcribe_and_translate_video', { path: p, targetLanguage: target });
      } else {
        await invoke('transcribe_video_to_srt', { path: p, lang: 'auto' });
      }
      ok += 1;
    } catch (err) {
      fail += 1;
      console.warn('video op failed:', err);
      statusEl.className = 'status error';
      statusEl.textContent = 'שגיאה: ' + (err?.toString?.() || 'נכשל');
    }
  }

  // Only a real, completed op is "saved" — never report success when nothing ran
  // (ok===0 && fail===0), which would otherwise show "✓ נשמר" with no output files.
  if (ok > 0 && fail === 0) {
    statusEl.className = 'status ok';
    statusEl.textContent = ok > 1 ? `✓ נשמרו ${ok} קבצים` : '✓ נשמר';
    setTimeout(close, 1200);
  } else {
    document.body.dataset.working = 'false';
    startBtn.disabled = false;
    cancelBtn.disabled = false;
    if (ok === 0 && fail === 0) {
      statusEl.className = 'status error';
      statusEl.textContent = 'לא נמצא קובץ לעיבוד — גרור את הסרטון שוב';
    }
  }
});
