//! Channel adapters.
//!
//! The agent core produces [`crate::action::Action`]s; an adapter knows how to
//! express them in its own protocol. Today only the station channel has
//! directives; Telegram and the web will add adapters behind the same trait.

pub mod station;

use crate::action::ChannelId;

pub trait Channel {
    fn id(&self) -> ChannelId;
}

impl Channel for station::StationSession {
    fn id(&self) -> ChannelId {
        ChannelId::Station
    }
}
