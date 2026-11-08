//! orbita-station-adapter: the Yandex station as one interface of the agent.
//!
//! quasar → `/process` → contract [`orbita_contracts::InboundEvent`] →
//! deterministic intents or the core (OpenClaw) → device actions →
//! station directives. AAPI/quasar vocabulary lives only in [`station`] and
//! [`server`]; the core sees contracts.

pub mod core;
pub mod schedule;
pub mod server;
pub mod station;

pub use crate::core::{Core, CoreTurn, OpenClawConfig, OpenClawCore};
pub use server::{AppState, Identity, build_router, handle_turn};
pub use station::{Directive, StationFunctions};
