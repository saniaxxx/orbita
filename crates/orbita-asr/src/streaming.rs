//! Per-connection VAD endpointing and transcript events.
//!
//! Port of `StreamingAsrSession` from `asr_core/streaming_asr.py`, without
//! speaker verification and separated streams.

use std::sync::Arc;

use serde::Serialize;
use uuid::Uuid;

use crate::{
    config::{Config, SAMPLE_RATE, StreamingMode},
    recognizer::Recognizer,
    vad::{StreamingVadBuffer, VadDetector},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptEvent {
    pub end_of_utt: bool,
    pub text: String,
    pub result_type: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrEvent {
    SpeechStart,
    SpeechEnd { reason: String, eou: bool },
    Transcript(TranscriptEvent),
    Done,
}

pub struct StreamingAsrSession {
    config: Arc<Config>,
    recognizer: Arc<dyn Recognizer>,
    vad: StreamingVadBuffer,
    phrase_id: String,
    last_partial_samples: usize,
    transcripts_sent: usize,
    speech_started: bool,
    full_audio_chunks: Vec<Vec<i16>>,
    last_quasistreaming_text: String,
    quasistreaming_stable_count: u32,
}

impl StreamingAsrSession {
    pub fn new(
        config: Arc<Config>,
        recognizer: Arc<dyn Recognizer>,
        detector: Box<dyn VadDetector>,
    ) -> Self {
        let vad = StreamingVadBuffer::new(
            SAMPLE_RATE as usize,
            config.vad_threshold,
            200,
            100,
            300,
            true,
            500,
            config.speech_pad_ms,
            detector,
        );
        Self {
            config,
            recognizer,
            vad,
            phrase_id: new_phrase_id(),
            last_partial_samples: 0,
            transcripts_sent: 0,
            speech_started: false,
            full_audio_chunks: Vec::new(),
            last_quasistreaming_text: String::new(),
            quasistreaming_stable_count: 0,
        }
    }

    pub async fn add_audio(&mut self, pcm16: &[i16]) -> Vec<AsrEvent> {
        if pcm16.is_empty() {
            return Vec::new();
        }
        self.full_audio_chunks.push(pcm16.to_vec());
        self.vad.add_audio(pcm16);
        self.vad.process_available_audio(false);
        self.events_after_vad_update(false, "final_flush").await
    }

    pub async fn finish(&mut self) -> Vec<AsrEvent> {
        let full_audio: Vec<i16> = self.full_audio_chunks.concat();
        let transcripts_before = self.transcripts_sent;
        let mut events = self.finalize_utterance("final_flush").await;
        if self.transcripts_sent == transcripts_before && !full_audio.is_empty() {
            events.extend(
                self.decode_audio(&full_audio, true, "vad_fallback", 0.0)
                    .await,
            );
        }
        events.push(AsrEvent::Done);
        events
    }

    pub async fn finalize_utterance(&mut self, reason: &str) -> Vec<AsrEvent> {
        self.vad.process_available_audio(true);
        self.events_after_vad_update(true, reason).await
    }

    async fn events_after_vad_update(
        &mut self,
        is_final_flush: bool,
        final_reason: &str,
    ) -> Vec<AsrEvent> {
        let mut events = Vec::new();
        let audio = self.vad.accumulated_audio().to_vec();
        if audio.is_empty() {
            return events;
        }

        if self.config.emit_vad_events && !self.speech_started {
            self.speech_started = true;
            events.push(AsrEvent::SpeechStart);
        }

        let duration_ms = (self.vad.duration_seconds() * 1000.0).round() as u64;
        let silence_boundary = self
            .vad
            .has_recent_silence_gap(self.config.eou_silence_ms as f64)
            && duration_ms >= self.config.min_utterance_ms;
        let max_reached = duration_ms >= self.config.max_utterance_ms;

        if is_final_flush || silence_boundary || max_reached {
            let reason = if is_final_flush {
                final_reason
            } else if max_reached {
                "max_utterance"
            } else {
                "eou_silence"
            };
            if self.config.emit_vad_events {
                events.push(AsrEvent::SpeechEnd {
                    reason: reason.to_owned(),
                    eou: true,
                });
            }
            let offset = self.utterance_offset_s();
            events.extend(self.decode_audio(&audio, true, reason, offset).await);
            self.reset_utterance();
            return events;
        }

        if !self.should_emit_partial(&audio) {
            return events;
        }

        let offset = self.utterance_offset_s();
        let mut partial_events = self.decode_audio(&audio, false, "partial", offset).await;
        if self.config.streaming_mode == StreamingMode::Quasistreaming {
            let stable = self.apply_quasistreaming_stability(&mut partial_events);
            events.extend(partial_events);
            if stable {
                self.reset_utterance();
                return events;
            }
        } else {
            events.extend(partial_events);
        }
        self.last_partial_samples = audio.len();
        events
    }

    fn apply_quasistreaming_stability(&mut self, events: &mut [AsrEvent]) -> bool {
        let text = events
            .iter()
            .filter_map(|event| match event {
                AsrEvent::Transcript(transcript) => Some(transcript.text.trim()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_owned();
        if text.is_empty() {
            return false;
        }

        if text == self.last_quasistreaming_text {
            self.quasistreaming_stable_count += 1;
        } else {
            self.last_quasistreaming_text = text;
            self.quasistreaming_stable_count = 1;
        }

        let stable = self.quasistreaming_stable_count >= self.config.quasistreaming_stable_repeats;
        if stable {
            for event in events {
                if let AsrEvent::Transcript(transcript) = event {
                    transcript.end_of_utt = true;
                    transcript.result_type = "final".to_owned();
                    transcript.reason = "quasistreaming_stable".to_owned();
                }
            }
        }
        stable
    }

    fn should_emit_partial(&self, audio: &[i16]) -> bool {
        if !self.config.partial_results {
            return false;
        }
        let min_samples = (self.config.min_partial_audio_ms as usize * SAMPLE_RATE as usize) / 1000;
        let interval_samples =
            (self.config.partial_interval_ms as usize * SAMPLE_RATE as usize) / 1000;
        audio.len() >= min_samples
            && (self.last_partial_samples == 0
                || audio.len() - self.last_partial_samples >= interval_samples)
    }

    async fn decode_audio(
        &mut self,
        audio: &[i16],
        is_final: bool,
        reason: &str,
        offset_s: f64,
    ) -> Vec<AsrEvent> {
        let samples: Vec<f32> = audio
            .iter()
            .map(|sample| (*sample as f32 / 32768.0 * self.config.input_gain).clamp(-1.0, 1.0))
            .collect();

        let segments = match self
            .recognizer
            .transcribe(&samples, offset_s, &self.config.language)
            .await
        {
            Ok(segments) => segments,
            Err(error) => {
                tracing::warn!(%error, "recognizer failed");
                return Vec::new();
            }
        };

        let mut events = Vec::new();
        for segment in segments {
            if segment.text.trim().is_empty() {
                continue;
            }
            let _ = (segment.start_s, segment.end_s, &self.phrase_id);
            events.push(AsrEvent::Transcript(TranscriptEvent {
                end_of_utt: is_final,
                text: segment.text,
                result_type: if is_final { "final" } else { "partial" }.to_owned(),
                reason: reason.to_owned(),
            }));
            self.transcripts_sent += 1;
        }
        events
    }

    fn utterance_offset_s(&self) -> f64 {
        self.vad
            .voice_segments()
            .first()
            .map(|segment| segment.start_sample as f64 / SAMPLE_RATE as f64)
            .unwrap_or(0.0)
    }

    fn reset_utterance(&mut self) {
        self.vad.clear_accumulated_audio();
        self.last_partial_samples = 0;
        self.speech_started = false;
        self.phrase_id = new_phrase_id();
        self.last_quasistreaming_text.clear();
        self.quasistreaming_stable_count = 0;
        self.full_audio_chunks.clear();
    }
}

fn new_phrase_id() -> String {
    Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{recognizer::FakeRecognizer, vad::VadDetector};

    struct ScriptedVad {
        frame: usize,
        speech_from: usize,
        speech_to: usize,
    }

    impl VadDetector for ScriptedVad {
        fn process(&mut self, _frame: &[i16]) -> (f32, bool) {
            let speech = self.frame >= self.speech_from && self.frame < self.speech_to;
            self.frame += 1;
            (if speech { 1.0 } else { 0.0 }, speech)
        }
    }

    fn config() -> Arc<Config> {
        Arc::new(Config {
            host: "127.0.0.1".to_owned(),
            port: 0,
            input_gain: 1.0,
            vad_threshold: 0.1,
            eou_silence_ms: 100,
            min_utterance_ms: 100,
            max_utterance_ms: 8_000,
            speech_pad_ms: 0,
            partial_results: false,
            partial_interval_ms: 1_000,
            min_partial_audio_ms: 1_800,
            emit_vad_events: false,
            streaming_mode: StreamingMode::Streaming,
            quasistreaming_stable_repeats: 3,
            language: "ru".to_owned(),
            engine: "fake".to_owned(),
            vad: "energy".to_owned(),
        })
    }

    fn speech() -> Vec<i16> {
        vec![1_000_i16; 8_000]
    }

    fn silence() -> Vec<i16> {
        vec![0_i16; 8_000]
    }

    #[tokio::test]
    async fn emits_a_final_after_silence() {
        let mut session = StreamingAsrSession::new(
            config(),
            Arc::new(FakeRecognizer::new("тест")),
            Box::new(ScriptedVad {
                frame: 0,
                speech_from: 0,
                speech_to: 30,
            }),
        );
        let first = session.add_audio(&speech()).await;
        assert!(first.is_empty(), "no final inside the first speech chunk");
        let events = session.add_audio(&silence()).await;
        assert!(events.iter().any(|event| matches!(
            event,
            AsrEvent::Transcript(TranscriptEvent {
                end_of_utt: true,
                text,
                ..
            }) if text == "тест"
        )));
    }

    #[tokio::test]
    async fn finish_flushes_pending_audio() {
        let mut session = StreamingAsrSession::new(
            config(),
            Arc::new(FakeRecognizer::new("тест")),
            Box::new(ScriptedVad {
                frame: 0,
                speech_from: 0,
                speech_to: 30,
            }),
        );
        session.add_audio(&speech()).await;
        let events = session.finish().await;
        assert!(events.iter().any(|event| matches!(
            event,
            AsrEvent::Transcript(TranscriptEvent {
                end_of_utt: true,
                ..
            })
        )));
        assert!(matches!(events.last(), Some(AsrEvent::Done)));
    }
}
