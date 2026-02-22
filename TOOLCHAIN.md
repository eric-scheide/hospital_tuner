# TOOLCHAIN.md — Build Pipeline Documentation

**Project:** Hospital Tuner
**Stack:** Rust/Wasm (wasm-pack) + Svelte + Vite
**Last updated:** 2026-02-19

---

## Overview

The build pipeline has two independent stages:

```
Stage 1: Rust → Wasm
  wasm-pack build wasm/ --target web --out-dir ../public/wasm-pkg

Stage 2: Svelte + Vite → Browser bundle
  vite build   (or vite for dev server)
```

These stages are wired together via npm scripts. Stage 1 always runs first
because the Wasm artifacts must exist in `public/wasm-pkg/` before Vite starts
(the dev server and build both serve those files statically).

---

## Why `--target web` (not `--target bundler`)

`wasm-pack` offers several output targets. The critical distinction for this
project is:

| Target      | .wasm handling              | ESM glue style          | Usable in AudioWorklet? |
|-------------|-----------------------------|-------------------------|-------------------------|
| `web`       | Fetched at runtime via URL  | `init(wasmUrl)` pattern | YES                     |
| `bundler`   | Imported by bundler as asset| Vite processes the .wasm| NO (breaks worklet path)|
| `no-modules`| Fetched at runtime via URL  | Global script style     | NO (not ESM)            |

With `--target bundler`, Vite processes the `.wasm` file through its module
graph. This means the `.wasm` gets a content-hashed filename unknown at
authoring time, and import paths inside the glue JS are bundler-specific. The
`AudioWorkletGlobalScope` cannot use Vite's module resolution — it loads scripts
as plain URLs. At runtime the worklet would get a 404.

With `--target web`, the `.wasm` glue uses `init(explicitUrl)` where the URL
is passed as an argument. The worklet hardcodes `/wasm-pkg/hospital_tuner_dsp_bg.wasm`
— a stable, origin-relative path served directly from `public/`. This works
regardless of whether the caller is the main thread or a worklet thread.

---

## Why Wasm Goes into `public/` (not `src/`)

Vite treats files inside `src/` as source modules — it processes them through
its bundler pipeline: transforms, tree-shaking, content hashing, etc.

Files inside `public/` are copied verbatim to `dist/` with no transformation.
They are served at a stable URL (e.g., `/wasm-pkg/hospital_tuner_dsp.js`)
regardless of the Vite build graph.

The `AudioWorkletGlobalScope` loads scripts via `addModule(url)` and dynamic
`import(url)` — plain HTTP URLs resolved from the browser's origin. It has no
access to Vite's module resolution, HMR, or bundled import maps. The worklet
code hardcodes `/wasm-pkg/...` URLs because those paths are stable and
origin-relative. If the Wasm artifacts were in `src/`, Vite would rename and
relocate them, breaking those hardcoded paths at runtime.

Additionally, keeping the Wasm artifacts outside the Vite module graph avoids
a class of subtle bugs where Vite's import analysis incorrectly tries to resolve
WebAssembly binary imports as JavaScript modules.

---

## COOP / COEP Header Requirement

The dev server and production host must send these HTTP headers on every response:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

These headers establish a **cross-origin isolated** browsing context. Without
them, the following break:

| Feature                    | Symptom without headers                                    |
|----------------------------|------------------------------------------------------------|
| `navigator.mediaDevices.getUserMedia` | May be blocked in some browser/OS combinations  |
| `AudioWorklet` (reliable)  | Some browsers restrict worklet module scripts without COEI |
| `SharedArrayBuffer`        | Entirely blocked — required if Wasm threading is added     |
| `WebAssembly.Memory` shared| Blocked — required for future multi-threaded DSP           |

In v1 the primary reason is microphone access reliability and AudioWorklet
module script loading across all major browsers (Chrome, Firefox, Safari).
The headers are cheap to send and unlock SharedArrayBuffer for free, enabling
Wasm threading as a zero-cost future upgrade.

**What breaks if you omit them:**
- Firefox blocks `getUserMedia` in non-secure or non-isolated contexts
- Safari may refuse to load AudioWorklet module scripts
- `SharedArrayBuffer` throws a `ReferenceError` — adding threading later would
  require a deployment change anyway, so set the headers now

The headers are set in `vite.config.js` under `server.headers`. For production
deployment, configure the static file host (nginx, Cloudflare, Netlify, etc.)
to send the same headers on all responses.

---

## Worklet File Strategy: Vite Chunk (not `public/`)

The `processor.js` AudioWorklet file lives at `src/worklet/processor.js` and
is loaded via this pattern in `App.svelte`:

```js
await audioContext.audioWorklet.addModule(
  new URL('./worklet/processor.js', import.meta.url)
);
```

The `new URL('./worklet/processor.js', import.meta.url)` pattern is recognized
by Vite's static analysis. Vite emits `processor.js` as a **separate output
chunk** with a stable URL at build time, and resolves the `new URL(...)` call
to that chunk's final URL. The browser receives the correct URL at runtime.

**Why NOT copy it to `public/worklet/processor.js`:**

If `processor.js` were placed in `public/`, it would be served at a known URL
(`/worklet/processor.js`) but Vite would not analyze or transform it. This is
fine today, but creates a maintenance hazard: any future `import` statements
inside the worklet that reference other source files (e.g., shared constants)
would silently break at build time because Vite wouldn't bundle those
dependencies into the worklet chunk. Using the `new URL(..., import.meta.url)`
pattern keeps the worklet in the Vite build graph, enabling future bundling of
worklet dependencies while still emitting it as a separate chunk inaccessible
to `addModule()`.

The worklet itself loads Wasm via hardcoded origin-relative URLs
(`/wasm-pkg/...`) rather than Vite-bundled imports, because dynamic `import()`
in `AudioWorkletGlobalScope` resolves URLs relative to the origin, not relative
to the worklet file. This is correct and intentional.

---

## Why `vite-plugin-top-level-await` Is NOT Included

The `contracts/build-pipeline.md` explicitly states:

> Do NOT use `vite-plugin-top-level-await` — not needed with `es2022` target.

With `build.target = 'es2022'`, Vite outputs native top-level `await` syntax.
All modern browsers that support AudioWorklet also support top-level await
natively. The plugin exists only as a polyfill for older targets (e.g., `es2017`,
`es2019`) that transpile top-level await into generator wrapper functions.
Including it with `es2022` would add unnecessary complexity and a larger bundle.

---

## npm Scripts Reference

| Script         | Command                                          | Purpose                              |
|----------------|--------------------------------------------------|--------------------------------------|
| `build:wasm`   | `wasm-pack build wasm/ --target web --out-dir ../public/wasm-pkg` | Compile Rust → Wasm artifacts |
| `dev`          | `npm run build:wasm && vite`                     | Build Wasm once, then start dev server with HMR |
| `build`        | `npm run build:wasm && vite build`               | Full production build                |
| `preview`      | `vite preview`                                   | Serve the `dist/` output locally     |

**Gotcha — Rust changes during dev:** Vite's HMR does not watch `wasm/src/**.rs`.
After editing Rust source, manually run `npm run build:wasm` to regenerate the
artifacts in `public/wasm-pkg/`. Vite will then pick up the new `.wasm` and
`.js` glue files because they are served as static assets (file changes in
`public/` are observed by Vite's dev server file watcher).

---

## Production Deployment Checklist

1. Run `npm run build` — outputs to `dist/`
2. Configure the static host to serve `dist/` with these response headers on **all** routes:
   ```
   Cross-Origin-Opener-Policy: same-origin
   Cross-Origin-Embedder-Policy: require-corp
   Content-Type: application/wasm   (for *.wasm files specifically)
   ```
3. Verify `dist/wasm-pkg/hospital_tuner_dsp_bg.wasm` is present and served with
   `Content-Type: application/wasm` — required for `WebAssembly.instantiateStreaming`.
   If the content type is wrong, the browser falls back to `WebAssembly.instantiate`
   (slower, downloads the full binary before compiling) or throws entirely.
4. The `dist/wasm-pkg/package.json` file generated by wasm-pack can be served or
   deleted — browsers never request it; it exists for npm publish workflows only.
