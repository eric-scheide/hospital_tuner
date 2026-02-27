<!--
  Chromagram.svelte — Octave-folded pitch contour tracker
  X-axis: time (scrolls left to right)
  Y-axis: pitch class (one octave, C at bottom, B at top)
  All octaves fold onto the same axis — C1 and C5 share the same line.
  Shows continuous pitch so you can see flat/sharp relative to each note.
-->

<script>
  import { onMount, onDestroy } from 'svelte';

  export let frame = null;
  export let sensitivity = 50; // 0–100: 0 = nothing visible, 100 = everything visible
  export let minDuration = 15; // ms pitch must be continuous before plotting
  export let smoothness = 0.385; // EMA factor: 0 = no smoothing, 1 = frozen
  export let scrollSpeed = 1.5; // pixels per frame

  let wrapper;
  let labelCanvas;
  let labelCtx;
  let gridCanvas;
  let gridCtx;
  let canvas;
  let ctx;

  const LABEL_W = 60;
  // Y-axis spans exactly one octave: 12 semitones
  // semitone 0 = C (bottom), semitone 12 = C (top, wraps)
  const NOTE_NAMES = ['C','C#','D','D#','E','F','F#','G','G#','A','A#','B'];
  const PITCH_HUES = [0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330];
  const NATURALS = new Set([0, 2, 4, 5, 7, 9, 11]);

  let logicalW = 0;
  let logicalH = 0;
  let dpr = 1;
  let prevY = null;
  let smoothY = null;
  $: SMOOTH = smoothness;
  $: MIN_DURATION_MS = minDuration; // pitch must be continuous this long before plotting
  let pitchOnsetMs = null;   // timestamp when current pitch class started
  let prevPc = null;         // previous pitch class (for continuity check)
  let scrollAccum = 0;       // fractional pixel accumulator for smooth scroll speed

  function freqToSemitone(freq) {
    if (freq <= 0) return -1;
    // MIDI note with fractional cents precision
    const midi = 12 * Math.log2(freq / 440) + 69;
    // Fold into 0..12 range (one octave). Use modulo to wrap.
    let semi = midi % 12;
    if (semi < 0) semi += 12;
    // Fix octave-boundary discontinuity: values > 11.5 are "almost C",
    // so map them to negative (e.g. 11.9 → -0.1) for display continuity.
    if (semi > 11.5) semi -= 12;
    return semi;
  }

  const Y_PAD_TOP = 0.005; // minimal top padding
  const Y_PAD_BOT = 0.07; // generous space below C
  function semitoneToY(semi) {
    // C (0) at bottom, B (11) at top, with asymmetric padding
    const usable = 1 - Y_PAD_TOP - Y_PAD_BOT;
    return logicalH * (Y_PAD_TOP + usable * (1 - semi / 12));
  }

  function drawLabels() {
    if (!labelCtx) return;
    const w = LABEL_W;
    const h = logicalH;

    labelCtx.fillStyle = '#0c0d10';
    labelCtx.fillRect(0, 0, w, h);

    for (let i = 0; i < 12; i++) {
      const y = Math.round(semitoneToY(i)) + 0.5;
      const isNatural = NATURALS.has(i);

      // Color dot — 10% in from right edge of label column, sized to overlap neighbors
      const hue = PITCH_HUES[i];
      const dotR = 5; // ~half the font height
      labelCtx.fillStyle = `hsla(${hue}, 90%, 50%, 0.6)`;
      labelCtx.beginPath();
      labelCtx.arc(w * 0.90, y, dotR, 0, 2 * Math.PI);
      labelCtx.fill();

      // Note name label — uniform font and color, left-aligned
      labelCtx.fillStyle = '#9ca0b0';
      labelCtx.font = '22px monospace';
      labelCtx.textAlign = 'left';
      labelCtx.textBaseline = 'middle';
      labelCtx.fillText(NOTE_NAMES[i], 4, y);

    }
  }

  function sizeCanvases() {
    if (!wrapper || !canvas || !labelCanvas || !gridCanvas) return;

    const rect = wrapper.getBoundingClientRect();
    const totalW = Math.floor(rect.width);
    logicalH = Math.floor(rect.height);
    logicalW = totalW - LABEL_W;
    if (logicalW <= 0 || logicalH <= 0) return;

    dpr = window.devicePixelRatio || 1;

    // Label canvas
    labelCanvas.width = LABEL_W * dpr;
    labelCanvas.height = logicalH * dpr;
    labelCanvas.style.width = LABEL_W + 'px';
    labelCanvas.style.height = logicalH + 'px';
    labelCtx = labelCanvas.getContext('2d');
    labelCtx.scale(dpr, dpr);
    drawLabels();

    // Static grid canvas (behind trace)
    gridCanvas.width = logicalW * dpr;
    gridCanvas.height = logicalH * dpr;
    gridCanvas.style.width = logicalW + 'px';
    gridCanvas.style.height = logicalH + 'px';
    gridCtx = gridCanvas.getContext('2d');
    gridCtx.scale(dpr, dpr);
    drawFullGrid();

    // Trace canvas (transparent background, on top of grid)
    canvas.width = logicalW * dpr;
    canvas.height = logicalH * dpr;
    canvas.style.width = logicalW + 'px';
    canvas.style.height = logicalH + 'px';
    ctx = canvas.getContext('2d');
    ctx.scale(dpr, dpr);

    prevY = null;
    smoothY = null;
  }

  function drawFullGrid() {
    if (!gridCtx) return;
    for (let i = 0; i < 12; i++) {
      const y = Math.round(semitoneToY(i)) + 0.5;
      gridCtx.strokeStyle = 'rgba(255,255,255,0.35)';
      gridCtx.lineWidth = 1.5;
      gridCtx.beginPath();
      gridCtx.moveTo(0, y);
      gridCtx.lineTo(logicalW, y);
      gridCtx.stroke();
    }
  }

  function drawFrame(f) {
    if (!ctx || logicalW <= 0 || logicalH <= 0) return;

    const W = logicalW;
    const H = logicalH;
    const HX = W - 11; // trace head 10px inset from right edge

    // 1. Shift trail content left (fractional speed via accumulator)
    scrollAccum += scrollSpeed;
    const shift = Math.floor(scrollAccum);
    scrollAccum -= shift;
    if (shift > 0) {
      const imgData = ctx.getImageData(shift * dpr, 0, (HX - shift) * dpr, H * dpr);
      ctx.clearRect(0, 0, W, H);
      ctx.putImageData(imgData, 0, 0);
    }

    // 3. Plot the dominant frequency (octave-folded)
    const freq = f.dominantFrequency;
    const now = f.timestamp ?? performance.now();
    if (freq > 0) {
      const semi = freqToSemitone(freq);
      if (semi >= -0.5) {
        let pc = Math.round(semi) % 12;
        if (pc < 0) pc += 12;

        // Track pitch continuity — reset onset and break the trace when pitch class changes
        if (pc !== prevPc) {
          pitchOnsetMs = now;
          prevPc = pc;
          prevY = null;
          smoothY = null;
        }

        // Gate: don't plot until pitch has been continuous for MIN_DURATION_MS
        if (now - pitchOnsetMs < MIN_DURATION_MS) return;

        const rawY = semitoneToY(semi);
        // EMA smoothing — snap on large jumps (octave wrap), blend otherwise
        if (smoothY === null || Math.abs(rawY - smoothY) > logicalH * 0.4) {
          smoothY = rawY;
        } else {
          smoothY = SMOOTH * smoothY + (1 - SMOOTH) * rawY;
        }
        const y = smoothY;

        const hue = PITCH_HUES[pc];
        const energy = f.chroma[pc];
        // sensitivity 0 → threshold=1 (nothing visible), 100 → threshold=0 (everything visible)
        const threshold = (100 - sensitivity) / 100;
        const rawLogE = energy > 0 ? Math.max(0, 1 + Math.log10(energy) / 2) : 0;
        const logE = threshold < 1 && rawLogE > threshold ? (rawLogE - threshold) / (1 - threshold) : 0;
        const alpha = logE;

        // Connect to previous point (unless it wrapped around the octave boundary)
        if (prevY !== null && Math.abs(y - prevY) < logicalH * 0.4) {
          // Outer glow — wide transparent fringe
          ctx.strokeStyle = `hsla(${hue}, 100%, 65%, ${alpha * 0.1})`;
          ctx.lineWidth = 14;
          ctx.lineCap = 'round';
          ctx.beginPath();
          ctx.moveTo(HX - shift, prevY);
          ctx.lineTo(HX, y);
          ctx.stroke();

          // Mid glow
          ctx.strokeStyle = `hsla(${hue}, 100%, 65%, ${alpha * 0.3})`;
          ctx.lineWidth = 8;
          ctx.lineCap = 'round';
          ctx.beginPath();
          ctx.moveTo(HX - shift, prevY);
          ctx.lineTo(HX, y);
          ctx.stroke();

          // Inner glow
          ctx.strokeStyle = `hsla(${hue}, 100%, 65%, ${alpha * 0.6})`;
          ctx.lineWidth = 4;
          ctx.lineCap = 'round';
          ctx.beginPath();
          ctx.moveTo(HX - shift, prevY);
          ctx.lineTo(HX, y);
          ctx.stroke();

          // Core stroke — bright center
          ctx.strokeStyle = `hsla(${hue}, 100%, 85%, ${alpha})`;
          ctx.lineWidth = 1.5;
          ctx.lineCap = 'round';
          ctx.beginPath();
          ctx.moveTo(HX - shift, prevY);
          ctx.lineTo(HX, y);
          ctx.stroke();
        }

        // Glow halo — wide soft dot
        ctx.fillStyle = `hsla(${hue}, 100%, 65%, ${alpha * 0.12})`;
        ctx.beginPath();
        ctx.arc(HX, y, 10, 0, 2 * Math.PI);
        ctx.fill();

        // Core dot
        ctx.fillStyle = `hsla(${hue}, 100%, 85%, ${alpha})`;
        ctx.beginPath();
        ctx.arc(HX, y, 2.5, 0, 2 * Math.PI);
        ctx.fill();

        prevY = y;
      } else {
        prevY = null;
        smoothY = null;
        prevPc = null;
        pitchOnsetMs = null;
      }
    } else {
      prevY = null;
      smoothY = null;
      prevPc = null;
      pitchOnsetMs = null;
    }
  }

  let resizeObserver;

  onMount(() => {
    sizeCanvases();
    resizeObserver = new ResizeObserver(() => sizeCanvases());
    resizeObserver.observe(wrapper);
  });

  onDestroy(() => {
    resizeObserver?.disconnect();
  });

  $: if (frame && ctx) drawFrame(frame);
</script>

<div class="pitch-tracker" bind:this={wrapper}>
  <canvas class="label-canvas" bind:this={labelCanvas}></canvas>
  <div class="canvas-stack">
    <canvas class="grid-canvas" bind:this={gridCanvas}></canvas>
    <canvas class="trace-canvas" bind:this={canvas}></canvas>
  </div>
</div>

<style>
  .pitch-tracker {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: row;
    background: #050508;
    line-height: 0;
    overflow: hidden;
  }

  .label-canvas {
    flex: 0 0 auto;
    background: #0c0d10;
  }

  .canvas-stack {
    flex: 1 1 auto;
    position: relative;
    background: #050508;
  }

  .grid-canvas {
    position: absolute;
    inset: 0;
  }

  .trace-canvas {
    position: absolute;
    inset: 0;
  }
</style>
