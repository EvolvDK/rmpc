// src/youtube/integration/mod.rs

pub mod event_adapter;
pub mod migration;

pub use event_adapter::CrossbeamEventEmitter;
