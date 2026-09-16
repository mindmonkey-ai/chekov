//! fixture-v1 — a small event-sourced ledger used to grade a candidate's
//! ability to integrate cross-file context, honour a stated invariant, and
//! choose the correct of two near-identical APIs.
//!
//! This is a CONTENT SLICE, not production chekov code. It is graded against
//! the manifest in `manifest.toml` and the held-out assertions in `hidden/`.
//! It is NOT wired into src/ (see `AGENTS.md` scope discipline and the
//! capability-spec §9). The compile/test tiers grade the masked bodies the
//! candidate writes; the compile gate and test gate are always available here
//! because chekov's own toolchain ships with the language.

pub mod domain;
pub mod projection;
pub mod api;
pub mod store;
