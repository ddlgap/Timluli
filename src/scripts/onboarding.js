import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

const win = getCurrentWindow();
const TOTAL_STEPS = 5;

// Custom title-bar close (decorations:false). Closing hides the window (lib.rs
// intercepts CloseRequested); onboarding_done stays unset, so it reappears next
// launch — a gentle nudge to finish setup.
document.getElementById('tb-close')?.addEventListener('click', () => win.close());

const THEMES = [
  { id: 'graphite',   label: 'גרפיט' },
  { id: 'crimson',    label: 'אדום' },
  { id: 'azure',      label: 'כחול' },
  { id: 'emerald',    label: 'ירוק' },
  { id: 'sunset',     label: 'שקיעה' },
  { id: 'ocean',      label: 'אוקיינוס' },
  { id: 'violet',     label: 'סגול' },
  { id: 'orb-plasma', label: 'פלזמה' },
  { id: 'orb-aurora', label: 'זוהר הצפון' },
];

const MIC_SVG = `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 14a3 3 0 0 0 3-3V6a3 3 0 1 0-6 0v5a3 3 0 0 0 3 3zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11h-2z"/></svg>`;

const DT_MODS = ['Ctrl', 'Alt', 'Shift', 'Win'];

// "Ctrl+Ctrl" → "Ctrl"; any real chord → null. (Mirrors settings.js.)
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

// ── Live state (kept in sync so the final save never clobbers a live choice) ──
let currentSettings = {};
const state = {
  engine: 'web-speech',
  display: 'side-panel',
  theme: 'graphite',
  shortcut: 'Ctrl+Ctrl',
};
let initialShortcut = 'Ctrl+Ctrl';

try {
  currentSettings = await invoke('get_settings');
  state.engine = currentSettings.engine_id || 'web-speech';
  state.display = currentSettings.display_mode || 'side-panel';
  state.theme = currentSettings.mic_theme || 'graphite';
  state.shortcut = currentSettings.shortcut || 'Ctrl+Ctrl';
  initialShortcut = state.shortcut;
} catch (_) {}

// Merge a partial patch against the freshest on-disk settings, then save. Because
// it always re-reads first, it can never overwrite a value another command (engine,
// display mode, shortcut) persisted live earlier in this wizard.
async function persist(patch) {
  try {
    const latest = await invoke('get_settings');
    await invoke('save_settings', { newSettings: { ...latest, ...patch } });
  } catch (e) {
    console.warn('persist failed', e);
  }
}

// ── Reusable ARIA radiogroup: click + arrow-key nav + roving tabindex ──────────
function wireRadioGroup(group, onSelect) {
  const items = Array.from(group.querySelectorAll('[role="radio"]'));
  items.forEach((i) => { i.tabIndex = i.getAttribute('aria-checked') === 'true' ? 0 : -1; });

  function select(item, focus) {
    items.forEach((i) => {
      const on = i === item;
      i.classList.toggle('selected', on);
      i.setAttribute('aria-checked', on ? 'true' : 'false');
      i.tabIndex = on ? 0 : -1;
    });
    if (focus) item.focus();
    onSelect(item);
  }

  group.addEventListener('click', (e) => {
    const it = e.target.closest('[role="radio"]');
    if (it) select(it, false);
  });
  group.addEventListener('keydown', (e) => {
    const idx = items.indexOf(document.activeElement);
    if (idx === -1) return;
    let next = null;
    // RTL: ArrowLeft advances, ArrowRight goes back (matches the settings tablist).
    if (e.key === 'ArrowLeft' || e.key === 'ArrowDown') next = items[(idx + 1) % items.length];
    else if (e.key === 'ArrowRight' || e.key === 'ArrowUp') next = items[(idx - 1 + items.length) % items.length];
    if (next) { e.preventDefault(); select(next, true); }
  });

  return {
    selectByData: (attr, val) => {
      const it = items.find((i) => i.getAttribute(attr) === val);
      if (it) select(it, false);
    },
  };
}

// ── Step 2 — engine ───────────────────────────────────────────────────────────
const engineNote = document.getElementById('engineNote');
const engineGroup = wireRadioGroup(document.getElementById('engineGroup'), (item) => {
  const id = item.dataset.engine;
  state.engine = id;
  engineNote.hidden = id !== 'whisper-local';
  invoke('set_active_engine', { engineId: id }).catch((e) => console.warn('set_active_engine', e));
});
engineGroup.selectByData('data-engine', state.engine);

// ── Step 3 — display mode (+ conditional theme strip) ──────────────────────────
const themeReveal = document.getElementById('themeReveal');
// Stays false during the initial programmatic select so the panel peek fires only
// on a real user click, never on load.
let displayReady = false;
const displayGroup = wireRadioGroup(document.getElementById('displayGroup'), async (item) => {
  const mode = item.dataset.display;
  state.display = mode;
  // The mic appears (so its colour matters) in floating-mic and hidden modes.
  themeReveal.hidden = !(mode === 'floating-mic' || mode === 'hidden-mic');
  try {
    await invoke('set_display_mode', { mode });
  } catch (e) {
    console.warn('set_display_mode', e);
  }
  // The side panel has no always-visible widget like the floating mic, so give it
  // the same instant feedback: auto-open the real panel for 4s, then close.
  if (mode === 'side-panel' && displayReady) {
    emit('speakly://panel-peek').catch(() => {});
  }
});

// Build the theme grid (buttons → keyboard-selectable, unlike the old divs).
const themeGrid = document.getElementById('themeGrid');
for (const t of THEMES) {
  const card = document.createElement('button');
  card.type = 'button';
  card.className = 'theme-card' + (t.id === state.theme ? ' selected' : '');
  card.setAttribute('role', 'radio');
  card.setAttribute('aria-checked', t.id === state.theme ? 'true' : 'false');
  card.setAttribute('aria-label', `ערכת צבע: ${t.label}`);
  card.dataset.theme = t.id;
  card.innerHTML = `<span class="mini-mic ${t.id}">${MIC_SVG}</span><span class="theme-label">${t.label}</span>`;
  themeGrid.appendChild(card);
}
wireRadioGroup(themeGrid, (item) => {
  state.theme = item.dataset.theme;
  persist({ mic_theme: state.theme }); // live preview on the floating mic
});

// Apply the saved display choice last so the theme strip reveals correctly. This
// runs before `displayReady` flips true, so it won't trigger the panel peek.
displayGroup.selectByData('data-display', state.display);
displayReady = true;

// ── Step 4 — shortcut recorder ────────────────────────────────────────────────
const shortcutBtn = document.getElementById('shortcutBtn');
const shortcutStatus = document.getElementById('shortcutStatus');
let recording = false;

function renderShortcut() {
  const mod = doubleTapModifierOf(state.shortcut);
  if (mod) {
    shortcutBtn.textContent = `הקשה כפולה על ${mod}`;
    shortcutBtn.classList.remove('combo');
  } else {
    shortcutBtn.textContent = state.shortcut;
    shortcutBtn.classList.add('combo');
  }
}
renderShortcut();

async function stopRecording(resume) {
  recording = false;
  shortcutBtn.classList.remove('recording');
  if (resume) { try { await invoke('resume_global_shortcut'); } catch (_) {} }
  renderShortcut();
}

shortcutBtn.addEventListener('click', async () => {
  if (recording) return;
  recording = true;
  shortcutBtn.classList.remove('combo');
  shortcutBtn.classList.add('recording');
  shortcutBtn.textContent = 'הקש קיצור (Esc לביטול)…';
  shortcutStatus.textContent = '';
  try { await invoke('pause_global_shortcut'); } catch (_) {}
});

window.addEventListener('keydown', async (e) => {
  if (!recording) return;
  e.preventDefault();
  if (e.key === 'Escape') { await stopRecording(true); return; }

  const parts = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Super');
  const key = e.key;
  if (['Control', 'Alt', 'Shift', 'Meta', 'OS'].includes(key)) return;
  parts.push(key === ' ' ? 'Space' : key.length === 1 ? key.toUpperCase() : key);
  if (parts.length < 2) {
    shortcutStatus.textContent = 'נדרש מודיפייר לפחות (Ctrl/Alt/Shift/Win) + מקש.';
    return;
  }
  state.shortcut = parts.join('+');
  await stopRecording(true);
});

// ── Step 5 — mic test (level meter; verifies access without full transcription) ─
const micTestBtn = document.getElementById('micTestBtn');
const micMeter = document.getElementById('micMeter');
const micMeterFill = document.getElementById('micMeterFill');
const micTestStatus = document.getElementById('micTestStatus');
let micStream = null, audioCtx = null, rafId = null, testActive = false;

function setMicStatus(text, kind) {
  micTestStatus.textContent = text;
  micTestStatus.className = 'mic-test-status' + (kind ? ' ' + kind : '');
}

function stopMicTest() {
  testActive = false;
  micTestBtn.disabled = false;
  if (rafId) { cancelAnimationFrame(rafId); rafId = null; }
  if (micStream) { micStream.getTracks().forEach((t) => t.stop()); micStream = null; }
  if (audioCtx) { audioCtx.close().catch(() => {}); audioCtx = null; }
  micMeterFill.style.width = '0%';
}

async function startMicTest() {
  if (testActive) return;
  testActive = true;
  micTestBtn.disabled = true;
  setMicStatus('מבקש גישה למיקרופון…', '');
  try {
    micStream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch (e) {
    testActive = false;
    micTestBtn.disabled = false;
    micMeter.hidden = true;
    setMicStatus('לא הצלחנו לגשת למיקרופון. בדקו את הרשאות המיקרופון בהגדרות Windows.', 'err');
    return;
  }
  micMeter.hidden = false;
  audioCtx = new (window.AudioContext || window.webkitAudioContext)();
  const srcNode = audioCtx.createMediaStreamSource(micStream);
  const analyser = audioCtx.createAnalyser();
  analyser.fftSize = 512;
  srcNode.connect(analyser);
  const data = new Uint8Array(analyser.frequencyBinCount);
  let peak = 0;
  const started = performance.now();
  setMicStatus('דברו עכשיו…', '');

  function loop() {
    if (!testActive) return;
    analyser.getByteTimeDomainData(data);
    let max = 0;
    for (let i = 0; i < data.length; i++) {
      const v = Math.abs(data[i] - 128);
      if (v > max) max = v;
    }
    const level = Math.min(100, Math.round((max / 128) * 100 * 1.6));
    micMeterFill.style.width = `${level}%`;
    if (level > 18) peak = Math.max(peak, level);
    if (performance.now() - started < 5000) {
      rafId = requestAnimationFrame(loop);
    } else {
      stopMicTest();
      if (peak > 18) setMicStatus('✓ המיקרופון עובד', 'ok');
      else setMicStatus('לא קלטנו קול. נסו לדבר חזק יותר או לבדוק שנבחר המיקרופון הנכון.', 'warn');
    }
  }
  loop();
}
micTestBtn.addEventListener('click', startMicTest);

// ── Step 5 — tips (adapt to the chosen display mode + shortcut) ─────────────────
function renderTips() {
  const tips = document.getElementById('tips');
  const mod = doubleTapModifierOf(state.shortcut);
  const startTip = mod
    ? `הקשה כפולה על ${mod} מתחילה ועוצרת תמלול`
    : `הקיצור ${state.shortcut} מתחיל ועוצר תמלול`;
  const second = state.display === 'side-panel'
    ? 'הידית הדקה בקצה הימני של המסך פותחת תפריט עם 3 פעולות'
    : 'לחיצה ימנית על הכדור פותחת הגדרות, השתקה והסתרה';
  const third = 'ההגדרות תמיד זמינות דרך אייקון Timluli בשורת המשימות';
  const lines = [startTip, second, third];
  tips.innerHTML = '';
  lines.forEach((text, i) => {
    const row = document.createElement('div');
    row.className = 'tip';
    row.innerHTML = `<span class="tip-num">${i + 1}</span><span></span>`;
    row.querySelector('span:last-child').textContent = text;
    tips.appendChild(row);
  });
}

// ── Step navigation ─────────────────────────────────────────────────────────────
let currentStep = 1;
const stepAnnounce = document.getElementById('stepAnnounce');

function goTo(n) {
  if (recording) stopRecording(true);
  if (currentStep === 5 && n !== 5) stopMicTest();

  document.querySelectorAll('.step').forEach((s, i) => s.classList.toggle('active', i + 1 === n));
  for (let i = 1; i <= TOTAL_STEPS; i++) {
    const dot = document.getElementById(`dot${i}`);
    if (!dot) continue;
    dot.classList.toggle('active', i === n);
    dot.classList.toggle('done', i < n);
  }
  const prevBtn = document.getElementById('prevBtn');
  const nextBtn = document.getElementById('nextBtn');
  prevBtn.style.visibility = n > 1 ? 'visible' : 'hidden';
  nextBtn.style.display = n < TOTAL_STEPS ? '' : 'none';

  if (n === 5) renderTips();
  currentStep = n;
  stepAnnounce.textContent = `שלב ${n} מתוך ${TOTAL_STEPS}`;

  // Move focus to the step heading for screen-reader + keyboard users.
  const heading = document.querySelector('.step.active .title');
  if (heading) heading.focus();
}

document.getElementById('nextBtn').addEventListener('click', () => { if (currentStep < TOTAL_STEPS) goTo(currentStep + 1); });
document.getElementById('prevBtn').addEventListener('click', () => { if (currentStep > 1) goTo(currentStep - 1); });

// ── Finish ──────────────────────────────────────────────────────────────────────
document.getElementById('startBtn').addEventListener('click', async () => {
  stopMicTest();
  try {
    if (state.shortcut !== initialShortcut) {
      await invoke('update_shortcut', { combo: state.shortcut }).catch(() => {});
    }
    // engine_id & display_mode were persisted live by their commands; this final
    // merge stamps the theme + the done flag without clobbering them.
    await persist({ mic_theme: state.theme, onboarding_done: true });
    await win.close();
  } catch (e) {
    console.error('onboarding complete failed', e);
  }
});

// Creator-seal email → open in the system mail client (webview swallows mailto:).
document.querySelector('.seal-email')?.addEventListener('click', (e) => {
  e.preventDefault();
  invoke('open_external', { url: 'mailto:ddlgap@gmail.com' }).catch(() => {});
});

goTo(1);
