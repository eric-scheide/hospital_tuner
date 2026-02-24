<!--
  App.svelte — Root layout component
  Owns AudioContext lifecycle, worklet registration, and message routing.
-->

<script>
  import { onDestroy } from 'svelte';
  import Chromagram from './components/Chromagram.svelte';


  // Reactive state
  let audioContext = null;
  let workletNode = null;
  let micStream = null;
  let frame = null;
  let status = 'idle'; // 'idle' | 'loading' | 'ready' | 'error'
  let errorMessage = '';
  let gateRatio = 1.5;
  let harmonicSuppression = true;
  let sensitivity = 50;

  // ----- Audio lifecycle -----

  async function handleStart() {
    status = 'loading';
    errorMessage = '';
    try {
      // 1. Request microphone access
      micStream = await navigator.mediaDevices.getUserMedia({
        audio: {
          echoCancellation: false,
          noiseSuppression: false,
          autoGainControl: false,
        },
        video: false,
      });

      // 2. Create AudioContext
      audioContext = new AudioContext({ sampleRate: 44100 });

      // 3. Register the AudioWorklet module and pre-compile the Wasm binary
      //    in parallel for faster startup.
      const [, wasmModule] = await Promise.all([
        audioContext.audioWorklet.addModule(
          new URL('./worklet/processor.js', import.meta.url)
        ),
        WebAssembly.compileStreaming(
          fetch('/wasm-pkg/hospital_tuner_dsp_bg.wasm')
        ),
      ]);

      // 4. Create the worklet node, passing the pre-compiled Wasm Module
      //    via processorOptions (structured-cloned to the worklet thread).
      workletNode = new AudioWorkletNode(audioContext, 'hospital-tuner-processor', {
        numberOfInputs: 1,
        numberOfOutputs: 0,
        channelCount: 1,
        channelCountMode: 'explicit',
        channelInterpretation: 'speakers',
        processorOptions: { wasmModule },
      });

      // 5. Listen for messages from the worklet
      workletNode.port.onmessage = handleWorkletMessage;

      // 6. Connect microphone → worklet
      const source = audioContext.createMediaStreamSource(micStream);
      source.connect(workletNode);

      // Status will be set to 'ready' when worklet posts its "ready" message.
      // If the worklet never posts "ready" (older skeleton), fall back here:
      // (No-op: we wait for the message.)

    } catch (err) {
      status = 'error';
      errorMessage = err.message || String(err);
      teardown();
    }
  }

  function handleWorkletMessage(event) {
    const data = event.data;
    switch (data.type) {
      case 'tunerFrame': {
        frame = data;
        break;
      }
      case 'ready':
        status = 'ready';
        // Apply initial control values to the worklet
        sendGateRatio(gateRatio);
        sendHarmonicSuppression(harmonicSuppression);
        break;
      case 'error':
        status = 'error';
        errorMessage = data.message || 'Unknown worklet error';
        break;
      default:
        // Forward compatibility: silently ignore unknown message types
        break;
    }
  }

  function handleStop() {
    teardown();
  }

  function teardown() {
    workletNode?.port?.close?.();
    workletNode?.disconnect?.();
    workletNode = null;

    if (micStream) {
      micStream.getTracks().forEach(t => t.stop());
      micStream = null;
    }

    audioContext?.close();
    audioContext = null;

    frame = null;
    if (status !== 'error') {
      status = 'idle';
    }
  }

  // ----- Control message senders -----

  function sendGateRatio(value) {
    workletNode?.port.postMessage({ type: 'setGateRatio', value });
  }

  function sendHarmonicSuppression(enabled) {
    workletNode?.port.postMessage({ type: 'setHarmonicSuppression', enabled });
  }

  function onGateRatioChange() {
    sendGateRatio(gateRatio);
  }

  function onHarmonicSuppressionChange() {
    sendHarmonicSuppression(harmonicSuppression);
  }

  // ----- Cleanup on component destroy -----
  onDestroy(() => {
    teardown();
  });
</script>

<svelte:head>
  <title>Hospital Tuner</title>
</svelte:head>

<div class="app">
  <!-- Header bar -->
  <header class="app-header">
    <span class="app-title">Hospital Tuner</span>
    <div class="header-controls">
      {#if status === 'idle' || status === 'error'}
        <button class="btn btn-start" on:click={handleStart}>Start</button>
      {:else if status === 'loading'}
        <button class="btn btn-start" disabled>Starting...</button>
      {:else if status === 'ready'}
        <button class="btn btn-stop" on:click={handleStop}>Stop</button>
      {/if}
    </div>
  </header>

  <!-- Status line -->
  {#if status === 'loading'}
    <div class="status-line status-loading">Initializing...</div>
  {:else if status === 'ready'}
    <div class="status-line status-running">Running</div>
  {:else if status === 'error'}
    <div class="status-line status-error">Error: {errorMessage}</div>
  {:else}
    <div class="status-line status-idle">Ready — click Start to begin</div>
  {/if}

  <!-- Chromagram canvas (fills available space) -->
  <div class="chromagram-container">
    <Chromagram {frame} {sensitivity} />
  </div>

<!-- Controls panel (only when running) -->
  {#if status === 'ready'}
    <div class="controls-panel">
      <label class="control-label">
        <span class="control-name">Gate ratio</span>
        <input
          type="range"
          min="0.5"
          max="5"
          step="0.1"
          bind:value={gateRatio}
          on:change={onGateRatioChange}
          class="slider"
        />
        <span class="control-value">{gateRatio.toFixed(1)}×</span>
      </label>

      <label class="control-label">
        <span class="control-name">Sensitivity</span>
        <input
          type="range"
          min="0"
          max="100"
          step="1"
          bind:value={sensitivity}
          class="slider"
        />
        <span class="control-value">{sensitivity}</span>
      </label>

      <label class="control-label">
        <span class="control-name">Harmonic suppression</span>
        <input
          type="checkbox"
          bind:checked={harmonicSuppression}
          on:change={onHarmonicSuppressionChange}
          class="checkbox"
        />
        <span class="control-value">{harmonicSuppression ? 'On' : 'Off'}</span>
      </label>
    </div>
  {/if}
</div>

<style>
  :global(body) {
    margin: 0;
    background: #000;
    color: #eee;
    font-family: monospace;
  }

  :global(*) {
    box-sizing: border-box;
  }

  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: #000;
    color: #eee;
    overflow: hidden;
  }

  /* Header */
  .app-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 16px;
    height: 48px;
    background: #0d0d0d;
    border-bottom: 1px solid #1a1a1a;
    flex-shrink: 0;
  }

  .app-title {
    font-size: 16px;
    font-weight: bold;
    color: #ddd;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .header-controls {
    display: flex;
    gap: 8px;
  }

  .btn {
    padding: 6px 18px;
    border: none;
    border-radius: 4px;
    font-family: monospace;
    font-size: 13px;
    font-weight: bold;
    cursor: pointer;
    transition: opacity 0.1s;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn-start {
    background: #22c55e;
    color: #000;
  }

  .btn-start:not(:disabled):hover {
    background: #16a34a;
  }

  .btn-stop {
    background: #ef4444;
    color: #fff;
  }

  .btn-stop:hover {
    background: #dc2626;
  }

  /* Status line */
  .status-line {
    padding: 4px 16px;
    font-size: 11px;
    letter-spacing: 0.04em;
    flex-shrink: 0;
  }

  .status-idle    { color: #555; }
  .status-loading { color: #facc15; }
  .status-running { color: #4ade80; }
  .status-error   { color: #f87171; }

  /* Chromagram area */
  .chromagram-container {
    flex: 1 1 0;
    position: relative;
    min-height: 0;
    overflow: hidden;
    background: #000;
  }

  /* Controls panel */
  .controls-panel {
    display: flex;
    gap: 24px;
    align-items: center;
    padding: 8px 16px;
    background: #0a0a0a;
    border-top: 1px solid #1a1a1a;
    flex-shrink: 0;
  }

  .control-label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: #999;
    cursor: pointer;
    user-select: none;
  }

  .control-name {
    min-width: 110px;
  }

  .control-value {
    min-width: 36px;
    color: #ccc;
  }

  .slider {
    -webkit-appearance: none;
    appearance: none;
    width: 120px;
    height: 4px;
    border-radius: 2px;
    background: #333;
    outline: none;
    cursor: pointer;
  }

  .slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #4ade80;
    cursor: pointer;
  }

  .slider::-moz-range-thumb {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #4ade80;
    cursor: pointer;
    border: none;
  }

  .checkbox {
    accent-color: #4ade80;
    width: 16px;
    height: 16px;
    cursor: pointer;
  }
</style>
