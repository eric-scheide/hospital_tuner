<!--
  TunerStrip.svelte — Tuner readout component
  Shows active note names, dominant frequency in Hz, and cents deviation bar.
-->

<script>
  // Props
  export let frame = null; // tunerFrame message data or null

  const NOTE_NAMES = ['C','C#','D','D#','E','F','F#','G','G#','A','A#','B'];

  const PITCH_COLORS = [
    { h: 0   }, // C
    { h: 30  }, // C#
    { h: 60  }, // D
    { h: 90  }, // D#
    { h: 120 }, // E
    { h: 150 }, // F
    { h: 180 }, // F#
    { h: 210 }, // G
    { h: 240 }, // G#
    { h: 270 }, // A
    { h: 300 }, // A#
    { h: 330 }, // B
  ];

  // Derive note name + octave from a frequency using A4=440 Hz reference.
  // MIDI note 69 = A4. semitones from A4 = round(12 * log2(freq / 440)).
  // MIDI note = 69 + semitones. Octave = floor(MIDI / 12) - 1.
  function deriveNoteWithOctave(frequency) {
    if (frequency <= 0) return null;
    const semitones = Math.round(12 * Math.log2(frequency / 440.0));
    const midi = 69 + semitones;
    const pitchClass = ((midi % 12) + 12) % 12;
    const octave = Math.floor(midi / 12) - 1;
    return { name: NOTE_NAMES[pitchClass], octave, pitchClass };
  }

  // Cents bar derived values
  $: cents = frame ? frame.cents : 0;
  $: dominantFrequency = frame ? frame.dominantFrequency : 0;
  $: activePitchClasses = frame ? frame.activePitchClasses : [];

  $: noteInfo = dominantFrequency > 0 ? deriveNoteWithOctave(dominantFrequency) : null;
  $: freqDisplay = dominantFrequency > 0 ? dominantFrequency.toFixed(1) + ' Hz' : '—';

  // Active note names colored by pitch class
  $: activeNotes = (activePitchClasses && activePitchClasses.length > 0)
    ? Array.from(activePitchClasses)
    : [];

  // Cents bar geometry: bar is 200px wide, center=0, max fill = 50% per side
  $: centsAbs = Math.abs(cents);
  $: centsFillWidth = Math.min(centsAbs / 50, 1) * 50; // percent of half-bar (0–50%)
  $: centsIsSharp = cents >= 0;

  $: centsColor = centsAbs < 10 ? '#4ade80'
                : centsAbs < 25 ? '#facc15'
                : '#f87171';

  // Fill style: extends right from center if sharp, left if flat
  $: centsFillStyle = centsIsSharp
    ? `left: 50%; width: ${centsFillWidth}%; background: ${centsColor};`
    : `right: 50%; width: ${centsFillWidth}%; background: ${centsColor};`;

  $: centsLabel = cents === 0 ? '0¢'
                : cents > 0  ? `+${cents}¢`
                : `${cents}¢`;
</script>

<div class="tuner-strip">
  {#if frame === null}
    <div class="waiting">Waiting for audio...</div>
  {:else}
    <!-- Active note names: large, each in its pitch class color -->
    <div class="notes-section">
      {#if activeNotes.length > 0}
        <span class="note-names">
          {#each activeNotes as pc, i}
            <span
              class="note-name"
              style="color: hsl({PITCH_COLORS[pc].h}, 100%, 65%);"
            >{NOTE_NAMES[pc]}</span>{#if i < activeNotes.length - 1}<span class="note-sep"> </span>{/if}
          {/each}
        </span>
      {:else}
        <span class="note-name muted">—</span>
      {/if}
    </div>

    <!-- Frequency and octave info -->
    <div class="freq-section">
      {#if noteInfo}
        <span class="note-octave"
          style="color: hsl({PITCH_COLORS[noteInfo.pitchClass].h}, 100%, 60%);"
        >{noteInfo.name}{noteInfo.octave}</span>
      {/if}
      <span class="freq-hz">{freqDisplay}</span>
    </div>

    <!-- Cents deviation bar -->
    <div class="cents-section">
      <div class="cents-bar">
        <div class="cents-fill" style="{centsFillStyle}"></div>
        <div class="cents-center-line"></div>
      </div>
      <span class="cents-label" style="color: {centsColor};">{centsLabel}</span>
    </div>
  {/if}
</div>

<style>
  .tuner-strip {
    display: flex;
    align-items: center;
    gap: 16px;
    height: 60px;
    padding: 0 16px;
    background: #0a0a0a;
    border-top: 1px solid #1a1a1a;
    font-family: monospace;
    overflow: hidden;
  }

  .waiting {
    color: #555;
    font-size: 13px;
    width: 100%;
    text-align: center;
  }

  /* Note names (left section) */
  .notes-section {
    flex: 0 0 auto;
    min-width: 80px;
  }

  .note-names {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }

  .note-name {
    font-size: 28px;
    font-weight: bold;
    line-height: 1;
    letter-spacing: -0.5px;
  }

  .note-name.muted {
    color: #444;
    font-size: 28px;
  }

  .note-sep {
    color: #333;
    font-size: 20px;
  }

  /* Frequency section (center) */
  .freq-section {
    flex: 0 0 auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    min-width: 80px;
    gap: 2px;
  }

  .note-octave {
    font-size: 14px;
    font-weight: bold;
    line-height: 1;
  }

  .freq-hz {
    font-size: 11px;
    color: #777;
    line-height: 1;
  }

  /* Cents bar (right section) */
  .cents-section {
    flex: 1 1 auto;
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 120px;
    max-width: 280px;
  }

  .cents-bar {
    position: relative;
    flex: 1 1 auto;
    height: 8px;
    background: #1a1a1a;
    border-radius: 4px;
    overflow: hidden;
  }

  .cents-fill {
    position: absolute;
    top: 0;
    height: 100%;
    border-radius: 2px;
    transition: width 0.05s linear, left 0.05s linear, right 0.05s linear;
  }

  .cents-center-line {
    position: absolute;
    top: 0;
    left: 50%;
    width: 1px;
    height: 100%;
    background: rgba(255, 255, 255, 0.4);
    transform: translateX(-0.5px);
  }

  .cents-label {
    flex: 0 0 auto;
    font-size: 11px;
    min-width: 28px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
</style>
