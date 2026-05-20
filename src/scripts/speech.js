import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';

const stateEl = document.getElementById('state');
const logEl = document.getElementById('log');

const SR = window.SpeechRecognition || window.webkitSpeechRecognition;

let recognition = null;
let language = 'he-IL';
let engineId = 'web-speech';   // cached from settings; updated via engine-changed event
let activeRun = false;
let silenceTimer = null;
let silenceTimeoutMs = 1500;
let userRequestedStop = false;

const QUICK_STOP_AFTER_SPEECH_END_MS = 1500;
let quickStopTimer = null;
let runStartedAt = 0;
let consecutiveShortRuns = 0;
const INITIAL_NO_SPEECH_TIMEOUT_MS = 10000;
let initialTimer = null;

// ── Local engine audio state ─────────────────────────────────────────────────
let localAudioCtx = null;
let localStream = null;
let localWorkletNode = null;   // preferred
let localScriptNode = null;    // fallback
let localChunks = [];          // Float32Array chunks collected during recording
let localRecording = false;
let localAnalyser = null;
let localVadTimer = null;      // silence-detection timer for local path
let localVadActive = false;

// ── Web Speech timers ────────────────────────────────────────────────────────
function startInitialTimer() {
  if (initialTimer) clearTimeout(initialTimer);
  initialTimer = setTimeout(() => {
    if (recognition && activeRun) {
      log(`no speech detected (${INITIAL_NO_SPEECH_TIMEOUT_MS}ms), stopping`);
      userRequestedStop = true;
      try { recognition.stop(); } catch (_) {}
    }
  }, INITIAL_NO_SPEECH_TIMEOUT_MS);
}

function clearInitialTimer() {
  if (initialTimer) { clearTimeout(initialTimer); initialTimer = null; }
}

function startQuickStopTimer() {
  // Clear silence timer so only one path fires recognition.stop().
  clearSilenceTimer();
  if (quickStopTimer) clearTimeout(quickStopTimer);
  quickStopTimer = setTimeout(() => {
    if (recognition && activeRun) {
      log(`speechend quick-stop (${QUICK_STOP_AFTER_SPEECH_END_MS}ms)`);
      userRequestedStop = true;
      try { recognition.stop(); } catch (_) {}
    }
  }, QUICK_STOP_AFTER_SPEECH_END_MS);
}

function cancelQuickStopTimer() {
  if (quickStopTimer) { clearTimeout(quickStopTimer); quickStopTimer = null; }
}

function resetSilenceTimer() {
  if (silenceTimer) clearTimeout(silenceTimer);
  silenceTimer = setTimeout(() => {
    if (recognition && activeRun) {
      log(`silence timeout (${silenceTimeoutMs}ms), stopping recognition`);
      userRequestedStop = true;
      try { recognition.stop(); } catch (_) {}
    }
  }, silenceTimeoutMs);
}

function clearSilenceTimer() {
  if (silenceTimer) { clearTimeout(silenceTimer); silenceTimer = null; }
  cancelQuickStopTimer();
  clearInitialTimer();
}

// ── Logging / UI ─────────────────────────────────────────────────────────────
function log(msg) {
  const time = new Date().toLocaleTimeString();
  const line = document.createElement('div');
  line.textContent = `[${time}] ${msg}`;
  logEl.appendChild(line);
  logEl.scrollTop = logEl.scrollHeight;
  if (logEl.children.length > 200) logEl.removeChild(logEl.firstChild);
}

function setUiState(state) {
  stateEl.dataset.state = state;
  stateEl.textContent = state;
}

// ── Web Speech API ────────────────────────────────────────────────────────────
if (!SR) {
  log('webkitSpeechRecognition לא זמין — Web Speech לא יעבוד.');
}

function buildRecognition() {
  if (!SR) return null;
  const rec = new SR();
  rec.lang = language;
  rec.continuous = true;
  rec.interimResults = true;
  rec.maxAlternatives = 3;

  rec.onstart = () => {
    log(`onstart lang=${rec.lang}`);
    setUiState('listening');
    invoke('report_state', { state: 'listening' }).catch(() => {});
    startInitialTimer();
  };

  rec.onspeechstart = () => {
    log('onspeechstart');
    clearInitialTimer();
    cancelQuickStopTimer();
    resetSilenceTimer();
  };

  rec.onspeechend = () => {
    log('onspeechend');
    startQuickStopTimer();
  };

  rec.onresult = (event) => {
    clearInitialTimer();
    resetSilenceTimer();
    let interim = '';
    for (let i = event.resultIndex; i < event.results.length; i++) {
      const result = event.results[i];
      if (result.isFinal) {
        // Pick the highest-confidence alternative (Chrome returns up to maxAlternatives).
        let best = result[0];
        for (let j = 1; j < result.length; j++) {
          if ((result[j].confidence ?? 0) > (best.confidence ?? 0)) best = result[j];
        }
        const text = best.transcript;
        if (text.trim()) {
          log(`final (conf=${(best.confidence ?? 1).toFixed(2)}): "${text}"`);
          invoke('inject_partial', { text: text.trim() + ' ' }).catch((e) => {
            log(`inject_partial failed: ${e}`);
            invoke('report_error', { message: String(e) }).catch(() => {});
          });
        }
      } else {
        // Chrome always returns confidence=0 for interim results — do not filter by it.
        interim += result[0].transcript;
      }
    }
    if (interim) {
      // Fire-and-forget broadcast straight to mic.js — saves a Rust-side
      // round-trip (no ACK wait, no return-value serialization).
      emit('speakly://interim', interim).catch(() => {});
    }
  };

  rec.onerror = (event) => {
    log(`onerror: ${event.error} ${event.message || ''}`);
    clearSilenceTimer();
    setUiState('error');
    invoke('report_error', { message: String(event.error || 'unknown') }).catch(() => {});
    activeRun = false;
  };

  rec.onend = () => {
    const ranFor = Date.now() - runStartedAt;
    log(`onend (ran ${ranFor}ms)`);

    if (!userRequestedStop && activeRun && ranFor > 1500) {
      consecutiveShortRuns = 0;
      log('unexpected onend, restarting...');
      clearSilenceTimer();
      activeRun = false;
      setTimeout(startWebSpeech, 50);
      return;
    }

    if (!userRequestedStop && ranFor <= 1500) {
      consecutiveShortRuns++;
      log(`short run (${ranFor}ms) #${consecutiveShortRuns} — not auto-restarting`);
    } else {
      consecutiveShortRuns = 0;
    }

    clearSilenceTimer();
    setUiState('idle');
    activeRun = false;
    invoke('report_state', { state: 'idle' }).catch(() => {});
  };

  return rec;
}

async function startWebSpeech() {
  if (activeRun) return;
  userRequestedStop = false;
  cancelQuickStopTimer();
  activeRun = true;
  runStartedAt = Date.now();
  recognition = buildRecognition();
  if (!recognition) {
    activeRun = false;
    invoke('report_error', { message: 'Web Speech API not available' }).catch(() => {});
    return;
  }
  try {
    recognition.start();
  } catch (e) {
    log(`start() threw: ${e?.message || e}`);
    activeRun = false;
    invoke('report_error', { message: String(e?.message || e) }).catch(() => {});
  }
}

function stopWebSpeech() {
  if (!recognition) return;
  try { recognition.stop(); } catch (e) {
    log(`stop() threw: ${e?.message || e}`);
  }
}

// ── Local engine audio capture ────────────────────────────────────────────────

// AudioWorklet processor source — loaded as a blob URL.
const WORKLET_SRC = `
class F32Collector extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0]?.[0];
    if (ch && ch.length > 0) {
      // Transfer a copy so the main thread can use it.
      const copy = new Float32Array(ch);
      this.port.postMessage(copy, [copy.buffer]);
    }
    return true;
  }
}
registerProcessor('f32-collector', F32Collector);
`;

async function startLocalEngine() {
  if (localRecording) return;
  log('local: starting audio capture');
  setUiState('listening');
  invoke('report_state', { state: 'listening' }).catch(() => {});

  try {
    localStream = await navigator.mediaDevices.getUserMedia({ audio: true, video: false });
    // AudioContext at 16 kHz — WebView2 resamples internally.
    localAudioCtx = new AudioContext({ sampleRate: 16000 });
    // Hidden windows have AudioContext auto-suspended by Chromium — force resume.
    if (localAudioCtx.state !== 'running') {
      await localAudioCtx.resume();
      log(`local: AudioContext resumed (was ${localAudioCtx.state})`);
    }
    const source = localAudioCtx.createMediaStreamSource(localStream);

    localChunks = [];
    localRecording = true;

    // Analyser for energy-based VAD
    localAnalyser = localAudioCtx.createAnalyser();
    localAnalyser.fftSize = 256;
    source.connect(localAnalyser);

    // Try AudioWorkletNode first; fall back to ScriptProcessorNode.
    let captureOk = false;
    try {
      const blob = new Blob([WORKLET_SRC], { type: 'application/javascript' });
      const url = URL.createObjectURL(blob);
      await localAudioCtx.audioWorklet.addModule(url);
      URL.revokeObjectURL(url);
      localWorkletNode = new AudioWorkletNode(localAudioCtx, 'f32-collector');
      localWorkletNode.port.onmessage = (e) => {
        if (localRecording) localChunks.push(e.data);
      };
      source.connect(localWorkletNode);
      localWorkletNode.connect(localAudioCtx.destination);
      captureOk = true;
      log('local: using AudioWorkletNode');
    } catch (workletErr) {
      log(`local: AudioWorklet failed (${workletErr?.message}), falling back to ScriptProcessorNode`);
    }

    if (!captureOk) {
      // ScriptProcessorNode fallback (deprecated but still works in WebView2)
      localScriptNode = localAudioCtx.createScriptProcessor(4096, 1, 1);
      localScriptNode.onaudioprocess = (e) => {
        if (!localRecording) return;
        const data = e.inputBuffer.getChannelData(0);
        localChunks.push(new Float32Array(data));
      };
      source.connect(localScriptNode);
      localScriptNode.connect(localAudioCtx.destination);
      log('local: using ScriptProcessorNode');
    }

    // VAD: check energy every 200ms; trigger silence timer after silenceTimeoutMs of quiet.
    startLocalVad();

  } catch (err) {
    log(`local: capture failed: ${err?.message || err}`);
    cleanupLocalAudio();
    invoke('report_error', { message: `שגיאת לכידת שמע: ${err?.message || err}` }).catch(() => {});
    invoke('report_state', { state: 'idle' }).catch(() => {});
  }
}

function startLocalVad() {
  const buf = new Uint8Array(localAnalyser?.frequencyBinCount || 128);
  const SILENCE_THRESHOLD = 45; // out of 255 — raised from 15 to clear ambient noise
  let quietMs = 0;
  const POLL_MS = 200;

  localVadActive = true;
  function poll() {
    if (!localVadActive || !localRecording) return;
    if (localAnalyser) {
      localAnalyser.getByteFrequencyData(buf);
      const energy = buf.reduce((s, v) => s + v, 0) / buf.length;
      if (energy >= SILENCE_THRESHOLD) {
        quietMs = 0; // speech detected — reset silence counter
      } else {
        quietMs += POLL_MS;
      }
    } else {
      quietMs += POLL_MS; // no analyser — count everything as silence
    }

    if (quietMs >= silenceTimeoutMs) {
      log(`local: silence ${quietMs}ms >= ${silenceTimeoutMs}ms, finalising`);
      finaliseLocal();
      return;
    }
    localVadTimer = setTimeout(poll, POLL_MS);
  }
  localVadTimer = setTimeout(poll, POLL_MS);

  // Hard cap: stop after 30 seconds regardless of VAD
  setTimeout(() => {
    if (localRecording) {
      log('local: 30s max duration reached');
      finaliseLocal();
    }
  }, 30_000);
}

async function finaliseLocal() {
  if (!localRecording) return;
  localVadActive = false;
  if (localVadTimer) { clearTimeout(localVadTimer); localVadTimer = null; }
  localRecording = false;
  log('local: finalising, encoding samples');
  setUiState('processing');

  const chunks = localChunks;
  localChunks = [];
  cleanupLocalAudio();

  if (chunks.length === 0) {
    log('local: no audio captured');
    setUiState('idle');
    invoke('report_state', { state: 'idle' }).catch(() => {});
    return;
  }

  // Concatenate all Float32Array chunks into one.
  const totalLen = chunks.reduce((s, c) => s + c.length, 0);
  const samples = new Float32Array(totalLen);
  let offset = 0;
  for (const c of chunks) { samples.set(c, offset); offset += c.length; }

  // Encode as base64 of little-endian f32 bytes.
  const bytes = new Uint8Array(samples.buffer);
  let binary = '';
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  const samplesB64 = btoa(binary);

  try {
    await invoke('transcribe_local', { samplesB64 });
    log('local: transcription complete');
  } catch (e) {
    log(`local: transcribe_local failed: ${e}`);
    invoke('report_error', { message: String(e) }).catch(() => {});
  }

  setUiState('idle');
  invoke('report_state', { state: 'idle' }).catch(() => {});
}

function stopLocalEngine() {
  if (!localRecording) return;
  log('local: stop requested');
  finaliseLocal();
}

function cleanupLocalAudio() {
  localVadActive = false;
  if (localVadTimer) { clearTimeout(localVadTimer); localVadTimer = null; }
  try { localWorkletNode?.disconnect(); } catch (_) {}
  try { localScriptNode?.disconnect(); } catch (_) {}
  try { localAnalyser?.disconnect(); } catch (_) {}
  localWorkletNode = null;
  localScriptNode = null;
  localAnalyser = null;
  localStream?.getTracks().forEach((t) => t.stop());
  localStream = null;
  localAudioCtx?.close().catch(() => {});
  localAudioCtx = null;
}

// ── Event listeners ───────────────────────────────────────────────────────────

await listen('speakly://start-listening', () => {
  log(`event: start-listening (engine=${engineId})`);
  if (engineId === 'whisper-local') {
    startLocalEngine();
  } else {
    startWebSpeech();
  }
});

await listen('speakly://stop-listening', () => {
  log('event: stop-listening');
  if (engineId === 'whisper-local') {
    stopLocalEngine();
  } else {
    userRequestedStop = true;
    stopWebSpeech();
  }
});

await listen('speakly://settings-changed', (e) => {
  if (e?.payload?.language) {
    language = e.payload.language;
    log(`language → ${language}`);
  }
  if (typeof e?.payload?.silence_timeout_ms === 'number') {
    silenceTimeoutMs = e.payload.silence_timeout_ms;
    log(`silenceTimeoutMs → ${silenceTimeoutMs}`);
  }
  if (e?.payload?.engine_id) {
    engineId = e.payload.engine_id;
    log(`engineId → ${engineId}`);
  }
});

await listen('speakly://engine-changed', (e) => {
  const newId = e?.payload?.engineId || 'web-speech';
  log(`engine-changed → ${newId}`);
  engineId = newId;
});

// Pull initial settings on startup.
try {
  const stg = await invoke('get_settings');
  if (stg?.language) language = stg.language;
  if (typeof stg?.silence_timeout_ms === 'number') silenceTimeoutMs = stg.silence_timeout_ms;
  if (stg?.engine_id) engineId = stg.engine_id;
  log(`ready — lang=${language}, engine=${engineId}, SR=${SR ? 'ok' : 'MISSING'}`);
} catch (e) {
  log(`settings load failed: ${e}`);
}
