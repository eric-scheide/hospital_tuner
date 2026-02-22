# postMessage Schema Contract — AudioWorklet → Main Thread Boundary

**Status: LOCKED**
**Last updated by: Architect Agent**

This document defines the exact shape of every message posted from
`HospitalTunerProcessor` (running in the AudioWorkletGlobalScope) to the
main thread (received by `App.svelte`).

Neither sender nor receiver may add, rename, or remove fields without a
contract revision. Unknown fields on the receiver side must be silently
ignored to allow forward compatibility.

---

## Message Types

All messages share a `type` discriminant string so the main thread can
route them with a simple `switch`. This leaves room to add control messages
(e.g. "error", "status") later without breaking the frame consumer.

---

## `"tunerFrame"` — the primary DSP output message

Posted once per completed DSP frame. At 44100 Hz with HOP_SIZE=512, this
is approximately 86 times per second. The main thread is not required to
paint every frame; it may drop frames during jank without consequence.

### Schema (TypeScript-style annotation for precision)

```ts
interface TunerFrameMessage {
  // Discriminant. Always the string "tunerFrame".
  type: "tunerFrame";

  // 12-element chroma energy vector, one value per pitch class.
  // Index 0 = C, Index 1 = C#/Db, Index 2 = D, ..., Index 11 = B.
  // Values are normalized: 0.0 (no energy) to 1.0 (maximum energy this frame).
  // Ownership is TRANSFERRED to the main thread (buffer is in the transfer list).
  chroma: Float32Array; // length: 12

  // Pitch class indices whose chroma energy >= presence_threshold.
  // These are the "active chord tones" to highlight in the chromagram.
  // Length is 0 when silent. Maximum length is 12 (all pitch classes active).
  // Ownership is TRANSFERRED to the main thread (buffer is in the transfer list).
  activePitchClasses: Uint8Array; // length: 0–12

  // Frequency in Hz of the strongest surviving spectral peak.
  // 0.0 indicates silence (no peaks survived the gate).
  dominantFrequency: number; // float, >= 0.0

  // Cents deviation of dominantFrequency from the nearest equal-temperament
  // semitone (A4 = 440 Hz reference).
  // Range: -50 to +50. Positive = sharp, negative = flat.
  // 0 when dominantFrequency == 0.0.
  cents: number; // integer, -50..+50

  // performance.now() timestamp (milliseconds) recorded in the worklet
  // immediately before postMessage is called.
  // Used by the main thread to compute display latency and drop stale frames.
  timestamp: number; // float, milliseconds
}
```

### Transfer List

The `chroma` and `activePitchClasses` ArrayBuffers are **transferred**, not
copied. This is zero-copy and moves ownership to the main thread.

```js
// Canonical postMessage call in processor.js:
this.port.postMessage(
  {
    type: "tunerFrame",
    chroma,               // Float32Array (new buffer created from wasm memory copy)
    activePitchClasses,   // Uint8Array   (new buffer created from wasm memory copy)
    dominantFrequency,    // number
    cents,                // number
    timestamp,            // number (performance.now())
  },
  [chroma.buffer, activePitchClasses.buffer] // transfer list
);
```

**Important**: After transfer, the worklet's `chroma` and `activePitchClasses`
variables point to detached ArrayBuffers with `byteLength === 0`. The worklet
must allocate fresh TypedArrays each frame (not reuse between frames).

---

## `"error"` — worklet-level error reporting

Posted when a fatal error occurs in the worklet (e.g., Wasm failed to load,
constructor threw, or a non-recoverable DSP error occurred).

```ts
interface WorkletErrorMessage {
  type: "error";

  // Human-readable error description.
  message: string;

  // Optional: the thrown Error object's stack trace (if available in worklet scope).
  stack?: string;
}
```

No transfer list. The main thread should display this error to the user
and halt further frame processing.

---

## `"ready"` — worklet initialization complete

Posted once from the worklet constructor after the Wasm module has been
successfully loaded and `TunerProcessor::new()` has been called.

```ts
interface WorkletReadyMessage {
  type: "ready";

  // The sample_rate the TunerProcessor was initialized with.
  sampleRate: number;
}
```

The main thread must not send audio to the worklet before receiving this message,
although in practice the AudioContext will not start processing until the node
is connected to a source.

---

## Control Messages (Main Thread → Worklet)

These messages flow in the **opposite direction**: from the main thread to the
worklet via `audioWorkletNode.port.postMessage(...)`.

```ts
interface SetGateRatioMessage {
  type: "setGateRatio";
  value: number; // float, 0.5–10.0
}

interface SetPresenceThresholdMessage {
  type: "setPresenceThreshold";
  value: number; // float, 0.0–1.0
}

interface SetHarmonicSuppressionMessage {
  type: "setHarmonicSuppression";
  enabled: boolean;
}

interface ResetMessage {
  type: "reset";
}
```

The worklet handles these in its `port.onmessage` handler and forwards the
values to `TunerProcessor`'s setter methods.

---

## Ordering and Delivery Guarantees

- postMessage between an AudioWorklet port and the main thread is ordered FIFO.
- There is no acknowledgement mechanism; the main thread must tolerate frame drops.
- The worklet does NOT wait for the main thread to consume a message before
  posting the next frame. If the main thread falls behind, messages queue in the
  port's internal buffer (bounded by browser implementation; typically several
  hundred messages before backpressure is applied).
- The main thread SHOULD drop frames older than 100ms (compare `timestamp` to
  `performance.now()`).

---

## Invariants the Receiver May Rely On

1. `chroma.length` is always exactly 12.
2. `chroma` values are always in [0.0, 1.0].
3. `activePitchClasses` values are always in [0, 11].
4. `activePitchClasses` is always a subset of indices where `chroma[i] > 0`.
5. `cents` is always in [−50, +50].
6. If `dominantFrequency === 0`, then `cents === 0` and `activePitchClasses.length === 0`.
7. `timestamp` is always a positive finite number.
