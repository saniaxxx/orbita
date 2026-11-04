//! End-to-end synthesis on the real model (skipped without `VOSK_MODEL_DIR`).
//!
//! `fixtures/vosk_e2e.json` comes from the Python package with noise scales
//! set to 0, which makes the output deterministic: the number of samples must
//! match and the level must agree.
#![cfg(feature = "vosk")]

use std::time::Instant;

use orbita_tts::vosk::{VoskConfig, VoskEngine, model::Scales};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    text: String,
    sid: i64,
    bert_rows: usize,
    bert_head: Vec<[f32; 3]>,
    samples: usize,
    rms: f32,
}

#[test]
fn deterministic_synthesis_matches_python() {
    if std::env::var_os("VOSK_MODEL_DIR").is_none() {
        eprintln!("VOSK_MODEL_DIR not set; skipping");
        return;
    }
    let references: Vec<Reference> =
        serde_json::from_str(include_str!("fixtures/vosk_e2e.json")).unwrap();
    let config = VoskConfig::from_env("cpu", 0.3).unwrap();
    let started = Instant::now();
    let mut engine = VoskEngine::load(config).unwrap();
    eprintln!("load {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);

    for reference in references {
        let (embeddings, rows) = engine_bert(&mut engine, &reference.text);
        assert_eq!(rows, reference.bert_rows);
        for (row, head) in reference.bert_head.iter().enumerate() {
            for (dim, expected) in head.iter().enumerate() {
                let actual = embeddings[row * 768 + dim];
                assert!(
                    (actual - expected).abs() < 2e-3,
                    "BERT[{row}][{dim}] {actual} vs {expected}"
                );
            }
        }

        let scales = Scales {
            noise: 0.0,
            length: 1.0,
            duration_noise: 0.0,
        };
        let started = Instant::now();
        let audio = engine
            .synthesize_with(&reference.text, reference.sid, scales)
            .unwrap();
        let elapsed = started.elapsed().as_secs_f64();
        let rms = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt();
        eprintln!(
            "{:?}: {} samples ({:.2} s) in {:.0} ms, rms {rms:.4}",
            reference.text,
            audio.len(),
            audio.len() as f64 / 22_050.0,
            elapsed * 1000.0
        );
        assert_eq!(
            audio.len(),
            reference.samples,
            "length of {:?}",
            reference.text
        );
        assert!(
            (rms - reference.rms).abs() / reference.rms < 0.05,
            "rms {rms} vs {}",
            reference.rms
        );
    }
}

fn engine_bert(engine: &mut VoskEngine, text: &str) -> (Vec<f32>, usize) {
    engine.bert(text).unwrap()
}
