//! AAPI framing and the subset of client/server messages the simulator uses.

use orbita_proto::n_alice::{
    EEventType, TAliceRequestProto, TEvent as AliceEvent,
    n_alice_api::{
        TAliceResponseProto, TClientMessage, TEventHeader, TServerMessage, TStreamControl,
        t_client_message::{Message as ClientMessage, TEvent as ClientEvent, t_event::Event},
        t_server_message::{Message as ServerMessage, TEvent as ServerEvent},
        t_stream_control::{Action as StreamAction, TActionClose, t_action_close::EReason},
    },
    t_alice_request_proto::{
        THeader, TRequest,
        t_request::{DeviceStateType, EnvironmentStateType},
    },
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
