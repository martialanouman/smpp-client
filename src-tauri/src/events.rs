//! Events emitted towards the frontend.
//!
//! The reverse direction of [`crate::commands`]: the backend pushes here the
//! state changes the interface cannot derive on its own — session
//! transitions, campaign progress, metrics.
//!
//! Conventions (guide §9.3):
//!
//! - `domain:action` naming — `sessions:state`, `message:update`,
//!   `metrics:tick`;
//! - high-frequency events are **throttled on the Rust side** (1 to 4 Hz for
//!   `metrics:tick`). Emitting at the real PDU rate would saturate the IPC
//!   bridge and make the WebView unusable during a campaign.
//!
//! Empty at milestone 000.
