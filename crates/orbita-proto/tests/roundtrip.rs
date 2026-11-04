//! The generated AAPI types must survive a prost encode/decode cycle.

use orbita_proto::n_alice::n_alice_api::{
    TEventHeader, TServerMessage, TTtsSpeak,
    t_server_message::{Message, TEvent, t_event::Event},
};
use prost::Message as ProstMessage;

#[test]
fn roundtrips_a_server_message_with_tts_speak() {
    let message = TServerMessage {
        timings: None,
        streaming: None,
        message_group: None,
        message: Some(Message::Event(TEvent {
            header: Some(TEventHeader {
                message_id: "message-1".to_owned(),
                ref_message_id: String::new(),
                sub_request_ref: None,
                stream_id: Some(7),
                ack: None,
                rt_log_id: None,
                retry_number: None,
                ..Default::default()
            }),
            event: Some(Event::TtsSpeak(TTtsSpeak {
                format: "audio/opus".to_owned(),
                lazy_tts_streaming: false,
                sound_chunk_type: 0,
                sub_stream_id: None,
            })),
        })),
    };

    let encoded = message.encode_to_vec();
    let decoded = TServerMessage::decode(encoded.as_slice()).expect("decode");
    assert_eq!(message, decoded);

    // Field 1 of TEvent is the header, field 14 is TtsSpeak; both must be on
    // the wire, not dropped as unknown fields.
    assert!(encoded.windows(2).any(|pair| pair == [0x0A, 0x09]));
}
