//! orbita-agent: voice dialogue, persistent sessions and reminders.

pub mod action;
pub mod agent;
pub mod channel;
pub mod config;
pub mod db;
pub mod fastpath;
pub mod llm;
pub mod notify;
pub mod reminder;
pub mod reminder_scheduler;
mod russian;
pub mod server;

pub use action::{Action, ChannelId, Outcome, SessionContext, SessionKey};
pub use agent::Agent;
pub use channel::station::{Directive, StationSession};
pub use config::Config;
pub use db::Store;
pub use llm::{LlmClient, OpenAiClient};
pub use notify::{Notifier, QuasarNotifier};
pub use reminder::Reminder;
pub use server::{AppState, build_router};
