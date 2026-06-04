import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open } from '@tauri-apps/plugin-dialog';

const body = document.body;
const statusEl = document.getElementById('status');
const statusText = statusEl.querySelector('.status-text');
const topWedge = document.querySelector('.wedge[data-sector="top"]');
const sectorTop = document.getElementById('sector-top');
// Note: the window hit-region (clipping the transparent area so clicks pass
// through) is applied in Rust (`panel::position_panel`), which knows the exact
// collapsed/expanded geometry — no JS timing races.

// ── Status line (spinner + text) ────────────────────────────────────────────
// `busy:true` keeps the message + spinner up until replaced (used while a file op
// runs); transient messages (done/error) auto-clear after `hold` ms.
let statusTimer = null;
function showStatus(text, { busy = false, hold = 1800 } = {}) {
  statusText.textContent = text || '';
  statusEl.classList.toggle('busy', busy);
  clearTimeout(statusTimer);
  if (text && hold && !busy) {
    statusTimer = setTimeout(() => {
      statusText.textContent = '';
      statusEl.classList.remove('busy');
    }, hold);
  }
}

// ── Expand / collapse ──────────────────────────────────────────────────────
let expanded = false;
// True while live dictation is active. Drives the collapsed-strip recording
// indicator and turns the strip into a one-click stop (see the trigger handler).
let recording = false;
async function setExpanded(next) {
  if (expanded === next) return;
  expanded = next;
  body.dataset.expanded = String(expanded);
  try {
    // Rust resizes the window AND re-applies the hit-region for the new state.
    await invoke('set_panel_expanded', { expanded });
  } catch (e) {
    console.warn('set_panel_expanded failed:', e);
  }
}
// Closed half-circle trigger → open the radial menu; but while dictating, the
// collapsed strip doubles as a one-click STOP (the pulsing red indicator signals
// it). The inner control always → close.
const triggerEl = document.getElementById('trigger');
triggerEl.addEventListener('click', () => {
  if (recording) toggleLive(); // stop the live dictation
  else setExpanded(true);      // open the menu
});
document.getElementById('close').addEventListener('click', () => setExpanded(false));

// Onboarding-only "peek": briefly auto-open, then auto-close after 4s, so first-run
// users instantly see what the side panel does — the same immediate feedback the
// floating mic gets by simply being visible. The onboarding wizard emits this when
// the user picks the side-panel display mode; nothing else fires this event.
let peekTimer = null;
await listen('speakly://panel-peek', async () => {
  clearTimeout(peekTimer);
  await setExpanded(true);
  peekTimer = setTimeout(() => setExpanded(false), 4000);
});

// ── Sector actions → existing pipelines ──────────────────────────────────────
// TOP = live transcription, MIDDLE = file translation, BOTTOM = file transcription.
sectorTop.addEventListener('click', toggleLive);
document.getElementById('sector-middle').addEventListener('click', () => pickAndRun('translate'));
document.getElementById('sector-bottom').addEventListener('click', () => pickAndRun('transcribe'));

async function toggleLive() {
  try {
    await invoke('toggle_listening');
  } catch (err) {
    console.warn('toggle_listening failed:', err);
  }
}

// Live-transcription indicator: light up the top wedge + sector while recording.
await listen('speakly://state-changed', (e) => {
  const listening = (e.payload || 'idle') === 'listening';
  topWedge?.classList.toggle('listening', listening);
  sectorTop?.classList.toggle('listening', listening);
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
  const translating = command === 'translate_file';
  showStatus(translating ? 'מתרגם…' : 'מתמלל…', { busy: true });
  try {
    await invoke(command, { path });
    showStatus('✓ נשמר');
  } catch (err) {
    console.warn(`${command} failed:`, err);
    showStatus('שגיאה');
  }
}

// Live progress from the backend (translation batches / transcription chunks) →
// dynamic "מתרגם… 3/12" status, mirroring the floating mic's bubble.
await listen('speakly://translate-progress', (e) => {
  const { batch, total } = e.payload || {};
  if (batch && total) showStatus(`מתרגם… ${batch}/${total}`, { busy: true });
});
await listen('speakly://transcribe-progress', (e) => {
  const { chunk, total } = e.payload || {};
  if (chunk && total > 1) showStatus(`מתמלל… ${chunk}/${total}`, { busy: true });
});

// ── Drag-and-drop onto the panel ─────────────────────────────────────────────
// The collapsed strip is a tiny target, so when a file is dragged over the panel
// we auto-open the radial menu (a big, obvious drop zone) and highlight the file
// wedges. On drop we route by extension into the same pipelines as a click. If we
// only opened *for* the drag (it wasn't already open), collapse back on a bare
// leave — but keep it open after an actual drop so progress/result stays visible.
let openedForDrag = false;
async function handleDrag(event) {
  const type = event.payload?.type;

  if (type === 'enter' || type === 'over') {
    if (!expanded) {
      openedForDrag = true;
      await setExpanded(true);
    }
    body.classList.add('drag-over');
    return;
  }

  if (type === 'leave') {
    body.classList.remove('drag-over');
    if (openedForDrag) {
      openedForDrag = false;
      setExpanded(false);
    }
    return;
  }

  if (type !== 'drop') return;
  body.classList.remove('drag-over');
  openedForDrag = false; // keep the menu open to show progress + result

  const paths = event.payload.paths || [];
  const audioPaths = paths.filter(isAudio);
  const docPaths = paths.filter(isTranslatable);
  if (audioPaths.length === 0 && docPaths.length === 0) {
    showStatus('פורמט לא נתמך');
    return;
  }
  for (const p of audioPaths) await runFile('transcribe_audio_file', p);
  for (const p of docPaths) await runFile('translate_file', p);
}
await getCurrentWebview().onDragDropEvent(handleDrag);
