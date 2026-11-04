//! Log-mel parity with the reference GigaAM v3 preprocessor.
//!
//! `fixtures/gigaam_features.json` was produced with NumPy by the formula of
//! onnx-asr's `GigaamPreprocessorV3` (bf16-rounded Hann window and HTK
//! filterbank, `rfft` power spectrum, `log(clip(x, 1e-9, 1e9))`) on
//! `0.3·sin(2π·440·n/16000) + 0.1·sin(2π·1234.5·n/16000)`, n < 4000.

use std::collections::BTreeMap;

use orbita_asr::gigaam::features::{LogMel, N_MELS};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    samples: usize,
    frames: usize,
    window_head: Vec<f32>,
    fbank_col5_nonzero: Vec<(usize, f32)>,
    feature_frames: BTreeMap<String, Vec<f32>>,
}

fn reference() -> Reference {
    serde_json::from_str(include_str!("fixtures/gigaam_features.json")).unwrap()
}

fn signal(samples: usize) -> Vec<f32> {
    (0..samples)
        .map(|n| {
            let t = n as f64 / 16_000.0;
            (0.3 * (std::f64::consts::TAU * 440.0 * t).sin()
                + 0.1 * (std::f64::consts::TAU * 1234.5 * t).sin()) as f32
        })
        .collect()
}

#[test]
fn window_and_filterbank_match() {
    let reference = reference();
    let log_mel = LogMel::new();
    assert_eq!(&log_mel.window()[..4], reference.window_head.as_slice());

    let nonzero: Vec<(usize, f32)> = (0..161)
        .map(|freq| (freq, log_mel.fbank(freq, 5)))
        .filter(|(_, weight)| *weight > 0.0)
        .collect();
    assert_eq!(nonzero, reference.fbank_col5_nonzero);
}

#[test]
fn features_match_the_reference() {
    let reference = reference();
    let features = LogMel::new().compute(&signal(reference.samples));
    assert_eq!(features.frames, reference.frames);

    let mut worst = 0.0f32;
    for (frame, expected) in &reference.feature_frames {
        let frame: usize = frame.parse().unwrap();
        assert_eq!(expected.len(), N_MELS);
        for (mel, expected) in expected.iter().enumerate() {
            let actual = features.get(mel, frame);
            worst = worst.max((actual - expected).abs());
        }
    }
    // f32 FFT against NumPy's f64 rfft; log features are O(10).
    assert!(worst < 2e-3, "max abs difference {worst}");
}
