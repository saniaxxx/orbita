//! GigaAM v3 log-mel features.
//!
//! Same maths as the `GigaamPreprocessorV3` graph of onnx-asr (MIT), which the
//! published ONNX models were benchmarked with: 16 kHz input, 20 ms Hann
//! window, 10 ms hop, no centre padding, power spectrum, 64 HTK mel bands up to
//! 8 kHz, natural log clamped to `[1e-9, 1e9]`. The window and the filterbank
//! are rounded through bfloat16, as in the reference.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex};

pub const SAMPLE_RATE: usize = 16_000;
pub const N_FFT: usize = SAMPLE_RATE / 50;
pub const WIN_LENGTH: usize = N_FFT;
pub const HOP_LENGTH: usize = SAMPLE_RATE / 100;
pub const N_MELS: usize = 64;
pub const N_FREQS: usize = N_FFT / 2 + 1;

const CLAMP_MIN: f32 = 1e-9;
const CLAMP_MAX: f32 = 1e9;

/// Features laid out as the encoder expects them: `[N_MELS, frames]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    pub data: Vec<f32>,
    pub frames: usize,
}

impl Features {
    pub fn get(&self, mel: usize, frame: usize) -> f32 {
        self.data[mel * self.frames + frame]
    }
}

pub struct LogMel {
    window: Vec<f32>,
    /// `[N_FREQS, N_MELS]`, row-major.
    fbank: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
}

impl LogMel {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        Self {
            window: hann_window(),
            fbank: mel_filterbank(),
            fft: planner.plan_fft_forward(N_FFT),
        }
    }

    pub fn window(&self) -> &[f32] {
        &self.window
    }

    pub fn fbank(&self, freq: usize, mel: usize) -> f32 {
        self.fbank[freq * N_MELS + mel]
    }

    /// Number of frames for `samples` input samples (0 when shorter than a window).
    pub fn frame_count(samples: usize) -> usize {
        if samples < WIN_LENGTH {
            0
        } else {
            (samples - WIN_LENGTH) / HOP_LENGTH + 1
        }
    }

    pub fn compute(&self, samples: &[f32]) -> Features {
        let frames = Self::frame_count(samples.len());
        let mut data = vec![0.0f32; N_MELS * frames];
        let mut buffer = vec![Complex::new(0.0f32, 0.0); N_FFT];
        let mut scratch = vec![Complex::new(0.0f32, 0.0); self.fft.get_inplace_scratch_len()];
        let mut power = [0.0f32; N_FREQS];

        for frame in 0..frames {
            let start = frame * HOP_LENGTH;
            for (index, slot) in buffer.iter_mut().enumerate() {
                *slot = Complex::new(samples[start + index] * self.window[index], 0.0);
            }
            self.fft.process_with_scratch(&mut buffer, &mut scratch);
            for (bin, value) in power.iter_mut().enumerate() {
                *value = buffer[bin].norm_sqr();
            }
            for mel in 0..N_MELS {
                let mut energy = 0.0f32;
                for (bin, value) in power.iter().enumerate() {
                    energy += value * self.fbank[bin * N_MELS + mel];
                }
                data[mel * frames + frame] = energy.clamp(CLAMP_MIN, CLAMP_MAX).ln();
            }
        }

        Features { data, frames }
    }
}

impl Default for LogMel {
    fn default() -> Self {
        Self::new()
    }
}

/// Round an f32 to the nearest bfloat16 (ties to even) and widen it back.
pub fn round_bf16(value: f32) -> f32 {
    let bits = value.to_bits();
    let rounding = 0x7FFF + ((bits >> 16) & 1);
    f32::from_bits(bits.wrapping_add(rounding) & 0xFFFF_0000)
}

/// Periodic Hann window, `np.hanning(WIN_LENGTH + 1)[:-1]`.
fn hann_window() -> Vec<f32> {
    (0..WIN_LENGTH)
        .map(|index| {
            let phase = std::f64::consts::TAU * index as f64 / WIN_LENGTH as f64;
            round_bf16((0.5 - 0.5 * phase.cos()) as f32)
        })
        .collect()
}

fn hz_to_mel(hz: f64) -> f64 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f64) -> f64 {
    700.0 * (10f64.powf(mel / 2595.0) - 1.0)
}

/// HTK triangular filters without normalisation, as `torchaudio.melscale_fbanks`.
fn mel_filterbank() -> Vec<f32> {
    let f_max = (SAMPLE_RATE / 2) as f64;
    let mel_max = hz_to_mel(f_max);
    let points: Vec<f64> = (0..N_MELS + 2)
        .map(|index| mel_to_hz(mel_max * index as f64 / (N_MELS + 1) as f64))
        .collect();

    let mut fbank = vec![0.0f32; N_FREQS * N_MELS];
    for bin in 0..N_FREQS {
        let freq = f_max * bin as f64 / (N_FREQS - 1) as f64;
        for mel in 0..N_MELS {
            let up = (freq - points[mel]) / (points[mel + 1] - points[mel]);
            let down = (points[mel + 2] - freq) / (points[mel + 2] - points[mel + 1]);
            let weight = up.min(down).max(0.0);
            fbank[bin * N_MELS + mel] = round_bf16(weight as f32);
        }
    }
    fbank
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bf16_rounding_matches_ml_dtypes() {
        assert_eq!(round_bf16(1.0), 1.0);
        assert_eq!(round_bf16(0.0), 0.0);
        // 1 + 2^-8 is a tie between 1.0 and 1 + 2^-7: ties go to even.
        assert_eq!(round_bf16(1.0 + 1.0 / 256.0), 1.0);
        assert_eq!(round_bf16(1.0 + 3.0 / 256.0), 1.0 + 4.0 / 256.0);
    }

    #[test]
    fn frame_count_has_no_centre_padding() {
        assert_eq!(LogMel::frame_count(0), 0);
        assert_eq!(LogMel::frame_count(319), 0);
        assert_eq!(LogMel::frame_count(320), 1);
        assert_eq!(LogMel::frame_count(479), 1);
        assert_eq!(LogMel::frame_count(480), 2);
        assert_eq!(LogMel::frame_count(16_000), 99);
    }

    #[test]
    fn short_input_gives_no_frames() {
        let features = LogMel::new().compute(&[0.1; 100]);
        assert_eq!(features.frames, 0);
        assert!(features.data.is_empty());
    }

    #[test]
    fn silence_is_clamped() {
        let features = LogMel::new().compute(&[0.0; 800]);
        assert_eq!(features.frames, 4);
        let floor = CLAMP_MIN.ln();
        assert!(features.data.iter().all(|value| *value == floor));
    }
}
