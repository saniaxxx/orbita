//! Minimal subset of the Android TV Remote protocol v2 messages.
//!
//! Field numbers and types match `remotemessage.proto` from
//! `tronikos/androidtvremote2` (Apache-2.0). Unknown fields are ignored, and
//! enums are kept as integers so a new firmware cannot break decoding.

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteMessage {
    #[prost(message, optional, tag = "1")]
    pub remote_configure: Option<RemoteConfigure>,
    #[prost(message, optional, tag = "2")]
    pub remote_set_active: Option<RemoteSetActive>,
    #[prost(message, optional, tag = "3")]
    pub remote_error: Option<RemoteError>,
    #[prost(message, optional, tag = "8")]
    pub remote_ping_request: Option<RemotePingRequest>,
    #[prost(message, optional, tag = "9")]
    pub remote_ping_response: Option<RemotePingResponse>,
    #[prost(message, optional, tag = "10")]
    pub remote_key_inject: Option<RemoteKeyInject>,
    #[prost(message, optional, tag = "20")]
    pub remote_ime_key_inject: Option<RemoteImeKeyInject>,
    #[prost(message, optional, tag = "40")]
    pub remote_start: Option<RemoteStart>,
    #[prost(message, optional, tag = "50")]
    pub remote_set_volume_level: Option<RemoteSetVolumeLevel>,
    #[prost(message, optional, tag = "90")]
    pub remote_app_link_launch_request: Option<RemoteAppLinkLaunchRequest>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteConfigure {
    #[prost(int32, tag = "1")]
    pub code1: i32,
    #[prost(message, optional, tag = "2")]
    pub device_info: Option<RemoteDeviceInfo>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteDeviceInfo {
    #[prost(string, tag = "1")]
    pub model: String,
    #[prost(string, tag = "2")]
    pub vendor: String,
    #[prost(int32, tag = "3")]
    pub unknown1: i32,
    #[prost(string, tag = "4")]
    pub unknown2: String,
    #[prost(string, tag = "5")]
    pub package_name: String,
    #[prost(string, tag = "6")]
    pub app_version: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteSetActive {
    #[prost(int32, tag = "1")]
    pub active: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteError {
    #[prost(bool, tag = "1")]
    pub value: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemotePingRequest {
    #[prost(int32, tag = "1")]
    pub val1: i32,
    #[prost(int32, tag = "2")]
    pub val2: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemotePingResponse {
    #[prost(int32, tag = "1")]
    pub val1: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteKeyInject {
    #[prost(int32, tag = "1")]
    pub key_code: i32,
    #[prost(int32, tag = "2")]
    pub direction: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteStart {
    #[prost(bool, tag = "1")]
    pub started: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteSetVolumeLevel {
    #[prost(uint32, tag = "1")]
    pub unknown1: u32,
    #[prost(uint32, tag = "2")]
    pub unknown2: u32,
    #[prost(string, tag = "3")]
    pub player_model: String,
    #[prost(uint32, tag = "4")]
    pub unknown4: u32,
    #[prost(uint32, tag = "5")]
    pub unknown5: u32,
    #[prost(uint32, tag = "6")]
    pub volume_max: u32,
    #[prost(uint32, tag = "7")]
    pub volume_level: u32,
    #[prost(bool, tag = "8")]
    pub volume_muted: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteAppLinkLaunchRequest {
    #[prost(string, tag = "1")]
    pub app_link: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteImeKeyInject {
    #[prost(message, optional, tag = "1")]
    pub app_info: Option<RemoteAppInfo>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoteAppInfo {
    #[prost(int32, tag = "1")]
    pub counter: i32,
    #[prost(int32, tag = "2")]
    pub int2: i32,
    #[prost(int32, tag = "3")]
    pub int3: i32,
    #[prost(string, tag = "4")]
    pub int4: String,
    #[prost(int32, tag = "7")]
    pub int7: i32,
    #[prost(int32, tag = "8")]
    pub int8: i32,
    #[prost(string, tag = "10")]
    pub label: String,
    #[prost(string, tag = "12")]
    pub app_package: String,
    #[prost(int32, tag = "13")]
    pub int13: i32,
}

/// Feature bits exchanged in `RemoteConfigure` / `RemoteSetActive`.
pub const FEATURE_PING: i32 = 1;
pub const FEATURE_KEY: i32 = 2;
pub const FEATURE_IME: i32 = 4;
pub const FEATURE_POWER: i32 = 32;
pub const FEATURE_VOLUME: i32 = 64;
pub const FEATURE_APP_LINK: i32 = 512;

/// PING | KEY | IME | POWER | VOLUME | APP_LINK
pub const REQUESTED_FEATURES: i32 =
    FEATURE_PING | FEATURE_KEY | FEATURE_IME | FEATURE_POWER | FEATURE_VOLUME | FEATURE_APP_LINK;

/// `RemoteDirection.SHORT`
pub const DIRECTION_SHORT: i32 = 3;

pub const KEYCODE_HOME: i32 = 3;
pub const KEYCODE_BACK: i32 = 4;
pub const KEYCODE_DPAD_UP: i32 = 19;
pub const KEYCODE_DPAD_DOWN: i32 = 20;
pub const KEYCODE_DPAD_LEFT: i32 = 21;
pub const KEYCODE_DPAD_RIGHT: i32 = 22;
pub const KEYCODE_DPAD_CENTER: i32 = 23;
pub const KEYCODE_VOLUME_UP: i32 = 24;
pub const KEYCODE_VOLUME_DOWN: i32 = 25;
pub const KEYCODE_POWER: i32 = 26;
pub const KEYCODE_MEDIA_PLAY_PAUSE: i32 = 85;
pub const KEYCODE_MEDIA_NEXT: i32 = 87;
pub const KEYCODE_MEDIA_PREVIOUS: i32 = 88;
pub const KEYCODE_MEDIA_STOP: i32 = 86;
pub const KEYCODE_MUTE: i32 = 91;
pub const KEYCODE_TV: i32 = 170;
pub const KEYCODE_TV_INPUT: i32 = 178;
pub const KEYCODE_TV_INPUT_HDMI_1: i32 = 243;
pub const KEYCODE_TV_INPUT_HDMI_2: i32 = 244;
pub const KEYCODE_TV_INPUT_HDMI_3: i32 = 245;
pub const KEYCODE_TV_INPUT_HDMI_4: i32 = 246;

/// Map a human key name to the protocol key code.
///
/// `POWER` is deliberately absent: power must go through the explicit state
/// check in `tv_power`, never through a blind toggle.
pub fn key_code(name: &str) -> Option<i32> {
    let name = name
        .trim()
        .trim_start_matches("KEYCODE_")
        .to_ascii_uppercase();
    Some(match name.as_str() {
        "HOME" => KEYCODE_HOME,
        "BACK" => KEYCODE_BACK,
        "UP" | "DPAD_UP" => KEYCODE_DPAD_UP,
        "DOWN" | "DPAD_DOWN" => KEYCODE_DPAD_DOWN,
        "LEFT" | "DPAD_LEFT" => KEYCODE_DPAD_LEFT,
        "RIGHT" | "DPAD_RIGHT" => KEYCODE_DPAD_RIGHT,
        "OK" | "ENTER" | "SELECT" | "DPAD_CENTER" => KEYCODE_DPAD_CENTER,
        "VOLUME_UP" => KEYCODE_VOLUME_UP,
        "VOLUME_DOWN" => KEYCODE_VOLUME_DOWN,
        "MUTE" => KEYCODE_MUTE,
        "PLAY_PAUSE" | "MEDIA_PLAY_PAUSE" => KEYCODE_MEDIA_PLAY_PAUSE,
        "NEXT" | "MEDIA_NEXT" => KEYCODE_MEDIA_NEXT,
        "PREVIOUS" | "MEDIA_PREVIOUS" => KEYCODE_MEDIA_PREVIOUS,
        "STOP" | "MEDIA_STOP" => KEYCODE_MEDIA_STOP,
        "TV" => KEYCODE_TV,
        "INPUT" | "TV_INPUT" => KEYCODE_TV_INPUT,
        "HDMI1" | "HDMI_1" | "TV_INPUT_HDMI_1" => KEYCODE_TV_INPUT_HDMI_1,
        "HDMI2" | "HDMI_2" | "TV_INPUT_HDMI_2" => KEYCODE_TV_INPUT_HDMI_2,
        "HDMI3" | "HDMI_3" | "TV_INPUT_HDMI_3" => KEYCODE_TV_INPUT_HDMI_3,
        "HDMI4" | "HDMI_4" | "TV_INPUT_HDMI_4" => KEYCODE_TV_INPUT_HDMI_4,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn round_trips_a_key_message() {
        let message = RemoteMessage {
            remote_key_inject: Some(RemoteKeyInject {
                key_code: KEYCODE_HOME,
                direction: DIRECTION_SHORT,
            }),
            ..Default::default()
        };
        let bytes = message.encode_length_delimited_to_vec();
        let decoded = RemoteMessage::decode_length_delimited(bytes.as_slice()).unwrap();
        assert_eq!(decoded.remote_key_inject.unwrap().key_code, KEYCODE_HOME);
    }

    #[test]
    fn maps_common_key_names() {
        assert_eq!(key_code("HOME"), Some(KEYCODE_HOME));
        assert_eq!(key_code("KEYCODE_DPAD_UP"), Some(KEYCODE_DPAD_UP));
        assert_eq!(key_code("ok"), Some(KEYCODE_DPAD_CENTER));
        assert_eq!(key_code("hdmi2"), Some(KEYCODE_TV_INPUT_HDMI_2));
        assert_eq!(key_code("power"), None);
    }
}
