/**
 * audio.js — Main-thread audio pipeline manager
 * ===============================================
 * Owns the AudioContext lifecycle, microphone stream, AudioWorkletNode, and
 * the message bridge between the main thread and the HospitalTunerProcessor
 * running in the AudioWorkletGlobalScope.
 *
 * This is an ES module (import/export at top level is fine here — it runs on
 * the main thread, not inside a worklet).
 *
 * Contracts:
 *   - /contracts/postmessage-schema.md — all message shapes
 *   - /contracts/wasm-api.md           — Wasm API (consumed indirectly via worklet)
 *   - /contracts/build-pipeline.md     — addModule() registration pattern
 *
 * Worklet registration: processor.js is emitted as a separate Vite chunk
 * because of the new URL('./worklet/processor.js', import.meta.url) pattern.
 * Vite statically analyses this call and emits the worklet file at a stable
 * URL, which is what the browser receives at runtime.
 */

/**
 * Start the audio pipeline.
 *
 * Requests microphone access, creates an AudioContext, registers and connects
 * the HospitalTunerProcessor AudioWorklet, and returns a controls object.
 *
 * The returned promise resolves once the AudioWorkletNode is connected and
 * the node is ready to receive audio (Wasm init is still async inside the
 * worklet — the caller learns about completion via the onReady callback).
 *
 * @param {function(object): void} onFrame
 *   Called with the full tunerFrame message object for each completed DSP
 *   frame (~86 times/second at 44100 Hz). The `chroma` and `activePitchClasses`
 *   fields are transferred TypedArrays — do not store references across frames
 *   without copying.
 *
 * @param {function(): void} [onReady]
 *   Called once when the worklet posts its "ready" message, indicating that
 *   the Wasm TunerProcessor is initialized and processing has begun.
 *
 * @param {function(string): void} [onError]
 *   Called with a human-readable error string if the worklet reports a fatal
 *   error (e.g., Wasm failed to load or a process() exception escaped the
 *   worklet's catch block).
 *
 * @returns {Promise<{
 *   stop(): void,
 *   setGateRatio(v: number): void,
 *   setPresenceThreshold(v: number): void,
 *   setHarmonicSuppression(enabled: boolean): void,
 * }>}
 *
 * @throws {Error} If microphone access is denied or AudioContext creation fails.
 */
export async function startAudio(onFrame, onReady, onError) {
  // -------------------------------------------------------------------------
  // Step 1 — Request microphone access.
  //
  // Raw capture settings: mono, no echo cancellation, no noise suppression,
  // no automatic gain control. The DSP engine needs unprocessed PCM to perform
  // its own spectral analysis — any browser pre-processing would corrupt the
  // chroma result.
  // -------------------------------------------------------------------------
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        echoCancellation: false,
        noiseSuppression: false,
        autoGainControl: false,
      },
    });
  } catch (err) {
    throw new Error(`Microphone access denied: ${err.message}`);
  }

  // -------------------------------------------------------------------------
  // Step 2 — Create AudioContext at the preferred sample rate.
  //
  // 44100 Hz is the rate the Wasm DSP engine is designed for (see wasm-api.md
  // constants: LOWEST_FREQ=55 Hz, FFT_SIZE=4096). The OS may not honour the
  // hint on all platforms, but in practice all major browsers on macOS/Win/Linux
  // support 44100 Hz natively.
  // -------------------------------------------------------------------------
  const audioCtx = new AudioContext({ sampleRate: 44100 });

  // -------------------------------------------------------------------------
  // Step 3 — Resume if suspended (required by Safari's autoplay policy and
  // Chromium's autoplay blocklist). An AudioContext starts in 'suspended' state
  // if it was created without a user gesture on some browsers. We resume here
  // immediately after the getUserMedia call (which itself counts as a gesture
  // interaction), so this should succeed.
  // -------------------------------------------------------------------------
  if (audioCtx.state === 'suspended') {
    await audioCtx.resume();
  }

  // -------------------------------------------------------------------------
  // Step 4 — Register the AudioWorklet module and pre-compile the Wasm binary.
  //
  // The new URL(..., import.meta.url) pattern is recognized by Vite's static
  // analyser, causing processor.js to be emitted as a separate output chunk.
  //
  // The Wasm Module is compiled on the main thread and passed to the worklet
  // via processorOptions (structured clone). This avoids dynamic import() in
  // the worklet, which is not supported in AudioWorkletGlobalScope.
  // -------------------------------------------------------------------------
  const [, wasmModule] = await Promise.all([
    audioCtx.audioWorklet.addModule(
      new URL('./worklet/processor.js', import.meta.url),
    ),
    WebAssembly.compileStreaming(
      fetch('/wasm-pkg/hospital_tuner_dsp_bg.wasm'),
    ),
  ]);

  // -------------------------------------------------------------------------
  // Step 5 — Create the AudioWorkletNode.
  //
  // Configuration:
  //   numberOfInputs:  1  — one input bus (the microphone source)
  //   numberOfOutputs: 0  — analysis only; no audio is produced
  //   channelCount:    1  — mono (matches getUserMedia channelCount: 1)
  //   channelCountMode: 'explicit' — browser must not up-mix/down-mix
  //   channelInterpretation: 'discrete' — channels are independent signals
  //   processorOptions.wasmModule — pre-compiled WebAssembly.Module
  //
  // The processor name must match the string passed to registerProcessor()
  // at the bottom of processor.js.
  // -------------------------------------------------------------------------
  const workletNode = new AudioWorkletNode(
    audioCtx,
    'hospital-tuner-processor',
    {
      numberOfInputs: 1,
      numberOfOutputs: 0,
      channelCount: 1,
      channelCountMode: 'explicit',
      channelInterpretation: 'discrete',
      processorOptions: { wasmModule },
    },
  );

  // -------------------------------------------------------------------------
  // Step 6 — Handle messages from the worklet.
  //
  // Message types per /contracts/postmessage-schema.md:
  //   "tunerFrame" — DSP output, posted ~86 times/second
  //   "ready"      — Wasm init complete; sampleRate field present
  //   "error"      — fatal worklet error; message (and optional stack) present
  //
  // Unknown message types are silently ignored for forward compatibility.
  // -------------------------------------------------------------------------
  workletNode.port.onmessage = (event) => {
    const msg = event.data;
    switch (msg.type) {
      case 'tunerFrame':
        onFrame(msg);
        break;
      case 'ready':
        if (onReady) onReady();
        break;
      case 'error':
        if (onError) onError(msg.message);
        break;
      // Unknown types: silently ignored.
    }
  };

  // -------------------------------------------------------------------------
  // Step 7 — Connect microphone source → worklet node.
  //
  // The worklet node has numberOfOutputs: 0, so it is NOT connected to
  // audioCtx.destination. Audio flows in but does not come out — pure analysis.
  // -------------------------------------------------------------------------
  const source = audioCtx.createMediaStreamSource(stream);
  source.connect(workletNode);

  // -------------------------------------------------------------------------
  // Step 8 — Return the controls object.
  //
  // send() is a convenience wrapper that posts a typed control message to the
  // worklet. The worklet's _handleMessage() routes these to TunerProcessor
  // setter methods after Wasm is ready.
  //
  // stop(): tears down the entire pipeline in the correct order:
  //   1. Disconnect DSP graph nodes (stops audio processing callbacks).
  //   2. Stop all microphone tracks (releases the OS-level mic resource and
  //      removes the browser's recording indicator).
  //   3. Close the AudioContext (releases audio hardware and thread resources).
  //   Closing the AudioContext before disconnecting is safe but closing it
  //   before stopping tracks would leave the OS recording indicator active.
  // -------------------------------------------------------------------------
  const send = (type, payload) =>
    workletNode.port.postMessage({ type, ...payload });

  return {
    /**
     * Tear down the audio pipeline. Safe to call multiple times (subsequent
     * calls are no-ops because the AudioContext transitions to 'closed').
     */
    stop() {
      source.disconnect();
      workletNode.disconnect();
      stream.getTracks().forEach((track) => track.stop());
      audioCtx.close();
    },

    /**
     * Set the spectral gate ratio.
     * @param {number} v — float, valid range 0.5–10.0 (clamped by DSP engine)
     */
    setGateRatio(v) {
      send('setGateRatio', { value: v });
    },

    /**
     * Set the chroma presence threshold.
     * @param {number} v — float, valid range 0.0–1.0 (clamped by DSP engine)
     */
    setPresenceThreshold(v) {
      send('setPresenceThreshold', { value: v });
    },

    /**
     * Toggle harmonic suppression.
     * @param {boolean} enabled — true = enabled (default), false = disabled
     */
    setHarmonicSuppression(enabled) {
      // Uses `enabled` field (not `value`) per postmessage-schema.md.
      send('setHarmonicSuppression', { enabled });
    },
  };
}
