// wasm/src/fft.rs — FFT and windowing
// =====================================
// Implements:
//   - Hann window generation
//   - In-place radix-2 Cooley-Tukey FFT (pure Rust, no external crate)
//   - Magnitude spectrum extraction (positive-frequency bins only)
//
// A lightweight FftPlanner type is provided so that lib.rs can hold one
// and pass &mut FftPlanner<f32> to compute_magnitude_spectrum — matching
// the API contract — without depending on the rustfft crate (which is not
// in Cargo.toml and the manifest is locked).

use core::f32::consts::PI;

// ---------------------------------------------------------------------------
// FftPlanner — thin wrapper that caches twiddle factors for a single size
// ---------------------------------------------------------------------------

/// Minimal FFT planner that pre-computes twiddle factors for the fixed
/// FFT_SIZE. Kept generic over `T` in name only to match the call-site
/// type annotation `FftPlanner<f32>`; internally only f32 is used.
pub struct FftPlanner<T> {
    /// Pre-computed twiddle factors: (cos, -sin) for each butterfly stage.
    /// Length = fft_size / 2.
    twiddles: Vec<(f32, f32)>,
    fft_size: usize,
    _marker: core::marker::PhantomData<T>,
}

impl<T> FftPlanner<T> {
    /// Create a planner pre-computing twiddle factors for `fft_size`.
    /// `fft_size` must be a power of two.
    pub fn new(fft_size: usize) -> Self {
        debug_assert!(fft_size.is_power_of_two(), "FFT size must be a power of two");
        let half = fft_size / 2;
        let mut twiddles = Vec::with_capacity(half);
        for k in 0..half {
            let angle = -2.0 * PI * k as f32 / fft_size as f32;
            twiddles.push((angle.cos(), angle.sin()));
        }
        FftPlanner {
            twiddles,
            fft_size,
            _marker: core::marker::PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// hann_window
// ---------------------------------------------------------------------------

/// Return a Hann window of `size` samples.
///
/// w[n] = 0.5 * (1.0 − cos(2π·n / (size − 1)))
///
/// Both endpoints (n=0 and n=size-1) are exactly 0.0; the centre is 1.0.
pub fn hann_window(size: usize) -> Vec<f32> {
    if size == 0 {
        return Vec::new();
    }
    if size == 1 {
        return vec![0.0];
    }
    let n_minus_1 = (size - 1) as f32;
    (0..size)
        .map(|n| 0.5 * (1.0 - (2.0 * PI * n as f32 / n_minus_1).cos()))
        .collect()
}

// ---------------------------------------------------------------------------
// apply_window
// ---------------------------------------------------------------------------

/// Multiply `samples` element-wise by `window` in place.
///
/// Panics in debug builds if lengths differ; silently truncates in release.
pub fn apply_window(samples: &mut [f32], window: &[f32]) {
    debug_assert_eq!(samples.len(), window.len(), "apply_window: length mismatch");
    let len = samples.len().min(window.len());
    for i in 0..len {
        samples[i] *= window[i];
    }
}

// ---------------------------------------------------------------------------
// Internal: bit-reversal permutation
// ---------------------------------------------------------------------------

fn bit_reverse_permute(buf: &mut Vec<(f32, f32)>) {
    let n = buf.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            buf.swap(i, j);
        }
    }
}

// ---------------------------------------------------------------------------
// Internal: in-place radix-2 DIT FFT (complex interleaved as Vec<(f32,f32)>)
// ---------------------------------------------------------------------------

fn fft_inplace(buf: &mut Vec<(f32, f32)>, twiddles: &[(f32, f32)]) {
    let n = buf.len();
    // Bit-reversal
    bit_reverse_permute(buf);

    // Butterfly stages
    let mut len = 2usize;
    while len <= n {
        let half_len = len / 2;
        let twiddle_step = n / len; // stride into twiddle table
        for chunk_start in (0..n).step_by(len) {
            for k in 0..half_len {
                let twiddle_idx = k * twiddle_step;
                let (wr, wi) = twiddles[twiddle_idx];
                let (ur, ui) = buf[chunk_start + k];
                let (vr, vi) = buf[chunk_start + k + half_len];
                // twiddle * v
                let tr = wr * vr - wi * vi;
                let ti = wr * vi + wi * vr;
                buf[chunk_start + k] = (ur + tr, ui + ti);
                buf[chunk_start + k + half_len] = (ur - tr, ui - ti);
            }
        }
        len <<= 1;
    }
}

// ---------------------------------------------------------------------------
// compute_magnitude_spectrum
// ---------------------------------------------------------------------------

/// Run a real FFT on `windowed` (f32 real samples).
///
/// Steps:
///   1. Promote each sample to a complex number with im = 0.
///   2. Run the forward FFT via the planner's cached twiddle factors.
///   3. Return magnitudes of bins 0 ..= size/2 (the positive-frequency half).
///
/// The returned Vec has length `windowed.len() / 2 + 1`.
pub fn compute_magnitude_spectrum(windowed: &[f32], planner: &mut FftPlanner<f32>) -> Vec<f32> {
    let n = windowed.len();
    debug_assert_eq!(n, planner.fft_size, "compute_magnitude_spectrum: size mismatch");

    // Build complex buffer (re = sample, im = 0)
    let mut buf: Vec<(f32, f32)> = windowed.iter().map(|&s| (s, 0.0)).collect();

    // Run FFT in place using cached twiddle factors
    fft_inplace(&mut buf, &planner.twiddles);

    // Extract magnitudes for bins 0 ..= n/2
    let num_bins = n / 2 + 1;
    let mut magnitudes = Vec::with_capacity(num_bins);
    for k in 0..num_bins {
        let (re, im) = buf[k];
        magnitudes.push((re * re + im * im).sqrt());
    }
    magnitudes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hann_window_endpoints_near_zero_middle_near_one() {
        let size = 4096;
        let w = hann_window(size);
        assert_eq!(w.len(), size);

        // First and last samples must be exactly 0 (or within floating-point epsilon)
        assert!(
            w[0].abs() < 1e-6,
            "hann_window[0] should be near 0, got {}",
            w[0]
        );
        assert!(
            w[size - 1].abs() < 1e-6,
            "hann_window[size-1] should be near 0, got {}",
            w[size - 1]
        );

        // Middle sample (n = size/2) should be near 1.0
        let mid = w[size / 2];
        assert!(
            (mid - 1.0).abs() < 0.001,
            "hann_window[size/2] should be near 1.0, got {}",
            mid
        );
    }

    #[test]
    fn hann_window_small_size() {
        // Smoke test for small sizes
        let w = hann_window(8);
        assert_eq!(w.len(), 8);
        assert!(w[0].abs() < 1e-6);
        assert!(w[7].abs() < 1e-6);
    }

    #[test]
    fn apply_window_scales_correctly() {
        let mut samples = vec![1.0f32; 8];
        let window = hann_window(8);
        apply_window(&mut samples, &window);
        // After applying window, samples should equal window values
        for (s, w) in samples.iter().zip(window.iter()) {
            assert!((s - w).abs() < 1e-6);
        }
    }

    #[test]
    fn magnitude_spectrum_dc_peak() {
        // All-ones input → DC bin should be the largest
        let n = 64;
        let mut planner = FftPlanner::new(n);
        let ones = vec![1.0f32; n];
        let mag = compute_magnitude_spectrum(&ones, &mut planner);
        assert_eq!(mag.len(), n / 2 + 1);
        // DC bin (index 0) should be the dominant peak
        let max_idx = mag
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap_or(0);
        assert_eq!(max_idx, 0, "DC input should produce peak at bin 0");
    }
}
