import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, PhysicalPosition } from '@tauri-apps/api/window';
import { getCurrentWebview } from '@tauri-apps/api/webview';

const mic = document.getElementById('mic');
const bubble = document.getElementById('bubble');

let bubbleTimer = null;

const winLabel = getCurrentWindow().label;

// Clip the (otherwise transparent) window down to just the mic circle so clicks
// around it pass through to the desktop. While the interim-text bubble or the
// context menu is showing we need the whole window, so the region is cleared.
async function updateMicRegion() {
  try {
    const bubbleUp = bubble.classList.contains('show');
    const menuUp = !!document.getElementById('__ctxmenu');
    if (bubbleUp || menuUp) {
      await invoke('clear_hit_region', { label: winLabel });
      return;
    }
    const dpr = window.devicePixelRatio || 1;
    const r = mic.offsetWidth / 2; // layout size, ignores hover transform
    if (r <= 0) return;
    // The mic is centered in its window (body margin:0 + grid place-items:center),
    // and SetWindowRgn coords are window-relative, so the window centre IS the mic
    // centre — use it directly (getBoundingClientRect would add the client/non-client
    // offset and shift the clip off the disc).
    const cx = (window.innerWidth / 2) * dpr;
    const cy = (window.innerHeight / 2) * dpr;
    // The webview backing is forced transparent (see lib.rs set_background_color),
    // so empty pixels are truly see-through. That lets the clip sit out in empty
    // space: the mic's own anti-aliased CSS circle (+ soft shadow) is the visible,
    // smooth edge, while SetWindowRgn's 1-bit jagged edge hides in transparent space.
    const rad = (r + 26) * dpr;
    await invoke('set_circle_region', { label: winLabel, cx, cy, r: rad });
  } catch (e) {
    /* ignore */
  }
}

function setState(state) {
  mic.dataset.state = state;
}

// Bubble fits ~3 lines × ~26 Hebrew chars in the 200px-wide bubble. When
// interim text grows past that, surface the most-recent words instead of the
// first ones — that's what the user is currently saying.
const BUBBLE_MAX_CHARS = 78;

function truncateForBubble(text) {
  if (!text || text.length <= BUBBLE_MAX_CHARS) return text;
  const tail = text.slice(text.length - BUBBLE_MAX_CHARS);
  const firstSpace = tail.indexOf(' ');
  const aligned = firstSpace >= 0 ? tail.slice(firstSpace + 1) : tail;
  return '… ' + aligned;
}

function showBubble(text) {
  if (!text) return;
  bubble.textContent = truncateForBubble(text);
  bubble.classList.add('show');
  updateMicRegion(); // bubble needs the full window
  clearTimeout(bubbleTimer);
  bubbleTimer = setTimeout(() => {
    bubble.classList.remove('show');
    updateMicRegion(); // re-clip to the circle
  }, 2500);
}

// Sticky status for file operations (transcription / subtitles / translation):
// stays visible — NO auto-hide — so the user sees the current stage during long
// processing (e.g. local CPU transcription that can take ~a minute), until the
// operation finishes and a transient ✓/error (via showBubble) replaces it. Distinct
// from showBubble, which auto-hides after 2.5s and is used for live interim text.
function showProgress(text) {
  if (!text) return;
  clearTimeout(bubbleTimer);
  bubble.textContent = truncateForBubble(text);
  bubble.classList.add('show');
  updateMicRegion();
}

mic.addEventListener('click', async (event) => {
  // Drag area swallows mousedown; click still fires when no drag actually happened.
  event.preventDefault();
  try {
    await invoke('toggle_listening');
  } catch (err) {
    console.warn('toggle_listening failed:', err);
    setState('error');
  }
});

mic.addEventListener('contextmenu', async (event) => {
  event.preventDefault();
  showContextMenu(event.clientX, event.clientY);
});

await listen('speakly://state-changed', (e) => {
  setState(e.payload || 'idle');
  if (e.payload !== 'listening') bubble.classList.remove('show');
  updateMicRegion();
});

await listen('speakly://interim', (e) => {
  showBubble(e.payload);
});

await listen('speakly://settings-changed', (e) => {
  applySettings(e.payload);
});

// --- File drag-drop onto the mic: video subtitles + document translation + audio transcription ---
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
// Read fresh each drop (cheap, infrequent); default-on if the field is unset.
async function videoSubtitlesEnabled() {
  try {
    const stg = await invoke('get_settings');
    return stg?.video_subtitles_enabled !== false;
  } catch {
    return true;
  }
}

await listen('speakly://translate-progress', (e) => {
  const { batch, total } = e.payload || {};
  if (batch && total) showProgress(total > 1 ? `מתרגם… ${batch}/${total}` : 'מתרגם…');
});

await listen('speakly://transcribe-progress', (e) => {
  const { chunk, total, phase } = e.payload || {};
  if (phase === 'extract') {
    showProgress('מחלץ אודיו מהסרטון…');
    return;
  }
  // Video subtitles emit phase="transcribe"; plain audio→txt has no phase.
  const label = phase === 'transcribe' ? 'יוצר כתוביות' : 'מתמלל';
  showProgress(chunk && total > 1 ? `${label}… ${chunk}/${total}` : `${label}…`);
});

await getCurrentWebview().onDragDropEvent(async (event) => {
  if (event.payload.type !== 'drop' || !event.payload.paths?.length) return;
  // Classify with video precedence (some video exts also live in AUDIO_EXTS).
  const videoOn = await videoSubtitlesEnabled();
  const videoPaths = [];
  const audioPaths = [];
  const docPaths = [];
  for (const p of event.payload.paths) {
    if (videoOn && isVideo(p)) videoPaths.push(p);
    else if (isAudio(p)) audioPaths.push(p);
    else if (isTranslatable(p)) docPaths.push(p);
  }
  if (!videoPaths.length && !audioPaths.length && !docPaths.length) {
    showBubble('פורמט לא נתמך');
    return;
  }

  setState('listening');
  let ok = 0;
  let fail = 0;

  if (videoPaths.length) {
    showProgress('מחלץ אודיו מהסרטון…');
    for (const p of videoPaths) {
      try {
        await invoke('transcribe_video_to_srt', { path: p });
        ok++;
      } catch (err) {
        fail++;
        console.warn('transcribe_video_to_srt failed:', err);
      }
    }
  }

  if (audioPaths.length) {
    showProgress('מתמלל…');
    for (const p of audioPaths) {
      try {
        await invoke('transcribe_audio_file', { path: p });
        ok++;
      } catch (err) {
        fail++;
        console.warn('transcribe_audio_file failed:', err);
      }
    }
  }

  if (docPaths.length) {
    showProgress('מתרגם…');
    for (const p of docPaths) {
      try {
        await invoke('translate_file', { path: p });
        ok++;
      } catch (err) {
        fail++;
        console.warn('translate_file failed:', err);
      }
    }
  }

  if (fail === 0) {
    setState('idle');
    showBubble('✓ נשמר');
  } else {
    setState('error');
    showBubble(ok > 0 ? `✓ ${ok} · ✗ ${fail}` : 'שגיאה');
    setTimeout(() => setState('idle'), 1500);
  }
});

// On startup, fetch current settings to apply size/opacity.
try {
  const stg = await invoke('get_settings');
  applySettings(stg);
} catch (e) { /* ignore */ }

function applySettings(stg) {
  if (!stg) return;
  document.body.dataset.size = stg.mic_size || 'medium';
  // Opacity MUST live on the mic element, not the body: body-level opacity forces
  // the whole page into a single compositing layer whose empty pixels WebView2
  // paints as opaque white, which the window region then exposes as a white arc /
  // dome above the mic. Applied to the element, the body's empty area stays truly
  // transparent and the float looks clean.
  document.body.style.opacity = '';
  mic.style.opacity = String(stg.mic_opacity ?? 0.95);
  mic.dataset.theme = stg.mic_theme || 'graphite';
  // Mic diameter may have changed → re-clip the window region.
  updateMicRegion();
}

// DPI changes (moving between monitors) alter physical sizes — re-clip.
window.addEventListener('resize', () => updateMicRegion());

// Persist position when window is moved by the user (drag region handles drag).
const win = getCurrentWindow();
let saveTimer = null;
function schedulePersistPosition() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      const pos = await win.outerPosition();
      await invoke('store_mic_position', { x: pos.x, y: pos.y });
    } catch (e) { /* ignore */ }
  }, 400);
}
await win.onMoved(() => schedulePersistPosition());

// --- Lightweight context menu ---
function showContextMenu(x, y) {
  removeContextMenu();
  const menu = document.createElement('div');
  menu.id = '__ctxmenu';
  Object.assign(menu.style, {
    position: 'fixed',
    top: `${y}px`,
    right: `auto`,
    left: `${x}px`,
    background: '#1b1f27',
    border: '1px solid #2e3542',
    borderRadius: '8px',
    padding: '4px',
    minWidth: '160px',
    zIndex: 9999,
    boxShadow: '0 6px 20px rgba(0,0,0,0.5)',
    fontSize: '13px',
    direction: 'rtl',
  });
  const items = [
    { label: 'הגדרות', action: () => invoke('open_settings') },
    { label: 'השתק / הפעל', action: () => invoke('toggle_mute') },
    { label: 'הסתר מיקרופון', action: () => invoke('set_mic_visible', { visible: false }) },
    { label: 'יציאה', action: () => invoke('quit_app'), danger: true },
  ];
  for (const it of items) {
    const el = document.createElement('div');
    el.textContent = it.label;
    Object.assign(el.style, {
      padding: '8px 12px',
      borderRadius: '6px',
      cursor: 'pointer',
      color: it.danger ? '#ef4444' : '#ecedee',
    });
    el.addEventListener('mouseenter', () => (el.style.background = '#242a35'));
    el.addEventListener('mouseleave', () => (el.style.background = 'transparent'));
    el.addEventListener('click', async () => {
      removeContextMenu();
      try { await it.action(); } catch (e) { console.warn(e); }
    });
    menu.appendChild(el);
  }
  document.body.appendChild(menu);
  updateMicRegion(); // menu extends beyond the circle → need the full window
  setTimeout(() => {
    document.addEventListener('click', removeContextMenu, { once: true });
  }, 0);
}

function removeContextMenu() {
  const m = document.getElementById('__ctxmenu');
  if (m) m.remove();
  updateMicRegion(); // re-clip to the circle
}
