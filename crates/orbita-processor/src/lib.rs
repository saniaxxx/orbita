//! Rust port of the upstream dialog processor.

pub mod config;
pub mod functions;
pub mod llm;
pub mod llm_client;
pub mod processor;
pub mod prompt;
pub mod protocol;
pub mod server;

pub use config::Config;
pub use functions::{
    DirectiveFunctionServer, FunctionServer, McpFunctionServer, RemoteFunctionServer,
    RemoteStateServer, StateServer, SystemStateServer,
};
pub use llm_client::{LlmClient, OpenAiClient};
pub use processor::Processor;
pub use prompt::PromptGenerator;
pub use server::build_router;
