//! Streaming VAD buffer, ported from `asr_core/processors/vad_processor.py`.
//!
//! TEN VAD is behind [`VadDetector`]; the default energy detector keeps the
//! pipeline usable without the native library, and the real TEN detector can be
//! added later without touching endpointing.

pub const HOP_SIZE: usize = 256;
pub const SAMPLE_RATE: usize = 16_000;

#[derive(Debug, Clone)]
pub struct VoiceSegment {
    pub start_sample: usize,
    pub end_sample: usize,
    pub audio: Vec<i16>,
}

pub trait VadDetector: Send {
    /// Return `(speech_probability, speech_flag)` for one 16 kHz frame.
    fn process(&mut self, frame: &[i16]) -> (f32, bool);
}

#[derive(Debug, Clone)]
pub struct EnergyVad {
    threshold: f32,
}

impl EnergyVad {
    pub fn new(threshold: f32) -> Self {
        Self { threshold }
    }
}

impl VadDetector for EnergyVad {
    fn process(&mut self, frame: &[i16]) -> (f32, bool) {
        if frame.is_empty() {
            return (0.0, false);
        }
        let sum: f64 = frame
            .iter()
            .map(|sample| {
                let value = *sample as f64 / 32768.0;
                value * value
            })
            .sum();
        let rms = (sum / frame.len() as f64).sqrt() as f32;
        (rms.min(1.0), rms >= self.threshold)
    }
}

/// Frame-level segmentation on the detector's 16 kHz timeline.
pub fn detect_segments(
    detector: &mut dyn VadDetector,
    audio: &[i16],
    min_speech_ms: u64,
    min_silence_ms: u64,
    sample_rate: usize,
) -> Vec<(usize, usize)> {
    if audio.is_empty() {
        return Vec::new();
    }

    let min_speech_samples = (min_speech_ms as usize * sample_rate) / 1000;
    let min_silence_samples = (min_silence_ms as usize * sample_rate) / 1000;
    let mut speech_start: Option<usize> = None;
    let mut silence_start: Option<usize> = None;
    let mut segments = Vec::new();

    let mut frame_start = 0;
    while frame_start < audio.len() {
        let mut frame = [0_i16; HOP_SIZE];
        let available = (audio.len() - frame_start).min(HOP_SIZE);
        frame[..available].copy_from_slice(&audio[frame_start..frame_start + available]);

        let (_, speech) = detector.process(&frame);
        let frame_end = (frame_start + HOP_SIZE).min(audio.len());

        if speech {
            if speech_start.is_none() {
                speech_start = Some(frame_start);
            }
            silence_start = None;
        } else if let Some(start) = speech_start {
            let silence = *silence_start.get_or_insert(frame_start);
            if frame_end - silence >= min_silence_samples {
                if silence - start >= min_speech_samples {
                    segments.push((start, silence));
                }
                speech_start = None;
                silence_start = None;
            }
        }

        frame_start += HOP_SIZE;
    }

    if let Some(start) = speech_start {
        let end = silence_start.unwrap_or(audio.len());
        if end - start >= min_speech_samples {
            segments.push((start, end));
        }
    }

    segments
}

fn rms_and_peak(audio: &[i16]) -> (f32, f32) {
    if audio.is_empty() {
        return (0.0, 0.0);
    }
    let mut sum = 0.0_f64;
    let mut peak = 0.0_f32;
    for sample in audio {
        let value = (*sample as f32 / 32768.0).abs();
        peak = peak.max(value);
        sum += (value as f64) * (value as f64);
    }
    ((sum / audio.len() as f64).sqrt() as f32, peak)
}

pub fn energy_fallback_segment(
    audio: &[i16],
    min_rms: f32,
    min_peak: f32,
) -> Option<(usize, usize)> {
    let (rms, peak) = rms_and_peak(audio);
    (rms >= min_rms && peak >= min_peak).then_some((0, audio.len()))
}

#[allow(clippy::too_many_arguments)]
pub fn extract_voice_segments(
    audio: &[i16],
    detector: &mut dyn VadDetector,
    threshold: f32,
    min_speech_ms: u64,
    min_silence_ms: u64,
    merge_segments: bool,
    max_gap_ms: u64,
    speech_pad_ms: u64,
    sample_rate: usize,
) -> Vec<VoiceSegment> {
    let _ = threshold; // the detector already applies it
    let mut segments = detect_segments(detector, audio, min_speech_ms, min_silence_ms, sample_rate);

    if segments.is_empty()
        && let Some(segment) = energy_fallback_segment(audio, 0.06, 0.25)
    {
        segments.push(segment);
    }

    if merge_segments && segments.len() > 1 {
        let max_gap_samples = (max_gap_ms as usize * sample_rate) / 1000;
        let mut merged = Vec::with_capacity(segments.len());
        let (mut current_start, mut current_end) = segments[0];
        for (start, end) in segments.into_iter().skip(1) {
            if start.saturating_sub(current_end) <= max_gap_samples {
                current_end = end;
            } else {
                merged.push((current_start, current_end));
                current_start = start;
                current_end = end;
            }
        }
        merged.push((current_start, current_end));
        segments = merged;
    }

    let pad = (speech_pad_ms as usize * sample_rate) / 1000;
    segments
        .into_iter()
        .filter_map(|(start, end)| {
            let start = start.saturating_sub(pad);
            let end = (end + pad).min(audio.len());
            (end > start).then(|| VoiceSegment {
                start_sample: start,
                end_sample: end,
                audio: audio[start..end].to_vec(),
            })
        })
        .collect()
}

pub struct StreamingVadBuffer {
    sample_rate: usize,
    threshold: f32,
    min_speech_ms: u64,
    min_silence_ms: u64,
    max_gap_ms: u64,
    merge_segments: bool,
    chunk_size_samples: usize,
    speech_pad_ms: u64,
    detector: Box<dyn VadDetector>,
    audio_buffer: Vec<i16>,
    total_samples_received: usize,
    total_samples_processed: usize,
    voice_segments: Vec<VoiceSegment>,
    accumulated_voice_audio: Vec<i16>,
    last_voice_segment_end: usize,
    recent_silence_gap_ms: f64,
}

impl StreamingVadBuffer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sample_rate: usize,
        threshold: f32,
        min_speech_ms: u64,
        min_silence_ms: u64,
        max_gap_ms: u64,
        merge_segments: bool,
        chunk_size_ms: u64,
        speech_pad_ms: u64,
        detector: Box<dyn VadDetector>,
    ) -> Self {
        Self {
            sample_rate,
            threshold,
            min_speech_ms,
            min_silence_ms,
            max_gap_ms,
            merge_segments,
            chunk_size_samples: ((chunk_size_ms as usize * sample_rate) / 1000).max(1),
            speech_pad_ms,
            detector,
            audio_buffer: Vec::new(),
            total_samples_received: 0,
            total_samples_processed: 0,
            voice_segments: Vec::new(),
            accumulated_voice_audio: Vec::new(),
            last_voice_segment_end: 0,
            recent_silence_gap_ms: 0.0,
        }
    }

    pub fn add_audio(&mut self, audio: &[i16]) {
        self.audio_buffer.extend_from_slice(audio);
        self.total_samples_received += audio.len();
    }

    /// Process complete chunks (or everything when `is_final`).
    pub fn process_available_audio(&mut self, is_final: bool) -> Option<Vec<i16>> {
        let buffer_samples = self.audio_buffer.len();
        let samples_to_process = if is_final {
            buffer_samples
        } else {
            (buffer_samples / self.chunk_size_samples) * self.chunk_size_samples
        };
        if samples_to_process == 0 {
            return None;
        }

        let audio: Vec<i16> = self.audio_buffer.drain(..samples_to_process).collect();
        let segments = extract_voice_segments(
            &audio,
            &mut *self.detector,
            self.threshold,
            self.min_speech_ms,
            self.min_silence_ms,
            self.merge_segments,
            self.max_gap_ms,
            self.speech_pad_ms,
            self.sample_rate,
        );

        if let Some(first) = segments.first() {
            let first_start = self.total_samples_processed + first.start_sample;
            if self.last_voice_segment_end > 0 {
                let gap = first_start.saturating_sub(self.last_voice_segment_end);
                self.recent_silence_gap_ms = (gap as f64 / self.sample_rate as f64) * 1000.0;
            }
            if let Some(last) = segments.last() {
                self.last_voice_segment_end = self.total_samples_processed + last.end_sample;
            }
        } else if self.last_voice_segment_end > 0 {
            let gap =
                self.total_samples_processed + samples_to_process - self.last_voice_segment_end;
            self.recent_silence_gap_ms = (gap as f64 / self.sample_rate as f64) * 1000.0;
        }

        let mut new_audio = Vec::new();
        for segment in segments {
            new_audio.extend_from_slice(&segment.audio);
            self.accumulated_voice_audio
                .extend_from_slice(&segment.audio);
            self.voice_segments.push(VoiceSegment {
                start_sample: self.total_samples_processed + segment.start_sample,
                end_sample: self.total_samples_processed + segment.end_sample,
                audio: segment.audio,
            });
        }
        self.total_samples_processed += samples_to_process;

        (!new_audio.is_empty()).then_some(new_audio)
    }

    pub fn has_recent_silence_gap(&self, min_gap_ms: f64) -> bool {
        self.recent_silence_gap_ms >= min_gap_ms
    }

    pub fn accumulated_audio(&self) -> &[i16] {
        &self.accumulated_voice_audio
    }

    pub fn duration_seconds(&self) -> f64 {
        self.accumulated_voice_audio.len() as f64 / self.sample_rate as f64
    }

    pub fn voice_segments(&self) -> &[VoiceSegment] {
        &self.voice_segments
    }

    pub fn clear_accumulated_audio(&mut self) {
        self.accumulated_voice_audio.clear();
        self.voice_segments.clear();
        self.recent_silence_gap_ms = 0.0;
    }

    pub fn clear(&mut self) {
        self.audio_buffer.clear();
        self.voice_segments.clear();
        self.accumulated_voice_audio.clear();
        self.total_samples_received = 0;
        self.total_samples_processed = 0;
        self.last_voice_segment_end = 0;
        self.recent_silence_gap_ms = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn tone(samples: usize, amplitude: f32) -> Vec<i16> {
        (0..samples)
            .map(|index| {
                let value = (index as f32 * 0.1).sin() * amplitude;
                (value * 32767.0) as i16
            })
            .collect()
    }

    #[test]
    fn detects_a_scripted_speech_region() {
        let audio = vec![0_i16; HOP_SIZE * 20];
        let mut detector = ScriptedVad {
            frame: 0,
            speech_from: 5,
            speech_to: 15,
        };
        let segments = detect_segments(&mut detector, &audio, 100, 100, SAMPLE_RATE);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].0, HOP_SIZE * 5);
        assert_eq!(segments[0].1, HOP_SIZE * 15);
    }

    #[test]
    fn energy_vad_sees_a_loud_tone() {
        let mut detector = EnergyVad::new(0.1);
        let (_, speech) = detector.process(&tone(HOP_SIZE, 0.5));
        assert!(speech);
        let (_, silence) = detector.process(&vec![0_i16; HOP_SIZE]);
        assert!(!silence);
    }

    #[test]
    fn streaming_buffer_accumulates_voice() {
        let speech = tone(8_000, 0.5);
        let silence = vec![0_i16; 8_000];
        let mut buffer = StreamingVadBuffer::new(
            SAMPLE_RATE,
            0.1,
            100,
            100,
            300,
            true,
            500,
            0,
            Box::new(EnergyVad::new(0.1)),
        );
        buffer.add_audio(&speech);
        buffer.process_available_audio(false);
        buffer.add_audio(&silence);
        buffer.process_available_audio(false);
        assert!(!buffer.accumulated_audio().is_empty());
        assert!(buffer.has_recent_silence_gap(100.0));
    }
}
