import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

const win = getCurrentWindow();

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

let currentStep = 1;
let selectedTheme = 'graphite';
let selectedShortcut = 'Ctrl+Super+Space';
let recording = false;
let currentSettings = {};

try {
  currentSettings = await invoke('get_settings');
  selectedTheme = currentSettings.mic_theme || 'graphite';
  selectedShortcut = currentSettings.shortcut || 'Ctrl+Super+Space';
} catch (_) {}

// Build theme grid
const grid = document.getElementById('themeGrid');
for (const t of THEMES) {
  const card = document.createElement('div');
  card.className = 'theme-card' + (t.id === selectedTheme ? ' selected' : '');
  card.innerHTML = `<div class="mini-mic ${t.id}">${MIC_SVG}</div><span class="theme-label">${t.label}</span>`;
  card.addEventListener('click', async () => {
    grid.querySelectorAll('.theme-card').forEach(c => c.classList.remove('selected'));
    card.classList.add('selected');
    selectedTheme = t.id;
    try {
      await invoke('save_settings', { newSettings: { ...currentSettings, mic_theme: t.id } });
    } catch (_) {}
  });
  grid.appendChild(card);
}

// Shortcut
const shortcutBtn = document.getElementById('shortcutBtn');
const shortcutStatus = document.getElementById('shortcutStatus');
shortcutBtn.textContent = selectedShortcut;

shortcutBtn.addEventListener('click', async () => {
  if (recording) return;
  recording = true;
  shortcutBtn.classList.add('recording');
  shortcutBtn.textContent = 'הקש קיצור (Esc לביטול)...';
  shortcutStatus.textContent = '';
  try { await invoke('pause_global_shortcut'); } catch (_) {}
});

window.addEventListener('keydown', async (e) => {
  if (!recording) return;
  e.preventDefault();
  if (e.key === 'Escape') {
    recording = false;
    shortcutBtn.classList.remove('recording');
    shortcutBtn.textContent = selectedShortcut;
    try { await invoke('resume_global_shortcut'); } catch (_) {}
    return;
  }
  const parts = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Super');
  const key = e.key;
  if (['Control','Alt','Shift','Meta','OS'].includes(key)) return;
  parts.push(key === ' ' ? 'Space' : key.length === 1 ? key.toUpperCase() : key);
  if (parts.length < 2) { shortcutStatus.textContent = 'נדרש לפחות מודיפייר (Ctrl/Alt/Shift/Win)'; return; }
  selectedShortcut = parts.join('+');
  shortcutBtn.textContent = selectedShortcut;
  shortcutBtn.classList.remove('recording');
  recording = false;
  try { await invoke('resume_global_shortcut'); } catch (_) {}
});

// Step nav
function goTo(n) {
  document.querySelectorAll('.step').forEach((s, i) => s.classList.toggle('active', i + 1 === n));
  ['dot1','dot2','dot3'].forEach((id, i) => {
    const el = document.getElementById(id);
    el.classList.toggle('active', i + 1 === n);
    el.classList.toggle('done', i + 1 < n);
  });
  const prevBtn = document.getElementById('prevBtn');
  const nextBtn = document.getElementById('nextBtn');
  prevBtn.style.visibility = n > 1 ? 'visible' : 'hidden';
  nextBtn.style.display = n < 3 ? '' : 'none';
  currentStep = n;
}

document.getElementById('nextBtn').addEventListener('click', () => { if (currentStep < 3) goTo(currentStep + 1); });
document.getElementById('prevBtn').addEventListener('click', () => { if (currentStep > 1) goTo(currentStep - 1); });

// Complete
document.getElementById('startBtn').addEventListener('click', async () => {
  try {
    const newSettings = { ...currentSettings, mic_theme: selectedTheme, shortcut: selectedShortcut, onboarding_done: true };
    if (selectedShortcut !== (currentSettings.shortcut || '')) {
      await invoke('update_shortcut', { combo: selectedShortcut }).catch(() => {});
    }
    await invoke('save_settings', { newSettings });
    await win.close();
  } catch (e) {
    console.error('onboarding complete failed', e);
  }
});

goTo(1);
