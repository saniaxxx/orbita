//! Home domain and Wiren Board MQTT transport.
//!
//! `Home` owns the device model, validates actions and publishes commands. It
//! deliberately does not decide who may perform an action: that is the agent's
//! policy layer, which passes a `Grant` for sensitive and dangerous actions.

pub mod config;
pub mod home;
pub mod model;
pub mod mqtt;

pub use config::{HomeConfig, MqttConfig};
pub use home::{Clock, Home, Publisher, SystemClock, action, subscriptions_for};
pub use model::{
    Action, ActionRequest, Capability, CapabilityView, DeviceConfig, DeviceView, Grant, HomeError,
    Published, Risk,
};
