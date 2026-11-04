//! Text clean-up before the Vosk pipeline.
//!
//! The Python package only maps `—` to `-` and raises `KeyError` on anything
//! its phoneme table does not know (digits, Latin letters, `%`…). The service
//! must not fail on assistant answers, so numbers are spelled out, Latin is
//! transliterated letter by letter and other symbols become spaces. Plain
//! Cyrillic text with the supported punctuation passes through unchanged, so
//! parity with the Python pipeline holds for it.

const UNITS: [&str; 20] = [
    "ноль",
    "один",
    "два",
    "три",
    "четыре",
    "пять",
    "шесть",
    "семь",
    "восемь",
    "девять",
    "десять",
    "одиннадцать",
    "двенадцать",
    "тринадцать",
    "четырнадцать",
    "пятнадцать",
    "шестнадцать",
    "семнадцать",
    "восемнадцать",
    "девятнадцать",
];
const TENS: [&str; 10] = [
    "",
    "",
    "двадцать",
    "тридцать",
    "сорок",
    "пятьдесят",
    "шестьдесят",
    "семьдесят",
    "восемьдесят",
    "девяносто",
];
const HUNDREDS: [&str; 10] = [
    "",
    "сто",
    "двести",
    "триста",
    "четыреста",
    "пятьсот",
    "шестьсот",
    "семьсот",
    "восемьсот",
    "девятьсот",
];

/// (one, few, many) and whether the scale word is feminine.
const SCALES: [(&str, &str, &str, bool); 3] = [
    ("тысяча", "тысячи", "тысяч", true),
    ("миллион", "миллиона", "миллионов", false),
    ("миллиард", "миллиарда", "миллиардов", false),
];

fn plural<'a>(n: u64, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let tail = n % 100;
    if (11..=14).contains(&tail) {
        return many;
    }
    match n % 10 {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

fn triplet(n: u64, feminine: bool, words: &mut Vec<&'static str>) {
    let hundreds = (n / 100) as usize;
    let rest = n % 100;
    if hundreds > 0 {
        words.push(HUNDREDS[hundreds]);
    }
    if rest >= 20 {
        words.push(TENS[(rest / 10) as usize]);
        let unit = (rest % 10) as usize;
        if unit > 0 {
            words.push(unit_word(unit, feminine));
        }
    } else if rest > 0 {
        words.push(unit_word(rest as usize, feminine));
    }
}

fn unit_word(unit: usize, feminine: bool) -> &'static str {
    match (unit, feminine) {
        (1, true) => "одна",
        (2, true) => "две",
        _ => UNITS[unit],
    }
}

/// Cardinal number in words (nominative, masculine), up to 999 999 999 999.
pub fn number_to_words(n: u64) -> String {
    if n == 0 {
        return UNITS[0].to_owned();
    }
    if n >= 1_000_000_000_000 {
        // Read long digit runs digit by digit.
        return n
            .to_string()
            .chars()
            .map(|digit| UNITS[digit.to_digit(10).unwrap_or(0) as usize])
            .collect::<Vec<_>>()
            .join(" ");
    }
    let mut words: Vec<&'static str> = Vec::new();
    let groups = [
        (n / 1_000_000_000) % 1000,
        (n / 1_000_000) % 1000,
        (n / 1000) % 1000,
    ];
    for (index, group) in groups.iter().enumerate() {
        if *group == 0 {
            continue;
        }
        let (one, few, many, feminine) = SCALES[2 - index];
        triplet(*group, feminine, &mut words);
        words.push(plural(*group, one, few, many));
    }
    triplet(n % 1000, false, &mut words);
    words.join(" ")
}

fn latin(c: char) -> Option<&'static str> {
    Some(match c.to_ascii_lowercase() {
        'a' => "а",
        'b' => "б",
        'c' => "к",
        'd' => "д",
        'e' => "е",
        'f' => "ф",
        'g' => "г",
        'h' => "х",
        'i' => "и",
        'j' => "дж",
        'k' => "к",
        'l' => "л",
        'm' => "м",
        'n' => "н",
        'o' => "о",
        'p' => "п",
        'q' => "к",
        'r' => "р",
        's' => "с",
        't' => "т",
        'u' => "у",
        'v' => "в",
        'w' => "в",
        'x' => "кс",
        'y' => "и",
        'z' => "з",
        _ => return None,
    })
}

fn is_cyrillic(c: char) -> bool {
    matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё')
}

/// Characters the pipeline handles natively.
fn is_supported(c: char) -> bool {
    is_cyrillic(c)
        || c.is_whitespace()
        || matches!(
            c,
            '+' | '-' | ',' | '.' | '?' | '!' | ';' | ':' | '"' | '(' | ')'
        )
}

/// Make arbitrary text safe for the Vosk pipeline (see module docs).
const ABBREVIATIONS: [(&str, &str); 8] = [
    ("мм рт. ст.", "миллиметров ртутного столба"),
    ("м/с", "метров в секунду"),
    ("км/ч", "километров в час"),
    ("гр.", "градусов"),
    ("т.е.", "то есть"),
    ("т.к.", "так как"),
    ("др.", "другие"),
    ("т.д.", "так далее"),
];

/// Expand spoken-unfriendly symbols and abbreviations before the pipeline.
fn expand_spoken(text: &str) -> String {
    let mut text = text.to_owned();
    for (from, to) in ABBREVIATIONS {
        text = text.replace(from, to);
    }
    text.replace(['/', '\\'], " ")
}

pub fn normalize(text: &str) -> String {
    let text = expand_spoken(text);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c.is_ascii_digit() {
            let start = index;
            while index < chars.len() && chars[index].is_ascii_digit() {
                index += 1;
            }
            let digits: String = chars[start..index].iter().collect();
            let is_decimal = index + 1 < chars.len()
                && matches!(chars[index], ',' | '.')
                && chars[index + 1].is_ascii_digit();
            let next = if is_decimal {
                None
            } else {
                chars.get(index).copied()
            };
            push_word(&mut out, &spell_digits(&digits), next);
            // "2,5" / "2.5" -> "два целых пять"
            if is_decimal {
                index += 1;
                let start = index;
                while index < chars.len() && chars[index].is_ascii_digit() {
                    index += 1;
                }
                let fraction: String = chars[start..index].iter().collect();
                push_word(&mut out, "целых", None);
                push_word(
                    &mut out,
                    &spell_digits(&fraction),
                    chars.get(index).copied(),
                );
            }
            continue;
        }
        match c {
            '—' | '–' | '‑' => out.push('-'),
            '−' => push_word(&mut out, "минус", chars.get(index + 1).copied()),
            '…' => out.push_str("..."),
            '«' | '»' | '„' | '“' | '”' => out.push('"'),
            '%' => push_word(&mut out, "процентов", chars.get(index + 1).copied()),
            '°' => push_word(&mut out, "градусов", chars.get(index + 1).copied()),
            '№' => push_word(&mut out, "номер", chars.get(index + 1).copied()),
            '&' => push_word(&mut out, "и", chars.get(index + 1).copied()),
            _ if is_supported(c) => out.push(c),
            _ => match latin(c) {
                Some(cyrillic) => out.push_str(cyrillic),
                None => out.push(' '),
            },
        }
        index += 1;
    }
    out
}

fn spell_digits(digits: &str) -> String {
    let trimmed = digits.trim_start_matches('0');
    if digits.len() > 12 || (digits.len() > 1 && digits.starts_with('0')) {
        // Codes like "007" or long runs: digit by digit.
        return digits
            .chars()
            .map(|digit| UNITS[digit.to_digit(10).unwrap_or(0) as usize])
            .collect::<Vec<_>>()
            .join(" ");
    }
    number_to_words(trimmed.parse().unwrap_or(0))
}

/// Append a word, keeping it apart from neighbouring letters and digits.
fn push_word(out: &mut String, word: &str, next: Option<char>) {
    if out.chars().last().is_some_and(|c| c.is_alphanumeric()) {
        out.push(' ');
    }
    out.push_str(word);
    if next.is_some_and(|c| c.is_alphanumeric()) {
        out.push(' ');
    }
}

/// Split long input at sentence ends so each piece stays well inside BERT's
/// 512 tokens. Short input is returned whole, exactly like the Python call.
pub fn split_long(text: &str, max_chars: usize) -> Vec<String> {
    if text.chars().count() <= max_chars {
        return vec![text.to_owned()];
    }
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut sentence = String::new();
    let flush_sentence = |sentence: &mut String, current: &mut String, pieces: &mut Vec<String>| {
        if sentence.trim().is_empty() {
            sentence.clear();
            return;
        }
        if !current.is_empty() && current.chars().count() + sentence.chars().count() > max_chars {
            pieces.push(std::mem::take(current).trim().to_owned());
        }
        current.push_str(sentence);
        sentence.clear();
        if current.chars().count() > max_chars {
            // A single overlong sentence: cut at the last space.
            while current.chars().count() > max_chars {
                let cut: String = current.chars().take(max_chars).collect();
                let at = cut.rfind(' ').filter(|at| *at > 0).unwrap_or(cut.len());
                pieces.push(current[..at].trim().to_owned());
                *current = current[at..].to_owned();
            }
        }
    };
    let chars: Vec<char> = text.chars().collect();
    for (index, c) in chars.iter().enumerate() {
        sentence.push(*c);
        let at_end = matches!(c, '.' | '!' | '?')
            && chars.get(index + 1).is_none_or(|next| next.is_whitespace());
        if at_end {
            flush_sentence(&mut sentence, &mut current, &mut pieces);
        }
    }
    flush_sentence(&mut sentence, &mut current, &mut pieces);
    if !current.trim().is_empty() {
        pieces.push(current.trim().to_owned());
    }
    pieces.retain(|piece| !piece.is_empty());
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_numbers() {
        assert_eq!(number_to_words(0), "ноль");
        assert_eq!(number_to_words(20), "двадцать");
        assert_eq!(number_to_words(21), "двадцать один");
        assert_eq!(number_to_words(115), "сто пятнадцать");
        assert_eq!(number_to_words(1000), "одна тысяча");
        assert_eq!(number_to_words(2024), "две тысячи двадцать четыре");
        assert_eq!(number_to_words(11_000), "одиннадцать тысяч");
        assert_eq!(number_to_words(1_500_000), "один миллион пятьсот тысяч");
        assert_eq!(number_to_words(3_000_000_001), "три миллиарда один");
    }

    #[test]
    fn plain_cyrillic_is_untouched() {
        for text in [
            "Привет, мир!",
            "Он сказал: \"Завтра будет дождь\" и ушёл.",
            "  Лишние   пробелы   внутри  ",
            "Мне нужен м+ука, а не мук+а.",
        ] {
            assert_eq!(normalize(text), text);
        }
    }

    #[test]
    fn makes_unsupported_text_safe() {
        assert_eq!(
            normalize("Таймер на 20 минут."),
            "Таймер на двадцать минут."
        );
        assert_eq!(normalize("Включи Spotify"), "Включи спотифи");
        assert_eq!(normalize("Ну… «да» — нет"), "Ну... \"да\" - нет");
        assert_eq!(normalize("2,5 км"), "два целых пять км");
        assert_eq!(normalize("50%"), "пятьдесят процентов");
        assert_eq!(normalize("код 007"), "код ноль ноль семь");
        assert_eq!(normalize("в 7ч"), "в семь ч");
        assert_eq!(normalize("a*b"), "а б");
        assert_eq!(
            normalize("Ветер 2 м/с, −2°"),
            "Ветер два метров в секунду, минус два градусов"
        );
        assert_eq!(
            normalize("751 мм рт. ст."),
            "семьсот пятьдесят один миллиметров ртутного столба"
        );
    }

    #[test]
    fn splits_only_long_text() {
        assert_eq!(split_long("Короткая фраза. Вторая.", 100).len(), 1);
        let long = "Первое предложение. Второе предложение! Третье?";
        let pieces = split_long(long, 25);
        assert_eq!(
            pieces,
            vec!["Первое предложение.", "Второе предложение!", "Третье?"]
        );
        let overlong = "слово ".repeat(20);
        assert!(
            split_long(&overlong, 30)
                .iter()
                .all(|piece| piece.chars().count() <= 30)
        );
    }
}
