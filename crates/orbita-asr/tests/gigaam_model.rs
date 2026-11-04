//! End-to-end check of the ONNX engine on real models (skipped without them).
//!
//! Needs `GIGAAM_MODEL_DIR` (onnx-asr exports of GigaAM v3), `GIGAAM_TEST_WAV`
//! (16 kHz mono PCM16) and `GIGAAM_TEST_TEXT` (expected transcript). Optional:
//! `GIGAAM_MODEL`, `GIGAAM_QUANTIZATION`, `GIGAAM_DEVICE` as for the service.
#![cfg(feature = "gigaam")]

use std::time::Instant;

use orbita_asr::{GigaamConfig, GigaamRecognizer};

fn read_pcm16_wav(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = &bytes[offset + 8..(offset + 8 + size).min(bytes.len())];
        if id == b"fmt " {
            let channels = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([body[14], body[15]]);
            assert_eq!(
                (channels, rate, bits),
                (1, 16_000, 16),
                "need 16 kHz mono PCM16"
            );
        } else if id == b"data" {
            return body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair) as f32 / 32768.0)
                .collect();
        }
        offset += 8 + size + (size & 1);
    }
    panic!("no data chunk in {path}");
}

#[test]
fn recognises_the_reference_phrase() {
    let (Ok(_), Ok(wav), Ok(expected)) = (
        std::env::var("GIGAAM_MODEL_DIR"),
        std::env::var("GIGAAM_TEST_WAV"),
        std::env::var("GIGAAM_TEST_TEXT"),
    ) else {
        eprintln!("GIGAAM_MODEL_DIR/GIGAAM_TEST_WAV/GIGAAM_TEST_TEXT not set; skipping");
        return;
    };

    let config = GigaamConfig::from_env().unwrap();
    let started = Instant::now();
    let recognizer = GigaamRecognizer::load(&config).unwrap();
    let load = started.elapsed();
    recognizer.warm_up().unwrap();

    let samples = read_pcm16_wav(&wav);
    let audio_s = samples.len() as f64 / 16_000.0;
    let mut best = f64::MAX;
    let mut text = String::new();
    for _ in 0..3 {
        let started = Instant::now();
        text = recognizer.recognize_blocking(&samples).unwrap();
        best = best.min(started.elapsed().as_secs_f64());
    }
    eprintln!(
        "model={} quant={:?} providers={:?} load={:.0}ms audio={audio_s:.2}s best={:.0}ms rtfx={:.1} text={text:?}",
        config.model,
        config.quantization,
        recognizer.providers(),
        load.as_secs_f64() * 1000.0,
        best * 1000.0,
        audio_s / best,
    );
    assert_eq!(text, expected);
}
