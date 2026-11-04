//! WAV writing and the ffmpeg stage, ported from the Python service.

use std::{
    fmt,
    io::Write,
    process::{Command, Stdio},
    str::FromStr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Wav,
    Opus,
    Mp3,
    Aac,
    Flac,
    Pcm,
}

pub const ALL_FORMATS: [Format; 6] = [
    Format::Wav,
    Format::Opus,
    Format::Mp3,
    Format::Aac,
    Format::Flac,
    Format::Pcm,
];

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Opus => "opus",
            Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Pcm => "pcm",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Opus => "audio/ogg; codecs=opus",
            Self::Mp3 => "audio/mp3",
            Self::Aac => "audio/aac",
            Self::Flac => "audio/flac",
            Self::Pcm => "audio/pcm; rate=48000",
        }
    }

    fn ffmpeg_args(self) -> &'static [&'static str] {
        match self {
            Self::Wav => &["-c:a", "pcm_s16le", "-f", "wav"],
            Self::Opus => &["-c:a", "libopus", "-f", "ogg"],
            Self::Mp3 => &["-c:a", "libmp3lame", "-f", "mp3"],
            Self::Aac => &["-c:a", "aac", "-f", "adts"],
            Self::Flac => &["-c:a", "flac", "-f", "flac"],
            Self::Pcm => &["-f", "s16le", "-acodec", "pcm_s16le"],
        }
    }
}

impl FromStr for Format {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "wav" => Ok(Self::Wav),
            "opus" => Ok(Self::Opus),
            "mp3" => Ok(Self::Mp3),
            "aac" => Ok(Self::Aac),
            "flac" => Ok(Self::Flac),
            "pcm" => Ok(Self::Pcm),
            _ => Err(format!(
                "'response_format' must be one of {}",
                ALL_FORMATS
                    .iter()
                    .map(|format| format.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub trait AudioEncoder: Send + Sync {
    fn encode(&self, wav: &[u8], format: Format) -> Result<Vec<u8>, String>;
}

/// ffmpeg pipe in / pipe out, mono 48 kHz, optional gain and limiter.
pub struct FfmpegEncoder {
    gain_db: f32,
}

impl FfmpegEncoder {
    pub fn new(gain_db: f32) -> Self {
        Self { gain_db }
    }

    fn filter(&self) -> Option<String> {
        if self.gain_db == 0.0 {
            None
        } else {
            Some(format!(
                "volume={:.1}dB,alimiter=limit=0.95:level=0",
                self.gain_db
            ))
        }
    }
}

impl AudioEncoder for FfmpegEncoder {
    fn encode(&self, wav: &[u8], format: Format) -> Result<Vec<u8>, String> {
        let mut command = Command::new("ffmpeg");
        command.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
        ]);
        if let Some(filter) = self.filter() {
            command.args(["-af", &filter]);
        }
        command.args(["-ac", "1", "-ar", "48000"]);
        command.args(format.ffmpeg_args());
        command.arg("pipe:1");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to start ffmpeg: {error}"))?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| "ffmpeg stdin is unavailable".to_owned())?
            .write_all(wav)
            .map_err(|error| format!("failed to write to ffmpeg: {error}"))?;
        drop(child.stdin.take());

        let output = child
            .wait_with_output()
            .map_err(|error| format!("failed to wait for ffmpeg: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "ffmpeg exited with {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(output.stdout)
    }
}

/// Test encoder: never invokes ffmpeg and makes the format visible in the body.
pub struct FakeEncoder;

impl AudioEncoder for FakeEncoder {
    fn encode(&self, _wav: &[u8], format: Format) -> Result<Vec<u8>, String> {
        Ok(format!("AUDIO:{}", format.as_str()).into_bytes())
    }
}

/// PCM16 mono WAV, same clamping and rounding as `writeWavFile` in the JS code.
pub fn wav_bytes(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut output = Vec::with_capacity(44 + data_len as usize);
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&(36 + data_len).to_le_bytes());
    output.extend_from_slice(b"WAVE");
    output.extend_from_slice(b"fmt ");
    output.extend_from_slice(&16u32.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes()); // PCM
    output.extend_from_slice(&1u16.to_le_bytes()); // mono
    output.extend_from_slice(&sample_rate.to_le_bytes());
    output.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    output.extend_from_slice(&2u16.to_le_bytes());
    output.extend_from_slice(&16u16.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&data_len.to_le_bytes());

    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * 32767.0).floor() as i16;
        output.extend_from_slice(&value.to_le_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_pcm16_mono_header() {
        let wav = wav_bytes(&[0.0, 0.5, -0.5], 44_100);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 44_100);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
        assert_eq!(wav.len(), 44 + 6);
    }

    #[test]
    fn parses_formats() {
        for format in ALL_FORMATS {
            assert_eq!(format.as_str().parse::<Format>().unwrap(), format);
        }
        assert!("ogg".parse::<Format>().is_err());
    }

    #[test]
    fn builds_the_expected_filter() {
        assert_eq!(
            FfmpegEncoder::new(9.0).filter().as_deref(),
            Some("volume=9.0dB,alimiter=limit=0.95:level=0")
        );
        assert_eq!(FfmpegEncoder::new(0.0).filter(), None);
    }

    #[test]
    fn ffmpeg_produces_ogg_opus_when_available() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg is not installed; skipping");
            return;
        }
        let samples = vec![0.0_f32; 4_410];
        let body = FfmpegEncoder::new(9.0)
            .encode(&wav_bytes(&samples, 44_100), Format::Opus)
            .unwrap();
        assert_eq!(&body[0..4], b"OggS");
    }
}
