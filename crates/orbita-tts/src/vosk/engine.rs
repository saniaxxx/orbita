//! [`SynthesisEngine`] over the Vosk model.

use std::time::{Duration, Instant};

use crate::engine::{EngineError, Providers, SynthesisEngine};

use super::{
    Lexicon, SAMPLE_RATE, VoskConfig,
    model::{PieceTimings, Scales, VoskModel},
    normalize::{normalize, split_long},
};

/// Where one request spent its time.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Timings {
    pub prepare: Duration,
    pub bert: Duration,
    pub g2p: Duration,
    pub synth: Duration,
    pub pieces: usize,
    pub phonemes: usize,
    pub audio_s: f32,
}

impl Timings {
    fn add_piece(&mut self, piece: PieceTimings) {
        self.bert += piece.bert;
        self.g2p += piece.g2p;
        self.synth += piece.synth;
        self.pieces += 1;
        self.phonemes += piece.phonemes;
    }

    /// Text preparation, BERT, g2p and synthesis together.
    pub fn pipeline(&self) -> Duration {
        self.prepare + self.bert + self.g2p + self.synth
    }
}

pub struct VoskEngine {
    model: VoskModel,
    config: VoskConfig,
    default_speaker: i64,
}

impl VoskEngine {
    pub fn load(config: VoskConfig) -> Result<Self, String> {
        let lexicon = Lexicon::load(&config.files)?;
        let default_speaker = lexicon.speaker(&config.default_speaker).ok_or_else(|| {
            format!(
                "VOSK_VOICE={} is not a voice of this model",
                config.default_speaker
            )
        })?;
        Ok(Self {
            model: VoskModel::load(&config, lexicon)?,
            config,
            default_speaker,
        })
    }

    pub fn lexicon(&self) -> &Lexicon {
        &self.model.lexicon
    }

    /// Voice names known to the model, sorted by id.
    pub fn voices(&self) -> Vec<(i64, String)> {
        let mut voices: Vec<(i64, String)> = self
            .lexicon()
            .speakers
            .iter()
            .map(|(name, id)| (*id, name.clone()))
            .collect();
        voices.sort();
        voices
    }

    /// Request voice → speaker id. Unknown names (quasar sends its own voice
    /// setting, e.g. "IVONA 2 Tatyana OEM") fall back to `VOSK_VOICE`, as the
    /// Python Silero service ignored foreign names.
    pub fn speaker(&self, voice: &str) -> i64 {
        self.lexicon()
            .speaker(voice)
            .unwrap_or(self.default_speaker)
    }

    pub fn default_scales(&self, speed: f32) -> Scales {
        let inference = self.lexicon().inference;
        let rate = (speed * inference.speech_rate).max(0.05);
        Scales {
            noise: inference.noise_level,
            length: 1.0 / rate,
            duration_noise: inference.duration_noise_level,
        }
    }

    /// Full pipeline with explicit scales: clean-up, splitting of long input,
    /// synthesis, pauses between pieces, output `scale`.
    pub fn synthesize_with(
        &mut self,
        text: &str,
        speaker: i64,
        scales: Scales,
    ) -> Result<Vec<f32>, String> {
        self.synthesize_timed(text, speaker, scales)
            .map(|(audio, _)| audio)
    }

    /// [`Self::synthesize_with`] with the time spent in each stage.
    pub fn synthesize_timed(
        &mut self,
        text: &str,
        speaker: i64,
        scales: Scales,
    ) -> Result<(Vec<f32>, Timings), String> {
        let mut timings = Timings::default();

        let started = Instant::now();
        let prepared = normalize(text.trim());
        let pieces = split_long(prepared.trim(), self.config.max_chars);
        timings.prepare = started.elapsed();
        let pause = vec![0f32; (SAMPLE_RATE as f32 * self.config.silence) as usize];
        let gain = self.lexicon().inference.scale;

        let mut samples = Vec::new();
        for (index, piece) in pieces.iter().enumerate() {
            let (audio, piece_timings) = self.model.synthesize_timed(piece, speaker, scales)?;
            timings.add_piece(piece_timings);
            if index > 0 && !audio.is_empty() {
                samples.extend_from_slice(&pause);
            }
            samples.extend(audio.iter().map(|sample| sample * gain));
        }
        if samples.is_empty() {
            // Nothing pronounceable: a short silence instead of an error.
            samples = vec![0f32; SAMPLE_RATE as usize / 10];
        }
        timings.audio_s = samples.len() as f32 / SAMPLE_RATE as f32;

        if self.config.timings {
            tracing::info!(
                prepare_ms = timings.prepare.as_secs_f64() * 1000.0,
                bert_ms = timings.bert.as_secs_f64() * 1000.0,
                g2p_ms = timings.g2p.as_secs_f64() * 1000.0,
                synth_ms = timings.synth.as_secs_f64() * 1000.0,
                pieces = timings.pieces,
                phonemes = timings.phonemes,
                audio_s = timings.audio_s,
                "vosk timings"
            );
        }
        Ok((samples, timings))
    }

    /// Selected BERT word rows for `text` (`[rows, 768]`, row-major).
    pub fn bert(&mut self, text: &str) -> Result<(Vec<f32>, usize), String> {
        self.model.word_embeddings(text)
    }

    /// The first ONNX Runtime calls are slow (CUDA in particular); run them
    /// before the port opens.
    pub fn warm_up(&mut self) -> Result<(), String> {
        let scales = self.default_scales(1.0);
        for text in ["Прогрев.", "Прогрев модели синтеза."] {
            self.synthesize_with(text, self.default_speaker, scales)?;
        }
        Ok(())
    }
}

impl SynthesisEngine for VoskEngine {
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn steps(&self) -> u32 {
        0
    }

    fn providers(&self) -> Providers {
        let (synthesizer, bert) = self.config.provider_names();
        Providers::from([("bert", bert), ("synthesizer", synthesizer)])
    }

    fn synthesize(
        &mut self,
        text: &str,
        voice: &str,
        speed: f32,
        _steps: u32,
    ) -> Result<Vec<f32>, EngineError> {
        let speaker = self.speaker(voice);
        let scales = self.default_scales(speed);
        self.synthesize_with(text, speaker, scales)
            .map_err(EngineError::Failed)
    }
}
