//! Spoken-reply helpers for streaming (D-19).
//!
//! The station hears a reply as TTS streams, one per part. Every new phrase
//! costs ~0.9 s of synthesis on the stand GPU (Vosk TTS pays per new input
//! shape), so the reply is split at most once: the first sentence goes out as
//! soon as the model finishes it, the rest follows as one part and is
//! synthesised while the first one is playing.

/// Cuts the first sentence off a token stream.
#[derive(Debug, Clone)]
pub struct FirstSentence {
    buffer: String,
    emitted: bool,
    min_chars: usize,
}

impl FirstSentence {
    /// `min_chars` keeps the first part long enough to cover the synthesis of
    /// the rest; a shorter sentence waits for the next boundary.
    pub fn new(min_chars: usize) -> Self {
        Self {
            buffer: String::new(),
            emitted: false,
            min_chars,
        }
    }

    /// Feed a delta; returns the first sentence once it is complete.
    pub fn push(&mut self, delta: &str) -> Option<String> {
        self.buffer.push_str(delta);
        if self.emitted {
            return None;
        }
        let mut chars = self.buffer.char_indices().peekable();
        let mut count = 0;
        while let Some((index, char)) = chars.next() {
            count += 1;
            let Some(&(_, next)) = chars.peek() else {
                break;
            };
            if count >= self.min_chars && is_terminator(char) && next.is_whitespace() {
                let end = index + char.len_utf8();
                let head = self.buffer[..end].trim().to_owned();
                if head.is_empty() {
                    continue;
                }
                self.buffer.replace_range(..end, "");
                self.emitted = true;
                return Some(head);
            }
        }
        None
    }

    /// Whether the first sentence has been cut off already.
    pub fn emitted(&self) -> bool {
        self.emitted
    }
}

fn is_terminator(char: char) -> bool {
    matches!(char, '.' | '!' | '?' | '…')
}

/// The reply without the part already spoken. `None` when `full` does not
/// start with `first`, so the caller never repeats or loses words silently.
pub fn remainder<'a>(full: &'a str, first: &str) -> Option<&'a str> {
    full.trim().strip_prefix(first.trim()).map(str::trim)
}

/// The reply ends with a question, so the station should listen for the
/// answer ("Хочешь ещё сказку?").
pub fn asks(text: &str) -> bool {
    text.trim_end_matches(|char: char| {
        char.is_whitespace() || matches!(char, '"' | '»' | ')' | '*')
    })
    .ends_with('?')
}

/// Strip markup a small model still emits despite the voice instructions;
/// TTS would read it out or stumble on it.
pub fn spoken(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|char| !matches!(char, '*' | '#' | '`'))
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(splitter: &mut FirstSentence, deltas: &[&str]) -> Option<String> {
        deltas.iter().find_map(|delta| splitter.push(delta))
    }

    #[test]
    fn first_sentence_is_cut_at_the_first_boundary_long_enough() {
        let mut splitter = FirstSentence::new(20);
        let first = feed(
            &mut splitter,
            &[
                "Я",
                " —",
                " голосовой",
                " ассистент",
                " OpenClaw",
                ".",
                " Я",
                " умею",
            ],
        );
        assert_eq!(first.as_deref(), Some("Я — голосовой ассистент OpenClaw."));
        assert!(splitter.emitted());
        // Later deltas never produce a second cut.
        assert_eq!(splitter.push(" ещё. Много. "), None);
    }

    #[test]
    fn short_sentences_and_decimals_do_not_cut() {
        let mut splitter = FirstSentence::new(20);
        assert_eq!(splitter.push("Привет! "), None);
        assert_eq!(splitter.push("Сейчас +2.5 градуса"), None);
        assert_eq!(
            splitter.push(", облачно. Ветер"),
            Some("Привет! Сейчас +2.5 градуса, облачно.".to_owned())
        );
    }

    #[test]
    fn a_boundary_needs_the_following_whitespace() {
        let mut splitter = FirstSentence::new(5);
        // The stream may end right after the dot: that is the whole reply,
        // not an early part.
        assert_eq!(splitter.push("Это весь ответ."), None);
        assert_eq!(splitter.push(" "), Some("Это весь ответ.".to_owned()));
    }

    #[test]
    fn remainder_is_the_unspoken_tail() {
        assert_eq!(
            remainder(" Первое предложение. Второе. ", "Первое предложение."),
            Some("Второе.")
        );
        assert_eq!(remainder("Только одно.", "Только одно."), Some(""));
        assert_eq!(remainder("Другой текст.", "Первое."), None);
    }

    #[test]
    fn a_closing_question_keeps_listening() {
        assert!(asks("Жили-были кот и пёс. Хочешь ещё сказку?"));
        assert!(asks("Включить свет в детской?» "));
        assert!(!asks("Почему небо голубое? Из-за рассеяния света."));
        assert!(!asks("Свет в кладовке включён."));
    }

    #[test]
    fn markup_is_not_spoken() {
        assert_eq!(
            spoken("Горит **остров** и\n`столовая` #1"),
            "Горит остров и столовая 1"
        );
    }
}
