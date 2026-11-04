//! Local Glagol message shapes.
//!
//! The local station protocol is JSON over WebSocket. Every request carries
//! `conversationToken`, `id`, `payload` and `sentTime`; responses echo the id
//! as `requestId`. Playback of arbitrary URLs goes through
//! `externalCommandBypass`, whose `data` field is a small protobuf with two
//! string fields: the command name and its JSON payload.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// A request as the station expects it.
pub fn request_json(
    conversation_token: &str,
    id: &str,
    payload: Value,
    sent_time_ms: u64,
) -> Value {
    json!({
        "conversationToken": conversation_token,
        "id": id,
        "payload": payload,
        "sentTime": sent_time_ms,
    })
}

/// `{"command": "..."}` payloads shared by most local commands.
pub fn command(name: &str) -> Value {
    json!({"command": name})
}

pub fn set_volume(volume: f32) -> Value {
    json!({"command": "setVolume", "volume": volume})
}

pub fn rewind(position_seconds: f64) -> Value {
    json!({"command": "rewind", "position": position_seconds})
}

pub fn play_music(id: &str, media_type: &str) -> Value {
    json!({"command": "playMusic", "id": id, "type": media_type})
}

/// Metadata attached to a local `audio_play` command. Music Assistant uses
/// `id` to keep the speaker state in sync with its own queue.
#[derive(Debug, Clone, Default)]
pub struct AudioMetadata {
    pub id: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub image_url: Option<String>,
}

/// Build the `audio_play` command for an HTTP stream.
///
/// The station detects the codec itself; `format` only has to be a valid enum
/// (`HLS` for playlists, `MP3` otherwise). A wrong value makes the station drop
/// the directive without even requesting the URL.
pub fn audio_play_command(url: &str, format: &str, metadata: Option<&AudioMetadata>) -> Value {
    let hls = format.eq_ignore_ascii_case("hls") || format.eq_ignore_ascii_case("m3u8");
    let mut stream = Map::new();
    stream.insert("url".to_owned(), Value::String(url.to_owned()));
    stream.insert(
        "format".to_owned(),
        Value::String(if hls { "HLS" } else { "MP3" }.to_owned()),
    );
    stream.insert(
        "type".to_owned(),
        Value::String(if hls { "FmRadio" } else { "Track" }.to_owned()),
    );

    let mut meta = Map::new();
    if let Some(metadata) = metadata {
        if let Some(id) = &metadata.id {
            stream.insert("id".to_owned(), Value::String(id.clone()));
        }
        if let Some(title) = &metadata.title {
            meta.insert("title".to_owned(), Value::String(title.clone()));
        }
        if let Some(artist) = &metadata.artist {
            meta.insert("subtitle".to_owned(), Value::String(artist.clone()));
        }
        if let Some(image) = &metadata.image_url {
            meta.insert("art_image_url".to_owned(), Value::String(image.clone()));
        }
    }

    external_command(
        "audio_play",
        &json!({"stream": Value::Object(stream), "metadata": Value::Object(meta)}),
    )
}

/// Wrap a command into `externalCommandBypass`.
pub fn external_command(name: &str, payload: &Value) -> Value {
    let data = encode_proto_strings(&[(1, name), (2, &payload.to_string())]);
    json!({
        "command": "externalCommandBypass",
        "data": STANDARD.encode(data),
    })
}

/// Encode two length-delimited protobuf string fields (`tag`, value).
fn encode_proto_strings(fields: &[(u32, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (tag, value) in fields {
        write_varint(&mut out, u64::from(tag << 3 | 2));
        write_varint(&mut out, value.len() as u64);
        out.extend_from_slice(value.as_bytes());
    }
    out
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(0x80 | (value as u8 & 0x7f));
        value >>= 7;
    }
    out.push(value as u8);
}

/// Minimal view of a station response. The rest of the message is kept as
/// JSON because it differs between firmware versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(rename = "requestId")]
    pub request_id: Option<String>,
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

impl Response {
    pub fn from_value(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }

    pub fn is_ok(&self) -> bool {
        self.status.as_deref() == Some("ok")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_matches_protobuf_encoding() {
        assert_eq!(write_varint_vec(0), [0x00]);
        assert_eq!(write_varint_vec(127), [0x7f]);
        assert_eq!(write_varint_vec(128), [0x80, 0x01]);
        assert_eq!(write_varint_vec(300), [0xac, 0x02]);
    }

    #[test]
    fn external_command_encodes_protobuf_fields() {
        let value = external_command("audio_play", &json!({"x": 1}));
        assert_eq!(value["command"], "externalCommandBypass");
        let data = STANDARD
            .decode(value["data"].as_str().expect("data is a string"))
            .expect("base64");
        assert_eq!(
            data,
            vec![
                0x0a, 0x0a, b'a', b'u', b'd', b'i', b'o', b'_', b'p', b'l', b'a', b'y', 0x12, 0x07,
                b'{', b'"', b'x', b'"', b':', b'1', b'}',
            ]
        );
    }

    #[test]
    fn audio_play_payload_has_stream_and_metadata() {
        let value = audio_play_command(
            "http://192.168.250.103:8097/a.mp3",
            "mp3",
            Some(&AudioMetadata {
                id: Some("http://192.168.250.103:8097/a.mp3".to_owned()),
                title: Some("Track".to_owned()),
                artist: Some("Artist".to_owned()),
                image_url: None,
            }),
        );
        assert_eq!(value["command"], "externalCommandBypass");
        let data = STANDARD
            .decode(value["data"].as_str().expect("data is a string"))
            .expect("base64");
        let text = String::from_utf8_lossy(&data);
        assert!(text.contains("audio_play"));
        assert!(text.contains("\"format\":\"MP3\""));
        assert!(text.contains("\"type\":\"Track\""));
        assert!(!text.contains("\"art_image_url\""));
    }

    #[test]
    fn hls_uses_radio_ui() {
        let value = audio_play_command("http://host/live.m3u8", "m3u8", None);
        let data = STANDARD
            .decode(value["data"].as_str().expect("data is a string"))
            .expect("base64");
        let text = String::from_utf8_lossy(&data);
        assert!(text.contains("\"format\":\"HLS\""));
        assert!(text.contains("\"type\":\"FmRadio\""));
    }

    #[test]
    fn request_uses_camel_case() {
        let value = request_json("token", "id-1", command("ping"), 123);
        assert_eq!(value["conversationToken"], "token");
        assert_eq!(value["id"], "id-1");
        assert_eq!(value["sentTime"], 123);
        assert_eq!(value["payload"]["command"], "ping");
    }

    #[test]
    fn response_parses_request_id() {
        let response = Response::from_value(&json!({
            "requestId": "id-1",
            "status": "ok",
            "state": {"playing": true}
        }))
        .expect("response");
        assert_eq!(response.request_id.as_deref(), Some("id-1"));
        assert!(response.is_ok());
    }

    fn write_varint_vec(value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        write_varint(&mut out, value);
        out
    }
}
