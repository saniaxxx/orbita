//! Golden test against the Python implementation.
//!
//! `text_parity.json` was produced by `services/tts-gpu/tests/text_parity.py`
//! from the same corpus; that Python implementation already matches the
//! upstream JS service byte for byte (see its own parity test).

use orbita_tts::text::{chunk_text, preprocess_text, text_ids_and_length};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Case {
    pre: String,
    chunks: Vec<String>,
    ids: Vec<u32>,
    len: usize,
}

#[test]
fn matches_the_python_text_pipeline() {
    let corpus: Vec<String> =
        serde_json::from_str(include_str!("fixtures/corpus.json")).expect("corpus fixture");
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("fixtures/text_parity.json")).expect("parity fixture");

    assert_eq!(corpus.len(), cases.len());
    assert_eq!(cases.len(), 26);

    for (index, (text, case)) in corpus.iter().zip(&cases).enumerate() {
        assert_eq!(
            preprocess_text(text, "ru").expect("valid language"),
            case.pre,
            "preprocess case {index}: {text:?}"
        );

        let (ids, length) = text_ids_and_length(&case.pre);
        assert_eq!(ids, case.ids, "ids case {index}: {text:?}");
        assert_eq!(length, case.len, "utf16 length case {index}: {text:?}");

        assert_eq!(
            chunk_text(text, 300),
            case.chunks,
            "chunks case {index}: {text:?}"
        );
    }
}
