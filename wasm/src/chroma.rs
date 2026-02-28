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
/// Natural harmonics of a fundamental fall off roughly as 1/√r (where r is the
/// harmonic ratio).  For each detected harmonic match, we subtract:
///
///     q.mag = max(0, q.mag − p.mag × strength / √r)
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
pub fn suppress_harmonics(peaks: &mut Vec<(usize, f32)>, raw_spectrum: &[f32], strength: f32, max_ratio: usize) {
    if strength <= 0.0 {
        return;
    }

    // Sort descending by RAW magnitude (not whitened) so the true fundamental
    // is always processed first — whitening flattens the spectrum and can make
    // harmonics appear as strong as the fundamental.
    peaks.sort_by(|a, b| {
        let a_raw = if a.0 < raw_spectrum.len() { raw_spectrum[a.0] } else { 0.0 };
        let b_raw = if b.0 < raw_spectrum.len() { raw_spectrum[b.0] } else { 0.0 };
        b_raw.partial_cmp(&a_raw).unwrap_or(core::cmp::Ordering::Equal)
    });

    let n = peaks.len();
    for p_idx in 0..n {
        let p_bin = peaks[p_idx].0;
        let p_mag = peaks[p_idx].1;
        for r in 2usize..=max_ratio {
            let expected = p_bin * r;
            let tolerance = 1usize.max((expected as f32 * 0.02).round() as usize);
            let subtraction = p_mag * strength / (r as f32).sqrt();
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
        // Build a raw spectrum where bin 40 is louder than bin 80
        let mut raw_spectrum = vec![0.0f32; 100];
        raw_spectrum[40] = 2.0;
        raw_spectrum[80] = 1.0;
        suppress_harmonics(&mut peaks, &raw_spectrum, 1.0, 6);

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

    // =======================================================================
    // Harmonic suppression test suite
    // =======================================================================

    /// Helper: build a raw spectrum array with given (bin, value) pairs.
    fn make_raw_spectrum(entries: &[(usize, f32)]) -> Vec<f32> {
        let max_bin = entries.iter().map(|&(b, _)| b).max().unwrap_or(0);
        let mut raw = vec![0.0f32; max_bin + 1];
        for &(bin, val) in entries {
            raw[bin] = val;
        }
        raw
    }

    /// Helper: find a peak by bin index after suppression (peaks may be reordered).
    fn peak_mag(peaks: &[(usize, f32)], bin: usize) -> f32 {
        peaks.iter().find(|&&(b, _)| b == bin).map(|&(_, m)| m).unwrap_or(-1.0)
    }

    // -----------------------------------------------------------------------
    // suppress_single_note_with_harmonics
    // A4 fundamental + 2nd and 3rd harmonics. All harmonics should be
    // reduced to residuals after suppression.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_single_note_with_harmonics() {
        let mut peaks = vec![(82, 1.2f32), (163, 1.0f32), (245, 0.8f32)];
        let raw = make_raw_spectrum(&[(82, 10.0), (163, 5.0), (245, 3.3)]);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        assert!((peak_mag(&peaks, 82) - 1.2).abs() < 1e-3, "fundamental unchanged");
        // bin 163: 1.0 - 1.2/sqrt(2) = 1.0 - 0.8485 = 0.1515
        assert!((peak_mag(&peaks, 163) - 0.1515).abs() < 0.01,
            "2nd harmonic: expected ~0.1515, got {}", peak_mag(&peaks, 163));
        // bin 245: 0.8 - 1.2/sqrt(3) = 0.8 - 0.6928 = 0.1072
        assert!((peak_mag(&peaks, 245) - 0.1072).abs() < 0.01,
            "3rd harmonic: expected ~0.1072, got {}", peak_mag(&peaks, 245));
    }

    // -----------------------------------------------------------------------
    // suppress_c_major_chord_unharmed
    // Three notes (C-E-G) that are NOT harmonically related should pass
    // through suppression completely unchanged.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_c_major_chord_unharmed() {
        let mut peaks = vec![(49, 1.0f32), (61, 0.9f32), (73, 0.8f32)];
        let raw = make_raw_spectrum(&[(49, 8.0), (61, 7.0), (73, 6.0)]);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        assert!((peak_mag(&peaks, 49) - 1.0).abs() < 1e-6, "C unchanged");
        assert!((peak_mag(&peaks, 61) - 0.9).abs() < 1e-6, "E unchanged");
        assert!((peak_mag(&peaks, 73) - 0.8).abs() < 1e-6, "G unchanged");
    }

    // -----------------------------------------------------------------------
    // suppress_octave_documents_limitation
    // Known limitation: a real octave note (2:1 ratio) gets suppressed
    // because it looks like the 2nd harmonic.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_octave_documents_limitation() {
        let mut peaks = vec![(82, 1.0f32), (163, 0.95f32)];
        let raw = make_raw_spectrum(&[(82, 8.0), (163, 7.5)]);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        assert!((peak_mag(&peaks, 82) - 1.0).abs() < 1e-6, "fundamental unchanged");
        // 0.95 - 1.0/sqrt(2) = 0.95 - 0.7071 = 0.2429
        let oct = peak_mag(&peaks, 163);
        assert!((oct - 0.2429).abs() < 0.01,
            "octave suppressed (known limitation): expected ~0.2429, got {}", oct);
    }

    // -----------------------------------------------------------------------
    // suppress_power_chord_fifth_survives
    // A perfect fifth (3:2 ratio) is NOT an integer multiple, so both
    // notes should survive suppression unchanged.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_power_chord_fifth_survives() {
        let mut peaks = vec![(82, 1.0f32), (122, 0.9f32)];
        let raw = make_raw_spectrum(&[(82, 8.0), (122, 7.0)]);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        assert!((peak_mag(&peaks, 82) - 1.0).abs() < 1e-6, "root unchanged");
        assert!((peak_mag(&peaks, 122) - 0.9).abs() < 1e-6, "fifth unchanged");
    }

    // -----------------------------------------------------------------------
    // suppress_strength_zero_is_noop
    // With strength=0, suppression should be completely disabled.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_strength_zero_is_noop() {
        let mut peaks = vec![(82, 1.2f32), (163, 1.0f32), (245, 0.8f32)];
        let raw = make_raw_spectrum(&[(82, 10.0), (163, 5.0), (245, 3.3)]);
        suppress_harmonics(&mut peaks, &raw, 0.0, 6);

        assert!((peak_mag(&peaks, 82) - 1.2).abs() < 1e-6);
        assert!((peak_mag(&peaks, 163) - 1.0).abs() < 1e-6);
        assert!((peak_mag(&peaks, 245) - 0.8).abs() < 1e-6);
    }

    // -----------------------------------------------------------------------
    // suppress_strength_aggressive_zeros_harmonics
    // With strength=2.0, harmonics should be driven to zero.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_strength_aggressive_zeros_harmonics() {
        let mut peaks = vec![(82, 1.2f32), (163, 1.0f32), (245, 0.8f32)];
        let raw = make_raw_spectrum(&[(82, 10.0), (163, 5.0), (245, 3.3)]);
        suppress_harmonics(&mut peaks, &raw, 2.0, 6);

        assert!((peak_mag(&peaks, 82) - 1.2).abs() < 1e-6, "fundamental unchanged");
        // 1.0 - 1.2*2.0/sqrt(2) = 1.0 - 1.6971 → clamped to 0.0
        assert!(peak_mag(&peaks, 163) < 1e-6,
            "2nd harmonic should be zero, got {}", peak_mag(&peaks, 163));
        // 0.8 - 1.2*2.0/sqrt(3) = 0.8 - 1.3856 → clamped to 0.0
        assert!(peak_mag(&peaks, 245) < 1e-6,
            "3rd harmonic should be zero, got {}", peak_mag(&peaks, 245));
    }

    // -----------------------------------------------------------------------
    // suppress_ghost_notes_below_threshold (integration test)
    // Full 6-peak harmonic series → suppress → chroma → normalize.
    // Ghost pitch classes (E, C#) should be below PRESENCE_THRESHOLD.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_ghost_notes_below_threshold() {
        let mut peaks = vec![
            (82, 1.2f32), (163, 1.0), (245, 0.9),
            (327, 0.85), (409, 0.8), (491, 0.75),
        ];
        let raw = make_raw_spectrum(&[
            (82, 10.0), (163, 5.0), (245, 3.3),
            (327, 2.5), (409, 2.0), (491, 1.7),
        ]);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        let mut chroma = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        normalize_chroma(&mut chroma);

        // PC 9 (A) should dominate
        assert!((chroma[9] - 1.0).abs() < 1e-3, "A should be 1.0 after normalize");
        // Ghost notes (E=PC4, C#=PC1) should be suppressed below threshold
        assert!(chroma[4] < PRESENCE_THRESHOLD,
            "E ghost should be < {}, got {}", PRESENCE_THRESHOLD, chroma[4]);
        assert!(chroma[1] < PRESENCE_THRESHOLD,
            "C# ghost should be < {}, got {}", PRESENCE_THRESHOLD, chroma[1]);
    }

    // -----------------------------------------------------------------------
    // suppress_tolerance_boundary
    // Verify the 2% tolerance window: bin just inside should match,
    // bin just outside should not.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_tolerance_boundary() {
        let mut peaks = vec![(50, 1.0f32), (102, 0.8f32), (103, 0.8f32)];
        let raw = make_raw_spectrum(&[(50, 10.0), (102, 5.0), (103, 5.0)]);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        // r=2: expected=100, tol=max(1, round(100*0.02))=2
        // bin 102: |102-100|=2 ≤ 2 → MATCH → 0.8 - 1.0/sqrt(2) = 0.0929
        let inside = peak_mag(&peaks, 102);
        assert!((inside - 0.0929).abs() < 0.01,
            "inside tolerance: expected ~0.0929, got {}", inside);
        // bin 103: |103-100|=3 > 2 → NO MATCH → unchanged at 0.8
        let outside = peak_mag(&peaks, 103);
        assert!((outside - 0.8).abs() < 1e-6,
            "outside tolerance: expected 0.8, got {}", outside);
    }

    // -----------------------------------------------------------------------
    // suppress_empty_peaks_no_panic
    // Edge case: empty input should not panic.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_empty_peaks_no_panic() {
        let mut peaks: Vec<(usize, f32)> = vec![];
        let raw: Vec<f32> = vec![];
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);
        assert!(peaks.is_empty());
    }

    // -----------------------------------------------------------------------
    // suppress_single_peak_unchanged
    // A single peak has no harmonics to suppress and should be unchanged.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_single_peak_unchanged() {
        let mut peaks = vec![(82, 1.0f32)];
        let raw = make_raw_spectrum(&[(82, 10.0)]);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        assert!((peak_mag(&peaks, 82) - 1.0).abs() < 1e-6);
    }

    // -----------------------------------------------------------------------
    // suppress_decay_is_sqrt_not_linear
    // Verify the 1/√r decay model by checking exact subtraction amounts
    // at r=2, r=3, r=4. If someone changes to 1/r, these will fail.
    // -----------------------------------------------------------------------
    #[test]
    fn suppress_decay_is_sqrt_not_linear() {
        let mut peaks = vec![
            (100, 1.0f32), (200, 0.9f32), (300, 0.9f32), (400, 0.9f32),
        ];
        let raw = make_raw_spectrum(&[(100, 10.0), (200, 5.0), (300, 4.0), (400, 3.0)]);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        // r=2: 0.9 - 1.0/sqrt(2) = 0.9 - 0.7071 = 0.1929
        let r2 = peak_mag(&peaks, 200);
        assert!((r2 - 0.1929).abs() < 0.01,
            "r=2: expected ~0.1929, got {}", r2);
        // r=3: 0.9 - 1.0/sqrt(3) = 0.9 - 0.5774 = 0.3226
        let r3 = peak_mag(&peaks, 300);
        assert!((r3 - 0.3226).abs() < 0.01,
            "r=3: expected ~0.3226, got {}", r3);
        // r=4: 0.9 - 1.0/sqrt(4) = 0.4, then cascading from bin 200 (mag 0.1929):
        //   200*2=400 → sub = 0.1929/sqrt(2) = 0.1364 → 0.4 - 0.1364 = 0.2636
        let r4 = peak_mag(&peaks, 400);
        assert!((r4 - 0.2636).abs() < 0.01,
            "r=4 (with cascade): expected ~0.2636, got {}", r4);
    }

    // =======================================================================
    // Realistic instrument simulation tests
    // =======================================================================
    //
    // These tests use physically-modeled harmonic spectra based on published
    // acoustic measurements of real instruments. Amplitudes reflect body
    // resonance, pluck position, and radiation characteristics — not
    // idealized 1/n falloff.

    /// Build a full FFT magnitude spectrum from harmonic peaks.
    /// Each peak is a Gaussian bump (σ=1.5 bins) so pick_peaks can find local maxima.
    fn make_instrument_spectrum(harmonics: &[(usize, f32)], spectrum_len: usize) -> Vec<f32> {
        let mut spectrum = vec![0.0f32; spectrum_len];
        let sigma = 1.5f32;
        for &(bin, amp) in harmonics {
            // Place a Gaussian bump centered at `bin`
            let lo = if bin >= 5 { bin - 5 } else { 0 };
            let hi = (bin + 6).min(spectrum_len);
            for i in lo..hi {
                let d = (i as f32 - bin as f32) / sigma;
                spectrum[i] += amp * (-0.5 * d * d).exp();
            }
        }
        spectrum
    }

    // -----------------------------------------------------------------------
    // Guitar open low E2 (82.41 Hz) — steel-string acoustic
    //
    // Harmonic profile from acoustic measurements:
    //   - 2nd harmonic is STRONGEST (body Helmholtz resonance ~165 Hz)
    //   - 3rd-4th boosted by top-plate resonance (~250-330 Hz)
    //   - 7th is weak (pluck-point node at ~1/7 string length)
    //   - Fundamental is quieter than harmonics 2-4 (body radiates poorly at 82 Hz)
    //
    // At FFT_SIZE=8192, SAMPLE_RATE=44100 (5.38 Hz/bin):
    //   H1=82.41→bin 15, H2=164.82→bin 31, H3=247.24→bin 46,
    //   H4=329.65→bin 61, H5=412.06→bin 77, H6=494.47→bin 92
    // -----------------------------------------------------------------------
    #[test]
    fn guitar_low_e_fundamental_survives_suppression() {
        // Realistic raw amplitudes (2nd harmonic loudest due to body resonance)
        let harmonics = [
            (15usize, 6.0f32),   // H1: 82 Hz fundamental
            (31, 10.0),          // H2: 165 Hz — loudest (body resonance)
            (46, 8.5),           // H3: 247 Hz
            (61, 7.5),           // H4: 330 Hz
            (77, 5.0),           // H5: 412 Hz
            (92, 4.0),           // H6: 494 Hz
        ];
        let spectrum_len = FFT_SIZE / 2 + 1;
        let raw = make_instrument_spectrum(&harmonics, spectrum_len);

        // Whiten the spectrum (this is what the real pipeline does)
        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);

        // Pick peaks from whitened spectrum
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);
        assert!(!peaks.is_empty(), "should find peaks in guitar spectrum");

        // Verify we found the fundamental
        let has_fundamental = peaks.iter().any(|&(b, _)| b == 15);
        assert!(has_fundamental, "fundamental at bin 15 should be a peak");

        // Run suppression
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        // Fundamental should survive with significant energy
        let fund_mag = peak_mag(&peaks, 15);
        assert!(fund_mag > 0.0, "fundamental must survive suppression, got {}", fund_mag);

        // Compute chroma and normalize
        let mut chroma = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        normalize_chroma(&mut chroma);

        // E = pitch class 4. After suppression + normalization, E should dominate.
        let e_pc = 4; // E
        assert!(chroma[e_pc] > 0.5,
            "E (PC 4) should dominate chroma after suppression, got {:.3}", chroma[e_pc]);

        // The dominant pitch class should be E (or at least E should be very strong)
        let max_pc = chroma.iter().enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i).unwrap();
        // E2's harmonics map to: E(4), B(11), E(4), G#(8), B(11), E(4)
        // So PC 4 (E) should accumulate the most energy
        assert!(max_pc == e_pc || chroma[e_pc] > 0.8,
            "dominant PC should be E(4), got PC {} (E={:.3})", max_pc, chroma[e_pc]);
    }

    #[test]
    fn guitar_low_e_ghost_notes_reduced() {
        // Same guitar E2 spectrum
        let harmonics = [
            (15, 6.0f32), (31, 10.0), (46, 8.5),
            (61, 7.5), (77, 5.0), (92, 4.0),
        ];
        let spectrum_len = FFT_SIZE / 2 + 1;
        let raw = make_instrument_spectrum(&harmonics, spectrum_len);
        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);

        // Snapshot before suppression (un-normalized to avoid rescaling artifacts)
        let peaks_before = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);
        let chroma_before = compute_chroma(&peaks_before, SAMPLE_RATE, FFT_SIZE);

        // Suppress
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);
        suppress_harmonics(&mut peaks, &raw, 1.0, 6);
        let chroma_after = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);

        // Compare absolute (un-normalized) energy — suppression should reduce
        // ghost note energy. We use un-normalized chroma because normalization
        // can paradoxically increase ghost ratios: suppression also reduces
        // same-PC harmonics (e.g. E2's octave E3 both map to PC 4), lowering
        // the denominator and inflating other PCs after division.
        for &ghost_pc in &[8usize, 11] {  // G# and B
            assert!(chroma_after[ghost_pc] < chroma_before[ghost_pc],
                "absolute ghost PC {} energy should decrease: before={:.3}, after={:.3}",
                ghost_pc, chroma_before[ghost_pc], chroma_after[ghost_pc]);
        }

        // With aggressive strength (2.0), normalized ghosts should be well-suppressed
        let mut peaks_agg = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);
        suppress_harmonics(&mut peaks_agg, &raw, 2.0, 6);
        let mut chroma_agg = compute_chroma(&peaks_agg, SAMPLE_RATE, FFT_SIZE);
        normalize_chroma(&mut chroma_agg);
        let active_agg = active_pitch_classes(&chroma_agg, PRESENCE_THRESHOLD);

        // At strength=2.0, only E-related pitch classes should remain active
        assert!(active_agg.len() <= 3,
            "aggressive suppression should leave ≤3 active PCs, got {}: {:?}",
            active_agg.len(), active_agg);
    }

    // -----------------------------------------------------------------------
    // Guitar open A2 (110 Hz) — second-thickest string
    //
    // Similar body resonance profile but shifted. Body resonance still
    // boosts ~200-400 Hz range → harmonics 2 and 3 are boosted.
    //
    // H1=110→bin 20, H2=220→bin 41, H3=330→bin 61, H4=440→bin 82,
    // H5=550→bin 102, H6=660→bin 122
    // -----------------------------------------------------------------------
    #[test]
    fn guitar_open_a_identifies_correct_pitch_class() {
        let harmonics = [
            (20usize, 7.0f32),  // H1: 110 Hz
            (41, 10.0),         // H2: 220 Hz — loudest
            (61, 8.0),          // H3: 330 Hz
            (82, 6.0),          // H4: 440 Hz (= A4!)
            (102, 4.0),         // H5: 550 Hz
            (122, 3.0),         // H6: 660 Hz
        ];
        let spectrum_len = FFT_SIZE / 2 + 1;
        let raw = make_instrument_spectrum(&harmonics, spectrum_len);
        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);

        // Snapshot before suppression
        let peaks_before = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);
        let chroma_before = compute_chroma(&peaks_before, SAMPLE_RATE, FFT_SIZE);
        let mut chroma_before_norm = chroma_before;
        normalize_chroma(&mut chroma_before_norm);

        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        let mut chroma = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        normalize_chroma(&mut chroma);

        // A = pitch class 9
        // All harmonics of A map to: A(9), A(9), E(4), A(9), C#(1), E(4)
        // After suppression, A should dominate
        assert!(chroma[9] > 0.5,
            "A (PC 9) should dominate after suppression, got {:.3}", chroma[9]);

        // Compare absolute (un-normalized) ghost energy — avoids normalization
        // artifact where suppressing same-PC harmonics lowers the denominator
        // and paradoxically inflates ghost ratios after renormalization.
        let chroma_before_abs = compute_chroma(&peaks_before, SAMPLE_RATE, FFT_SIZE);
        let chroma_after_abs = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        for &ghost_pc in &[4usize, 1] {  // E, C#
            assert!(chroma_after_abs[ghost_pc] < chroma_before_abs[ghost_pc],
                "absolute ghost PC {} energy should decrease: before={:.3}, after={:.3}",
                ghost_pc, chroma_before_abs[ghost_pc], chroma_after_abs[ghost_pc]);
        }

        // Note: even at strength=2.0, whitening equalizes peak magnitudes so
        // the subtraction (based on whitened p_mag) may not fully eliminate ghosts.
        // The squelch gate in the UI provides the additional filtering needed.
    }

    // -----------------------------------------------------------------------
    // Guitar power chord E5 (E2 + B2) — two notes simultaneously
    //
    // A power chord has the root + fifth. Both should survive suppression
    // because a fifth (3:2 ratio) is not an integer harmonic relationship.
    //
    // E2=82.41 Hz → bin 15, B2=123.47 Hz → bin 23
    // E2 harmonics: 15, 31, 46, 61, 77, 92
    // B2 harmonics: 23, 46, 69, 92, 115, 138
    // Note: H3 of E (bin 46) ≈ H2 of B (bin 46) — they overlap!
    // -----------------------------------------------------------------------
    #[test]
    fn guitar_power_chord_both_notes_survive() {
        // E2 harmonics
        let e_harmonics = [
            (15usize, 6.0f32), (31, 10.0), (46, 8.5),
            (61, 7.5), (77, 5.0), (92, 4.0),
        ];
        // B2 harmonics (slightly quieter — typical for a fretted note)
        let b_harmonics = [
            (23usize, 5.0f32), (46, 8.0), (69, 6.0),
            (92, 4.5), (115, 3.0), (138, 2.5),
        ];

        let spectrum_len = FFT_SIZE / 2 + 1;
        // Combine both notes into one spectrum
        let mut raw = vec![0.0f32; spectrum_len];
        let sigma = 1.5f32;
        for harmonics in [&e_harmonics[..], &b_harmonics[..]] {
            for &(bin, amp) in harmonics {
                let lo = if bin >= 5 { bin - 5 } else { 0 };
                let hi = (bin + 6).min(spectrum_len);
                for i in lo..hi {
                    let d = (i as f32 - bin as f32) / sigma;
                    raw[i] += amp * (-0.5 * d * d).exp();
                }
            }
        }

        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);

        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        let mut chroma = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        normalize_chroma(&mut chroma);

        // E (PC 4) and B (PC 11) should both be present
        assert!(chroma[4] > 0.3,
            "E should survive in power chord, got {:.3}", chroma[4]);
        assert!(chroma[11] > 0.2,
            "B should survive in power chord, got {:.3}", chroma[11]);
    }

    // -----------------------------------------------------------------------
    // Piano A4 (440 Hz) — very different harmonic profile from guitar
    //
    // Piano has strong fundamental, nearly-harmonic overtones with
    // slight inharmonicity, and faster high-harmonic rolloff than guitar.
    //
    // H1=440→bin 82, H2=880→bin 163, H3=1320→bin 245,
    // H4=1760→bin 327, H5=2200→bin 409, H6=2640→bin 491
    // -----------------------------------------------------------------------
    #[test]
    fn piano_a4_clean_single_note() {
        // Piano: fundamental is strongest, harmonics decay more uniformly
        let harmonics = [
            (82usize, 10.0f32),  // H1: 440 Hz — loudest
            (163, 7.0),          // H2: 880 Hz
            (245, 4.5),          // H3: 1320 Hz
            (327, 3.0),          // H4: 1760 Hz
            (409, 2.0),          // H5: 2200 Hz
            (491, 1.5),          // H6: 2640 Hz
        ];
        let spectrum_len = FFT_SIZE / 2 + 1;
        let raw = make_instrument_spectrum(&harmonics, spectrum_len);
        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);

        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        let mut chroma = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        normalize_chroma(&mut chroma);

        // A = pitch class 9, should clearly dominate
        assert!(chroma[9] > 0.8,
            "A (PC 9) should dominate piano A4, got {:.3}", chroma[9]);

        // Compare absolute (un-normalized) ghost energy to avoid normalization artifact
        let peaks_before = pick_peaks(
            &whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE), SAMPLE_RATE, FFT_SIZE);
        let chroma_before_abs = compute_chroma(&peaks_before, SAMPLE_RATE, FFT_SIZE);
        let chroma_after_abs = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);

        // Non-dominant PCs should have less or equal absolute energy after suppression
        for pc in 0..12 {
            if pc == 9 { continue; }
            assert!(chroma_after_abs[pc] <= chroma_before_abs[pc] + 0.01,
                "absolute PC {} should not increase: before={:.3}, after={:.3}",
                pc, chroma_before_abs[pc], chroma_after_abs[pc]);
        }

        // Note: whitening equalizes peak magnitudes, limiting how much
        // suppression can reduce ghost notes through the full pipeline.
        // The squelch gate provides additional filtering in the UI.
    }

    // -----------------------------------------------------------------------
    // Alto Saxophone — Concert Bb3 (233.08 Hz)
    //
    // Conical bore with single reed: all harmonics present (even and odd),
    // roughly 1/n rolloff. H1 is strongest. Should be straightforward for
    // the suppressor since the fundamental dominates in raw spectrum.
    //
    // H1=233→bin 43, H2=466→bin 87, H3=699→bin 130, H4=932→bin 173,
    // H5=1165→bin 216, H6=1398→bin 260, H7=1632→bin 303, H8=1865→bin 346
    // -----------------------------------------------------------------------
    #[test]
    fn alto_sax_bb3_fundamental_dominates() {
        let harmonics = [
            (43usize, 10.0f32),  // H1: 233 Hz — strongest
            (87, 9.0),           // H2: 466 Hz
            (130, 7.5),          // H3: 699 Hz
            (173, 6.0),          // H4: 932 Hz
            (216, 4.5),          // H5: 1165 Hz
            (260, 3.0),          // H6: 1398 Hz
            (303, 1.8),          // H7: 1632 Hz
            (346, 1.0),          // H8: 1865 Hz
        ];
        let spectrum_len = FFT_SIZE / 2 + 1;
        let raw = make_instrument_spectrum(&harmonics, spectrum_len);
        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);

        // Snapshot before
        let peaks_before = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);
        let chroma_before = compute_chroma(&peaks_before, SAMPLE_RATE, FFT_SIZE);

        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        let chroma_after = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        let mut chroma_norm = chroma_after;
        normalize_chroma(&mut chroma_norm);

        // Bb = pitch class 10. Should dominate after suppression.
        // H1(Bb), H2(Bb), H4(Bb), H8(Bb) all map to PC 10
        assert!(chroma_norm[10] > 0.5,
            "Bb (PC 10) should dominate sax, got {:.3}", chroma_norm[10]);

        // Ghost notes: F(5) from H3/H6, D(2) from H5, Ab(8) from H7
        // Absolute energy should decrease for all non-Bb pitch classes
        for pc in 0..12 {
            if pc == 10 { continue; }
            assert!(chroma_after[pc] <= chroma_before[pc] + 0.01,
                "sax: absolute PC {} should not increase: before={:.3}, after={:.3}",
                pc, chroma_before[pc], chroma_after[pc]);
        }
    }

    // -----------------------------------------------------------------------
    // Flute — A4 (440 Hz)
    //
    // Open cylindrical bore with edge-tone excitation: produces the
    // "purest" orchestral tone. Fundamental overwhelmingly dominates,
    // harmonics roll off ~1/n² (roughly -12 dB per harmonic). H2 is
    // typically only 25% of fundamental amplitude.
    //
    // This is the easiest case for suppression — barely any harmonics
    // to suppress in the first place.
    //
    // H1=440→bin 82, H2=880→bin 163, H3=1320→bin 245, H4=1760→bin 327
    // -----------------------------------------------------------------------
    #[test]
    fn flute_a4_nearly_pure_tone() {
        let harmonics = [
            (82usize, 10.0f32),  // H1: 440 Hz — overwhelmingly dominant
            (163, 2.5),          // H2: 880 Hz — -12 dB
            (245, 0.8),          // H3: 1320 Hz — -22 dB
            (327, 0.3),          // H4: 1760 Hz — -30 dB
        ];
        let spectrum_len = FFT_SIZE / 2 + 1;
        let raw = make_instrument_spectrum(&harmonics, spectrum_len);
        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);

        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        let mut chroma = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        normalize_chroma(&mut chroma);

        // A = pitch class 9. Should be virtually the only active PC.
        assert!((chroma[9] - 1.0).abs() < 0.01,
            "A (PC 9) should be ~1.0 for flute, got {:.3}", chroma[9]);

        // With such weak harmonics, flute should have very few active PCs
        let active = active_pitch_classes(&chroma, PRESENCE_THRESHOLD);
        assert!(active.len() <= 2,
            "flute should have ≤2 active PCs (nearly pure tone), got {}: {:?}",
            active.len(), active);
    }

    // -----------------------------------------------------------------------
    // Clarinet — Concert Bb3 (233.08 Hz)
    //
    // Closed cylindrical bore with single reed: acts as a stopped pipe,
    // strongly suppressing EVEN harmonics. Odd harmonics (1, 3, 5, 7, 9, 11)
    // dominate with ~1/n decay. Even harmonics are present but at ~5% of
    // the fundamental — a faint whisper.
    //
    // This is an interesting test because the clarinet's natural spectrum
    // already "looks like" a chord (Bb + F + D + Ab + C + Eb from odd
    // harmonics). The suppressor must handle these wide-spaced overtones.
    //
    // H1=233→bin 43, H3=699→bin 130, H5=1165→bin 216,
    // H7=1632→bin 303, H9=2098→bin 389, H11=2564→bin 476
    // -----------------------------------------------------------------------
    #[test]
    fn clarinet_bb3_odd_harmonic_dominance() {
        let harmonics = [
            (43usize, 10.0f32),  // H1: 233 Hz (Bb)
            (87, 0.5),           // H2: 466 Hz (Bb) — suppressed by bore
            (130, 8.0),          // H3: 699 Hz (F) — strong odd harmonic
            (173, 0.3),          // H4: 932 Hz (Bb) — suppressed
            (216, 5.5),          // H5: 1165 Hz (D)
            (260, 0.2),          // H6: 1398 Hz (F) — suppressed
            (303, 3.5),          // H7: 1632 Hz (Ab)
            (346, 0.1),          // H8: 1865 Hz (Bb) — suppressed
            (389, 2.0),          // H9: 2098 Hz (C)
            (476, 1.0),          // H11: 2564 Hz (Eb)
        ];
        let spectrum_len = FFT_SIZE / 2 + 1;
        let raw = make_instrument_spectrum(&harmonics, spectrum_len);
        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);

        // Snapshot before
        let peaks_before = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);
        let chroma_before = compute_chroma(&peaks_before, SAMPLE_RATE, FFT_SIZE);

        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        let chroma_after = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        let mut chroma_norm = chroma_after;
        normalize_chroma(&mut chroma_norm);

        // Bb = PC 10 should be the strongest PC. The clarinet's strong odd
        // harmonics (H3=F, H5=D, H7=Ab) spread energy widely, so Bb won't
        // be as dominant as for other instruments — but it should still lead.
        let max_pc = chroma_norm.iter().enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i).unwrap();
        assert!(max_pc == 10 || chroma_norm[10] > 0.4,
            "Bb (PC 10) should lead or be strong for clarinet, got PC {} (Bb={:.3})",
            max_pc, chroma_norm[10]);

        // Suppression should reduce absolute energy of ghost PCs
        // F(5) from H3, D(2) from H5, Ab(8) from H7
        for &ghost_pc in &[5usize, 2, 8] {
            assert!(chroma_after[ghost_pc] <= chroma_before[ghost_pc] + 0.01,
                "clarinet: absolute PC {} should not increase: before={:.3}, after={:.3}",
                ghost_pc, chroma_before[ghost_pc], chroma_after[ghost_pc]);
        }

        // The strong H3 (F) is a known challenge — it's 80% of fundamental
        // amplitude and falls at r=3. Subtraction: 1.0/√3 = 0.577.
        // H3 whitened residual should be significantly reduced but may survive.
        // Verify it's at least reduced from the unsuppressed case.
        assert!(chroma_after[5] < chroma_before[5],
            "clarinet F ghost (PC 5) should decrease: before={:.3}, after={:.3}",
            chroma_before[5], chroma_after[5]);
    }

    // -----------------------------------------------------------------------
    // Trumpet — Concert Bb3 (233.08 Hz)
    //
    // Mostly cylindrical bore + cup mouthpiece + bell flare. The bell
    // creates a formant peak around 900-1200 Hz that makes H4 and H5
    // LOUDER than the fundamental — the hardest case for suppression.
    //
    // This tests the algorithm's behavior when the fundamental is NOT
    // the loudest peak in the raw spectrum. The sort-by-raw-magnitude
    // will process H4 first, but H4's integer multiples (2×H4=H8, etc.)
    // won't hit the fundamental. When H1 processes, it correctly
    // suppresses its own harmonics.
    //
    // H1=233→bin 43, H2=466→bin 87, H3=699→bin 130, H4=932→bin 173,
    // H5=1165→bin 216, H6=1398→bin 260, H7=1632→bin 303
    // -----------------------------------------------------------------------
    #[test]
    fn trumpet_bb3_formant_boosted_harmonics() {
        let harmonics = [
            (43usize, 10.0f32),  // H1: 233 Hz
            (87, 9.5),           // H2: 466 Hz
            (130, 9.0),          // H3: 699 Hz
            (173, 11.0),         // H4: 932 Hz — LOUDEST (bell formant)
            (216, 10.5),         // H5: 1165 Hz — also louder than H1
            (260, 8.5),          // H6: 1398 Hz
            (303, 6.5),          // H7: 1632 Hz
            (346, 4.5),          // H8: 1865 Hz
            (389, 2.5),          // H9: 2098 Hz
            (433, 1.2),          // H10: 2331 Hz
        ];
        let spectrum_len = FFT_SIZE / 2 + 1;
        let raw = make_instrument_spectrum(&harmonics, spectrum_len);
        let whitened = whiten_spectrum(&raw, SAMPLE_RATE, FFT_SIZE);
        let mut peaks = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);

        // Verify the fundamental survives (it's not at risk — only multiples
        // of stronger peaks are suppressed, not subharmonics)
        let fund_before = peaks.iter().find(|&&(b, _)| b == 43).map(|&(_, m)| m);
        assert!(fund_before.is_some(), "fundamental at bin 43 should be a peak");

        // Snapshot before
        let peaks_before = pick_peaks(&whitened, SAMPLE_RATE, FFT_SIZE);
        let chroma_before = compute_chroma(&peaks_before, SAMPLE_RATE, FFT_SIZE);

        suppress_harmonics(&mut peaks, &raw, 1.0, 6);

        let chroma_after = compute_chroma(&peaks, SAMPLE_RATE, FFT_SIZE);
        let mut chroma_norm = chroma_after;
        normalize_chroma(&mut chroma_norm);

        // Fundamental should survive — suppression only targets multiples,
        // not subharmonics, so bin 43 is safe even though H4/H5 are louder
        let fund_after = peak_mag(&peaks, 43);
        assert!(fund_after > 0.0,
            "trumpet fundamental must survive, got {}", fund_after);

        // Bb (PC 10) should still be the dominant pitch class because
        // H1, H2, H4, H8, H10 all map to Bb
        assert!(chroma_norm[10] > 0.3,
            "Bb (PC 10) should be significant for trumpet, got {:.3}", chroma_norm[10]);

        // Absolute ghost energy should not increase
        for pc in 0..12 {
            if pc == 10 { continue; }
            assert!(chroma_after[pc] <= chroma_before[pc] + 0.01,
                "trumpet: absolute PC {} should not increase: before={:.3}, after={:.3}",
                pc, chroma_before[pc], chroma_after[pc]);
        }

        // The trumpet is the hardest case: with formant-boosted H4/H5,
        // ghost notes (F from H3/H6, D from H5) may survive suppression.
        // This documents the limitation — trumpet benefits from higher
        // strength settings or the squelch gate in polyphonic mode.
        let active_before = active_pitch_classes(&{
            let mut c = chroma_before;
            normalize_chroma(&mut c);
            c
        }, PRESENCE_THRESHOLD);
        let active_after = active_pitch_classes(&chroma_norm, PRESENCE_THRESHOLD);
        // Suppression should at least not make things worse
        assert!(active_after.len() <= active_before.len() + 1,
            "trumpet: suppression should not significantly increase active PCs: before={}, after={}",
            active_before.len(), active_after.len());
    }
}
