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

use std::path::Path;

use crate::error::ChekovError;

/// Everything one `--codebase` run needs, sampled once before launch — the
/// worktree is gone by the time this returns.
pub struct Prepared {
    pub head: String,
    pub set_hash: String,
    pub tasks: Vec<CodebaseTask>,
    pub shortfall: Vec<String>,
    pub symbols: ladder::Symbols,
}

/// Assembled tasks for the picked spans, matched back to their file text.
fn assembled_tasks(files: &[(String, String)], picked: &[sample::Picked]) -> Vec<CodebaseTask> {
    let by_path: std::collections::HashMap<&str, &str> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    picked
        .iter()
        .filter_map(|p| {
            by_path
                .get(p.path.as_str())
                .map(|text| filter::assemble(&p.path, text, p))
        })
        .collect()
}

/// Gate, worktree, walk, mask, sample, assemble, symbol set — then the
/// worktree is removed. Everything the run needs is in memory, and the
/// user's checkout was never read directly.
pub fn prepare(repo: &Path, scratch_tree: &Path, tasks: u32) -> Result<Prepared, ChekovError> {
    use masker::MaskSource;
    tree::assert_clean(repo)?;
    let head = tree::head_sha(repo)?;
    let worktree = tree::Worktree::add(repo, scratch_tree)?;
    let files = tree::rust_sources(&worktree.path);
    let candidates: Vec<sample::FileCandidates> = files
        .iter()
        .map(|(path, text)| sample::FileCandidates {
            path: path.clone(),
            candidates: masker::RustBraceMasker.candidates(text),
        })
        .collect();
    let set = sample::sample(
        candidates,
        sample::quota(tasks),
        sample::seed_from_head(&head),
    );
    let symbols = ladder::repo_symbols(&files);
    worktree.remove()?;
    if set.picked.is_empty() {
        return Err(ChekovError::CodebaseNoTasks {
            path: repo.to_path_buf(),
            reason: format!("scanned {} files, 0 candidate spans", files.len()),
        });
    }
    Ok(Prepared {
        head,
        set_hash: sample::task_set_hash(&set),
        tasks: assembled_tasks(&files, &set.picked),
        shortfall: set.shortfall,
        symbols,
    })
}
