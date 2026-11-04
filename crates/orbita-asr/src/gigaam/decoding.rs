//! Vocabulary and greedy CTC / RNN-T decoding for GigaAM.
//!
//! The loops mirror onnx-asr (MIT): RNN-T emits at most `max_tokens_per_step`
//! tokens per encoder frame and re-runs the prediction network only after a
//! non-blank token; CTC takes the per-frame argmax and collapses repeats and
//! blanks.

use std::sync::OnceLock;

use regex::{Captures, Regex};

pub const BLANK_TOKEN: &str = "<blk>";
const WORD_SEPARATOR: char = '\u{2581}';

#[derive(Debug, Clone)]
pub struct Vocab {
    tokens: Vec<String>,
    blank: usize,
}

impl Vocab {
    /// Parse `token id` lines (the onnx-asr `*_vocab.txt` format).
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut entries = Vec::new();
        for (line_no, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let (token, id) = line
                .rsplit_once(' ')
                .ok_or_else(|| format!("vocab line {}: expected 'token id'", line_no + 1))?;
            let id: usize = id
                .parse()
                .map_err(|_| format!("vocab line {}: bad id {id:?}", line_no + 1))?;
            entries.push((id, token.replace(WORD_SEPARATOR, " ")));
        }
        if entries.is_empty() {
            return Err("vocab is empty".to_owned());
        }

        let size = entries.iter().map(|(id, _)| id + 1).max().unwrap_or(0);
        let mut tokens = vec![String::new(); size];
        let mut seen = vec![false; size];
        for (id, token) in entries {
            if seen[id] {
                return Err(format!("vocab id {id} is duplicated"));
            }
            seen[id] = true;
            tokens[id] = token;
        }
        if let Some(missing) = seen.iter().position(|seen| !seen) {
            return Err(format!("vocab id {missing} is missing"));
        }
        let blank = tokens
            .iter()
            .position(|token| token == BLANK_TOKEN)
            .ok_or_else(|| format!("vocab has no {BLANK_TOKEN} token"))?;
        Ok(Self { tokens, blank })
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn blank(&self) -> usize {
        self.blank
    }

    /// Join token pieces and normalise spaces like onnx-asr
    /// (`\A\s|\s\B|(\s)\b`): drop the leading space and spaces before
    /// punctuation, keep one space before a word.
    pub fn decode(&self, ids: &[usize]) -> String {
        let joined: String = ids
            .iter()
            .filter_map(|id| self.tokens.get(*id))
            .map(String::as_str)
            .collect();
        space_pattern()
            .replace_all(
                &joined,
                |captures: &Captures<'_>| {
                    if captures.get(1).is_some() { " " } else { "" }
                },
            )
            .into_owned()
    }
}

fn space_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\A\s|\s\B|(\s)\b").expect("valid regex"))
}

/// Index of the largest value; the first one on ties, like `numpy.argmax`.
pub fn argmax(values: &[f32]) -> usize {
    let mut best = 0;
    for (index, value) in values.iter().enumerate() {
        if *value > values[best] {
            best = index;
        }
    }
    best
}

/// Greedy CTC over row-major `[frames, classes]` log-probabilities.
pub fn ctc_greedy(log_probs: &[f32], classes: usize, frames: usize, blank: usize) -> Vec<usize> {
    let mut tokens = Vec::new();
    let mut previous = blank;
    for frame in 0..frames {
        let row = &log_probs[frame * classes..(frame + 1) * classes];
        let token = argmax(row);
        if token != blank && token != previous {
            tokens.push(token);
        }
        previous = token;
    }
    tokens
}

/// Prediction-network output plus its LSTM state.
#[derive(Debug, Clone, PartialEq)]
pub struct DecoderState {
    pub output: Vec<f32>,
    pub hidden: Vec<f32>,
    pub cell: Vec<f32>,
}

/// The two networks a transducer needs besides the encoder.
pub trait Transducer {
    type Error;

    /// Run the prediction network on `token`, starting from `previous`
    /// (`None` means a zero state).
    fn predict(
        &mut self,
        token: usize,
        previous: Option<&DecoderState>,
    ) -> Result<DecoderState, Self::Error>;

    /// Joint network logits for one encoder frame.
    fn joint(&mut self, frame: &[f32], decoder: &DecoderState) -> Result<Vec<f32>, Self::Error>;
}

/// Greedy RNN-T decoding over `frames` encoder frames (`frame(t)` yields one).
pub fn rnnt_greedy<'a, T: Transducer>(
    model: &mut T,
    frames: usize,
    mut frame: impl FnMut(usize) -> &'a [f32],
    blank: usize,
    max_tokens_per_step: usize,
) -> Result<Vec<usize>, T::Error> {
    let mut tokens = Vec::new();
    let mut decoder = model.predict(blank, None)?;
    let mut time = 0;
    let mut emitted = 0;
    while time < frames {
        let logits = model.joint(frame(time), &decoder)?;
        let token = argmax(&logits);
        if token != blank {
            tokens.push(token);
            emitted += 1;
            decoder = model.predict(token, Some(&decoder))?;
        }
        if token == blank || emitted == max_tokens_per_step {
            time += 1;
            emitted = 0;
        }
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocab {
        Vocab::parse("<unk> 0\n. 1\n, 2\n\u{2581} 3\nпри 4\nвет 5\n\u{2581}мир 6\n<blk> 7\n")
            .unwrap()
    }

    #[test]
    fn parses_the_onnx_asr_format() {
        let vocab = vocab();
        assert_eq!(vocab.len(), 8);
        assert_eq!(vocab.blank(), 7);
    }

    #[test]
    fn rejects_broken_vocabularies() {
        assert!(Vocab::parse("").is_err());
        assert!(Vocab::parse("a 0\nb 2\n<blk> 3\n").is_err());
        assert!(Vocab::parse("a 0\nb 0\n<blk> 1\n").is_err());
        assert!(Vocab::parse("a 0\nb 1\n").is_err());
    }

    #[test]
    fn decodes_word_pieces_and_spaces() {
        let vocab = vocab();
        // "▁" + "при" + "вет" + "," + "▁мир" + "."
        assert_eq!(vocab.decode(&[3, 4, 5, 2, 6, 1]), "привет, мир.");
        assert_eq!(vocab.decode(&[]), "");
    }

    #[test]
    fn ctc_collapses_repeats_and_blanks() {
        let classes = 3;
        let blank = 2;
        #[rustfmt::skip]
        let log_probs = [
            0.9, 0.0, 0.1, // 0
            0.9, 0.0, 0.1, // 0 (repeat)
            0.0, 0.0, 0.9, // blank
            0.9, 0.0, 0.1, // 0 again after blank
            0.0, 0.9, 0.1, // 1
        ];
        assert_eq!(ctc_greedy(&log_probs, classes, 5, blank), vec![0, 0, 1]);
    }

    /// Joint emits a scripted token per call; the decoder output counts tokens.
    struct Scripted {
        script: Vec<usize>,
        calls: usize,
        predicted: Vec<usize>,
    }

    impl Transducer for Scripted {
        type Error = String;

        fn predict(
            &mut self,
            token: usize,
            previous: Option<&DecoderState>,
        ) -> Result<DecoderState, String> {
            self.predicted.push(token);
            let count = previous.map_or(0.0, |state| state.output[0] + 1.0);
            Ok(DecoderState {
                output: vec![count],
                hidden: vec![],
                cell: vec![],
            })
        }

        fn joint(&mut self, _frame: &[f32], _decoder: &DecoderState) -> Result<Vec<f32>, String> {
            let token = self.script.get(self.calls).copied().unwrap_or(0);
            self.calls += 1;
            let mut logits = vec![0.0; 4];
            logits[token] = 1.0;
            Ok(logits)
        }
    }

    #[test]
    fn rnnt_caps_tokens_per_frame_and_advances_on_blank() {
        let blank = 0;
        // frame 0: 1, 2, 3 (cap reached -> next frame); frame 1: 1, blank; frame 2: blank
        let mut model = Scripted {
            script: vec![1, 2, 3, 1, 0, 0],
            calls: 0,
            predicted: Vec::new(),
        };
        let frame = [0.0f32];
        let tokens = rnnt_greedy(&mut model, 3, |_| &frame[..], blank, 3).unwrap();
        assert_eq!(tokens, vec![1, 2, 3, 1]);
        assert_eq!(model.calls, 6);
        // the prediction network starts from blank and runs once per emitted token
        assert_eq!(model.predicted, vec![0, 1, 2, 3, 1]);
    }

    #[test]
    fn rnnt_on_empty_input_emits_nothing() {
        let mut model = Scripted {
            script: vec![],
            calls: 0,
            predicted: Vec::new(),
        };
        let frame = [0.0f32];
        assert!(
            rnnt_greedy(&mut model, 0, |_| &frame[..], 0, 3)
                .unwrap()
                .is_empty()
        );
    }
}
