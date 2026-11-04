//! Live pipeline dashboard: trace events in, SSE and a static UI out.

pub mod demo;
pub mod http;
pub mod state;

pub use http::build_router;
pub use state::{DashboardState, Metrics, TraceEvent};
