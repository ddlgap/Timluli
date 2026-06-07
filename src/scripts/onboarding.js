import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
const win = getCurrentWindow();
const TOTAL_STEPS = 6;

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

// ── Shared mic analyser (used by the mic test AND the dictation demo) ──────────
// Opens getUserMedia + an AnalyserNode and exposes a peak-level reader (0–100).
async function openMicAnalyser() {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const ctx = new (window.AudioContext || window.webkitAudioContext)();
  const src = ctx.createMediaStreamSource(stream);
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 512;
  src.connect(analyser);
  const data = new Uint8Array(analyser.frequencyBinCount);
  return {
    level() {
      analyser.getByteTimeDomainData(data);
      let max = 0;
      for (let i = 0; i < data.length; i++) {
        const v = Math.abs(data[i] - 128);
        if (v > max) max = v;
      }
      return Math.min(100, Math.round((max / 128) * 100 * 1.6));
    },
    stop() {
      try { stream.getTracks().forEach((t) => t.stop()); } catch (_) {}
      try { ctx.close(); } catch (_) {}
    },
  };
}

// ── Step 6 — mic test (level meter; verifies access without full transcription) ─
const micTestBtn = document.getElementById('micTestBtn');
const micMeter = document.getElementById('micMeter');
const micMeterFill = document.getElementById('micMeterFill');
const micTestStatus = document.getElementById('micTestStatus');
let micTest = null, micTestRaf = null, micTestActive = false;

function setMicStatus(text, kind) {
  micTestStatus.textContent = text;
  micTestStatus.className = 'mic-test-status' + (kind ? ' ' + kind : '');
}

function stopMicTest() {
  micTestActive = false;
  micTestBtn.disabled = false;
  if (micTestRaf) { cancelAnimationFrame(micTestRaf); micTestRaf = null; }
  if (micTest) { micTest.stop(); micTest = null; }
  micMeterFill.style.width = '0%';
}

async function startMicTest() {
  if (micTestActive) return;
  micTestActive = true;
  micTestBtn.disabled = true;
  setMicStatus('מבקש גישה למיקרופון…', '');
  try {
    micTest = await openMicAnalyser();
  } catch (e) {
    micTestActive = false;
    micTestBtn.disabled = false;
    micMeter.hidden = true;
    setMicStatus('לא הצלחנו לגשת למיקרופון. בדקו את הרשאות המיקרופון בהגדרות Windows.', 'err');
    return;
  }
  micMeter.hidden = false;
  let peak = 0;
  const started = performance.now();
  setMicStatus('דברו עכשיו…', '');

  function loop() {
    if (!micTestActive) return;
    const level = micTest.level();
    micMeterFill.style.width = `${level}%`;
    if (level > 18) peak = Math.max(peak, level);
    if (performance.now() - started < 5000) {
      micTestRaf = requestAnimationFrame(loop);
    } else {
      stopMicTest();
      if (peak > 18) setMicStatus('✓ המיקרופון עובד', 'ok');
      else setMicStatus('לא קלטנו קול. נסו לדבר חזק יותר או לבדוק שנבחר המיקרופון הנכון.', 'warn');
    }
  }
  loop();
}
micTestBtn.addEventListener('click', startMicTest);

// ══════════════════════════════════════════════════════════════════════════════
// Step 5 — Hands-on missions. Each must be performed to advance.
//   Mission 1 (dictation): REAL — the real shortcut/engine transcribes live into a
//     focused textbox. Clear "speak now" overlay; skip if the mic/engine isn't ready.
//   Missions 2-3 (audio / PDF): real gesture (drag OR file-picker), fast simulated
//     result (no network/keys needed mid-onboarding).
// ══════════════════════════════════════════════════════════════════════════════
const extOf = (p) => { const m = /\.([^.\\/]+)$/.exec(p || ''); return m ? m[1].toLowerCase() : ''; };
const stripExt = (n) => n.replace(/\.[^.]+$/, '');

const MISSIONS = [
  { badge: '1', title: 'דברו — וזה נכתב לבד' },
  { badge: '2', title: 'תמלול קובץ אודיו' },
  { badge: '3', title: 'תרגום מסמך' },
];

const demoEl = {
  fill: document.getElementById('missionFill'),
  steps: document.getElementById('missionSteps'),
  badge: document.getElementById('missionBadge'),
  title: document.getElementById('missionTitle'),
  sub: document.getElementById('missionSub'),
  windowTitle: document.getElementById('demoWindowTitle'),
  doc: document.getElementById('demoDoc'),
  files: document.getElementById('demoFiles'),
  zone: document.getElementById('demoDropZone'),
  keys: document.getElementById('demoKeys'),
  mic: document.getElementById('demoMic'),
  zoneHint: document.getElementById('demoZoneHint'),
  result: document.getElementById('missionResult'),
  resultText: document.getElementById('missionResultText'),
  trigger: document.getElementById('demoTrigger'),
  secondary: document.getElementById('demoSecondary'),
  advance: document.getElementById('demoAdvance'),
  speakOverlay: document.getElementById('speakOverlay'),
  speakMic: document.getElementById('speakMic'),
  speakHint: document.getElementById('speakHint'),
  speakSkip: document.getElementById('speakSkip'),
};

const demo = {
  active: 1,
  done: [false, false, false],
  phase: '',            // '', 'm1-idle', 'm1-listening', 'm1-done'
  keyTimes: [],
  listening: false,     // a real listening session is active
  busy: false,          // a file is being processed
  procTimer: null,
  escTimer: null,
  inputTimer: null,
};

const nextBtnEl = document.getElementById('nextBtn');
const setSub = (html) => { demoEl.sub.innerHTML = html; };
function showResult(html) { demoEl.resultText.innerHTML = html; demoEl.result.hidden = false; }
function hideResult() { demoEl.result.hidden = true; demoEl.resultText.innerHTML = ''; }

function initDemoMic() {
  demoEl.mic.className = 'demo-mic mini-mic ' + (state.theme || 'graphite');
  demoEl.mic.dataset.state = 'idle';
  demoEl.mic.innerHTML = MIC_SVG;
  demoEl.speakMic.className = 'speak-mic mini-mic ' + (state.theme || 'graphite');
  demoEl.speakMic.innerHTML = MIC_SVG;
}

function clearDemoTimers() {
  clearTimeout(demo.procTimer);
  clearTimeout(demo.escTimer);
  clearTimeout(demo.inputTimer);
}

function updateTrack() {
  const doneCount = demo.done.filter(Boolean).length;
  demoEl.fill.style.width = (doneCount / 3 * 100) + '%';
  for (let i = 1; i <= 3; i++) {
    const dot = demoEl.steps.querySelector(`.mtrack-dot[data-m="${i}"]`);
    if (!dot) continue;
    dot.classList.toggle('done', demo.done[i - 1]);
    dot.classList.toggle('active', i === demo.active && !demo.done[i - 1]);
  }
}

function setMissionHead(n) {
  const m = MISSIONS[n - 1];
  demoEl.badge.textContent = m.badge;
  demoEl.title.textContent = m.title;
}

function showDocView(title) { demoEl.files.hidden = true; demoEl.doc.hidden = false; demoEl.windowTitle.textContent = title; }
function showFilesView(title) { demoEl.doc.hidden = true; demoEl.files.hidden = false; demoEl.windowTitle.textContent = title; }

function fileChipHTML(cls, ico, name, sub) {
  return `<div class="file-chip ${cls}">
      <span class="chip-ico">${ico}</span>
      <span class="chip-meta"><span class="chip-name">${name}</span>${sub ? `<span class="chip-sub">${sub}</span>` : ''}</span>
    </div>`;
}
const arrowHTML = () => '<div class="demo-files-arrow">↓ נשמר באותה תיקייה</div>';

// ── Mission 1 — real dictation ────────────────────────────────────────────────
function renderKeys() {
  const mod = doubleTapModifierOf(state.shortcut);
  if (mod) {
    demoEl.keys.innerHTML = `<kbd class="demo-keycap" data-k="1">${mod}</kbd><span class="demo-keys-plus">‹‹</span><kbd class="demo-keycap" data-k="2">${mod}</kbd>`;
  } else {
    demoEl.keys.innerHTML = state.shortcut.split('+')
      .map((k) => `<kbd class="demo-keycap">${k.trim()}</kbd>`)
      .join('<span class="demo-keys-plus">+</span>');
  }
}
function flashKey(idx) {
  const ks = demoEl.keys.querySelectorAll('.demo-keycap');
  const k = ks[idx - 1] || ks[ks.length - 1];
  if (!k) return;
  k.classList.remove('tap');
  void k.offsetWidth;
  k.classList.add('tap');
}

function startMission1() {
  demo.phase = 'm1-idle';
  demo.keyTimes = [];
  showDocView('מסמך — Notepad');
  demoEl.doc.value = '';
  demoEl.zone.classList.remove('droppable', 'is-over');
  demoEl.zoneHint.hidden = true;
  demoEl.mic.dataset.state = 'idle';
  renderKeys();
  demoEl.keys.hidden = false;
  const mod = doubleTapModifierOf(state.shortcut);
  setSub(mod
    ? `לחצו פעמיים על <span class="hl-key">${mod}</span> ואז דברו — מה שתגידו ייכתב לבד.`
    : `לחצו <span class="hl-key">${state.shortcut}</span> ואז דברו — מה שתגידו ייכתב לבד.`);
  demoEl.trigger.hidden = false;
  demoEl.trigger.textContent = '🎤 התחילו הקלטה';
  demoEl.secondary.hidden = true;
  demoEl.advance.hidden = true;
  setTimeout(() => { if (demo.active === 1) demoEl.doc.focus(); }, 60);
}

function modifierName(e) {
  if (e.key === 'Control') return 'Ctrl';
  if (e.key === 'Alt') return 'Alt';
  if (e.key === 'Shift') return 'Shift';
  if (e.key === 'Meta' || e.key === 'OS') return 'Win';
  return null;
}
function comboOf(e) {
  const parts = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Super');
  const key = e.key;
  if (['Control', 'Alt', 'Shift', 'Meta', 'OS'].includes(key)) return null;
  parts.push(key === ' ' ? 'Space' : key.length === 1 ? key.toUpperCase() : key);
  return parts.length >= 2 ? parts.join('+') : null;
}

// Show the "speak now" overlay (UI only — the engine is started elsewhere).
function showSpeakOverlay() {
  if (demo.phase === 'm1-listening') return;
  demo.phase = 'm1-listening';
  demoEl.keys.hidden = true;
  demoEl.mic.dataset.state = 'listening';
  demoEl.doc.focus();
  demoEl.speakOverlay.hidden = false;
  demoEl.speakSkip.hidden = true;
  demoEl.speakHint.textContent = 'אמרו משפט קצר — זה ייעצר לבד כשתסיימו';
  clearTimeout(demo.escTimer);
  demo.escTimer = setTimeout(() => {
    demoEl.speakHint.textContent = 'לא קלטנו טקסט. נסו לדבר שוב — או דלגו.';
    demoEl.speakSkip.hidden = false;
  }, 13000);
}

// "Start recording" button — we toggle the real engine ourselves.
async function triggerListen() {
  if (demo.phase === 'm1-listening') return;
  demoEl.doc.focus();
  showSpeakOverlay();
  try {
    await invoke('toggle_listening');
    demo.listening = true;
  } catch (e) {
    demoEl.speakHint.textContent = 'המנוע עדיין לא מוכן. אפשר לדלג ולהמשיך.';
    demoEl.speakSkip.hidden = false;
  }
}

function stopListening() {
  if (demo.listening) {
    invoke('toggle_listening').catch(() => {});
    demo.listening = false;
  }
}

function finishMission1() {
  if (demo.phase === 'm1-done') return;
  demo.phase = 'm1-done';
  clearTimeout(demo.escTimer);
  clearTimeout(demo.inputTimer);
  stopListening();
  demoEl.speakOverlay.hidden = true;
  demoEl.mic.dataset.state = 'idle';
  if (!demoEl.doc.value.trim()) demoEl.doc.value = 'שלום, זאת דוגמה להכתבה קולית.';
  showResult('<b>מעולה!</b> מה שאמרתם נכתב אוטומטית. בדיוק כך זה יעבוד בכל תוכנה — וורד, וואטסאפ, דפדפן, הכול.');
  completeMission(1);
}

// ── Missions 2 & 3 — drag a fake file onto the mic (pure on-screen simulation) ──
function startFileMission(n) {
  demo.busy = false;
  const audio = n === 2;
  const fname = audio ? 'הקלטה.mp3' : 'מסמך.pdf';
  const ico = audio ? '🎵' : '📕';
  showFilesView('סייר הקבצים');
  demoEl.files.innerHTML =
    '<p class="demo-drag-label">גררו את הקובץ אל המיקרופון:</p>' +
    `<div class="file-chip draggable" data-fname="${fname}">
       <span class="chip-ico">${ico}</span>
       <span class="chip-meta"><span class="chip-name">${fname}</span><span class="chip-sub">גררו אותי ←</span></span>
     </div>`;
  demoEl.keys.hidden = true;
  demoEl.mic.dataset.state = 'idle';
  demoEl.zone.classList.add('droppable');
  demoEl.zoneHint.hidden = false;
  demoEl.zoneHint.textContent = 'שחררו כאן';
  setSub(audio
    ? 'גררו את <span class="hl">קובץ האודיו</span> אל המיקרופון כדי לתמלל אותו.'
    : 'גררו את <span class="hl">קובץ ה-PDF</span> אל המיקרופון כדי לתרגם אותו.');
  demoEl.trigger.hidden = true;
  demoEl.secondary.hidden = true;
  demoEl.advance.hidden = true;
  wireChipDrag();
}

// Pointer-based drag of the fake file chip onto the mic (no real files involved).
let chipDrag = null;
function wireChipDrag() {
  const chip = demoEl.files.querySelector('.file-chip.draggable');
  if (!chip) return;
  chip.addEventListener('pointerdown', (e) => {
    if (chipDrag || demo.busy) return;
    e.preventDefault();
    const ghost = chip.cloneNode(true);
    ghost.classList.remove('draggable');
    ghost.classList.add('chip-ghost');
    document.body.appendChild(ghost);
    chip.classList.add('dragging-origin');
    chipDrag = { ghost, chip, fname: chip.dataset.fname };
    moveChipGhost(e);
    window.addEventListener('pointermove', moveChipGhost);
    window.addEventListener('pointerup', dropChip);
  });
}
function overMic(e) {
  const r = demoEl.zone.getBoundingClientRect();
  return e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
}
function moveChipGhost(e) {
  if (!chipDrag) return;
  chipDrag.ghost.style.left = e.clientX + 'px';
  chipDrag.ghost.style.top = e.clientY + 'px';
  demoEl.zone.classList.toggle('is-over', overMic(e));
}
function dropChip(e) {
  window.removeEventListener('pointermove', moveChipGhost);
  window.removeEventListener('pointerup', dropChip);
  const d = chipDrag;
  chipDrag = null;
  if (!d) return;
  d.ghost.remove();
  demoEl.zone.classList.remove('is-over');
  if (overMic(e)) {
    runFileSim(demo.active === 2 ? 'audio' : 'doc', d.fname);
  } else {
    d.chip.classList.remove('dragging-origin'); // not on the mic → snap back
  }
}

function runFileSim(kind, name) {
  demo.busy = true;
  const audio = kind === 'audio';
  demoEl.trigger.hidden = true;
  demoEl.zoneHint.hidden = true;
  demoEl.zone.classList.remove('droppable', 'is-over');
  showFilesView(audio ? 'תיקיית האודיו' : 'תיקיית המסמכים');
  demoEl.files.innerHTML = fileChipHTML(audio ? 'audio' : 'doc', audio ? '🎵' : '📕', name, 'הקובץ שלכם');
  demoEl.mic.dataset.state = 'processing';
  const steps = audio
    ? ['מתמלל…', 'מתמלל… חצי הדרך', 'כמעט סיימנו…']
    : ['מתרגם…', 'מתרגם… 2/5', 'מתרגם… 4/5'];
  setSub(steps[0]);
  let i = 0;
  const tick = () => {
    if (currentStep !== 5) return;
    i++;
    if (i < steps.length) { setSub(steps[i]); demo.procTimer = setTimeout(tick, 760); }
    else finishFileSim(kind, name);
  };
  demo.procTimer = setTimeout(tick, 760);
}

function finishFileSim(kind, name) {
  demoEl.mic.dataset.state = 'idle';
  setSub('');
  if (kind === 'audio') {
    const out = stripExt(name) + '.txt';
    demoEl.files.innerHTML += arrowHTML() + fileChipHTML('result', '📄', out, 'קובץ התמלול');
    showResult(`<b>הצלחתם!</b> קובץ התמלול <code>${out}</code> נשמר באותה תיקייה, ממש ליד הקובץ המקורי.`);
    completeMission(2);
  } else {
    const ext = extOf(name);
    const outExt = (ext === 'pdf' || ext === 'doc') ? 'docx' : ext;
    const out = stripExt(name) + '.he.' + outExt;
    demoEl.files.innerHTML += arrowHTML() + fileChipHTML('result', '📘', out, 'הקובץ המתורגם');
    showResult(`<b>הצלחתם!</b> הקובץ המתורגם <code>${out}</code> נשמר באותה תיקייה. הקובץ המקורי נשאר כמו שהוא.`);
    completeMission(3);
  }
}

// ── Lifecycle / gating ────────────────────────────────────────────────────────
function startMission(n) {
  demo.active = n;
  clearDemoTimers();
  hideResult();
  demoEl.speakOverlay.hidden = true;
  demoEl.advance.hidden = true;
  setMissionHead(n);
  updateTrack();
  if (n === 1) startMission1();
  else startFileMission(n);
}

function completeMission(n) {
  demo.done[n - 1] = true;
  demo.busy = false;
  updateTrack();
  demoEl.trigger.hidden = true;
  demoEl.secondary.hidden = true;
  if (n < 3) {
    demoEl.advance.hidden = false;
    demoEl.advance.textContent = 'המשך →';
    demoEl.advance.dataset.next = String(n + 1);
  } else {
    demoEl.advance.hidden = true;
    setSub('סיימתם את כל המשימות 🎉  לחצו „הבא” כדי להמשיך.');
  }
  if (demo.done.every(Boolean)) nextBtnEl.disabled = false;
}

function firstIncompleteMission() {
  const idx = demo.done.findIndex((d) => !d);
  return idx === -1 ? 0 : idx + 1;
}

function enterDemo() {
  initDemoMic();
  updateTrack();
  const next = firstIncompleteMission();
  if (next === 0) {
    setMissionHead(3);
    setSub('סיימתם את כל המשימות 🎉');
    demoEl.advance.hidden = true;
    nextBtnEl.disabled = false;
  } else {
    nextBtnEl.disabled = true;
    startMission(next);
  }
}

function leaveDemo() {
  clearDemoTimers();
  demoEl.speakOverlay.hidden = true;
  stopListening();
  demoEl.mic.dataset.state = 'idle';
}

// Mission 1 — detect the real double-tap / chord to surface the "speak now" overlay.
// The OS global shortcut already started the real engine; we only mirror the UI.
window.addEventListener('keydown', (e) => {
  if (currentStep !== 5 || demo.active !== 1 || demo.phase !== 'm1-idle') return;
  if (e.repeat) return;
  const mod = doubleTapModifierOf(state.shortcut);
  if (mod) {
    if (modifierName(e) !== mod) return;
    demo.keyTimes = demo.keyTimes.concat(performance.now()).slice(-2);
    flashKey(Math.min(demo.keyTimes.length, 2));
    if (demo.keyTimes.length === 2 && demo.keyTimes[1] - demo.keyTimes[0] <= 600) {
      demo.keyTimes = [];
      showSpeakOverlay();
      demo.listening = true;
    }
  } else if (comboOf(e) === state.shortcut) {
    showSpeakOverlay();
    demo.listening = true;
  }
});

// Mission 1 — the real transcript is injected into this focused textbox.
demoEl.doc.addEventListener('input', () => {
  if (currentStep !== 5 || demo.active !== 1 || demo.phase !== 'm1-listening') return;
  if (!demoEl.doc.value.trim()) return;
  clearTimeout(demo.inputTimer);
  demo.inputTimer = setTimeout(finishMission1, 650); // let the full sentence land
});

demoEl.trigger.addEventListener('click', () => {
  if (demo.active === 1) triggerListen();
});

demoEl.speakSkip.addEventListener('click', finishMission1);

demoEl.advance.addEventListener('click', () => {
  const n = Number(demoEl.advance.dataset.next || 0);
  if (n) startMission(n);
});

// ── Step 6 — tips (adapt to the chosen display mode + shortcut) ─────────────────
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
  if (currentStep === 5 && n !== 5) leaveDemo();
  if (currentStep === 6 && n !== 6) stopMicTest();

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
  nextBtn.disabled = false; // reset the demo gate; enterDemo() re-locks it if needed

  currentStep = n;

  if (n === 5) enterDemo();
  if (n === 6) renderTips();
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
