import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open } from '@tauri-apps/plugin-dialog';

const body = document.body;
const statusEl = document.getElementById('status');
const topWedge = document.querySelector('.wedge[data-sector="top"]');
// Note: the window hit-region (clipping the transparent area so clicks pass
// through) is applied in Rust (`panel::position_panel`), which knows the exact
// collapsed/expanded geometry — no JS timing races.

let statusTimer = null;
function showStatus(text, hold = 1800) {
  statusEl.textContent = text || '';
  clearTimeout(statusTimer);
  if (text && hold) statusTimer = setTimeout(() => (statusEl.textContent = ''), hold);
}

// ── Expand / collapse ──────────────────────────────────────────────────────
let expanded = false;
async function setExpanded(next) {
  expanded = next;
  body.dataset.expanded = String(expanded);
  try {
    // Rust resizes the window AND re-applies the hit-region for the new state.
    await invoke('set_panel_expanded', { expanded });
  } catch (e) {
    console.warn('set_panel_expanded failed:', e);
  }
}
// Closed half-circle trigger → open.
document.getElementById('trigger').addEventListener('click', () => setExpanded(true));
// Inner dark half-circle control → close.
document.getElementById('close').addEventListener('click', () => setExpanded(false));

// ── Sector actions → existing pipelines ──────────────────────────────────────
// TOP = live transcription, MIDDLE = file translation, BOTTOM = file transcription.
document.getElementById('sector-top').addEventListener('click', toggleLive);
document.getElementById('sector-middle').addEventListener('click', () => pickAndRun('translate'));
document.getElementById('sector-bottom').addEventListener('click', () => pickAndRun('transcribe'));

async function toggleLive() {
  try {
    await invoke('toggle_listening');
  } catch (err) {
    console.warn('toggle_listening failed:', err);
  }
}

await listen('speakly://state-changed', (e) => {
  const state = e.payload || 'idle';
  topWedge?.classList.toggle('listening', state === 'listening');
});

// ── File pipelines (click → native picker, drag → route by extension) ────────
const TRANSLATE_EXTS = ['srt', 'vtt', 'sbv', 'txt', 'md', 'markdown', 'docx', 'doc', 'pdf'];
const AUDIO_EXTS = ['mp3', 'wav', 'm4a', 'aac', 'flac', 'ogg', 'oga', 'opus', 'wma', 'webm', 'mp4', 'mpeg', 'mpga'];
const extOf = (p) => {
  const m = /\.([^.\\/]+)$/.exec(p);
  return m ? m[1].toLowerCase() : '';
};
const isTranslatable = (p) => TRANSLATE_EXTS.includes(extOf(p));
const isAudio = (p) => AUDIO_EXTS.includes(extOf(p));

async function pickAndRun(kind) {
  const filters =
    kind === 'translate'
      ? [{ name: 'מסמכים', extensions: TRANSLATE_EXTS }]
      : [{ name: 'אודיו', extensions: AUDIO_EXTS }];
  let selected;
  try {
    selected = await open({ multiple: false, directory: false, filters });
  } catch (e) {
    console.warn('dialog open failed:', e);
    return;
  }
  if (!selected) return; // user cancelled
  const path = typeof selected === 'string' ? selected : selected.path || selected;
  await runFile(kind === 'translate' ? 'translate_file' : 'transcribe_audio_file', path);
}

async function runFile(command, path) {
  showStatus(command === 'translate_file' ? 'מתרגם…' : 'מתמלל…', 0);
  try {
    await invoke(command, { path });
    showStatus('✓ נשמר');
  } catch (err) {
    console.warn(`${command} failed:`, err);
    showStatus('שגיאה');
  }
}

await getCurrentWebview().onDragDropEvent(async (event) => {
  if (event.payload.type !== 'drop' || !event.payload.paths?.length) return;
  const audioPaths = event.payload.paths.filter(isAudio);
  const docPaths = event.payload.paths.filter(isTranslatable);
  if (audioPaths.length === 0 && docPaths.length === 0) {
    showStatus('פורמט לא נתמך');
    return;
  }
  for (const p of audioPaths) await runFile('transcribe_audio_file', p);
  for (const p of docPaths) await runFile('translate_file', p);
});
