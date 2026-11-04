//! BERT WordPiece tokenizer, as `tokenizers.BertWordPieceTokenizer(vocab,
//! lowercase=False)` sets it up: BERT normaliser (clean text, CJK spacing, no
//! lowercasing or accent stripping), BERT pre-tokeniser (whitespace and
//! punctuation), greedy longest-match WordPiece with `##`, `[CLS] … [SEP]`.

use std::{collections::HashMap, sync::OnceLock};

use regex::Regex;

const MAX_INPUT_CHARS_PER_WORD: usize = 100;

#[derive(Debug, Clone)]
pub struct WordPiece {
    vocab: HashMap<String, i64>,
    unk: i64,
    cls: i64,
    sep: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoding {
    pub tokens: Vec<String>,
    pub ids: Vec<i64>,
}

impl WordPiece {
    /// One token per line, the id is the line number (BERT `vocab.txt`).
    pub fn from_vocab(text: &str) -> Result<Self, String> {
        let vocab: HashMap<String, i64> = text
            .lines()
            .enumerate()
            .map(|(index, token)| (token.trim_end_matches('\r').to_owned(), index as i64))
            .collect();
        let special = |token: &str| {
            vocab
                .get(token)
                .copied()
                .ok_or_else(|| format!("BERT vocab has no {token}"))
        };
        Ok(Self {
            unk: special("[UNK]")?,
            cls: special("[CLS]")?,
            sep: special("[SEP]")?,
            vocab,
        })
    }

    pub fn encode(&self, text: &str) -> Encoding {
        let mut tokens = vec!["[CLS]".to_owned()];
        let mut ids = vec![self.cls];
        for word in pre_tokenize(&normalize(text)) {
            self.word_piece(&word, &mut tokens, &mut ids);
        }
        tokens.push("[SEP]".to_owned());
        ids.push(self.sep);
        Encoding { tokens, ids }
    }

    fn word_piece(&self, word: &str, tokens: &mut Vec<String>, ids: &mut Vec<i64>) {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() > MAX_INPUT_CHARS_PER_WORD {
            tokens.push("[UNK]".to_owned());
            ids.push(self.unk);
            return;
        }
        let mut pieces = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let mut end = chars.len();
            let mut found = None;
            while start < end {
                let mut piece: String = chars[start..end].iter().collect();
                if start > 0 {
                    piece.insert_str(0, "##");
                }
                if let Some(id) = self.vocab.get(&piece) {
                    found = Some((piece, *id));
                    break;
                }
                end -= 1;
            }
            match found {
                Some(piece) => {
                    pieces.push(piece);
                    start = end;
                }
                None => {
                    tokens.push("[UNK]".to_owned());
                    ids.push(self.unk);
                    return;
                }
            }
        }
        for (piece, id) in pieces {
            tokens.push(piece);
            ids.push(id);
        }
    }
}

fn category(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn is_control(c: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    if matches!(c, '\t' | '\n' | '\r') {
        return false;
    }
    category(r"\A\p{C}\z", &RE).is_match(c.encode_utf8(&mut [0; 4]))
}

fn is_whitespace(c: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    matches!(c, ' ' | '\t' | '\n' | '\r')
        || category(r"\A\p{Zs}\z", &RE).is_match(c.encode_utf8(&mut [0; 4]))
}

fn is_punctuation(c: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    c.is_ascii_punctuation() || category(r"\A\p{P}\z", &RE).is_match(c.encode_utf8(&mut [0; 4]))
}

fn is_chinese(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF
        | 0x3400..=0x4DBF
        | 0x20000..=0x2A6DF
        | 0x2A700..=0x2B73F
        | 0x2B740..=0x2B81F
        | 0x2B920..=0x2CEAF
        | 0xF900..=0xFAFF
        | 0x2F800..=0x2FA1F)
}

/// BERT normaliser with `clean_text` and `handle_chinese_chars`.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\0' || c == '\u{fffd}' || is_control(c) {
            continue;
        }
        if is_whitespace(c) {
            out.push(' ');
        } else if is_chinese(c) {
            out.push(' ');
            out.push(c);
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// Split on whitespace (dropped) and isolate every punctuation character.
fn pre_tokenize(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else if is_punctuation(c) {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            words.push(c.to_string());
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Rows of the BERT output used as word embeddings: every token that does not
/// start with `#` and is not one of `-,.?!;:"` (`get_word_bert(nopunc=True)`).
pub fn word_rows(tokens: &[String]) -> Vec<usize> {
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| {
            let first = token.chars().next().unwrap_or('#');
            first != '#' && !matches!(first, '-' | ',' | '.' | '?' | '!' | ';' | ':' | '"')
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenizer() -> WordPiece {
        WordPiece::from_vocab("[PAD]\n[UNK]\n[CLS]\n[SEP]\nПри\n##вет\n,\nмир\n!\n-\nто\nКто\n")
            .unwrap()
    }

    #[test]
    fn splits_words_and_punctuation() {
        let encoding = tokenizer().encode("Привет, мир!");
        assert_eq!(
            encoding.tokens,
            vec!["[CLS]", "При", "##вет", ",", "мир", "!", "[SEP]"]
        );
        assert_eq!(encoding.ids, vec![2, 4, 5, 6, 7, 8, 3]);
    }

    #[test]
    fn unknown_word_is_one_unk() {
        let encoding = tokenizer().encode("Кто-то   абв");
        assert_eq!(
            encoding.tokens,
            vec!["[CLS]", "Кто", "-", "то", "[UNK]", "[SEP]"]
        );
    }

    #[test]
    fn word_rows_skip_pieces_and_punctuation() {
        let tokens: Vec<String> = ["[CLS]", "При", "##вет", ",", "мир", "(", "!", "[SEP]"]
            .iter()
            .map(|t| (*t).to_owned())
            .collect();
        // '(' is not in the nopunc set, so it keeps its row
        assert_eq!(word_rows(&tokens), vec![0, 1, 4, 5, 7]);
    }
}
