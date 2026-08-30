//! The repository side of codebase mode: the clean-tree gate, HEAD, a
//! detached worktree to read from, and the Rust file walk with the leakage
//! filter's test-FILE rule applied at the source.
//!
//! A file with inline `#[cfg(test)]` items is not a test file and is handed
//! back whole; `codebase::prepare` cuts those items out. The walk knows file
//! names and file sizes, not Rust.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::ChekovError;

const MAX_FILE_BYTES: u64 = 200 * 1024;

fn git(repo: &Path, args: &[&str], step: &str) -> Result<String, ChekovError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| ChekovError::CodebaseWorktreeFailed {
            step: step.to_owned(),
            reason: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(ChekovError::CodebaseWorktreeFailed {
            step: step.to_owned(),
            reason: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `git status --porcelain` must be empty — untracked files included.
///
/// This is the first git command `--codebase` runs, so it is also where a
/// path that is not a repository at all surfaces: the step carries the
/// question the git error alone would not answer.
pub fn assert_clean(repo: &Path) -> Result<(), ChekovError> {
    let step = format!("git status (is {} a git repository?)", repo.display());
    let status = git(repo, &["status", "--porcelain"], &step)?;
    if status.is_empty() {
        Ok(())
    } else {
        Err(ChekovError::WorkingTreeDirty {
            path: repo.to_path_buf(),
        })
    }
}

pub fn head_sha(repo: &Path) -> Result<String, ChekovError> {
    let step = format!(
        "git rev-parse HEAD (is {} a git repository?)",
        repo.display()
    );
    git(repo, &["rev-parse", "HEAD"], &step)
}

/// A detached checkout of HEAD that the run reads from; removed after.
pub struct Worktree {
    pub path: PathBuf,
    repo: PathBuf,
    removed: bool,
}

impl Worktree {
    /// A leftover at `dest` — from a crash, a ctrl-C, a kill — must not
    /// refuse the next run: it is cleared (as a worktree first, then as a
    /// directory) and the registration pruned before `add`.
    pub fn add(repo: &Path, dest: &Path) -> Result<Self, ChekovError> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
        }
        let dest_s = dest.display().to_string();
        if dest.exists() {
            let _ = git(repo, &["worktree", "remove", "--force", &dest_s], "");
            if dest.exists() {
                std::fs::remove_dir_all(dest)
                    .map_err(|e| ChekovError::io(format!("clearing {dest_s}"), e))?;
            }
        }
        git(repo, &["worktree", "prune"], "git worktree prune")?;
        git(
            repo,
            &["worktree", "add", "--detach", &dest_s, "HEAD"],
            "git worktree add",
        )?;
        Ok(Self {
            path: dest.to_path_buf(),
            repo: repo.to_path_buf(),
            removed: false,
        })
    }

    /// The explicit removal: its failure is the caller's to report.
    pub fn remove(mut self) -> Result<(), ChekovError> {
        self.removed = true;
        let path = self.path.display().to_string();
        git(
            &self.repo,
            &["worktree", "remove", "--force", &path],
            "git worktree remove",
        )?;
        git(&self.repo, &["worktree", "prune"], "git worktree prune")?;
        Ok(())
    }
}

/// The removal a panic or an early `?` between `add` and `remove` would
/// otherwise skip. Best-effort and silent — `remove` is the path that
/// reports, and a `Drop` that failed loudly would bury the real error.
impl Drop for Worktree {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        let path = self.path.display().to_string();
        let _ = git(&self.repo, &["worktree", "remove", "--force", &path], "");
        let _ = git(&self.repo, &["worktree", "prune"], "");
    }
}

/// What the walk found.
///
/// The eligible files, and enough of what it passed over to say so: a file
/// skipped for its size is a file the run silently would not have drawn
/// from, and the count makes that visible in the shortfall.
pub struct Sources {
    pub files: Vec<(String, String)>,
    pub oversized: usize,
    pub scanned: usize,
}

/// Every `*.rs` under `root` except test files, sorted by relative path.
///
/// The leakage filter's rule (a) is applied at the source, so a test is never
/// a task or context; oversized files are skipped and counted.
#[must_use]
pub fn rust_sources(root: &Path) -> Sources {
    let mut out = Sources {
        files: Vec::new(),
        oversized: 0,
        scanned: 0,
    };
    walk(root, root, &mut out);
    out.files.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Sources) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | "tests" | ".git") {
                walk(root, &path, out);
            }
            continue;
        }
        take_source(root, &path, out);
    }
}

/// One file's contribution: every Rust file is scanned, and the eligible ones
/// are kept under their `root`-relative path.
fn take_source(root: &Path, path: &Path, out: &mut Sources) {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
    {
        return;
    }
    out.scanned += 1;
    let Some(text) = source_text(path, &mut out.oversized) else {
        return;
    };
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    out.files.push((rel, text));
}

fn source_text(path: &Path, oversized: &mut usize) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().to_string();
    if name.ends_with("_test.rs") || name.starts_with("test_") {
        return None;
    }
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        *oversized += 1;
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::{Worktree, assert_clean, head_sha, rust_sources};
    use crate::error::ChekovError;

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

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-codebase-tree")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::create_dir_all(dir.join("tests")).expect("mkdir");
        std::fs::write(dir.join("src/lib.rs"), "pub fn a() -> i32 {\n    1\n}\n").expect("write");
        std::fs::write(
            dir.join("src/cov.rs"),
            "fn b() {}\n#[cfg(test)]\nmod t {}\n",
        )
        .expect("write");
        std::fs::write(dir.join("tests/it.rs"), "fn c() {}\n").expect("write");
        git(&dir, &["init", "-q"]);
        git(
            &dir,
            &["-c", "user.email=t@t", "-c", "user.name=t", "add", "."],
        );
        git(
            &dir,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        dir
    }

    #[test]
    fn a_clean_tree_passes_and_a_dirty_or_untracked_one_is_refused() {
        let dir = repo("gate");
        assert_clean(&dir).expect("clean");
        std::fs::write(dir.join("src/new.rs"), "fn d() {}\n").expect("write");
        assert!(
            matches!(
                assert_clean(&dir),
                Err(ChekovError::WorkingTreeDirty { .. })
            ),
            "untracked counts"
        );
    }

    #[test]
    fn head_is_a_full_sha_and_a_worktree_is_a_detached_copy_that_removes_cleanly() {
        let dir = repo("wt");
        let sha = head_sha(&dir).expect("head");
        assert_eq!(sha.len(), 40, "{sha}");
        let dest = dir.join("eval").join("tree");
        let wt = Worktree::add(&dir, &dest).expect("add");
        assert!(dest.join("src/lib.rs").exists());
        assert_eq!(head_sha(&dest).expect("head of the copy"), sha);
        wt.remove().expect("remove");
        assert!(!dest.exists());
        assert_clean(&dir).expect("the repo is untouched");
    }

    #[test]
    fn a_path_that_is_not_a_repository_says_so_in_the_step() {
        let dir = std::env::temp_dir()
            .join("chekov-test-codebase-tree")
            .join("not-a-repo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let Err(ChekovError::CodebaseWorktreeFailed { step, .. }) = assert_clean(&dir) else {
            panic!("a non-repository is a worktree failure, not a dirty tree");
        };
        assert!(
            step.contains("is") && step.contains("a git repository?"),
            "{step}"
        );
    }

    #[test]
    fn a_leftover_directory_at_the_destination_does_not_block_the_next_run() {
        let dir = repo("leftover");
        let dest = dir.join("scratch").join("codebase-tree-abc123");
        std::fs::create_dir_all(&dest).expect("mkdir");
        std::fs::write(dest.join("stale.txt"), "from a run that crashed").expect("write");
        let wt = Worktree::add(&dir, &dest).expect("a leftover is cleared, not a refusal");
        assert!(dest.join("src/lib.rs").exists());
        assert!(!dest.join("stale.txt").exists(), "the leftover is gone");
        wt.remove().expect("remove");
    }

    #[test]
    fn a_worktree_dropped_without_remove_cleans_up_after_itself() {
        let dir = repo("dropped");
        let dest = dir.join("scratch").join("codebase-tree-def456");
        {
            let _wt = Worktree::add(&dir, &dest).expect("add");
            assert!(dest.join("src/lib.rs").exists());
        }
        assert!(!dest.exists(), "Drop removes what `?` or a panic skipped");
        assert_clean(&dir).expect("the repo is untouched");
    }

    /// A file with an inline `#[cfg(test)]` module is kept, and kept verbatim:
    /// the cut is `prepare`'s job, so the walk stays ignorant of Rust beyond
    /// the file's name and its size.
    #[test]
    fn rust_sources_skip_test_named_files_and_hand_back_inline_tests_uncut() {
        let dir = repo("walk");
        let sources = rust_sources(&dir);
        let mut paths: Vec<&str> = sources.files.iter().map(|(p, _)| p.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["src/cov.rs", "src/lib.rs"], "{paths:?}");
        let cov = sources
            .files
            .iter()
            .find(|(p, _)| p.as_str() == "src/cov.rs")
            .expect("src/cov.rs is eligible now");
        assert!(cov.1.contains("#[cfg(test)]"), "the tree does not cut");
        assert_eq!(
            sources.scanned, 2,
            "src/lib.rs and src/cov.rs; tests/ is not walked"
        );
        assert_eq!(sources.oversized, 0);
    }

    #[test]
    fn a_file_over_the_size_cap_is_counted_not_silently_dropped() {
        let dir = repo("oversize");
        std::fs::write(dir.join("src/huge.rs"), "// ".repeat(120 * 1024)).expect("write");
        let sources = rust_sources(&dir);
        assert_eq!(sources.oversized, 1, "{:?}", sources.files.len());
        assert!(
            !sources.files.iter().any(|(p, _)| p.contains("huge")),
            "the file is still skipped — it is now also counted"
        );
    }
}
