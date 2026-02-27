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
  let gateSlider = 39; // 0–100 linear slider position
  // Exponential mapping: slider 0 → 0, slider 100 → 15
  // f(x) = 15 * ((e^(x/100*k) - 1) / (e^k - 1)), k controls curvature
  const GATE_K = 3;
  const GATE_MAX = 15;
  function sliderToGate(s) {
    if (s <= 0) return 0;
    return GATE_MAX * (Math.exp(GATE_K * s / 100) - 1) / (Math.exp(GATE_K) - 1);
  }
  $: gateRatio = sliderToGate(gateSlider);
  let harmonicSuppression = true;
  let sensitivity = 10;
  let minDuration = 15;
  let smoothness = 0.385;
  let scrollSpeed = 2.0;
  let gearOpen = false;

  function toggleGear() {
    gearOpen = !gearOpen;
  }

  function closeGear(e) {
    // Close when clicking outside the gear menu
    if (gearOpen && !e.target.closest('.gear-wrapper')) {
      gearOpen = false;
    }
  }

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

<svelte:window on:click={closeGear} />
<div class="app">
  <!-- Header bar -->
  <header class="app-header">
    <span class="app-title">Hospital Tuner</span>
    <div class="header-controls">
      <div class="gear-wrapper">
          <button class="btn btn-gear" on:click={toggleGear} title="Settings">
            <svg width="18" height="18" viewBox="0 0 20 20" fill="currentColor">
              <path fill-rule="evenodd" d="M11.49 3.17c-.38-1.56-2.6-1.56-2.98 0a1.532 1.532 0 01-2.286.948c-1.372-.836-2.942.734-2.106 2.106.54.886.061 2.042-.947 2.287-1.561.379-1.561 2.6 0 2.978a1.532 1.532 0 01.947 2.287c-.836 1.372.734 2.942 2.106 2.106a1.532 1.532 0 012.287.947c.379 1.561 2.6 1.561 2.978 0a1.533 1.533 0 012.287-.947c1.372.836 2.942-.734 2.106-2.106a1.533 1.533 0 01.947-2.287c1.561-.379 1.561-2.6 0-2.978a1.532 1.532 0 01-.947-2.287c.836-1.372-.734-2.942-2.106-2.106a1.532 1.532 0 01-2.287-.947zM10 13a3 3 0 100-6 3 3 0 000 6z" clip-rule="evenodd"/>
            </svg>
          </button>
          {#if gearOpen}
            <div class="gear-menu">
              <label class="control-label">
                <span class="control-name">Smoothness</span>
                <input type="range" min="0" max="0.8" step="0.01" bind:value={smoothness} class="slider" />
                <span class="control-value">{(smoothness * 100).toFixed(0)}%</span>
              </label>
              <label class="control-label">
                <span class="control-name">Sensitivity</span>
                <input type="range" min="0" max="100" step="1" bind:value={sensitivity} class="slider" />
                <span class="control-value">{sensitivity}</span>
              </label>
              <label class="control-label">
                <span class="control-name">Min duration</span>
                <input type="range" min="0" max="100" step="5" bind:value={minDuration} class="slider" />
                <span class="control-value">{minDuration} ms</span>
              </label>
              <label class="control-label">
                <span class="control-name">Scroll speed</span>
                <input type="range" min="0.5" max="4" step="0.1" bind:value={scrollSpeed} class="slider" />
                <span class="control-value">{scrollSpeed.toFixed(1)}x</span>
              </label>
              <label class="control-label">
                <span class="control-name">Harmonic suppression</span>
                <input type="checkbox" bind:checked={harmonicSuppression} on:change={onHarmonicSuppressionChange} class="checkbox" />
                <span class="control-value">{harmonicSuppression ? 'On' : 'Off'}</span>
              </label>
            </div>
          {/if}
        </div>
      {#if status === 'idle' || status === 'error'}
        <button class="btn btn-start" on:click={handleStart}>Start</button>
      {:else if status === 'loading'}
        <button class="btn btn-start" disabled>Starting...</button>
      {:else if status === 'ready'}
        <button class="btn btn-stop" on:click={handleStop}>Stop</button>
      {/if}
    </div>
  </header>

  <!-- Status line (hidden when running to maximize canvas space) -->
  {#if status === 'loading'}
    <div class="status-line status-loading">Initializing...</div>
  {:else if status === 'error'}
    <div class="status-line status-error">Error: {errorMessage}</div>
  {:else if status === 'idle'}
    <div class="status-line status-idle">Ready — click Start to begin</div>
  {/if}

  <!-- Chromagram canvas (fills available space) -->
  <div class="chromagram-container">
    <Chromagram {frame} {sensitivity} {minDuration} {smoothness} {scrollSpeed} />
  </div>

</div>

<style>
  :global(body) {
    margin: 0;
    background: #050508;
    color: #e8eaed;
    font-family: 'Inter', system-ui, sans-serif;
  }

  :global(*) {
    box-sizing: border-box;
  }

  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: #050508;
    color: #e8eaed;
    overflow: hidden;
  }

  /* Header */
  .app-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 16px;
    height: 32px;
    background: linear-gradient(180deg, #10111a 0%, #0c0d10 100%);
    border-bottom: 1px solid #1a1c22;
    flex-shrink: 0;
  }

  .app-title {
    font-size: 12px;
    font-weight: 600;
    color: #e8eaed;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding-left: 10px;
    border-left: 2px solid #34d399;
  }

  .header-controls {
    display: flex;
    gap: 8px;
  }

  .btn {
    padding: 6px 18px;
    border: none;
    border-radius: 6px;
    font-family: 'Inter', sans-serif;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    cursor: pointer;
    transition: background 0.15s ease, transform 0.1s ease;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn:active:not(:disabled) {
    transform: scale(0.97);
  }

  .btn:focus-visible {
    outline: 2px solid rgba(52, 211, 153, 0.4);
    outline-offset: 4px;
  }

  .btn-start {
    background: #10b981;
    color: #000;
  }

  .btn-start:not(:disabled):hover {
    background: #059669;
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

  .status-idle    { color: #4a4e5a; }
  .status-loading { color: #facc15; }
  .status-running { color: #34d399; }
  .status-error   { color: #f87171; }

  /* Chromagram area */
  .chromagram-container {
    flex: 1 1 0;
    position: relative;
    min-height: 0;
    overflow: hidden;
    background: #050508;
  }

  /* Gear menu */
  .gear-wrapper {
    position: relative;
  }

  .btn-gear {
    background: transparent;
    border: 1px solid #2a2c34;
    border-radius: 6px;
    color: #8b8fa3;
    padding: 5px 7px;
    cursor: pointer;
    display: flex;
    align-items: center;
    transition: color 0.15s ease, border-color 0.15s ease;
  }

  .btn-gear:hover {
    color: #34d399;
    border-color: #34d399;
  }

  .gear-menu {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 8px;
    background: #12131a;
    border: 1px solid #1a1c22;
    border-radius: 8px;
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    z-index: 100;
    box-shadow: 0 8px 24px rgba(0,0,0,0.5);
    min-width: 300px;
  }

  .control-label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: #8b8fa3;
    cursor: pointer;
    user-select: none;
  }

  .control-name {
    min-width: 110px;
    font-weight: 500;
    text-transform: uppercase;
    font-size: 10px;
    letter-spacing: 0.06em;
    color: #6b7080;
  }

  .control-value {
    min-width: 36px;
    color: #c4c7d0;
    font-family: 'SF Mono', 'Cascadia Code', monospace;
    font-size: 11px;
  }

  .slider {
    -webkit-appearance: none;
    appearance: none;
    width: 120px;
    height: 4px;
    border-radius: 2px;
    background: #1a1c22;
    outline: none;
    cursor: pointer;
  }

  .slider:focus-visible {
    outline: 2px solid rgba(52, 211, 153, 0.4);
    outline-offset: 4px;
  }

  .slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #34d399;
    cursor: pointer;
    box-shadow: 0 0 6px rgba(52, 211, 153, 0.3);
    transition: transform 0.1s ease, box-shadow 0.1s ease;
  }

  .slider::-webkit-slider-thumb:hover {
    transform: scale(1.15);
    box-shadow: 0 0 10px rgba(52, 211, 153, 0.5);
  }

  .slider::-moz-range-thumb {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #34d399;
    cursor: pointer;
    border: none;
    box-shadow: 0 0 6px rgba(52, 211, 153, 0.3);
  }

  .checkbox {
    accent-color: #34d399;
    width: 16px;
    height: 16px;
    cursor: pointer;
  }
</style>
