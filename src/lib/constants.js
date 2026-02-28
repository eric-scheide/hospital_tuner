// Shared constants for Hospital Tuner
export const NOTE_NAMES = ['C','C#','D','D#','E','F','F#','G','G#','A','A#','B'];

export const PITCH_HUES = [0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330];

// Pre-computed HSL color strings for pitch classes
export const PITCH_STROKE_COLORS = PITCH_HUES.map(h => `hsl(${h}, 90%, 55%)`);
export const PITCH_FILL_COLORS = PITCH_HUES.map(h => `hsl(${h}, 100%, 70%)`);

export const NATURALS = new Set([0, 2, 4, 5, 7, 9, 11]);

export const DEFAULT_A4 = 440;
