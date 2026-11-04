//! Shared configuration and logging for orbita services.

use std::str::FromStr;

use tracing_subscriber::EnvFilter;

/// Initialise stderr logging. `RUST_LOG` wins, otherwise `info`.
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Read a string environment variable or fall back to `default`.
pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

/// Read and parse an environment variable, using `default` when it is unset.
///
/// The error names the variable so a bad `.env` fails at startup, not on the
/// first request.
pub fn env_parse<T>(key: &str, default: T) -> Result<T, String>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(raw) => raw
            .trim()
            .parse::<T>()
            .map_err(|error| format!("{key}: {error}")),
        Err(_) => Ok(default),
    }
}

/// Read a boolean the way the Python services do: `1/true/yes/on` and
/// `0/false/no/off`.
pub fn env_bool(key: &str, default: bool) -> Result<bool, String> {
    match std::env::var(key) {
        Ok(raw) => match raw.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" | "" => Ok(false),
            other => Err(format!("{key}: expected a boolean, got {other}")),
        },
        Err(_) => Ok(default),
    }
}
