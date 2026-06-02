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
    const cx = (window.innerWidth / 2) * dpr; // mic is centered in its window
    const cy = (window.innerHeight / 2) * dpr;
    // Pad to fully CONTAIN the mic's soft drop-shadow / orb glow (box-shadow blur
    // is 24–28px with a fade tail beyond). The clip must land where the glow has
    // decayed to ~0: if the hard circle cuts through the still-visible glow, the
    // step between the shadow-darkened area inside and the clear background outside
    // reads as a faint arc above the mic (obvious on light backgrounds, invisible
    // on dark ones). Cap to just inside the window so the circle never reaches the
    // 160px frame and flatten into a straight edge.
    const half = Math.min(window.innerWidth, window.innerHeight) / 2;
    const rad = Math.min(r + 40, half - 1) * dpr;
    await invoke('set_circle_region', { label: winLabel, cx, cy, r: rad });
  } catch (e) {
    /* ignore */
  }
}

function setState(state) {
  mic.dataset.state = state;
}

// Bubble fits ~3 lines × ~22 Hebrew chars in the 142px-wide bubble. When
// interim text grows past that, surface the most-recent words instead of the
// first ones — that's what the user is currently saying.
const BUBBLE_MAX_CHARS = 70;

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
  }, 1500);
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

// --- File drag-drop onto the mic: document translation + audio transcription ---
const TRANSLATE_EXTS = ['srt', 'vtt', 'sbv', 'txt', 'md', 'markdown', 'docx', 'doc', 'pdf'];
const AUDIO_EXTS = ['mp3', 'wav', 'm4a', 'aac', 'flac', 'ogg', 'oga', 'opus', 'wma', 'webm', 'mp4', 'mpeg', 'mpga'];
const extOf = (p) => {
  const m = /\.([^.\\/]+)$/.exec(p);
  return m ? m[1].toLowerCase() : '';
};
const isTranslatable = (p) => TRANSLATE_EXTS.includes(extOf(p));
const isAudio = (p) => AUDIO_EXTS.includes(extOf(p));

await listen('speakly://translate-progress', (e) => {
  const { batch, total } = e.payload || {};
  if (batch && total) showBubble(`מתרגם… ${batch}/${total}`);
});

await listen('speakly://transcribe-progress', (e) => {
  const { chunk, total } = e.payload || {};
  if (chunk && total > 1) showBubble(`מתמלל… ${chunk}/${total}`);
});

await getCurrentWebview().onDragDropEvent(async (event) => {
  if (event.payload.type !== 'drop' || !event.payload.paths?.length) return;
  const audioPaths = event.payload.paths.filter(isAudio);
  const docPaths = event.payload.paths.filter(isTranslatable);
  if (audioPaths.length === 0 && docPaths.length === 0) {
    showBubble('פורמט לא נתמך');
    return;
  }

  setState('listening');
  let ok = 0;
  let fail = 0;

  if (audioPaths.length) {
    showBubble('מתמלל…');
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
    showBubble('מתרגם…');
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
  document.body.style.opacity = String(stg.mic_opacity ?? 0.95);
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
