//! Latency bench: `orbita-tts --bench [runs]`.
//!
//! Loads the engine exactly like the service does and prints per-stage times
//! for a fixed phrase set (or the file named by `VOSK_BENCH_TEXTS`, one phrase
//! per line). Each phrase is synthesised `runs` times; the fastest run is
//! reported. Meant to be run on the stand with the models in `VOSK_MODEL_DIR`.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use crate::config::Config;

use super::{VoskConfig, VoskEngine};

const DEFAULT_PHRASES: [&str; 4] = [
    "Привет, мир!",
    "Я включила свет в гостиной.",
    "Таймер на двадцать минут поставлен.",
    "Добрый вечер! Сегодня отличная погода, плюс пять градусов и без осадков.",
];

pub fn run(runs: usize) -> Result<(), String> {
    let service = Config::from_env()?;
    let config = VoskConfig::from_env(&service.providers, 0.0)?;
    let (synth_providers, bert_providers) = config.provider_names();
    println!(
        "model_dir={} voice={} synthesizer={:?} bert={:?} threads={} cudnn={:?} tf32={:?} arena={:?}",
        config.model_dir,
        config.default_speaker,
        synth_providers,
        bert_providers,
        config.threads,
        config.cudnn_search,
        config.tf32,
        config.arena
    );

    let started = Instant::now();
    let mut engine = VoskEngine::load(config.clone())?;
    println!("load_ms={:.0}", ms(started.elapsed()));
    let started = Instant::now();
    engine.warm_up()?;
    println!("warm_ms={:.0}", ms(started.elapsed()));

    let speaker = engine.speaker(&config.default_speaker);
    let scales = engine.default_scales(1.0);
    for text in phrases()? {
        let mut fastest = f64::MAX;
        let mut audio_s = 0.0;
        let mut best = (
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
        );
        let mut phonemes = 0;
        for _ in 0..runs.max(1) {
            let started = Instant::now();
            let (_audio, timings) = engine.synthesize_timed(&text, speaker, scales)?;
            let total = ms(started.elapsed());
            if total < fastest {
                fastest = total;
                audio_s = timings.audio_s as f64;
                phonemes = timings.phonemes;
                best = (timings.prepare, timings.bert, timings.g2p, timings.synth);
            }
        }
        println!(
            "total_ms={fastest:.1} prepare_ms={:.1} bert_ms={:.1} g2p_ms={:.1} synth_ms={:.1} audio_s={audio_s:.2} rtf={:.2} phonemes={phonemes} text={text:?}",
            ms(best.0),
            ms(best.1),
            ms(best.2),
            ms(best.3),
            if audio_s > 0.0 {
                fastest / 1000.0 / audio_s
            } else {
                0.0
            }
        );
    }
    Ok(())
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn phrases() -> Result<Vec<String>, String> {
    let Some(path) = std::env::var_os("VOSK_BENCH_TEXTS") else {
        return Ok(DEFAULT_PHRASES
            .iter()
            .map(|text| (*text).to_owned())
            .collect());
    };
    let path = Path::new(&path);
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let phrases: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if phrases.is_empty() {
        return Err(format!("{} has no phrases", path.display()));
    }
    Ok(phrases)
}
