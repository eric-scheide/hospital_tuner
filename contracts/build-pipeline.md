# Build Pipeline Contract

**Status: LOCKED**
**Last updated by: Architect Agent**

This document defines the complete build pipeline: what wasm-pack produces,
where the artifacts land, how Vite imports them, and what the npm scripts look
like. Both the DSP subagent (wasm/) and the Build subagent (vite.config.js,
package.json) must conform to this contract.

---

## Overview

```
wasm/ (Rust crate)
  │
  │  wasm-pack build --target web --out-dir ../public/wasm-pkg
  ▼
public/wasm-pkg/          ← static assets, served by Vite as-is
  hospital_tuner_dsp_bg.wasm
  hospital_tuner_dsp.js
  hospital_tuner_dsp.d.ts
  package.json            ← ignored by Vite; used if published to npm
  .gitignore              ← wasm-pack writes this; commit the wasm-pkg dir anyway
  │
  │  loaded by AudioWorklet at runtime (fetch + instantiateStreaming)
  ▼
src/worklet/processor.js  ← AudioWorkletGlobalScope; imports wasm glue + binary
  │
  │  registered via audioContext.audioWorklet.addModule(...)
  ▼
src/App.svelte            ← main thread; posts control messages, receives frames
```

---

## Step 1 — Rust Crate (`wasm/`)

### Directory

```
wasm/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── fft.rs
    └── chroma.rs
```

### Cargo.toml requirements

```toml
[package]
name = "hospital-tuner-dsp"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
wasm-bindgen = "0.2.90"
js-sys = "0.3.67"

[profile.release]
opt-level = "s"       # optimize for binary size
lto = true            # link-time optimization
codegen-units = 1     # maximize LTO effectiveness
panic = "abort"       # smaller panic handler; no unwinding needed in Wasm
```

No `std` features beyond what wasm-bindgen requires. `wee_alloc` is NOT used
(deprecated); the default allocator is acceptable for v1.

---

## Step 2 — wasm-pack Build

### Command

```sh
wasm-pack build wasm/ --target web --out-dir ../public/wasm-pkg
```

Run from the project root (`/Users/eric/hospitalTuner`).

### Flags explained

| Flag | Value | Reason |
|---|---|---|
| `--target` | `web` | Produces ESM glue compatible with `import()` in modern browsers and worklets |
| `--out-dir` | `../public/wasm-pkg` | Lands in the Vite `public/` directory → served at `/wasm-pkg/` |

Do NOT use `--target bundler` (requires Vite to process the .wasm; breaks the
worklet import path). Do NOT use `--target no-modules` (does not produce ESM).

### Output artifacts

| File | Served at | Purpose |
|---|---|---|
| `hospital_tuner_dsp_bg.wasm` | `/wasm-pkg/hospital_tuner_dsp_bg.wasm` | Wasm binary |
| `hospital_tuner_dsp.js` | `/wasm-pkg/hospital_tuner_dsp.js` | ESM JS glue (init + bindings) |
| `hospital_tuner_dsp.d.ts` | not served | TypeScript types (optional IDE benefit) |
| `package.json` | not served | npm publish metadata only |

---

## Step 3 — Vite Configuration

### `vite.config.js`

```js
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import wasm from 'vite-plugin-wasm';

export default defineConfig({
  plugins: [
    svelte(),
    wasm(),          // enables ?init Wasm imports on the main thread (dev convenience)
  ],

  build: {
    target: 'es2022', // required for top-level await and WebAssembly ESM integration
  },

  // The worklet script must NOT be bundled with the main entry point.
  // It is registered via audioContext.audioWorklet.addModule(url) at runtime.
  // Vite handles this correctly when the URL is constructed with:
  //   new URL('./worklet/processor.js', import.meta.url)
  // which causes Vite to emit it as a separate chunk.

  server: {
    headers: {
      // Required for SharedArrayBuffer (not needed in v1, but stubbed here).
      // Uncomment if Wasm threading is added later:
      // 'Cross-Origin-Opener-Policy': 'same-origin',
      // 'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },

  // public/ directory is served as-is; wasm-pkg lives there.
  // No additional publicDir config needed (Vite default is 'public').
});
```

### `vite-plugin-wasm` scope

`vite-plugin-wasm` only handles main-thread Wasm imports (e.g., if a Svelte
component directly `import init from '/wasm-pkg/hospital_tuner_dsp.js'`). The
worklet path does NOT use this plugin — it loads Wasm directly via
`WebAssembly.instantiateStreaming`. The plugin is still included for development
ergonomics and potential future main-thread use.

---

## Step 4 — Worklet Wasm Loading Pattern

The `AudioWorkletGlobalScope` does not support standard ESM `import` statements
at module evaluation time in all browsers. The canonical pattern is:

```js
// src/worklet/processor.js

// The glue module URL is constructed relative to the origin, not the worklet
// file itself, because the worklet file is registered via addModule() and its
// base URL in the globalScope is the AudioWorklet's base URL.
const WASM_PKG_BASE = '/wasm-pkg/hospital_tuner_dsp';

class HospitalTunerProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super(options);
    this._ready = this._initialize();
  }

  async _initialize() {
    // Step 1: import the JS glue (ESM import() works in worklet module scripts)
    const { default: init, TunerProcessor, memory } =
      await import(WASM_PKG_BASE + '.js');

    // Step 2: initialize the Wasm module (fetches and compiles the .wasm binary)
    await init(WASM_PKG_BASE + '_bg.wasm');

    // Step 3: construct the stateful DSP engine
    this._processor = new TunerProcessor(sampleRate);
    this._memory = memory;

    this.port.postMessage({ type: 'ready', sampleRate });
  }

  // ...
}
```

This works because the worklet is registered as a **module** script:
```js
// App.svelte
await audioContext.audioWorklet.addModule(
  new URL('./worklet/processor.js', import.meta.url),
  { credentials: 'omit' }
);
```

Vite emits `processor.js` as a separate chunk at build time due to the
`new URL(..., import.meta.url)` pattern.

---

## Step 5 — npm Scripts

```json
{
  "scripts": {
    "build:wasm": "wasm-pack build wasm/ --target web --out-dir ../public/wasm-pkg",
    "dev": "npm run build:wasm && vite",
    "build": "npm run build:wasm && vite build",
    "preview": "vite preview"
  }
}
```

During development (`npm run dev`), `wasm-pack` runs once at startup. The Rust
source is not watched by Vite's HMR — re-run `npm run build:wasm` manually
after editing Rust code, then Vite's HMR will pick up the new `.wasm` and
`.js` glue files from `public/wasm-pkg/`.

---

## Step 6 — `package.json` devDependencies

```json
{
  "devDependencies": {
    "vite": "^5.0.0",
    "@sveltejs/vite-plugin-svelte": "^3.0.0",
    "svelte": "^4.0.0",
    "vite-plugin-wasm": "^3.3.0"
  }
}
```

No runtime npm dependencies. The Wasm binary is loaded at runtime from
the `public/` static directory, not via npm.

---

## Artifact Lifecycle

| Phase | Who produces | Where |
|---|---|---|
| `wasm-pack build` | DSP subagent (wasm/) | `public/wasm-pkg/` |
| Vite dev server | Build subagent | serves `public/` at `/` |
| `vite build` | Build subagent | `dist/` (including copied `public/wasm-pkg/`) |
| Deployment | CI / static host | serve `dist/` as static files |

The `public/wasm-pkg/` directory SHOULD be committed to git so the project
can `npm run dev` without requiring the Rust toolchain. The CI/CD pipeline
regenerates it from source on each release build.

---

## Constraints and Non-Goals

- Do NOT use `import.meta.env` inside `processor.js` — it is not available in
  the AudioWorkletGlobalScope.
- Do NOT use `vite-plugin-top-level-await` — not needed with `es2022` target.
- Do NOT place the wasm-pkg output inside `src/` — Vite would attempt to
  process it as a bundled module, breaking the worklet import path.
- The `.wasm` binary must be served with `Content-Type: application/wasm`
  for `instantiateStreaming` to work. Vite's dev server does this automatically.
  Production hosting must be configured accordingly.
