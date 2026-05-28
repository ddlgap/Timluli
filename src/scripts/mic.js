import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, PhysicalPosition } from '@tauri-apps/api/window';
import { getCurrentWebview } from '@tauri-apps/api/webview';

const mic = document.getElementById('mic');
const bubble = document.getElementById('bubble');

let bubbleTimer = null;

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
  clearTimeout(bubbleTimer);
  bubbleTimer = setTimeout(() => bubble.classList.remove('show'), 1500);
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
});

await listen('speakly://interim', (e) => {
  showBubble(e.payload);
});

await listen('speakly://settings-changed', (e) => {
  applySettings(e.payload);
});

// --- Document translation via drag-drop onto the mic ---
const TRANSLATE_EXTS = ['srt', 'vtt', 'sbv', 'txt', 'md', 'markdown', 'docx', 'doc', 'pdf'];
const isTranslatable = (p) => {
  const m = /\.([^.\\/]+)$/.exec(p);
  return !!m && TRANSLATE_EXTS.includes(m[1].toLowerCase());
};

await listen('speakly://translate-progress', (e) => {
  const { batch, total } = e.payload || {};
  if (batch && total) showBubble(`מתרגם… ${batch}/${total}`);
});

await getCurrentWebview().onDragDropEvent(async (event) => {
  if (event.payload.type !== 'drop' || !event.payload.paths?.length) return;
  const paths = event.payload.paths.filter(isTranslatable);
  if (paths.length === 0) {
    showBubble('פורמט לא נתמך');
    return;
  }
  setState('listening');
  showBubble('מתרגם…');
  let ok = 0;
  let fail = 0;
  for (const p of paths) {
    try {
      await invoke('translate_file', { path: p });
      ok++;
    } catch (err) {
      fail++;
      console.warn('translate_file failed:', err);
    }
  }
  if (fail === 0) {
    setState('idle');
    showBubble('✓ נשמר');
  } else {
    setState('error');
    showBubble(ok > 0 ? `✓ ${ok} · ✗ ${fail}` : 'שגיאה בתרגום');
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
}

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
  setTimeout(() => {
    document.addEventListener('click', removeContextMenu, { once: true });
  }, 0);
}

function removeContextMenu() {
  const m = document.getElementById('__ctxmenu');
  if (m) m.remove();
}
