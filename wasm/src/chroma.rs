// wasm/src/chroma.rs — Peak-gated chroma pipeline
// ==================================================
// Implements the full DSP pipeline downstream of the FFT magnitude spectrum:
//
//   RingBuffer          — sample accumulator with hop-based advancement
//   estimate_noise_floor — per-bin EMA noise floor tracker
//   whiten_spectrum      — local-mean spectral whitening (flattens tilt)
//   apply_gate           — zero bins below noise_floor * gate_ratio
//   pick_peaks           — local maxima within MIN_FREQ..MAX_FREQ
//   suppress_harmonics   — attenuate peaks that are overtones of a stronger peak
//   compute_chroma       — map surviving peaks to 12 pitch-class bins
//   normalize_chroma     — scale chroma to [0.0, 1.0]
//   find_dominant        — Hz and cents deviation of the strongest peak
//   active_pitch_classes — indices above a presence threshold

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const FFT_SIZE: usize = 8192;
pub const HOP_SIZE: usize = 1024;
pub const SAMPLE_RATE: f32 = 44100.0;
pub const A4_HZ: f32 = 440.0;
pub const A4_MIDI: f32 = 69.0;
pub const MIN_FREQ: f32 = 27.0;   // A0 — lowest note on a piano
pub const MAX_FREQ: f32 = 4186.0; // C8 — highest note we care about
pub const PRESENCE_THRESHOLD: f32 = 0.4;
pub const DEFAULT_GATE_RATIO: f32 = 1.5;

// ---------------------------------------------------------------------------
// RingBuffer
// ---------------------------------------------------------------------------

/// A simple ring buffer that accumulates incoming audio samples.
/// It keeps at most `capacity` samples. `push_chunk` appends to the end,
/// discarding the oldest samples when the buffer is full. `advance` removes
/// `hop` samples from the front, enabling overlapped analysis frames.
pub struct RingBuffer {
    buf: Vec<f32>,
    capacity: usize,
    /// Number of valid samples currently stored (0..=capacity).
    len: usize,
    /// Index of the oldest sample.
    head: usize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        RingBuffer {
            buf: vec![0.0f32; capacity],
            capacity,
            len: 0,
            head: 0,
        }
    }

    /// Append `chunk` to the ring buffer.
    /// If the chunk is larger than capacity, only the last `capacity` samples
    /// are retained (older samples are silently dropped).
    pub fn push_chunk(&mut self, chunk: &[f32]) {
        let n = chunk.len();
        if n == 0 {
            return;
        }
        if n >= self.capacity {
            // Keep only the last `capacity` samples from the chunk.
            let start = n - self.capacity;
            for i in 0..self.capacity {
                self.buf[i] = chunk[start + i];
            }
            self.head = 0;
            self.len = self.capacity;
            return;
        }
        // Write samples one by one into the circular buffer.
        for &s in chunk {
            let write_idx = (self.head + self.len) % self.capacity;
            if self.len == self.capacity {
                // Overwrite oldest — advance head.
                self.buf[write_idx] = s;
                self.head = (self.head + 1) % self.capacity;
            } else {
                self.buf[write_idx] = s;
                self.len += 1;
            }
        }
    }

    /// Returns `true` when at least `FFT_SIZE` samples are available.
    pub fn is_ready(&self) -> bool {
        self.len >= FFT_SIZE
    }

    /// Return a contiguous slice (or copy) of the most recent `FFT_SIZE` samples.
    /// Allocates a temporary Vec only when the frame wraps around the ring boundary.
    ///
    /// Caller should call `.to_vec()` to own the data before calling `advance`.
    pub fn get_frame(&self) -> Vec<f32> {
        debug_assert!(self.is_ready(), "get_frame called before ring buffer is ready");
        let avail = self.len.min(self.capacity);
        let start_offset = if avail >= FFT_SIZE { avail - FFT_SIZE } else { 0 };
        let start = (self.head + start_offset) % self.capacity;

        let mut frame = Vec::with_capacity(FFT_SIZE);
        for i in 0..FFT_SIZE {
            frame.push(self.buf[(start + i) % self.capacity]);
        }
        frame
    }

    /// Discard the `hop` oldest samples from the buffer.
    pub fn advance(&mut self, hop: usize) {
        let drop = hop.min(self.len);
        self.head = (self.head + drop) % self.capacity;
        self.len -= drop;
    }

    /// Reset to empty.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        for s in self.buf.iter_mut() {
            *s = 0.0;
        }
    }
}

// ---------------------------------------------------------------------------
// estimate_noise_floor
// ---------------------------------------------------------------------------

/// Update the per-bin noise floor EMA in place.
///
/// Uses asymmetric tracking so sustained signals don't inflate the floor:
///   - When spectrum[i] <= ema[i]: track downward at normal rate (alpha).
///   - When spectrum[i] > ema[i]: track upward very slowly (alpha * 0.01).
///
/// On the first call (`ema.is_empty()`), `ema` is initialised to zero so
/// that all peaks pass the gate on the very first frame.
///
/// `alpha ≈ 0.1` → normal decay rate; upward rate is 100× slower.
pub fn estimate_noise_floor(spectrum: &[f32], ema: &mut Vec<f32>, alpha: f32) {
    if ema.is_empty() {
        ema.resize(spectrum.len(), 0.0);
        return;
    }
    let len = ema.len().min(spectrum.len());
    for i in 0..len {
        if spectrum[i] <= ema[i] {
            // Signal at or below floor: track downward normally
            ema[i] = alpha * spectrum[i] + (1.0 - alpha) * ema[i];
        } else {
            // Signal above floor: rise very slowly to avoid tracking sustained tones
            let slow_alpha = alpha * 0.01;
            ema[i] = slow_alpha * spectrum[i] + (1.0 - slow_alpha) * ema[i];
        }
    }
}

// ---------------------------------------------------------------------------
// whiten_spectrum
// ---------------------------------------------------------------------------

/// Spectral whitening: divide each bin by its local mean.
///
/// Uses a frequency-proportional window: for each bin, the window radius is
/// `max(3, round(bin * 0.15))` — approximately 15% of the bin index.
/// At low bins (bass), the window is small (preserving narrow peaks); at high
/// bins, the window is larger (effective broadband whitening).
///
/// Returns `spectrum[i] / (local_mean + 1e-9)` to prevent division by zero.
pub fn whiten_spectrum(spectrum: &[f32], sample_rate: f32, fft_size: usize) -> Vec<f32> {
    let _ = (sample_rate, fft_size); // reserved for future use; window scales by bin index
    let n = spectrum.len();
    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let window_radius = 3usize.max((i as f32 * 0.15).round() as usize);
        let lo = if i >= window_radius { i - window_radius } else { 0 };
        let hi = (i + window_radius + 1).min(n); // exclusive
        let count = (hi - lo) as f32;
        let mut sum = 0.0f32;
        for j in lo..hi {
            sum += spectrum[j];
        }
        let local_mean = sum / count;
        out.push(spectrum[i] / (local_mean + 1e-9));
    }
    out
}

// ---------------------------------------------------------------------------
// apply_gate
// ---------------------------------------------------------------------------

/// Zero out `whitened[i]` where the **raw** spectrum is below the noise floor.
///
/// Gate condition: `raw[i] < ema[i] * gate_ratio` → `whitened[i] = 0.0`.
///
/// This keeps the whitened (relative) values for peaks that clearly exceed
/// the running noise floor, and silences everything else.
pub fn apply_gate(whitened: &mut [f32], raw: &[f32], ema: &[f32], gate_ratio: f32) {
    let len = whitened.len().min(raw.len()).min(ema.len());
    for i in 0..len {
        if raw[i] < ema[i] * gate_ratio {
            whitened[i] = 0.0;
        }
    }
}

// ---------------------------------------------------------------------------
// pick_peaks
// ---------------------------------------------------------------------------

/// Find local maxima in the gated spectrum within the frequency range
/// `[MIN_FREQ, MAX_FREQ]`.
///
/// A bin `i` is a peak if:
///   - `gated[i] > gated[i-1]`
///   - `gated[i] > gated[i+1]`
///   - `gated[i] > 0.0`
///   - `MIN_FREQ ≤ freq ≤ MAX_FREQ`
///
/// Returns a `Vec<(bin_index, magnitude)>`.
pub fn pick_peaks(gated: &[f32], sample_rate: f32, fft_size: usize) -> Vec<(usize, f32)> {
    let n = gated.len();
    if n < 3 {
        return Vec::new();
    }
    let mut peaks = Vec::new();
    let bin_to_hz = sample_rate / fft_size as f32;

    for i in 1..n - 1 {
        let val = gated[i];
        if val > 0.0 && val > gated[i - 1] && val > gated[i + 1] {
            let freq = i as f32 * bin_to_hz;
            if freq >= MIN_FREQ && freq <= MAX_FREQ {
                peaks.push((i, val));
            }
        }
    }
    peaks
}

// ---------------------------------------------------------------------------
// suppress_harmonics
// ---------------------------------------------------------------------------

/// Suppress peaks that appear to be integer harmonics of a stronger peak
/// by subtracting the expected harmonic contribution.
///
/// Natural harmonics of a fundamental fall off roughly as 1/r (where r is the
/// harmonic ratio).  For each detected harmonic match, we subtract:
///
///     q.mag = max(0, q.mag − p.mag × strength / r)
///
/// This scales with the fundamental's loudness: a loud note subtracts more
/// from its harmonics than a quiet one.  If energy remains after subtraction,
/// it's likely an independent pitch (e.g. a real chord note).
///
/// Parameters:
///   - `strength`: 0.0 = off, 1.0 = standard 1/r model, 2.0 = aggressive.
///   - `max_ratio`: highest harmonic ratio to check (default 6).
///
/// This is O(n²) over peaks; in practice there are < 20 peaks per frame.
pub fn suppress_harmonics(peaks: &mut Vec<(usize, f32)>, strength: f32, max_ratio: usize) {
    if strength <= 0.0 {
        return;
    }

    // Sort descending by magnitude (strongest first).
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(core::cmp::Ordering::Equal));

    let n = peaks.len();
    for p_idx in 0..n {
        let p_bin = peaks[p_idx].0;
        let p_mag = peaks[p_idx].1;
        for r in 2usize..=max_ratio {
            let expected = p_bin * r;
            let tolerance = 1usize.max((expected as f32 * 0.02).round() as usize);
            let subtraction = p_mag * strength / r as f32;
            for q_idx in (p_idx + 1)..n {
                let q_bin = peaks[q_idx].0;
                let diff = if q_bin >= expected {
                    q_bin - expected
                } else {
                    expected - q_bin
                };
                if diff <= tolerance {
                    peaks[q_idx].1 = (peaks[q_idx].1 - subtraction).max(0.0);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// compute_chroma
// ---------------------------------------------------------------------------

/// Map surviving peaks to a 12-element chroma vector.
///
/// For each peak `(bin, mag)`:
///   freq = bin * sample_rate / fft_size
///   midi = 12.0 * log2(freq / A4_HZ) + A4_MIDI
///   pitch_class = midi.round() as i32 % 12   (adjusted to 0..11)
///   chroma[pitch_class] += mag
///
/// Returns the un-normalised chroma array.
pub fn compute_chroma(peaks: &[(usize, f32)], sample_rate: f32, fft_size: usize) -> [f32; 12] {
    let mut chroma = [0.0f32; 12];
    let bin_to_hz = sample_rate / fft_size as f32;

    for &(bin, mag) in peaks {
        let freq = bin as f32 * bin_to_hz;
        if freq <= 0.0 {
            continue;
        }
        let midi = 12.0 * (freq / A4_HZ).log2() + A4_MIDI;
        let mut pc = midi.round() as i32 % 12;
        if pc < 0 {
            pc += 12;
        }
        chroma[pc as usize] += mag;
    }
    chroma
}

// ---------------------------------------------------------------------------
// compute_chroma_freqs
// ---------------------------------------------------------------------------

/// For each pitch class, return the frequency (Hz) of the strongest peak
/// that maps to that PC.  Returns 0.0 for pitch classes with no peaks.
/// Call on the same peaks used for `compute_chroma`.
pub fn compute_chroma_freqs(peaks: &[(usize, f32)], sample_rate: f32, fft_size: usize) -> [f32; 12] {
    let mut best_freq = [0.0f32; 12];
    let mut best_mag  = [0.0f32; 12];
    let bin_to_hz = sample_rate / fft_size as f32;

    for &(bin, mag) in peaks {
        let freq = bin as f32 * bin_to_hz;
        if freq <= 0.0 {
            continue;
        }
        let midi = 12.0 * (freq / A4_HZ).log2() + A4_MIDI;
        let mut pc = midi.round() as i32 % 12;
        if pc < 0 {
            pc += 12;
        }
        let pc = pc as usize;
        if mag > best_mag[pc] {
            best_mag[pc] = mag;
            best_freq[pc] = freq;
        }
    }
    best_freq
}

// ---------------------------------------------------------------------------
// normalize_chroma
// ---------------------------------------------------------------------------

/// Scale `chroma` in-place so the maximum value is 1.0.
/// If all bins are 0.0 (silence), the array is left unchanged.
pub fn normalize_chroma(chroma: &mut [f32; 12]) {
    let max = chroma.iter().cloned().fold(0.0f32, f32::max);
    if max > 0.0 {
        for v in chroma.iter_mut() {
            *v /= max;
        }
    }
}

// ---------------------------------------------------------------------------
// find_dominant
// ---------------------------------------------------------------------------

/// Return `(dominant_freq_hz, cents)` for the fundamental pitch.
///
/// Strategy: pick the **lowest-frequency** peak whose magnitude is at least
/// 30% of the strongest peak. Then apply **parabolic interpolation** on the
/// raw magnitude spectrum to get sub-bin frequency accuracy (~1 Hz instead
/// of ~10.77 Hz bin-width steps).
///
/// Parabolic interpolation formula (around peak bin k):
///   delta = 0.5 * (mag[k-1] - mag[k+1]) / (mag[k-1] - 2*mag[k] + mag[k+1])
///   interpolated_bin = k + delta
///
/// Returns `(0.0, 0)` if there are no peaks.
pub fn find_dominant(
    peaks: &[(usize, f32)],
    raw_spectrum: &[f32],
    sample_rate: f32,
    fft_size: usize,
) -> (f32, i32) {
    if peaks.is_empty() {
        return (0.0, 0);
    }

    let max_mag = peaks
        .iter()
        .map(|&(_, m)| m)
        .fold(0.0f32, f32::max);

    if max_mag <= 0.0 {
        return (0.0, 0);
    }

    // Pick the lowest-frequency peak that is at least 30% of the max.
    let threshold = max_mag * 0.3;
    let best = peaks
        .iter()
        .filter(|&&(_, m)| m >= threshold)
        .min_by_key(|&&(bin, _)| bin);

    let &(best_bin, _) = match best {
        Some(p) => p,
        None => return (0.0, 0),
    };

    // Parabolic interpolation using the RAW spectrum (not whitened) for
    // accurate peak shape. Requires bin to have valid neighbors.
    let interp_bin = if best_bin > 0 && best_bin + 1 < raw_spectrum.len() {
        let alpha = raw_spectrum[best_bin - 1];
        let beta  = raw_spectrum[best_bin];
        let gamma = raw_spectrum[best_bin + 1];
        let denom = alpha - 2.0 * beta + gamma;
        if denom.abs() > 1e-12 {
            let delta = 0.5 * (alpha - gamma) / denom;
            best_bin as f32 + delta.clamp(-0.5, 0.5)
        } else {
            best_bin as f32
        }
    } else {
        best_bin as f32
    };

    let freq = interp_bin * sample_rate / fft_size as f32;
    if freq <= 0.0 {
        return (0.0, 0);
    }

    let nearest_midi = (12.0 * (freq / A4_HZ).log2() + A4_MIDI).round();
    let nearest_hz = A4_HZ * 2.0f32.powf((nearest_midi - A4_MIDI) / 12.0);
    let cents = (1200.0 * (freq / nearest_hz).log2()).round() as i32;

    (freq, cents)
}

// ---------------------------------------------------------------------------
// active_pitch_classes
// ---------------------------------------------------------------------------

/// Return indices where `chroma[i] >= threshold`.
/// Input chroma should already be normalised to [0.0, 1.0].
pub fn active_pitch_classes(chroma: &[f32; 12], threshold: f32) -> Vec<u8> {
    chroma
        .iter()
        .enumerate()
        .filter_map(|(i, &v)| if v >= threshold { Some(i as u8) } else { None })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Test 1 — hann_window (defined in fft.rs but tested via chroma pipeline)
    // We mirror the test here to keep chroma tests self-contained.
    // -----------------------------------------------------------------------
    #[test]
    fn chroma_constants_are_consistent() {
        // Sanity: A4 at 440 Hz should map to pitch class 9 (A).
        let freq = A4_HZ;
        let midi = 12.0 * (freq / A4_HZ).log2() + A4_MIDI;
        let mut pc = midi.round() as i32 % 12;
        if pc < 0 { pc += 12; }
        assert_eq!(pc, 9, "A4 should be pitch class 9 (A)");
    }

    // -----------------------------------------------------------------------
    // Test 2 — compute_chroma: a synthetic peak at A4 → pitch_class 9
    // -----------------------------------------------------------------------
    #[test]
    fn compute_chroma_a4_peak_maps_to_pitch_class_9() {
        // A4 = 440 Hz; with FFT_SIZE=4096 and SAMPLE_RATE=44100:
        //   bin = round(440 * 4096 / 44100) = round(40.84) = 41
        // Verify that a peak at bin 41 ends up in chroma[9].
        let a4_bin = (A4_HZ * FFT_SIZE as f32 / SAMPLE_RATE).round() as usize;
        let peaks = vec![(a4_bin, 1.0f32)];
        let chroma = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);

        // pitch class 9 = A
        assert!(
            chroma[9] > 0.0,
            "A4 peak (bin {}) should contribute to chroma[9], got {:?}",
            a4_bin,
            chroma
        );
        // And it should be the dominant pitch class
        let max_pc = chroma
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap_or(0);
        assert_eq!(max_pc, 9, "pitch class 9 should have highest chroma energy");
    }

    // -----------------------------------------------------------------------
    // Test 3 — active_pitch_classes: values above threshold are returned
    // -----------------------------------------------------------------------
    #[test]
    fn active_pitch_classes_above_threshold() {
        let mut chroma = [0.0f32; 12];
        // Place energy in pitch classes 0 (C), 4 (E), 7 (G) — a C major chord
        chroma[0] = 1.0;
        chroma[4] = 0.8;
        chroma[7] = 0.6;
        chroma[2] = 0.3; // below default threshold — should be excluded

        let threshold = PRESENCE_THRESHOLD; // 0.4
        let active = active_pitch_classes(&chroma, threshold);

        assert!(active.contains(&0), "C should be active");
        assert!(active.contains(&4), "E should be active");
        assert!(active.contains(&7), "G should be active");
        assert!(!active.contains(&2), "D (0.3) should not be active at threshold 0.4");
        assert_eq!(active.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Additional: RingBuffer accumulates correctly
    // -----------------------------------------------------------------------
    #[test]
    fn ring_buffer_accumulates_and_is_ready() {
        let mut rb = RingBuffer::new(FFT_SIZE);
        assert!(!rb.is_ready());

        // Push FFT_SIZE - 1 samples → not ready yet
        let chunk = vec![1.0f32; FFT_SIZE - 1];
        rb.push_chunk(&chunk);
        assert!(!rb.is_ready());

        // Push one more sample → now ready
        rb.push_chunk(&[1.0f32]);
        assert!(rb.is_ready());

        // Frame should be FFT_SIZE samples
        let frame = rb.get_frame();
        assert_eq!(frame.len(), FFT_SIZE);
    }

    // -----------------------------------------------------------------------
    // Additional: advance reduces count
    // -----------------------------------------------------------------------
    #[test]
    fn ring_buffer_advance_reduces_ready_state() {
        let mut rb = RingBuffer::new(FFT_SIZE);
        let chunk = vec![0.5f32; FFT_SIZE];
        rb.push_chunk(&chunk);
        assert!(rb.is_ready());

        rb.advance(HOP_SIZE);
        // After advancing by HOP_SIZE, FFT_SIZE - HOP_SIZE samples remain → not ready
        assert!(!rb.is_ready());
    }

    // -----------------------------------------------------------------------
    // Additional: normalize_chroma scales to 1.0
    // -----------------------------------------------------------------------
    #[test]
    fn normalize_chroma_max_becomes_one() {
        let mut chroma = [0.0f32; 12];
        chroma[3] = 5.0;
        chroma[7] = 2.5;
        normalize_chroma(&mut chroma);
        assert!((chroma[3] - 1.0).abs() < 1e-6, "max should be 1.0");
        assert!((chroma[7] - 0.5).abs() < 1e-6, "half-max should be 0.5");
    }

    // -----------------------------------------------------------------------
    // Additional: suppress_harmonics attenuates harmonic peaks
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_harmonics_attenuates_overtones() {
        // Fundamental at bin 40, harmonic at bin 80 (2x)
        let mut peaks = vec![(40usize, 1.0f32), (80usize, 0.8f32)];
        suppress_harmonics(&mut peaks, 1.0, 6);

        // Find the peak at bin 80 after suppression
        let harmonic_mag = peaks
            .iter()
            .find(|&&(b, _)| b == 80)
            .map(|&(_, m)| m)
            .unwrap_or(0.0);
        assert!(
            harmonic_mag < 0.8,
            "harmonic at bin 80 should be attenuated, got {}",
            harmonic_mag
        );
    }

    // -----------------------------------------------------------------------
    // Additional: whiten_spectrum produces near-uniform output for flat input
    // -----------------------------------------------------------------------
    #[test]
    fn whiten_spectrum_flat_input_near_one() {
        // A flat spectrum of constant value c → local_mean = c → output = c/(c+eps) ≈ 1.0
        let n = 64;
        let spectrum = vec![2.0f32; n];
        let whitened = whiten_spectrum(&spectrum, SAMPLE_RATE, FFT_SIZE);
        assert_eq!(whitened.len(), n);
        for &v in &whitened {
            assert!((v - 1.0).abs() < 0.01, "flat spectrum should whiten to ~1.0, got {}", v);
        }
    }

    // -----------------------------------------------------------------------
    // Additional: find_dominant returns correct Hz and cents for A4 peak
    // -----------------------------------------------------------------------
    #[test]
    fn find_dominant_a4_bin_returns_near_zero_cents() {
        let a4_bin = (A4_HZ * FFT_SIZE as f32 / SAMPLE_RATE).round() as usize;
        let peaks = vec![(a4_bin, 1.0f32)];

        // Build a synthetic spectrum with a peak at a4_bin for parabolic interp
        let spectrum_len = FFT_SIZE / 2 + 1;
        let mut spectrum = vec![0.0f32; spectrum_len];
        if a4_bin < spectrum_len {
            spectrum[a4_bin] = 1.0;
            if a4_bin > 0 { spectrum[a4_bin - 1] = 0.3; }
            if a4_bin + 1 < spectrum_len { spectrum[a4_bin + 1] = 0.3; }
        }

        let (freq, cents) = find_dominant(&peaks, &spectrum, SAMPLE_RATE, FFT_SIZE);

        // Frequency should be close to 440 Hz (within one bin width ≈ 10.77 Hz)
        assert!(
            (freq - A4_HZ).abs() < 12.0,
            "dominant freq should be near 440 Hz, got {}",
            freq
        );
        // Cents should be within ±50
        assert!(
            cents.abs() <= 50,
            "cents should be in [-50,50], got {}",
            cents
        );
    }

    // -----------------------------------------------------------------------
    // Test: compute_chroma_freqs — A4 peak returns Hz in PC 9
    // -----------------------------------------------------------------------
    #[test]
    fn compute_chroma_freqs_a4_peak_returns_hz_in_pc9() {
        let a4_bin = (A4_HZ * FFT_SIZE as f32 / SAMPLE_RATE).round() as usize;
        let peaks = vec![(a4_bin, 1.0f32)];
        let freqs = compute_chroma_freqs(&peaks, SAMPLE_RATE, FFT_SIZE);

        let expected_hz = a4_bin as f32 * SAMPLE_RATE / FFT_SIZE as f32;
        assert!(
            (freqs[9] - expected_hz).abs() < 1.0,
            "chroma_freqs[9] should be ~{:.1} Hz, got {:.1}",
            expected_hz, freqs[9]
        );
        for (i, &f) in freqs.iter().enumerate() {
            if i != 9 {
                assert!(f == 0.0, "chroma_freqs[{}] should be 0.0, got {}", i, f);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Test: compute_chroma_freqs — empty peaks returns all zeros
    // -----------------------------------------------------------------------
    #[test]
    fn compute_chroma_freqs_empty_peaks_returns_zeros() {
        let peaks: Vec<(usize, f32)> = vec![];
        let freqs = compute_chroma_freqs(&peaks, SAMPLE_RATE, FFT_SIZE);
        for (i, &f) in freqs.iter().enumerate() {
            assert!(f == 0.0, "chroma_freqs[{}] should be 0.0, got {}", i, f);
        }
    }

    // -----------------------------------------------------------------------
    // Test: compute_chroma_freqs — multiple peaks in same PC keeps strongest
    // -----------------------------------------------------------------------
    #[test]
    fn compute_chroma_freqs_keeps_strongest_peak_per_pc() {
        let a4_bin = (A4_HZ * FFT_SIZE as f32 / SAMPLE_RATE).round() as usize;
        let a5_bin = (880.0 * FFT_SIZE as f32 / SAMPLE_RATE).round() as usize;
        let peaks = vec![(a4_bin, 0.5f32), (a5_bin, 1.0f32)];
        let freqs = compute_chroma_freqs(&peaks, SAMPLE_RATE, FFT_SIZE);

        let a5_hz = a5_bin as f32 * SAMPLE_RATE / FFT_SIZE as f32;
        assert!(
            (freqs[9] - a5_hz).abs() < 1.0,
            "chroma_freqs[9] should be A5 (~{:.1} Hz), got {:.1}",
            a5_hz, freqs[9]
        );
    }

    // -----------------------------------------------------------------------
    // Test: compute_chroma_freqs — chord populates multiple PCs
    // -----------------------------------------------------------------------
    #[test]
    fn compute_chroma_freqs_chord_populates_multiple_pcs() {
        let c4_bin = (261.63 * FFT_SIZE as f32 / SAMPLE_RATE).round() as usize;
        let e4_bin = (329.63 * FFT_SIZE as f32 / SAMPLE_RATE).round() as usize;
        let g4_bin = (392.00 * FFT_SIZE as f32 / SAMPLE_RATE).round() as usize;
        let peaks = vec![(c4_bin, 1.0f32), (e4_bin, 0.8f32), (g4_bin, 0.6f32)];
        let freqs = compute_chroma_freqs(&peaks, SAMPLE_RATE, FFT_SIZE);

        assert!(freqs[0] > 0.0, "C should have frequency, got {}", freqs[0]);
        assert!(freqs[4] > 0.0, "E should have frequency, got {}", freqs[4]);
        assert!(freqs[7] > 0.0, "G should have frequency, got {}", freqs[7]);

        for i in [1, 2, 3, 5, 6, 8, 9, 10, 11] {
            assert!(freqs[i] == 0.0, "chroma_freqs[{}] should be 0.0, got {}", i, freqs[i]);
        }
    }
}
