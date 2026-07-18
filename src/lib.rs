//! Shared, GUI-free ASense control core.
//!
//! The privileged `asensed` helper links this library without the optional
//! desktop feature, keeping GTK/WebKit and their transitive attack surface out
//! of the root process.  The unprivileged `asense` frontend reuses the exact
//! same protocol and validation types.

pub mod control;
pub mod daemon;
pub mod hardware;
pub mod lighting;
pub mod mutation_lock;
pub mod nvidia;
pub mod platform;
pub mod telemetry;
pub mod tuning;
