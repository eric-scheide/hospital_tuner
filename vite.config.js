// vite.config.js — Vite build configuration
// ============================================
// Ref: /contracts/build-pipeline.md for full rationale.

import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import wasm from 'vite-plugin-wasm';

export default defineConfig({
  base: '/static/hospital_tuner/',
  plugins: [
    // Svelte compiler plugin — handles .svelte files
    svelte(),

    // vite-plugin-wasm — enables ?init imports for main-thread Wasm modules.
    // Not used by the AudioWorklet path (worklet loads Wasm via fetch directly),
    // but included for dev convenience and potential future main-thread Wasm use.
    wasm(),
  ],

  build: {
    // es2022 required for:
    //   - Top-level await (Wasm module initialization pattern)
    //   - WebAssembly ESM integration proposal
    //   - AudioWorklet module scripts
    target: 'es2022',
  },

  server: {
    headers: {
      // Required for microphone access via getUserMedia in cross-origin-isolated
      // contexts, and for AudioWorklet reliability across browsers.
      // Also required for SharedArrayBuffer if Wasm threading is added later.
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },

  // The `public/` directory is Vite's default publicDir — no config needed.
  // All files in public/ are served at the root URL as-is, including:
  //   /wasm-pkg/hospital_tuner_dsp_bg.wasm
  //   /wasm-pkg/hospital_tuner_dsp.js
  //
  // The AudioWorklet processor is loaded via:
  //   new URL('./worklet/processor.js', import.meta.url)
  // This pattern causes Vite to emit processor.js as a separate chunk,
  // preventing it from being bundled into the main entry point.
  // processor.js must NOT be in public/ — it uses import() at runtime,
  // and Vite needs to see the new URL(..., import.meta.url) pattern in
  // the calling code to emit it correctly as a separate asset.
});
