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
///
/// `cfg_test_lines` is what the `#[cfg(test)]` cutter took out of this task's
/// file before anything else read it. Rows written before the cutter existed
/// load as 0, which is what they were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub doc_comment: u8,
    pub cross_file: String,
    #[serde(default)]
    pub cfg_test_lines: usize,
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
    /// Lines the `#[cfg(test)]` cutter removed across the whole walk, and how
    /// many files gave some up — printed, never silently absorbed.
    pub cfg_test_lines: usize,
    pub cfg_test_files: usize,
}

/// Every walked file with its `#[cfg(test)]` items already cut, keyed back to
/// what each cut cost so a task's row can carry its own file's number.
struct Elisions {
    files: Vec<(String, String)>,
    per_file: std::collections::HashMap<String, usize>,
}

impl Elisions {
    fn lines(&self) -> usize {
        self.per_file.values().sum()
    }

    fn files_cut(&self) -> usize {
        self.per_file.values().filter(|n| **n > 0).count()
    }
}

/// The cut applied to every file before masking, sampling, or the symbol set.
///
/// Idiomatic Rust keeps unit tests inline; excluding those files outright
/// would leave a real repository with almost nothing to sample from, and
/// leaving them in would offer the model its own test module as an answer.
fn elide_tests(files: Vec<(String, String)>) -> Elisions {
    let mut per_file = std::collections::HashMap::new();
    let files = files
        .into_iter()
        .map(|(path, text)| {
            let cut = filter::elide_cfg_test(&text);
            per_file.insert(path.clone(), cut.lines_removed);
            (path, cut.text)
        })
        .collect();
    Elisions { files, per_file }
}

/// The short HEAD every codebase-mode name is keyed by.
#[must_use]
pub fn head12(head: &str) -> &str {
    &head[..12.min(head.len())]
}

/// Assembled tasks for the picked spans, matched back to their file's elided
/// text and to that file's own elision count.
fn assembled_tasks(elided: &Elisions, picked: &[sample::Picked]) -> Vec<CodebaseTask> {
    let by_path: std::collections::HashMap<&str, &str> = elided
        .files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    picked
        .iter()
        .filter_map(|p| {
            let lines = elided.per_file.get(&p.path).copied().unwrap_or(0);
            by_path
                .get(p.path.as_str())
                .map(|text| filter::assemble(text, p, lines))
        })
        .collect()
}

/// Gate, worktree, walk, mask, sample, assemble, symbol set — then the
/// worktree is removed. Everything the run needs is in memory, and the
/// user's checkout was never read directly.
///
/// The scratch tree is `<scratch_root>/codebase-tree-<head12>`: keyed by the
/// HEAD it checks out, so two runs of different commits never share one, and
/// derived here rather than by the caller, which does not know the HEAD yet.
pub fn prepare(repo: &Path, scratch_root: &Path, tasks: u32) -> Result<Prepared, ChekovError> {
    tree::assert_clean(repo)?;
    let head = tree::head_sha(repo)?;
    let scratch_tree = scratch_root.join(format!("codebase-tree-{}", head12(&head)));
    let worktree = tree::Worktree::add(repo, &scratch_tree)?;
    let sources = tree::rust_sources(&worktree.path);
    let elided = elide_tests(sources.files);
    let set = sample::sample(
        file_candidates(&elided.files),
        sample::quota(tasks),
        sample::seed_from_head(&head),
    );
    let symbols = ladder::repo_symbols(&elided.files);
    worktree.remove()?;
    if set.picked.is_empty() {
        return Err(ChekovError::CodebaseNoTasks {
            path: repo.to_path_buf(),
            reason: format!(
                "scanned {} files, {} eligible, 0 candidate spans",
                sources.scanned,
                elided.files.len()
            ),
        });
    }
    Ok(Prepared {
        head,
        set_hash: sample::task_set_hash(&set),
        tasks: assembled_tasks(&elided, &set.picked),
        shortfall: with_oversized(set.shortfall, sources.oversized),
        symbols,
        cfg_test_lines: elided.lines(),
        cfg_test_files: elided.files_cut(),
    })
}

/// Every file's candidate spans, in the shape the sampler strata want.
fn file_candidates(files: &[(String, String)]) -> Vec<sample::FileCandidates> {
    use masker::MaskSource;
    files
        .iter()
        .map(|(path, text)| sample::FileCandidates {
            path: path.clone(),
            candidates: masker::RustBraceMasker.candidates(text),
        })
        .collect()
}

/// The sampler's shortfall, plus the files the walk never offered it — a task
/// set drawn from less than the repository says so.
fn with_oversized(mut shortfall: Vec<String>, oversized: usize) -> Vec<String> {
    if oversized > 0 {
        shortfall.push(format!("{oversized} files over 200 KiB skipped"));
    }
    shortfall
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::prepare;

    /// A production function with a real body, and the inline test module a
    /// Rust file of this shape always has.
    fn source(name: &str) -> String {
        format!(
            "pub fn {name}(a: i32) -> i32 {{\n    let b = a + 1;\n    let c = b * 2;\n    c\n}}\n\n\
             #[cfg(test)]\nmod tests {{\n    #[test]\n    fn t() {{\n        \
             assert_eq!(super::{name}(1), 4);\n    }}\n}}\n"
        )
    }

    fn git(repo: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    }

    fn repo_with_inline_tests() -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-codebase-prepare")
            .join("inline");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        for name in ["one", "two", "three"] {
            std::fs::write(dir.join(format!("src/{name}.rs")), source(name)).expect("write");
        }
        let author = ["-c", "user.email=t@t", "-c", "user.name=t"];
        git(&dir, &["init", "-q"]);
        git(&dir, &[&author[..], &["add", "."]].concat());
        git(
            &dir,
            &[&author[..], &["commit", "-q", "-m", "init"]].concat(),
        );
        dir
    }

    /// Three files that would all have been excluded under the old rule now
    /// all produce tasks, and no task can see a test the cutter removed.
    #[test]
    fn prepare_keeps_files_with_inline_tests_and_cuts_the_tests_out_of_them() {
        let dir = repo_with_inline_tests();
        let scratch = std::env::temp_dir()
            .join("chekov-test-codebase-prepare")
            .join("scratch");
        let prepared = prepare(&dir, &scratch, 6).expect("prepare");
        let files: std::collections::BTreeSet<&str> =
            prepared.tasks.iter().map(|t| t.file.as_str()).collect();
        assert_eq!(files.len(), 3, "{files:?}");
        assert_eq!(prepared.cfg_test_files, 3);
        assert!(prepared.cfg_test_lines > 0, "{}", prepared.cfg_test_lines);
        for task in &prepared.tasks {
            assert!(task.excluded.cfg_test_lines > 0, "{}", task.file);
            for part in [&task.prefix, &task.gold, &task.suffix] {
                assert!(!part.contains("#[cfg(test)]"), "{part:?}");
                assert!(!part.contains("mod tests"), "{part:?}");
            }
        }
    }
}
