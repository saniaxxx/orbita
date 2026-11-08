//! AAPI framing and the subset of client/server messages the simulator uses.

use orbita_proto::n_alice::{
    EEventType, TAliceRequestProto, TEvent as AliceEvent,
    n_alice_api::{
        TAliceResponseProto, TClientMessage, TDirective, TEventHeader, TServerMessage,
        TStreamControl,
        t_client_message::{Message as ClientMessage, TEvent as ClientEvent, t_event::Event},
        t_directive::PayloadType as DirectivePayload,
        t_server_message::{Message as ServerMessage, TEvent as ServerEvent},
        t_stream_control::{Action as StreamAction, TActionClose, t_action_close::EReason},
    },
    t_alice_request_proto::{
        THeader, TRequest,
        t_request::{DeviceStateType, EnvironmentStateType},
    },
    t_event::PayloadType as EventPayload,
};
use prost::Message as ProstMessage;

const AAPI: &[u8; 4] = b"AAPI";

pub fn text_input(text: &str, dialog_id: &str) -> TClientMessage {
    let message_id = uuid::Uuid::new_v4().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();

    TClientMessage {
        message: Some(ClientMessage::Event(ClientEvent {
            header: Some(TEventHeader {
                message_id,
                ref_message_id: String::new(),
                ..Default::default()
            }),
            event: Some(Event::TextInput(TAliceRequestProto {
                header: Some(THeader {
                    request_id: Some(request_id),
                    sequence_number: Some(0),
                    dialog_id: Some(dialog_id.to_owned()),
                    ..Default::default()
                }),
                request: Some(TRequest {
                    event: Some(AliceEvent {
                        r#type: Some(EEventType::TextInput as i32),
                        text: Some(text.to_owned()),
                        ..Default::default()
                    }),
                    device_state_type: Some(DeviceStateType::DeviceStateRaw(Vec::new())),
                    environment_state_type: Some(EnvironmentStateType::EnvironmentStateRaw(
                        Vec::new(),
                    )),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        })),
    }
}

/// The station echoes a `@@mm_semantic_frame` server action back as a
/// request; quasar's "continue" handshake (stage 1 → stage 2 → next partial
/// response) depends on it. `None` for any other directive.
pub fn semantic_frame_callback(directive: &TDirective, dialog_id: &str) -> Option<TClientMessage> {
    if directive.name != "@@mm_semantic_frame" {
        return None;
    }
    let payload = match directive.payload_type.clone()? {
        DirectivePayload::Payload(payload) => EventPayload::Payload(payload),
        DirectivePayload::PayloadRaw(raw) => EventPayload::PayloadRaw(raw),
    };
    let mut message = text_input("", dialog_id);
    if let Some(ClientMessage::Event(ClientEvent {
        event: Some(Event::TextInput(request)),
        ..
    })) = message.message.as_mut()
        && let Some(event) = request
            .request
            .as_mut()
            .and_then(|request| request.event.as_mut())
    {
        event.r#type = Some(EEventType::ServerAction as i32);
        event.text = None;
        event.name = Some(directive.name.clone());
        event.payload_type = Some(payload);
    }
    Some(message)
}

pub fn voice_input(dialog_id: &str, stream_id: i64) -> TClientMessage {
    let message_id = uuid::Uuid::new_v4().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();

    TClientMessage {
        message: Some(ClientMessage::Event(ClientEvent {
            header: Some(TEventHeader {
                message_id,
                ref_message_id: String::new(),
                stream_id: Some(stream_id),
                ..Default::default()
            }),
            event: Some(Event::VoiceInput(TAliceRequestProto {
                header: Some(THeader {
                    request_id: Some(request_id),
                    sequence_number: Some(0),
                    dialog_id: Some(dialog_id.to_owned()),
                    ..Default::default()
                }),
                request: Some(TRequest {
                    event: Some(AliceEvent {
                        r#type: Some(EEventType::VoiceInput as i32),
                        ..Default::default()
                    }),
                    device_state_type: Some(DeviceStateType::DeviceStateRaw(Vec::new())),
                    environment_state_type: Some(EnvironmentStateType::EnvironmentStateRaw(
                        Vec::new(),
                    )),
                    voice_session: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        })),
    }
}

pub fn stream_close(stream_id: i64) -> TClientMessage {
    TClientMessage {
        message: Some(ClientMessage::StreamControl(TStreamControl {
            stream_id,
            message_id: uuid::Uuid::new_v4().to_string(),
            action: Some(StreamAction::Close(TActionClose {
                reason: EReason::StopRecording as i32,
            })),
        })),
    }
}

pub fn audio_frame(stream_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = stream_id.to_be_bytes().to_vec();
    frame.extend_from_slice(payload);
    frame
}

pub fn encode_client(message: &TClientMessage) -> Vec<u8> {
    let mut frame = AAPI.to_vec();
    frame.extend_from_slice(&message.encode_to_vec());
    frame
}

pub fn decode_server(frame: &[u8]) -> Result<TServerMessage, String> {
    let Some(payload) = frame.strip_prefix(AAPI) else {
        return Err("frame does not start with AAPI".to_owned());
    };
    TServerMessage::decode(payload).map_err(|error| error.to_string())
}

pub fn is_aapi(frame: &[u8]) -> bool {
    frame.starts_with(AAPI)
}

pub fn audio_stream_id(frame: &[u8]) -> Option<u32> {
    let bytes: [u8; 4] = frame.get(..4)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

pub fn response_text(response: &TAliceResponseProto) -> Option<String> {
    if let Some(voice) = &response.voice_response
        && let Some(speech) = &voice.output_speech
        && let Some(text) = &speech.text
        && !text.is_empty()
    {
        return Some(text.clone());
    }

    let response = response.response.as_ref()?;
    for card in &response.cards {
        if let Some(text) = &card.text
            && !text.is_empty()
        {
            return Some(text.clone());
        }
    }
    None
}

pub fn server_event(message: &TServerMessage) -> Option<&ServerEvent> {
    match message.message.as_ref()? {
        ServerMessage::Event(event) => Some(event),
        ServerMessage::StreamControl(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_frames_are_echoed_as_server_actions() {
        let directive = TDirective {
            name: "@@mm_semantic_frame".into(),
            r#type: "server_action".into(),
            payload_type: Some(DirectivePayload::Payload(Default::default())),
            ..Default::default()
        };
        let message = semantic_frame_callback(&directive, "dialog-1").expect("echo");
        let Some(ClientMessage::Event(ClientEvent {
            event: Some(Event::TextInput(request)),
            ..
        })) = message.message
        else {
            panic!("not a TextInput event");
        };
        assert_eq!(
            request.header.unwrap().dialog_id.as_deref(),
            Some("dialog-1")
        );
        let event = request.request.unwrap().event.unwrap();
        assert_eq!(event.r#type, Some(EEventType::ServerAction as i32));
        assert_eq!(event.name.as_deref(), Some("@@mm_semantic_frame"));
        assert!(matches!(event.payload_type, Some(EventPayload::Payload(_))));

        let other = TDirective {
            name: "tts_play_placeholder".into(),
            ..Default::default()
        };
        assert!(semantic_frame_callback(&other, "dialog-1").is_none());
    }
}
