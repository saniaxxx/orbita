//! prost bindings for the vendored Yandex AAPI schemas (`crates/orbita-proto/protos`).
//!
//! Generated at build time from the vendored Yandex AAPI schemas. Lints are
//! allowed on the generated module: prost emits large enums and deprecated
//! fields by design, and neither is actionable here.

#[allow(clippy::all, clippy::pedantic, deprecated, unused)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/protos.rs"));
}

pub use generated::*;
