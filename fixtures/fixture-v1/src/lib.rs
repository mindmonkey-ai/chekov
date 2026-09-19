//! fixture-v1 — a small event-sourced ledger used to grade a candidate's
//! ability to integrate cross-file context, honour a stated invariant, and
//! choose the correct of two near-identical APIs.
//!
//! This is a content slice, not production chekov code. It is graded against
//! the manifest in `manifest.toml`. It is embedded into the chekov binary by
//! `build.rs` and run by `chekov capability bench --fixture --allow-exec`
//! (the capability-spec §9).

pub mod domain;
pub mod projection;
pub mod api;
pub mod store;
