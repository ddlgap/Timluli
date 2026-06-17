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
document.getElementById('sector-burn').addEventListener('click', () => pickBurn());

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
// Video containers → <stem>.srt (when video_subtitles_enabled). Checked BEFORE
// AUDIO_EXTS, so mp4/webm/mpeg (which also appear there) route to subtitles; with the
// setting off the video branch is skipped and they fall through to the audio path.
const VIDEO_EXTS = ['mp4', 'mkv', 'mov', 'webm', 'avi', 'm4v', 'mpg', 'mpeg', 'wmv', 'flv'];
const extOf = (p) => {
  const m = /\.([^.\\/]+)$/.exec(p);
  return m ? m[1].toLowerCase() : '';
};
const isTranslatable = (p) => TRANSLATE_EXTS.includes(extOf(p));
const isAudio = (p) => AUDIO_EXTS.includes(extOf(p));
const isVideo = (p) => VIDEO_EXTS.includes(extOf(p));
const isSrt = (p) => extOf(p) === 'srt';
// Exactly one video + one SRT dropped together = a burn-in pair. Must be checked
// BEFORE extension classification: a lone .srt routes to translation, but next to
// a video it means "burn these subtitles onto it".
function asBurnPair(paths) {
  if (paths.length !== 2) return null;
  const [a, b] = paths;
  if (isVideo(a) && isSrt(b)) return { video: a, srt: b };
  if (isVideo(b) && isSrt(a)) return { video: b, srt: a };
  return null;
}
async function videoSubtitlesEnabled() {
  try {
    const stg = await invoke('get_settings');
    return stg?.video_subtitles_enabled !== false;
  } catch {
    return true;
  }
}

async function pickAndRun(kind) {
  const videoOn = kind === 'transcribe' && (await videoSubtitlesEnabled());
  const transcribeExts = videoOn ? [...new Set([...AUDIO_EXTS, ...VIDEO_EXTS])] : AUDIO_EXTS;
  const filters =
    kind === 'translate'
      ? [{ name: 'מסמכים', extensions: TRANSLATE_EXTS }]
      : [{ name: videoOn ? 'אודיו ווידאו' : 'אודיו', extensions: transcribeExts }];
  let selected;
  try {
    selected = await open({ multiple: false, directory: false, filters });
  } catch (e) {
    console.warn('dialog open failed:', e);
    return;
  }
  if (!selected) return; // user cancelled
  const path = typeof selected === 'string' ? selected : selected.path || selected;
  // A picked video goes through the chooser (transcribe only / + translate), same
  // as a drop; audio and documents run straight through.
  if (videoOn && isVideo(path)) {
    chooserActive = true;
    showStatus('בחר פעולה…', { busy: true });
    try {
      await invoke('show_video_options', { paths: [path] });
    } catch (e) {
      chooserActive = false;
      console.warn('show_video_options failed:', e);
    }
    return;
  }
  const command = kind === 'translate' ? 'translate_file' : 'transcribe_audio_file';
  await runFile(command, path);
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

async function runBurn(videoPath, srtPath) {
  showStatus('צורב כתוביות…', { busy: true });
  try {
    const outcome = await invoke('burn_subtitles', { videoPath, srtPath });
    // degraded = karaoke requested but no usable word timings → box style.
    showStatus(outcome?.degraded ? 'נשמר בסגנון קופסה (אין תזמוני מילים)' : '✓ נשמר');
  } catch (err) {
    console.warn('burn_subtitles failed:', err);
    showStatus('שגיאה');
  }
}

// Two-step native picker for the burn wedge: the video, then its SRT.
async function pickBurn() {
  const pickOne = async (filters) => {
    let sel;
    try {
      sel = await open({ multiple: false, directory: false, filters });
    } catch (e) {
      console.warn('dialog open failed:', e);
      return null;
    }
    if (!sel) return null; // user cancelled
    return typeof sel === 'string' ? sel : sel.path || sel;
  };
  const video = await pickOne([{ name: 'וידאו', extensions: VIDEO_EXTS }]);
  if (!video) return;
  const srt = await pickOne([{ name: 'כתוביות SRT', extensions: ['srt'] }]);
  if (!srt) return;
  await runBurn(video, srt);
}

// Live progress from the backend (translation batches / transcription chunks) →
// dynamic "מתרגם… 3/12" status, mirroring the floating mic's bubble.
await listen('speakly://translate-progress', (e) => {
  const { batch, total } = e.payload || {};
  if (batch && total) showStatus(total > 1 ? `מתרגם… ${batch}/${total}` : 'מתרגם…', { busy: true });
});
await listen('speakly://transcribe-progress', (e) => {
  const { chunk, total, phase } = e.payload || {};
  if (phase === 'extract') {
    showStatus('מחלץ אודיו מהסרטון…', { busy: true });
    return;
  }
  // Video subtitles emit phase="transcribe"; plain audio→txt has no phase.
  const label = phase === 'transcribe' ? 'יוצר כתוביות' : 'מתמלל';
  showStatus(chunk && total > 1 ? `${label}… ${chunk}/${total}` : `${label}…`, { busy: true });
});

// Burn-in re-encodes the whole video, so a live percent (not chunk counts).
await listen('speakly://burn-progress', (e) => {
  const { percent } = e.payload || {};
  showStatus(percent ? `צורב כתוביות… ${percent}%` : 'צורב כתוביות…', { busy: true });
});

// A video drop hands off to the chooser window, which drives the backend; the panel
// only mirrors progress (above) and the final result here. `chooserActive` gates
// these so they never collide with the inline audio/doc path's own status updates.
let chooserActive = false;
const onChooserDone = () => {
  if (!chooserActive) return;
  chooserActive = false;
  showStatus('✓ נשמר');
};
const onChooserError = () => {
  if (!chooserActive) return;
  chooserActive = false;
  showStatus('שגיאה');
};
await listen('speakly://transcribe-done', onChooserDone);
await listen('speakly://translate-done', onChooserDone);
await listen('speakly://transcribe-error', onChooserError);
await listen('speakly://translate-error', onChooserError);

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
  chooserActive = false;

  const paths = event.payload.paths || [];
  // Video + SRT pair → subtitle burn-in (style from settings).
  const pair = asBurnPair(paths);
  if (pair) {
    await runBurn(pair.video, pair.srt);
    return;
  }
  // Classify with video precedence (some video exts also live in AUDIO_EXTS).
  const videoOn = await videoSubtitlesEnabled();
  const videoPaths = [];
  const audioPaths = [];
  const docPaths = [];
  for (const p of paths) {
    if (videoOn && isVideo(p)) videoPaths.push(p);
    else if (isAudio(p)) audioPaths.push(p);
    else if (isTranslatable(p)) docPaths.push(p);
  }
  if (!videoPaths.length && !audioPaths.length && !docPaths.length) {
    showStatus('פורמט לא נתמך');
    return;
  }
  // Video → open the chooser (transcribe only / transcribe + translate).
  if (videoPaths.length) {
    chooserActive = true;
    showStatus('בחר פעולה…', { busy: true });
    try {
      await invoke('show_video_options', { paths: videoPaths });
    } catch (e) {
      chooserActive = false;
      console.warn('show_video_options failed:', e);
    }
    return;
  }
  for (const p of audioPaths) await runFile('transcribe_audio_file', p);
  for (const p of docPaths) await runFile('translate_file', p);
}
await getCurrentWebview().onDragDropEvent(handleDrag);
