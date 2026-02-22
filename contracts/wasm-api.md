# WASM API Contract — DSP → AudioWorklet Boundary

**Status: LOCKED**
**Last updated by: Architect Agent**

This document is the authoritative specification for the Rust/WebAssembly API
surface. The DSP subagent (wasm/) must implement exactly this interface.
The Worklet subagent (src/worklet/processor.js) must consume exactly this
interface. Neither side may deviate without a contract revision approved by
the Architect.

---

## Crate Identity

| Field | Value |
|---|---|
| Crate name (Cargo.toml) | `hospital-tuner-dsp` |
| JS module name (generated) | `hospital_tuner_dsp` |
| wasm-pack target | `web` |
| Output directory | `public/wasm-pkg/` |

---

## Constants (fixed, not tunable at runtime)

| Constant | Value | Rationale |
|---|---|---|
| FFT_SIZE | 4096 | ~10.77 Hz/bin @ 44100 Hz; resolves A1=55 Hz cleanly |
| RING_BUFFER_SIZE | 4096 | Matches FFT_SIZE; newest 4096 samples are windowed each frame |
| HOP_SIZE | 512 | New frame computed every 512 input samples; ~86 fps @ 44100 Hz |
| LOWEST_FREQ | 55.0 Hz | A1; covers bass guitar |
| HARMONIC_ATTENUATION_DB | 6.0 | Suppress harmonic if stronger fundamental exists within 6 dB |

---

## Types Exposed via `#[wasm_bindgen]`

### `ChromaResult`

Returned by `TunerProcessor::process()`. JS caller **must call `.free()` on
this object** immediately after reading all fields to avoid a memory leak in
the Wasm heap.

```rust
#[wasm_bindgen]
pub struct ChromaResult { /* opaque */ }

#[wasm_bindgen]
impl ChromaResult {
    /// Returns a pointer into Wasm linear memory at the start of the 12-element
    /// chroma array. Valid only while this ChromaResult is alive (before .free()).
    /// JS reads it as: new Float32Array(memory.buffer, result.chroma_ptr(), 12)
    pub fn chroma_ptr(&self) -> u32;

    /// Always returns 12. Provided for symmetry / defensive JS code.
    pub fn chroma_len(&self) -> u32;

    /// Active pitch class indices (0=C … 11=B) whose normalized chroma energy
    /// is >= presence_threshold. Transferred as a Uint8Array copy (not a view).
    /// Length is 0 if silent.
    pub fn active_pitch_classes(&self) -> Vec<u8>;

    /// Hz of the strongest surviving spectral peak after gating.
    /// Returns 0.0 if the frame is silent (no peaks survive the gate).
    pub fn dominant_frequency(&self) -> f32;

    /// Cents deviation of dominant_frequency from the nearest equal-temperament
    /// semitone. Range: −50 to +50. Returns 0 if dominant_frequency == 0.0.
    pub fn cents(&self) -> i32;
}
```

#### Chroma Array Layout

Index 0 = C, Index 1 = C#/Db, Index 2 = D, …, Index 11 = B.
All values are normalized to the range [0.0, 1.0] where 1.0 is the
energy of the strongest surviving pitch class in the current frame.
A value of 0.0 means no detected energy in that pitch class.

#### Reading `chroma_ptr` from JS (canonical pattern)

```js
// wasm_memory is the WebAssembly.Memory exported as "memory" from the glue module
const chroma = new Float32Array(
  wasm_memory.buffer,
  result.chroma_ptr(),
  12
).slice(); // .slice() copies before calling .free()
result.free();
```

---

### `TunerProcessor`

Stateful DSP engine. One instance per AudioWorklet. Constructed once in the
worklet constructor; `process()` called on every hop boundary.

```rust
#[wasm_bindgen]
pub struct TunerProcessor { /* opaque */ }

#[wasm_bindgen]
impl TunerProcessor {
    /// Constructor. sample_rate must be the AudioContext sample rate (typically
    /// 44100.0 or 48000.0). Panics (traps) if sample_rate <= 0.
    #[wasm_bindgen(constructor)]
    pub fn new(sample_rate: f32) -> TunerProcessor;

    /// Feed raw PCM float32 samples into the ring buffer. samples.length must
    /// equal the AudioWorklet render quantum size (always 128 in current spec).
    /// Returns a ChromaResult if a complete hop (HOP_SIZE=512 new samples) has
    /// accumulated since the last frame, otherwise returns null/undefined.
    ///
    /// Ownership: if a ChromaResult is returned, JS owns it and must call .free().
    ///
    /// This function must complete in < 3ms. It must not allocate on the heap
    /// during steady-state operation (pre-allocate all buffers in ::new()).
    pub fn process(&mut self, samples: &[f32]) -> Option<ChromaResult>;

    /// Set the spectral gate ratio. Bins below (noise_floor * ratio) are zeroed.
    /// Default: 1.5. Valid range: 0.5–10.0. Clamped silently outside range.
    pub fn set_gate_ratio(&mut self, ratio: f32);

    /// Set the chroma presence threshold. Pitch classes with normalized chroma
    /// energy below this value are excluded from active_pitch_classes.
    /// Default: 0.15. Valid range: 0.0–1.0. Clamped silently.
    pub fn set_presence_threshold(&mut self, threshold: f32);

    /// Toggle harmonic suppression. Default: true (enabled).
    pub fn set_harmonic_suppression(&mut self, enabled: bool);

    /// Flush the ring buffer and reset the noise floor estimator.
    /// Call when the audio stream is interrupted or restarted.
    pub fn reset(&mut self);
}
```

---

## Exports Required from the Wasm Module

The wasm-pack JS glue (`hospital_tuner_dsp.js`) must re-export:

| Export name | Type | Purpose |
|---|---|---|
| `default` | `async fn(input?) -> WasmModule` | Module init (fetches + compiles .wasm) |
| `TunerProcessor` | class | Main DSP engine |
| `ChromaResult` | class | Result type (needed for instanceof checks) |
| `memory` | `WebAssembly.Memory` | Required for `chroma_ptr()` zero-copy read |

The `memory` export is produced by passing `{ wasm-bindgen: { weak-refs: false } }`
or by default in wasm-bindgen ≥ 0.2.84.

---

## Error / Edge Case Contracts

| Situation | Behavior |
|---|---|
| `process()` called with samples.length != 128 | Debug build: panic. Release build: silently truncate/pad. |
| `sample_rate` not in [8000, 192000] | Panic (constructor precondition). |
| All samples in a frame are 0.0 (silence) | Returns ChromaResult with chroma all-zeros, dominant_frequency=0.0, cents=0, active_pitch_classes empty. |
| `set_gate_ratio(0.0)` | Clamped to 0.5 (minimum). |

---

## Version

This contract applies to wasm-bindgen **0.2.90** or later.
Pin the exact version in `wasm/Cargo.toml`.
