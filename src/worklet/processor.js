/**
 * processor.js — AudioWorkletProcessor
 * ======================================
 * Runs inside the AudioWorkletGlobalScope (a dedicated audio thread).
 *
 * Dynamic import() is NOT available in AudioWorkletGlobalScope, so the
 * wasm-bindgen JS glue is inlined directly into this file. The main thread
 * pre-compiles the WebAssembly.Module and passes it via processorOptions
 * to avoid needing fetch() in the worklet.
 *
 * Contracts:
 *   - /contracts/wasm-api.md        — Wasm types and method signatures
 *   - /contracts/postmessage-schema.md — Message shapes
 *   - /contracts/build-pipeline.md  — Wasm loading pattern
 */

// ---------------------------------------------------------------------------
// Inline wasm-bindgen glue (from hospital_tuner_dsp.js)
// ---------------------------------------------------------------------------

let wasm;
let WASM_VECTOR_LEN = 0;

let cachedFloat32ArrayMemory0 = null;
function getFloat32ArrayMemory0() {
  if (cachedFloat32ArrayMemory0 === null || cachedFloat32ArrayMemory0.byteLength === 0) {
    cachedFloat32ArrayMemory0 = new Float32Array(wasm.memory.buffer);
  }
  return cachedFloat32ArrayMemory0;
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
  if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
    cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
  }
  return cachedUint8ArrayMemory0;
}

function passArrayF32ToWasm0(arg, malloc) {
  const ptr = malloc(arg.length * 4, 4) >>> 0;
  getFloat32ArrayMemory0().set(arg, ptr / 4);
  WASM_VECTOR_LEN = arg.length;
  return ptr;
}

function getArrayU8FromWasm0(ptr, len) {
  ptr = ptr >>> 0;
  return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedTextDecoder = null;
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;

function getTextDecoder() {
  if (!cachedTextDecoder) {
    if (typeof TextDecoder !== 'undefined') {
      cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
      cachedTextDecoder.decode();
    }
  }
  return cachedTextDecoder;
}

function decodeText(ptr, len) {
  const decoder = getTextDecoder();
  if (!decoder) {
    // Fallback for environments without TextDecoder (e.g. Firefox AudioWorkletGlobalScope)
    const bytes = getUint8ArrayMemory0().subarray(ptr, ptr + len);
    let str = '';
    for (let i = 0; i < bytes.length; i++) str += String.fromCharCode(bytes[i]);
    return str;
  }
  numBytesDecoded += len;
  if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
    cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
    cachedTextDecoder.decode();
    numBytesDecoded = len;
  }
  return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

function getStringFromWasm0(ptr, len) {
  ptr = ptr >>> 0;
  return decodeText(ptr, len);
}

// FinalizationRegistry for preventing Wasm heap leaks if JS forgets .free()
const ChromaResultFinalization = (typeof FinalizationRegistry === 'undefined')
  ? { register: () => {}, unregister: () => {} }
  : new FinalizationRegistry(ptr => wasm.__wbg_chromaresult_free(ptr >>> 0, 1));
const TunerProcessorFinalization = (typeof FinalizationRegistry === 'undefined')
  ? { register: () => {}, unregister: () => {} }
  : new FinalizationRegistry(ptr => wasm.__wbg_tunerprocessor_free(ptr >>> 0, 1));

class ChromaResult {
  static __wrap(ptr) {
    ptr = ptr >>> 0;
    const obj = Object.create(ChromaResult.prototype);
    obj.__wbg_ptr = ptr;
    ChromaResultFinalization.register(obj, obj.__wbg_ptr, obj);
    return obj;
  }
  __destroy_into_raw() {
    const ptr = this.__wbg_ptr;
    this.__wbg_ptr = 0;
    ChromaResultFinalization.unregister(this);
    return ptr;
  }
  free() {
    const ptr = this.__destroy_into_raw();
    wasm.__wbg_chromaresult_free(ptr, 0);
  }
  active_pitch_classes() {
    const ret = wasm.chromaresult_active_pitch_classes(this.__wbg_ptr);
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
  }
  cents() {
    return wasm.chromaresult_cents(this.__wbg_ptr);
  }
  chroma_len() {
    return wasm.chromaresult_chroma_len(this.__wbg_ptr) >>> 0;
  }
  chroma_freqs_ptr() {
    return wasm.chromaresult_chroma_freqs_ptr(this.__wbg_ptr) >>> 0;
  }
  chroma_max() {
    return wasm.chromaresult_chroma_max(this.__wbg_ptr);
  }
  chroma_ptr() {
    return wasm.chromaresult_chroma_ptr(this.__wbg_ptr) >>> 0;
  }
  dominant_frequency() {
    return wasm.chromaresult_dominant_frequency(this.__wbg_ptr);
  }
}

class TunerProcessor {
  __destroy_into_raw() {
    const ptr = this.__wbg_ptr;
    this.__wbg_ptr = 0;
    TunerProcessorFinalization.unregister(this);
    return ptr;
  }
  free() {
    const ptr = this.__destroy_into_raw();
    wasm.__wbg_tunerprocessor_free(ptr, 0);
  }
  constructor(sample_rate) {
    const ret = wasm.tunerprocessor_new(sample_rate);
    this.__wbg_ptr = ret >>> 0;
    TunerProcessorFinalization.register(this, this.__wbg_ptr, this);
    return this;
  }
  process(samples) {
    const ptr0 = passArrayF32ToWasm0(samples, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.tunerprocessor_process(this.__wbg_ptr, ptr0, len0);
    return ret === 0 ? undefined : ChromaResult.__wrap(ret);
  }
  reset() {
    wasm.tunerprocessor_reset(this.__wbg_ptr);
  }
  set_gate_ratio(ratio) {
    wasm.tunerprocessor_set_gate_ratio(this.__wbg_ptr, ratio);
  }
  set_harmonic_suppression(enabled) {
    wasm.tunerprocessor_set_harmonic_suppression(this.__wbg_ptr, enabled);
  }
  set_harmonic_strength(strength) {
    wasm.tunerprocessor_set_harmonic_strength(this.__wbg_ptr, strength);
  }
  set_harmonic_max_ratio(max_ratio) {
    wasm.tunerprocessor_set_harmonic_max_ratio(this.__wbg_ptr, max_ratio);
  }
  set_presence_threshold(threshold) {
    wasm.tunerprocessor_set_presence_threshold(this.__wbg_ptr, threshold);
  }
}

function __wbg_get_imports() {
  return {
    __proto__: null,
    "./hospital_tuner_dsp_bg.js": {
      __proto__: null,
      __wbg___wbindgen_throw_be289d5034ed271b: function(arg0, arg1) {
        throw new Error(getStringFromWasm0(arg0, arg1));
      },
      __wbindgen_init_externref_table: function() {
        const table = wasm.__wbindgen_externrefs;
        const offset = table.grow(4);
        table.set(0, undefined);
        table.set(offset + 0, undefined);
        table.set(offset + 1, null);
        table.set(offset + 2, true);
        table.set(offset + 3, false);
      },
    },
  };
}

/**
 * Initialize the Wasm module from a pre-compiled WebAssembly.Module.
 * Synchronous — no fetch or async operations needed.
 */
function initWasmSync(wasmModule) {
  const imports = __wbg_get_imports();
  const instance = new WebAssembly.Instance(wasmModule, imports);
  wasm = instance.exports;
  cachedFloat32ArrayMemory0 = null;
  cachedUint8ArrayMemory0 = null;
  wasm.__wbindgen_start();
}

/**
 * Fallback: initialize the Wasm module by fetching the binary directly.
 * Used if the main thread did not pass a pre-compiled Module.
 */
async function initWasmFromFetch(wasmUrl) {
  const imports = __wbg_get_imports();
  const { instance } = await WebAssembly.instantiateStreaming(
    fetch(wasmUrl),
    imports,
  );
  wasm = instance.exports;
  cachedFloat32ArrayMemory0 = null;
  cachedUint8ArrayMemory0 = null;
  wasm.__wbindgen_start();
}

// ---------------------------------------------------------------------------
// AudioWorkletProcessor
// ---------------------------------------------------------------------------

const WASM_URL = '../wasm-pkg/hospital_tuner_dsp_bg.wasm';

class HospitalTunerProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super(options);

    this._processor = null;
    this._ready     = false;

    this.port.onmessage = this._handleMessage.bind(this);

    const wasmModule = options.processorOptions?.wasmModule;
    if (wasmModule instanceof WebAssembly.Module) {
      // Fast path: main thread pre-compiled the Module — init synchronously.
      try {
        initWasmSync(wasmModule);
        this._processor = new TunerProcessor(sampleRate);
        this._ready = true;
        this.port.postMessage({ type: 'ready', sampleRate });
      } catch (err) {
        this.port.postMessage({
          type: 'error',
          message: err.message || String(err),
          stack: err.stack,
        });
      }
    } else {
      // Fallback: fetch the .wasm binary from within the worklet.
      this._initialize().catch((err) => {
        this.port.postMessage({
          type: 'error',
          message: err.message || String(err),
          stack: err.stack,
        });
      });
    }
  }

  async _initialize() {
    await initWasmFromFetch(WASM_URL);
    this._processor = new TunerProcessor(sampleRate);
    this._ready = true;
    this.port.postMessage({ type: 'ready', sampleRate });
  }

  _handleMessage(event) {
    if (!this._ready) return;

    const { type, value, enabled } = event.data;
    switch (type) {
      case 'setGateRatio':
        this._processor.set_gate_ratio(value);
        break;
      case 'setPresenceThreshold':
        this._processor.set_presence_threshold(value);
        break;
      case 'setHarmonicSuppression':
        this._processor.set_harmonic_suppression(enabled);
        break;
      case 'setHarmonicStrength':
        this._processor.set_harmonic_strength(value);
        break;
      case 'setHarmonicMaxRatio':
        this._processor.set_harmonic_max_ratio(value);
        break;
      case 'reset':
        this._processor.reset();
        break;
    }
  }

  process(inputs, outputs, parameters) {
    if (!this._ready || !inputs[0] || !inputs[0][0]) return true;

    try {
      const samples = inputs[0][0];
      const result = this._processor.process(samples);

      if (result !== null && result !== undefined) {
        const chroma = new Float32Array(
          wasm.memory.buffer,
          result.chroma_ptr(),
          result.chroma_len(),
        ).slice();

        const chromaFreqs = new Float32Array(
          wasm.memory.buffer,
          result.chroma_freqs_ptr(),
          12,
        ).slice();

        const activePitchClasses = result.active_pitch_classes();
        const chromaMax          = result.chroma_max();
        const dominantFrequency  = result.dominant_frequency();
        const cents              = result.cents();

        result.free();

        this.port.postMessage(
          {
            type: 'tunerFrame',
            chroma,
            chromaFreqs,
            chromaMax,
            activePitchClasses,
            dominantFrequency,
            cents,
            timestamp: currentTime * 1000,
          },
          [chroma.buffer, chromaFreqs.buffer, activePitchClasses.buffer],
        );
      }
    } catch (err) {
      try {
        this.port.postMessage({
          type: 'error',
          message: err.message || String(err),
          stack: err.stack,
        });
      } catch (_) {
        // port closed — give up silently
      }
    }

    return true;
  }
}

registerProcessor('hospital-tuner-processor', HospitalTunerProcessor);
