//! Local Glagol spike: discover a station, obtain a token from our quasar and
//! talk to the station's local WebSocket.
//!
//! This is a diagnostic tool, not a production service. It intentionally
//! accepts the station's self-signed certificate and trusts the LAN.

pub mod client;
pub mod discovery;
pub mod protocol;

pub use client::{GlagolClient, fetch_token};
pub use discovery::{Station, discover};
pub use protocol::{
    AudioMetadata, audio_play_command, command, play_music, request_json, rewind, set_volume,
};
