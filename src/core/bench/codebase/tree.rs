//! The repository side of codebase mode: the clean-tree gate, HEAD, a
//! detached worktree to read from, and the Rust file walk with the leakage
//! filter's test-file rule applied at the source.

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
pub fn assert_clean(repo: &Path) -> Result<(), ChekovError> {
    let status = git(repo, &["status", "--porcelain"], "git status")?;
    if status.is_empty() {
        Ok(())
    } else {
        Err(ChekovError::WorkingTreeDirty {
            path: repo.to_path_buf(),
        })
    }
}

pub fn head_sha(repo: &Path) -> Result<String, ChekovError> {
    git(repo, &["rev-parse", "HEAD"], "git rev-parse HEAD")
}

/// A detached checkout of HEAD that the run reads from; removed after.
pub struct Worktree {
    pub path: PathBuf,
    repo: PathBuf,
}

impl Worktree {
    pub fn add(repo: &Path, dest: &Path) -> Result<Self, ChekovError> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
        }
        let dest_s = dest.display().to_string();
        git(
            repo,
            &["worktree", "add", "--detach", &dest_s, "HEAD"],
            "git worktree add",
        )?;
        Ok(Self {
            path: dest.to_path_buf(),
            repo: repo.to_path_buf(),
        })
    }

    pub fn remove(self) -> Result<(), ChekovError> {
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

/// Every `*.rs` under `root` except test files (the leakage filter's rule
/// (a), applied at the source so a test is never a task or context), with
/// oversized files skipped. Sorted by relative path.
#[must_use]
pub fn rust_sources(root: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
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
        if let Some(text) = source_text(&path, &name) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, text));
        }
    }
}

fn source_text(path: &Path, name: &str) -> Option<String> {
    let is_rust = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"));
    let is_test = name.ends_with("_test.rs") || name.starts_with("test_");
    if !is_rust || is_test {
        return None;
    }
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    (!text.contains("#[cfg(test)]")).then_some(text)
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
    fn rust_sources_skip_tests_and_cfg_test_files() {
        let dir = repo("walk");
        let files = rust_sources(&dir);
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["src/lib.rs"], "{paths:?}");
        assert!(files[0].1.contains("pub fn a()"));
    }
}
