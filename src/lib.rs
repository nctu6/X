//! On-demand X feed.
//!
//! Posts live in JSONL files. A tab view asks the X API for new posts only
//! when that tab is opened, and at most once per six-hour slot. Handlers
//! serve the saved posts from memory.

#![forbid(unsafe_code)]

pub mod cli;
pub mod config;
pub mod http;
pub mod images;
pub mod model;
pub mod render;
pub mod slot;
pub mod store;
pub mod xapi;
