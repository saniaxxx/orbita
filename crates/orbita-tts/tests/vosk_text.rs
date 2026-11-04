//! Text-side parity with the Python `vosk-tts` package (0.3.61).
//!
//! `fixtures/vosk_text.json` was produced by running the package's own
//! `BertWordPieceTokenizer`, `get_word_bert` row selection,
//! `Synth.g2p_multistream` and `g2p.convert` on the corpus below, with the
//! Vosk TTS ru 0.10 vocab, dictionary and config. Word rules are checked
//! always; the full pipeline needs the model files (`VOSK_MODEL_DIR`).

use std::path::Path;

use orbita_tts::vosk::{
    Lexicon, ModelFiles,
    g2p::{convert, multistream},
    tokenizer::word_rows,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
    convert: Vec<Word>,
}

#[derive(Deserialize)]
struct Case {
    text: String,
    tokens: Vec<String>,
    token_ids: Vec<i64>,
    selected: Vec<usize>,
    ids: Vec<[i64; 5]>,
    bert_index: Vec<usize>,
}

#[derive(Deserialize)]
struct Word {
    word: String,
    phonemes: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/vosk_text.json")).unwrap()
}

#[test]
fn word_rules_match_python() {
    for word in fixture().convert {
        assert_eq!(convert(&word.word), word.phonemes, "word {:?}", word.word);
    }
}

#[test]
fn full_text_pipeline_matches_python() {
    let Ok(dir) = std::env::var("VOSK_MODEL_DIR") else {
        eprintln!("VOSK_MODEL_DIR not set; skipping");
        return;
    };
    let lexicon = Lexicon::load(&ModelFiles::resolve(Path::new(&dir)).unwrap()).unwrap();
    for case in fixture().cases {
        // synth_audio: strip, '—' -> '-'
        let text = case.text.trim().replace('—', "-");
        let encoding = lexicon.tokenizer.encode(&text.replace('+', ""));
        assert_eq!(encoding.tokens, case.tokens, "tokens of {:?}", case.text);
        assert_eq!(encoding.ids, case.token_ids, "ids of {:?}", case.text);
        let rows = word_rows(&encoding.tokens);
        assert_eq!(rows, case.selected, "BERT rows of {:?}", case.text);

        let out = multistream(&text, &lexicon.dictionary, &lexicon.phoneme_ids, rows.len());
        assert!(out.dropped.is_empty(), "dropped {:?}", out.dropped);
        assert_eq!(out.ids, case.ids, "phoneme streams of {:?}", case.text);
        assert_eq!(
            out.bert_index, case.bert_index,
            "BERT index of {:?}",
            case.text
        );
    }
}
