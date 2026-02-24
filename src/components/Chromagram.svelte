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

  let wrapper;
  let labelCanvas;
  let labelCtx;
  let canvas;
  let ctx;

  const LABEL_W = 40;
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
  const SMOOTH = 0.35; // EMA factor: 0 = no smoothing, 1 = frozen

  function freqToSemitone(freq) {
    if (freq <= 0) return -1;
    // MIDI note with fractional cents precision
    const midi = 12 * Math.log2(freq / 440) + 69;
    // Fold into 0..12 range (one octave). Use modulo to wrap.
    let semi = midi % 12;
    if (semi < 0) semi += 12;
    return semi;
  }

  function semitoneToY(semi) {
    // C (0) at bottom, B (11) at top
    // Add 0.5 offset so note lines sit in the center of their band
    return logicalH * (1 - semi / 12);
  }

  function drawLabels() {
    if (!labelCtx) return;
    const w = LABEL_W;
    const h = logicalH;

    labelCtx.fillStyle = '#0a0a0a';
    labelCtx.fillRect(0, 0, w, h);

    for (let i = 0; i < 12; i++) {
      const y = Math.round(semitoneToY(i)) + 0.5;
      const isNatural = NATURALS.has(i);

      // Note name label
      if (isNatural) {
        labelCtx.fillStyle = i === 0 ? '#bbb' : '#777';
        labelCtx.font = i === 0 ? 'bold 12px monospace' : '11px monospace';
      } else {
        labelCtx.fillStyle = '#444';
        labelCtx.font = '9px monospace';
      }
      labelCtx.textAlign = 'right';
      labelCtx.textBaseline = 'middle';
      labelCtx.fillText(NOTE_NAMES[i], w - 6, y);

      // Grid line
      if (i === 0) {
        labelCtx.strokeStyle = 'rgba(255,255,255,0.3)';
      } else if (isNatural) {
        labelCtx.strokeStyle = 'rgba(255,255,255,0.15)';
      } else {
        labelCtx.strokeStyle = 'rgba(255,255,255,0.06)';
      }
      labelCtx.lineWidth = 1;
      labelCtx.beginPath();
      labelCtx.moveTo(0, y);
      labelCtx.lineTo(w, y);
      labelCtx.stroke();
    }
  }

  function sizeCanvases() {
    if (!wrapper || !canvas || !labelCanvas) return;

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

    // Main scrolling canvas
    canvas.width = logicalW * dpr;
    canvas.height = logicalH * dpr;
    canvas.style.width = logicalW + 'px';
    canvas.style.height = logicalH + 'px';
    ctx = canvas.getContext('2d');
    ctx.scale(dpr, dpr);
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, logicalW, logicalH);

    drawFullGrid();
    prevY = null;
    smoothY = null;
  }

  function drawFullGrid() {
    if (!ctx) return;
    for (let i = 0; i < 12; i++) {
      const y = Math.round(semitoneToY(i)) + 0.5;
      if (i === 0) {
        ctx.strokeStyle = 'rgba(255,255,255,0.25)';
      } else if (NATURALS.has(i)) {
        ctx.strokeStyle = 'rgba(255,255,255,0.1)';
      } else {
        ctx.strokeStyle = 'rgba(255,255,255,0.04)';
      }
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(logicalW, y);
      ctx.stroke();
    }
  }

  function drawFrame(f) {
    if (!ctx || logicalW <= 0 || logicalH <= 0) return;

    const W = logicalW;
    const H = logicalH;

    // 1. Shift content left by 1px
    const imgData = ctx.getImageData(dpr, 0, (W - 1) * dpr, H * dpr);
    ctx.putImageData(imgData, 0, 0);

    // 2. Clear rightmost column
    ctx.fillStyle = '#000';
    ctx.fillRect(W - 1, 0, 1, H);

    // 3. Draw grid ticks on the rightmost column
    for (let i = 0; i < 12; i++) {
      const y = Math.round(semitoneToY(i));
      if (i === 0) {
        ctx.fillStyle = 'rgba(255,255,255,0.25)';
      } else if (NATURALS.has(i)) {
        ctx.fillStyle = 'rgba(255,255,255,0.1)';
      } else {
        ctx.fillStyle = 'rgba(255,255,255,0.04)';
      }
      ctx.fillRect(W - 1, y, 1, 1);
    }

    // 4. Plot the dominant frequency (octave-folded)
    const freq = f.dominantFrequency;
    if (freq > 0) {
      const semi = freqToSemitone(freq);
      if (semi >= 0) {
        const rawY = semitoneToY(semi);
        // EMA smoothing — snap on large jumps (octave wrap), blend otherwise
        if (smoothY === null || Math.abs(rawY - smoothY) > logicalH * 0.4) {
          smoothY = rawY;
        } else {
          smoothY = SMOOTH * smoothY + (1 - SMOOTH) * rawY;
        }
        const y = smoothY;

        const pc = Math.round(semi) % 12;
        const hue = PITCH_HUES[pc];
        const energy = f.chroma[pc];
        // sensitivity 0 → threshold=0 (everything visible), 100 → threshold=1 (nothing visible)
        const threshold = sensitivity / 100;
        const rawLogE = energy > 0 ? Math.max(0, 1 + Math.log10(energy) / 2) : 0;
        const logE = threshold < 1 && rawLogE > threshold ? (rawLogE - threshold) / (1 - threshold) : 0;
        const sat = Math.round(100 * logE);
        const alpha = logE;

        // Connect to previous point (unless it wrapped around the octave boundary)
        if (prevY !== null && Math.abs(y - prevY) < logicalH * 0.4) {
          ctx.strokeStyle = `hsla(${hue}, ${sat}%, 55%, ${alpha})`;
          ctx.lineWidth = 2;
          ctx.beginPath();
          ctx.moveTo(W - 2, prevY);
          ctx.lineTo(W - 1, y);
          ctx.stroke();
        }

        // Bright dot
        ctx.fillStyle = `hsla(${hue}, ${sat}%, 70%, ${alpha})`;
        ctx.beginPath();
        ctx.arc(W - 1, y, 1.5, 0, 2 * Math.PI);
        ctx.fill();

        prevY = y;
      } else {
        prevY = null;
        smoothY = null;
      }
    } else {
      prevY = null;
      smoothY = null;
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
  <canvas class="main-canvas" bind:this={canvas}></canvas>
</div>

<style>
  .pitch-tracker {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: row;
    background: #000;
    line-height: 0;
    overflow: hidden;
  }

  .label-canvas {
    flex: 0 0 auto;
    background: #0a0a0a;
  }

  .main-canvas {
    flex: 1 1 auto;
    background: #000;
  }
</style>
