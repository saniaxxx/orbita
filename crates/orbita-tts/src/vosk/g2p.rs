//! Grapheme-to-phoneme for Vosk TTS ru 0.10 (`multistream_v1`).
//!
//! Port of `vosk_tts/g2p.py` (`convert`) and `Synth.g2p_multistream` from
//! alphacep/vosk-tts (Apache-2.0). Quirks are kept on purpose so the phoneme
//! streams match the Python package; the golden fixtures in
//! `tests/fixtures/vosk_text.json` come from it.

use std::{collections::HashMap, sync::OnceLock};

use regex::Regex;

/// One phoneme with its four side streams, as fed to the synthesizer:
/// `[phoneme, punctuation after it, in-quote flag, last punctuation,
/// last sentence punctuation]`.
pub type PhonemeIds = [i64; 5];

#[derive(Debug, Clone, PartialEq)]
pub struct G2pOutput {
    pub ids: Vec<PhonemeIds>,
    /// Row of the per-word BERT matrix each phoneme takes its embedding from.
    pub bert_index: Vec<usize>,
    /// Phonemes that were not in `phoneme_id_map` and were dropped (the
    /// Python package raises `KeyError` on them instead).
    pub dropped: Vec<String>,
}

const SOFT_LETTERS: &[char] = &['я', 'ё', 'ю', 'и', 'ь', 'е'];
const START_SYLLABLE: &[&str] = &[
    "#", "ъ", "ь", "а", "я", "о", "ё", "у", "ю", "э", "е", "и", "ы", "-",
];
const OTHERS: &[&str] = &["#", "+", "-", "ь", "ъ"];
const IOTATED: &[char] = &['я', 'ю', 'е', 'ё'];

fn soft_hard(letter: char) -> Option<&'static str> {
    Some(match letter {
        'б' => "b",
        'в' => "v",
        'г' | 'Г' => "g",
        'д' => "d",
        'з' => "z",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'ф' => "f",
        'х' => "h",
        _ => return None,
    })
}

fn other_consonant(letter: char) -> Option<&'static str> {
    Some(match letter {
        'ж' => "zh",
        'ц' => "c",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "sch",
        'й' => "j",
        _ => return None,
    })
}

fn vowel(letter: char) -> Option<&'static str> {
    Some(match letter {
        'а' | 'я' => "a",
        'у' | 'ю' => "u",
        'о' | 'ё' => "o",
        'э' | 'е' => "e",
        'и' => "i",
        'ы' => "y",
        _ => return None,
    })
}

/// Rule-based phonemes for a lowercase word; `+` marks the stressed vowel
/// that follows it. Port of `g2p.convert`.
pub fn convert(stress_word: &str) -> String {
    // (symbol, stress) after removing '+' marks.
    let mut phones: Vec<(String, u8)> = Vec::new();
    let mut stress = 0u8;
    for symbol in std::iter::once('#')
        .chain(stress_word.chars())
        .chain(std::iter::once('#'))
    {
        if symbol == '+' {
            stress = 1;
        } else {
            phones.push((symbol.to_string(), stress));
            stress = 0;
        }
    }

    // pallatize: iterates over a snapshot, so `phone` is always the original
    // symbol while `phones[i + 1]` is not yet rewritten.
    let original: Vec<String> = phones.iter().map(|(symbol, _)| symbol.clone()).collect();
    for index in 0..original.len().saturating_sub(1) {
        let letter = first_char(&original[index]);
        if let Some(latin) = soft_hard(letter) {
            let next = first_char(&phones[index + 1].0);
            phones[index] = if SOFT_LETTERS.contains(&next) {
                (format!("{latin}j"), 0)
            } else {
                (latin.to_owned(), 0)
            };
        }
        if let Some(latin) = other_consonant(letter) {
            phones[index] = (latin.to_owned(), 0);
        }
    }

    // convert_vowels
    let mut converted: Vec<String> = Vec::new();
    let mut previous = String::new();
    for (symbol, stress) in &phones {
        let letter = first_char(symbol);
        if START_SYLLABLE.contains(&previous.as_str()) && IOTATED.contains(&letter) {
            converted.push("j".to_owned());
        }
        match vowel(letter) {
            Some(base) if symbol.chars().count() == 1 => {
                converted.push(format!("{base}{stress}"));
            }
            _ => converted.push(symbol.clone()),
        }
        previous = symbol.clone();
    }

    converted
        .into_iter()
        .filter(|phone| !OTHERS.contains(&phone.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_char(text: &str) -> char {
    text.chars().next().unwrap_or('\0')
}

fn split_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r#"(\.\.\.|- |[ ,.?!;:"()])"#).expect("valid regex"))
}

/// `re.split` with a capturing group: pieces and separators interleaved.
fn split_keep(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut last = 0;
    for found in split_pattern().find_iter(text) {
        parts.push(&text[last..found.start()]);
        parts.push(found.as_str());
        last = found.end();
    }
    parts.push(&text[last..]);
    parts
}

/// Port of `Synth.g2p_multistream(text, bert, word_pos=False)`.
///
/// `text` is the prepared input (after `—` → `-`), `bert_rows` the number of
/// rows of the selected BERT matrix: indices past it are clamped to the last
/// row where Python would raise `IndexError`.
pub fn multistream(
    text: &str,
    dictionary: &HashMap<String, String>,
    phoneme_ids: &HashMap<String, i64>,
    bert_rows: usize,
) -> G2pOutput {
    struct Phone {
        phoneme: String,
        punctuation: Vec<String>,
        in_quote: i64,
        bert_index: usize,
    }

    let text = text.replace(" -", "- ").to_lowercase();
    let mut phones = vec![Phone {
        phoneme: "^".to_owned(),
        punctuation: Vec::new(),
        in_quote: 0,
        bert_index: 0,
    }];
    let mut in_quote = 0i64;
    let mut current_punctuation: Vec<String> = Vec::new();
    let mut bert_index = 1usize;

    for word in split_keep(&text) {
        if word.is_empty() {
            continue;
        }
        if word == "\"" {
            in_quote = 1 - in_quote;
            continue;
        }
        if word == "- " || word == "-" {
            current_punctuation.push("-".to_owned());
            continue;
        }
        let is_separator = split_pattern()
            .find(word)
            .is_some_and(|found| found.start() == 0);
        if is_separator && word != " " {
            current_punctuation.push(word.to_owned());
            continue;
        }
        if word == " " {
            phones.push(Phone {
                phoneme: " ".to_owned(),
                punctuation: std::mem::take(&mut current_punctuation),
                in_quote,
                bert_index,
            });
            continue;
        }

        let phonemes = match dictionary.get(word) {
            Some(phonemes) => phonemes.clone(),
            None => convert(word),
        };
        for phoneme in phonemes.split_whitespace() {
            phones.push(Phone {
                phoneme: phoneme.to_owned(),
                punctuation: Vec::new(),
                in_quote,
                bert_index,
            });
        }
        current_punctuation.clear();
        bert_index += 1;
    }
    phones.push(Phone {
        phoneme: " ".to_owned(),
        punctuation: current_punctuation,
        in_quote,
        bert_index,
    });
    phones.push(Phone {
        phoneme: "$".to_owned(),
        punctuation: Vec::new(),
        in_quote: 0,
        bert_index,
    });

    let id = |symbol: &str| phoneme_ids.get(symbol).copied();
    let mut last_punctuation = " ".to_owned();
    let mut last_sentence_punctuation = " ".to_owned();
    let mut ids = Vec::with_capacity(phones.len());
    let mut indices = Vec::with_capacity(phones.len());
    let mut dropped = Vec::new();

    for phone in phones.iter().rev() {
        let has = |mark: &str| phone.punctuation.iter().any(|p| p == mark);
        if has("...") {
            last_sentence_punctuation = "...".to_owned();
        } else if has(".") {
            last_sentence_punctuation = ".".to_owned();
        } else if has("!") {
            last_sentence_punctuation = "!".to_owned();
        } else if has("?") {
            last_sentence_punctuation = "?".to_owned();
        } else if has("-") {
            last_sentence_punctuation = "-".to_owned();
        }
        if let Some(first) = phone.punctuation.first() {
            last_punctuation = first.clone();
        }
        let current = phone.punctuation.first().map(String::as_str).unwrap_or("_");

        let (Some(phoneme), Some(current), Some(last), Some(sentence)) = (
            id(&phone.phoneme),
            id(current),
            id(&last_punctuation),
            id(&last_sentence_punctuation),
        ) else {
            dropped.push(phone.phoneme.clone());
            continue;
        };
        ids.push([phoneme, current, phone.in_quote, last, sentence]);
        indices.push(phone.bert_index.min(bert_rows.saturating_sub(1)));
    }
    ids.reverse();
    indices.reverse();
    dropped.reverse();

    G2pOutput {
        ids,
        bert_index: indices,
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_matches_known_words() {
        assert_eq!(convert("мама"), "m a0 m a0");
        assert_eq!(convert("м+ама"), "m a1 m a0");
        assert_eq!(convert("ёжик"), "j o0 zh i0 k");
        assert_eq!(convert("съесть"), "s j e0 s tj");
        assert_eq!(convert("пьеса"), "pj j e0 s a0");
    }

    #[test]
    fn split_keeps_separators_like_re_split() {
        assert_eq!(
            split_keep("ну что ж... да"),
            vec!["ну", " ", "что", " ", "ж", "...", "", " ", "да"]
        );
        assert_eq!(split_keep("а- б"), vec!["а", "- ", "б"]);
    }

    fn maps() -> (HashMap<String, String>, HashMap<String, i64>) {
        let dictionary = HashMap::from([("да".to_owned(), "d a1".to_owned())]);
        let ids = [
            "_", "^", "$", " ", "!", "'", "(", ")", ",", "-", ".", "...", ":", ";", "?", "a0",
            "a1", "d", "n", "j", "e0", "t",
        ];
        let phoneme_ids = ids
            .iter()
            .enumerate()
            .map(|(index, symbol)| ((*symbol).to_owned(), index as i64))
            .collect();
        (dictionary, phoneme_ids)
    }

    #[test]
    fn multistream_tracks_punctuation_streams() {
        let (dictionary, phoneme_ids) = maps();
        let out = multistream("Да, да!", &dictionary, &phoneme_ids, 4);
        let phonemes: Vec<i64> = out.ids.iter().map(|ids| ids[0]).collect();
        // ^ d a1 ' ' d a1 ' ' $
        assert_eq!(phonemes, vec![1, 17, 16, 3, 17, 16, 3, 2]);
        // the space after "да" carries ',' as its own punctuation
        assert_eq!(out.ids[3][1], 8);
        // the final space carries '!'
        assert_eq!(out.ids[6][1], 4);
        // last sentence punctuation is '!' everywhere up to the end
        assert!(out.ids[..7].iter().all(|ids| ids[4] == 4));
        assert_eq!(out.bert_index, vec![0, 1, 1, 2, 2, 2, 3, 3]);
        assert!(out.dropped.is_empty());
    }

    #[test]
    fn unknown_phonemes_are_dropped_not_fatal() {
        let (dictionary, phoneme_ids) = maps();
        let out = multistream("zz", &dictionary, &phoneme_ids, 2);
        assert_eq!(out.dropped, vec!["z".to_owned(), "z".to_owned()]);
    }

    #[test]
    fn bert_index_is_clamped() {
        let (dictionary, phoneme_ids) = maps();
        let out = multistream("да да да", &dictionary, &phoneme_ids, 2);
        assert!(out.bert_index.iter().all(|index| *index <= 1));
    }
}
