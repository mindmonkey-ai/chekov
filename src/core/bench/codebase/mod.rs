//! `chekov capability bench --codebase` — the user's own Rust repository as
//! graded same-file infill tasks (spec §8, slice A).

pub mod filter;
pub mod ladder;
pub mod masker;
pub mod sample;
pub mod tree;

use serde::{Deserialize, Serialize};

/// Printed once per run: the masks come from a brace scanner, not a parser.
pub const MASK_LABEL: &str = "boundary-scanned (not AST)";

/// Which kind of span was masked (`RepoBench` taxonomy; cross-file is slice B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTier {
    InFile,
    FunctionBody,
}

impl TaskTier {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::InFile => "in_file",
            Self::FunctionBody => "function_body",
        }
    }
}

/// What the leakage filter removed from this task's context, per rule. Slice
/// A has no cross-file context, and says so rather than claiming a count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub doc_comment: u8,
    pub cross_file: String,
}

/// One assembled task: what the model sees, what was hidden, and the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodebaseTask {
    pub id: String,
    pub tier: TaskTier,
    pub file: String,
    pub line: usize,
    pub gold: String,
    pub prefix: String,
    pub suffix: String,
    pub excluded: Excluded,
}
