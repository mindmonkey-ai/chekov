//! fixture-v1 — a small event-sourced ledger used to grade a candidate's
//! ability to integrate cross-file context, honour a stated invariant, and
//! choose the correct of two near-identical APIs.
//!
//! This is a content slice, not production chekov code. It is graded against
//! the manifest in `manifest.toml`. It is NOT wired into src/ (see
//! `AGENTS.md` scope discipline and the capability-spec §9).

pub mod domain;
pub mod projection;
pub mod api;
pub mod store;
