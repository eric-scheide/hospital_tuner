# Waterfall Chromatic Tuner — Design Doc

## Overview

A browser-based chromatic tuner with a real-time **chromagram waterfall** display. No server required. Runs entirely in the browser.

The key visual idea: the display shows **12 pitch-class columns** (C through B), one octave wide. Every C across all octaves (C1, C2, C3, …) contributes energy to the same "C" column. The newest row is always painted at the **bottom**; old content shifts upward (the canvas image scrolls up).

---

## Goals

- Sub-20ms latency from audio input to display update
- No audio glitches caused by main-thread jank
- Accurate pitch detection across the full chromatic scale
- Lightweight — loads fast, runs on modest hardware

---

## Tech Stack

| Layer | Choice | Reason |
|---|---|---|
| UI Framework | Svelte | No VDOM overhead; compiles to small, fast vanilla JS |
| DSP / FFT + Chroma | Rust → WebAssembly | No GC pauses; near-native perf for signal processing |
| Audio Pipeline | AudioWorklet | Runs off main thread; real-time audio thread in browser |
| Visualization | Canvas 2D API | Fastest path to pixels; no library overhead |
| Build System | Vite + `wasm-pack` | First-class Wasm support; fast HMR for Svelte |

---

## Architecture

```
[Microphone]
     │
     ▼
[AudioWorklet Thread]  ← Rust/Wasm loaded here
  - Receives raw PCM chunks (128 samples each)
  - Accumulates into ring buffer
  - Windowed FFT → magnitude spectrum
  - Chroma folding → 12-bin chroma vector
  - Pitch detection → detected note + cents deviation
  - Packages message: { chroma[12], detectedPitchClass, cents, frequency }
     │
     │  postMessage (~60/sec)
     ▼
[Main Thread — Svelte Component]
  - Receives message
  - Paints one new ROW on the chromagram canvas (scrolls downward)
  - Updates tuner needle / note name display
  - Does NOT do any DSP
```

### Key Invariant
The AudioWorklet thread **never blocks** on the main thread. The main thread is "display only."

### Inter-Module Contracts
All boundary specifications are locked in `contracts/`. Do not implement across
a boundary without reading the corresponding contract first.

| Boundary | Contract file |
|---|---|
| Rust/Wasm API (DSP → Worklet) | `contracts/wasm-api.md` |
| postMessage schema (Worklet → Main thread) | `contracts/postmessage-schema.md` |
| Build pipeline (wasm-pack → Vite) | `contracts/build-pipeline.md` |

---

## DSP Pipeline (Rust/Wasm)

### Core Philosophy: Peak-Gated Chroma
Simple chroma folding sums *all* spectral energy — including harmonics — into pitch-class bins. This makes chords unreadable because a single C note's overtones pollute the E and G columns. Instead, we gate on **spectral peaks above a dynamic noise floor**, so only fundamentals (and strong partials) survive into the chroma.

### Steps

1. **Input**: 128-sample PCM float32 chunks from the AudioWorklet processor callback
2. **Accumulate**: Ring buffer — hold the most recent N samples (e.g. 4096)
3. **Window**: Apply Hann window to reduce spectral leakage
4. **FFT**: Compute real-valued FFT → magnitude spectrum (linear, not dB yet)
5. **Noise Floor Estimation**: Rolling average of the magnitude spectrum (exponential moving average, slow decay). Represents the "ambient" spectral level.
6. **Spectral Whitening**: Divide each bin by its local spectral mean (computed over a narrow band of neighbors). This flattens the broad tilt of the spectrum and makes peaks stand out relative to their local context — not relative to the global maximum.
7. **Threshold Gate**: Zero out any bin below `noise_floor * gate_ratio` (e.g. 1.5×). What remains are only significant peaks.
8. **Peak Picking**: Find local maxima in the gated spectrum. A bin is a peak if it is greater than both neighbors. This gives a sparse set of candidate fundamentals and partials.
9. **Harmonic Suppression** *(optional, improves chord accuracy)*: For each identified peak, check if it is approximately a harmonic (2×, 3×, 4×…) of a stronger lower peak. If so, attenuate it. This reduces harmonic crosstalk between chord columns.
10. **Chroma Mapping**: Map only the surviving peaks to pitch classes (`semitone % 12`). Accumulate their magnitudes into a 12-bin chroma vector.
11. **Normalize**: Scale chroma vector to 0.0–1.0
12. **Chord Candidate Detection**: Report the top N pitch classes above a presence threshold as active chord tones.
13. **Output message**:
    - `chroma: [f32; 12]` — gated energy per pitch class, C=0 … B=11
    - `activePitchClasses: u8[]` — indices with energy above presence threshold (the chord)
    - `dominantFrequency: f32` — Hz of the strongest peak
    - `cents: i32` — deviation of dominant peak from nearest semitone (−50 to +50)

### Why This Enables Chord Display
| Approach | C major chord result |
|---|---|
| Raw chroma fold | C bright, but E and G columns also lit from C's overtones (3rd, 5th harmonic) |
| Peak-gated chroma | Only C, E, G peaks survive the gate — three distinct bright columns |

The threshold is the key parameter. Too aggressive → fundamentals disappear. Too loose → harmonics bleed back in. The gate ratio should be tunable in the UI.

---

## Visualization

### Chromagram Canvas

```
  C  C# D  D# E  F  F# G  G# A  A# B
  |  |  |  |  |  |  |  |  |  |  |  |
  ████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   ← oldest row (top)
  ░░░░░░░░░░░░████░░░░░░░░░░░░░░░░░░
  ░░░░░░░░░░░░░░░░░░░░████░░░░░░░░░░
  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░████░░   ← newest row (bottom)
```

- **X axis**: 12 equal-width columns, one per pitch class (C … B)
- **Y axis**: time — newest row painted at bottom, canvas scrolls upward each frame
- **Color**: chroma energy → heat map (e.g. black → blue → cyan → yellow → white)
- **Highlight**: the detected pitch class column gets a subtle glow/outline overlay
- **Grid lines**: thin lines between columns; note names along the top (static header)

### Scroll Mechanic
Each frame: `drawImage(canvas, 0, 0, w, h, 0, -1, w, h)` shifts everything up 1px, then paint the new row at `y = h - 1`.

### Tuner Overlay (below or beside canvas)
- Large note name with octave (e.g. "A4") — derived from `frequency`, not pitch class alone
- Cents deviation bar (−50 … 0 … +50) with color: green at center, red at edges
- Frequency readout in Hz

---

## Canvas Layout (sketch)

```
┌─────────────────────────────────────────────┐
│  C  C# D  D# E  F  F# G  G# A  A# B        │  ← static note header
├──┬──┬──┬──┬──┬──┬──┬──┬──┬──┬──┬──┤        │
│  │  │  │  │  │  │  │  │  │  │  │  │        │
│  │  │  │  │  │  │  │  │  │  │  │  │        │  ← chromagram canvas
│  │  │  │  │  │  │  │  │  │  │  │  │        │    (scrolls upward)
│  │  │  │  │  │  │  │██ │  │  │  │  │        │
└──┴──┴──┴──┴──┴──┴──┴──┴──┴──┴──┴──┘        │
│         A4  440.2 Hz  [────●────]  +2¢      │  ← tuner strip
└─────────────────────────────────────────────┘
```

---

## File Structure (final — scaffolded by Architect Agent)

```
hospitalTuner/
├── contracts/                      # Inter-module contracts (locked by Architect)
│   ├── wasm-api.md                 # Rust/wasm_bindgen API surface
│   ├── postmessage-schema.md       # AudioWorklet → main thread message schema
│   └── build-pipeline.md          # wasm-pack output, Vite import, npm scripts
├── src/
│   ├── App.svelte                  # Root layout; AudioContext + worklet lifecycle
│   ├── components/
│   │   ├── Chromagram.svelte       # Canvas waterfall + heat-map paint
│   │   └── TunerStrip.svelte       # Note name, cents bar, Hz readout
│   └── worklet/
│       └── processor.js            # AudioWorkletProcessor — loads Wasm, posts frames
├── wasm/                           # Rust crate (hospital-tuner-dsp)
│   ├── src/
│   │   ├── lib.rs                  # wasm_bindgen entry: TunerProcessor, ChromaResult
│   │   ├── fft.rs                  # Hann window + radix-2 FFT + magnitude spectrum
│   │   └── chroma.rs               # Full peak-gated chroma pipeline (8 steps)
│   └── Cargo.toml
├── public/
│   └── wasm-pkg/                   # wasm-pack output (committed; rebuilt by CI)
│       ├── hospital_tuner_dsp_bg.wasm
│       └── hospital_tuner_dsp.js
├── vite.config.js
└── package.json
```

Note: `pitch.rs` from the original sketch is merged into `chroma.rs`
(the `dominant_peak()` function). There is no separate HPS module in v1.

---

## Decisions (locked by Architect Agent — 2026-02-19)

All open questions resolved. Do not change these without a contract revision.

| Decision | Choice | Rationale |
|---|---|---|
| **FFT size** | **4096** | ~10.77 Hz/bin @ 44100 Hz; resolves A1=55 Hz (bin ≈5) cleanly. Ring buffer = 4096 samples. |
| **Hop size** | **512 samples** | New DSP frame every 512 input samples ≈ 86 fps @ 44100 Hz. Display throttled to ≤60 fps via rAF. |
| **Gate ratio** | **1.5× default**, UI slider exposed | Tunable via `setGateRatio` control message; range 0.5–10.0. |
| **Harmonic suppression** | **Included in v1** | Suppress peak if a stronger peak exists at its half/third/quarter frequency (6 dB threshold). Enabled by default; toggled via `setHarmonicSuppression`. |
| **Presence threshold** | **0.15** (of normalized chroma max) | `activePitchClasses` includes bins ≥ 0.15. Exposed as `setPresenceThreshold`. |
| **Scroll direction** | Oldest content scrolls **upward**; newest row always at **bottom** | `drawImage(canvas, 0,0,W,H, 0,-1,W,H)` then paint at `y = H−1`. |
| **Scroll speed** | **1 px/frame** at ≤60 fps | ≈8 seconds of history in a 512px-tall canvas. Fixed in v1; adjustable in v2. |
| **Color palette** | **Heat map**: black→blue→cyan→yellow→white | Continuous energy mapping; more musical than binary. |
| **Lowest note** | **A1 (55 Hz)** | Covers bass guitar. Bins below A1 are excluded from chroma folding. |
| **Column layout** | **12 equal-width columns** | Simpler; piano layout deferred to v2. |
| **Wasm threading** | **Single-threaded** | No SharedArrayBuffer / COOP/COEP headers needed in v1. Config stubs left in `vite.config.js` for future upgrade. |
| **Framework** | **Plain Svelte** (not SvelteKit) | No routing needed; smaller bundle. |
| **A4 reference** | **440 Hz** | Standard concert pitch; used for cents deviation and octave labeling. |

---

## Non-Goals (v1)

- No server, no accounts, no data persistence
- No mobile-specific UI (desktop browser first)
- No polyphonic chord detection
- No recording / export

---

## Performance Budget

| Metric | Target |
|---|---|
| AudioWorklet callback duration | < 3ms |
| Main thread frame time | < 8ms |
| Initial JS bundle | < 100KB gzip |
| Wasm binary | < 200KB |
