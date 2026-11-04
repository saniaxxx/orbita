//! Port of `UnicodeProcessor` and `chunkText` from the supertonic JS service.

use std::sync::OnceLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

pub const LANGUAGE: &str = "ru";

pub const AVAILABLE_LANGS: &[&str] = &[
    "en", "ko", "ja", "ar", "bg", "cs", "da", "de", "el", "es", "et", "fi", "fr", "hi", "hr", "hu",
    "id", "it", "lt", "lv", "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "tr", "uk", "vi", "na",
];

const ABBREVIATIONS: &[&str] = &[
    "Mr.", "Mrs.", "Ms.", "Dr.", "Prof.", "Sr.", "Jr.", "Ph.D.", "etc.", "e.g.", "i.e.", "vs.",
    "Inc.", "Ltd.", "Co.", "Corp.", "St.", "Ave.", "Blvd.",
];

const CHAR_REPLACEMENTS: &[(&str, &str)] = &[
    ("–", "-"),
    ("‑", "-"),
    ("—", "-"),
    ("_", " "),
    ("“", "\""),
    ("”", "\""),
    ("‘", "'"),
    ("’", "'"),
    ("´", "'"),
    ("`", "'"),
    ("[", " "),
    ("]", " "),
    ("|", " "),
    ("/", " "),
    ("#", " "),
    ("→", " "),
    ("←", " "),
];

const EXPR_REPLACEMENTS: &[(&str, &str)] = &[
    ("@", " at "),
    ("e.g.,", "for example, "),
    ("i.e.,", "that is, "),
];

const SPACING_FIXES: &[(&str, &str)] = &[
    (" ,", ","),
    (" .", "."),
    (" !", "!"),
    (" ?", "?"),
    (" ;", ";"),
    (" :", ":"),
    (" '", "'"),
];

fn emoji_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"[\u{1F600}-\u{1F64F}\u{1F300}-\u{1F5FF}\u{1F680}-\u{1F6FF}\u{1F700}-\u{1F77F}\u{1F780}-\u{1F7FF}\u{1F800}-\u{1F8FF}\u{1F900}-\u{1F9FF}\u{1FA00}-\u{1FA6F}\u{1FA70}-\u{1FAFF}\u{2600}-\u{26FF}\u{2700}-\u{27BF}\u{1F1E6}-\u{1F1FF}]+",
        )
        .expect("valid emoji regex")
    })
}

fn terminal_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"[.!?;:,'"')\]…。」』】〉》›»]$"#).expect("valid terminal regex")
    })
}

fn paragraph_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\n\s*\n+").expect("valid paragraph regex"))
}

/// Port of `UnicodeProcessor._preprocessText`.
pub fn preprocess_text(text: &str, lang: &str) -> Result<String, String> {
    let mut text: String = text.nfkd().collect();
    text = emoji_re().replace_all(&text, "").into_owned();

    for (from, to) in CHAR_REPLACEMENTS {
        text = text.replace(from, to);
    }

    text = text
        .chars()
        .filter(|character| !matches!(character, '♥' | '☆' | '♡' | '©' | '\\'))
        .collect();

    for (from, to) in EXPR_REPLACEMENTS {
        text = text.replace(from, to);
    }

    for (from, to) in SPACING_FIXES {
        text = text.replace(from, to);
    }

    while text.contains("\"\"") {
        text = text.replace("\"\"", "\"");
    }
    while text.contains("''") {
        text = text.replace("''", "'");
    }
    while text.contains("``") {
        text = text.replace("``", "`");
    }

    text = collapse_whitespace(&text);

    if !terminal_re().is_match(&text) {
        text.push('.');
    }

    if !AVAILABLE_LANGS.contains(&lang) {
        return Err(format!(
            "Invalid language: {lang}. Available: {}",
            AVAILABLE_LANGS.join(", ")
        ));
    }

    Ok(format!("<{lang}>{text}</{lang}>"))
}

/// Mirror JS `Array.from` + `charCodeAt(0)` and `length`.
///
/// `Array.from` walks code points, while `length` and `charCodeAt` count UTF-16
/// units. For a non-BMP character JS stores the high surrogate and counts two
/// units; we reproduce that so the text mask has the same size as the Node
/// service.
pub fn text_ids_and_length(text: &str) -> (Vec<u32>, usize) {
    let mut ids = Vec::with_capacity(text.chars().count());
    for character in text.chars() {
        let code_point = character as u32;
        if code_point <= 0xFFFF {
            ids.push(code_point);
        } else {
            ids.push(0xD800 + ((code_point - 0x1_0000) >> 10));
        }
    }
    let length = text.encode_utf16().count();
    (ids, length)
}

/// Port of `chunkText`: paragraphs, then sentences, packed up to `max_len`.
pub fn chunk_text(text: &str, max_len: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    for paragraph in paragraph_re().split(text.trim()) {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }

        let mut current = String::new();
        for sentence in split_sentences(paragraph) {
            let needed = current.chars().count() + sentence.chars().count() + 1;
            if needed <= max_len {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(&sentence);
            } else {
                if !current.is_empty() {
                    chunks.push(current.trim().to_owned());
                }
                current = sentence;
            }
        }
        if !current.is_empty() {
            chunks.push(current.trim().to_owned());
        }
    }
    chunks
}

fn split_sentences(paragraph: &str) -> Vec<String> {
    let characters: Vec<char> = paragraph.chars().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut start = 0;
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];
        if matches!(character, '.' | '!' | '?')
            && index + 1 < characters.len()
            && characters[index + 1].is_whitespace()
        {
            let mut next = index + 1;
            while next < characters.len() && characters[next].is_whitespace() {
                next += 1;
            }

            let prefix: String = characters[..=index].iter().collect();
            if ABBREVIATIONS
                .iter()
                .any(|abbreviation| prefix.ends_with(abbreviation))
            {
                index = next;
                continue;
            }

            // (?<!\b[A-Z]\.) — do not split after a single capital initial.
            if character == '.'
                && index >= 1
                && characters[index - 1].is_ascii_uppercase()
                && (index - 1 == 0 || !is_ascii_word(characters[index - 2]))
            {
                index = next;
                continue;
            }

            parts.push(characters[start..=index].iter().collect());
            start = next;
            index = next;
            continue;
        }
        index += 1;
    }

    parts.push(characters[start..].iter().collect());
    parts.retain(|part| !part.is_empty());
    parts
}

fn is_ascii_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocesses_like_the_python_port() {
        let text = "  «Привет» — мир!  ";
        assert_eq!(
            preprocess_text(text, "ru").unwrap(),
            "<ru>«Привет» - мир!</ru>"
        );
    }

    #[test]
    fn adds_a_terminal_period() {
        assert_eq!(preprocess_text("привет", "ru").unwrap(), "<ru>привет.</ru>");
    }

    #[test]
    fn rejects_unknown_languages() {
        assert!(preprocess_text("hello", "xx").is_err());
    }

    #[test]
    fn counts_utf16_units_for_non_bmp() {
        let (ids, length) = text_ids_and_length("<ru>𝄞</ru>");
        assert_eq!(length, ids.len() + 1);
        assert_eq!(ids[4], 0xD834);
    }

    #[test]
    fn splits_paragraphs_and_sentences() {
        let chunks = chunk_text("Первая строка.\n\nВторой абзац.", 300);
        assert_eq!(chunks, vec!["Первая строка.", "Второй абзац."]);
    }

    #[test]
    fn does_not_split_abbreviations() {
        let chunks = chunk_text("Dr. Smith пришёл. Он ушёл.", 300);
        assert_eq!(chunks, vec!["Dr. Smith пришёл. Он ушёл."]);
    }

    #[test]
    fn packs_sentences_up_to_the_limit() {
        let chunks = chunk_text("Один. Два. Три.", 10);
        assert_eq!(chunks, vec!["Один. Два.", "Три."]);
    }
}
