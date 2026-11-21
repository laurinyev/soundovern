//! Shared audio-path logic: Opus codec wrappers, adaptive jitter buffer, drift control.
pub mod codec;
pub mod drift;
pub mod jitter;
pub mod client;
