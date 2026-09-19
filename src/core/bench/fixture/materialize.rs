//! The visible fixture tree on disk as a committed git repository.
//!
//! So the codebase-mode pipeline can check it out like a user's repository.
//! The hidden tests never touch this tree: they come back in memory.

use std::path::{Path, PathBuf};

use super::embedded::FILES;
use super::manifest;
use crate::core::bench::codebase::tree;
use crate::error::ChekovError;

const HIDDEN_DIR: &str = "hidden/";

/// Where the visible files went, and the hidden files that did not.
pub struct Materialized {
    pub repo: PathBuf,
    /// `(embedded path, text)` for every file under `hidden/`.
    pub hidden: Vec<(String, String)>,
}

/// Write every embedded file but `hidden/*` and `manifest.toml` under
/// `<scratch_root>/fixture-v1-<hash12>/` and commit it.
///
/// Keyed by the content hash, so a second run of the same fixture reuses the
/// repository, and a different fixture never shares one. The manifest stays out
/// of the graded tree because its comments narrate every device.
pub fn materialize(scratch_root: &Path, hash12: &str) -> Result<Materialized, ChekovError> {
    let repo = scratch_root.join(format!("fixture-v1-{hash12}"));
    let mut hidden = Vec::new();
    for (path, text) in FILES {
        if path.starts_with(HIDDEN_DIR) {
            hidden.push(((*path).to_owned(), (*text).to_owned()));
            continue;
        }
        if *path == manifest::MANIFEST_PATH {
            continue;
        }
        write_file(&repo.join(path), text)?;
    }
    if !repo.join(".git").exists() {
        tree::init_and_commit(&repo, "fixture-v1 materialized")?;
    }
    Ok(Materialized { repo, hidden })
}

fn write_file(dest: &Path, text: &str) -> Result<(), ChekovError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    std::fs::write(dest, text)
        .map_err(|e| ChekovError::io(format!("writing {}", dest.display()), e))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::materialize;
    use crate::core::bench::codebase::tree;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-materialize")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn the_tree_has_the_crate_a_head_and_neither_hidden_nor_the_manifest() {
        let root = scratch("tree");
        let m = materialize(&root, "0123456789ab").expect("materialize");
        assert_eq!(m.repo, root.join("fixture-v1-0123456789ab"));
        assert!(m.repo.join("Cargo.toml").exists());
        assert!(m.repo.join("src/api/mod.rs").exists());
        assert!(
            !m.repo.join("manifest.toml").exists(),
            "the manifest's comments narrate every device"
        );
        assert!(
            !m.repo.join("hidden").exists(),
            "hidden tests are never on disk here"
        );
        tree::assert_clean(&m.repo).expect("committed, nothing untracked");
        tree::head_sha(&m.repo).expect("a HEAD");
        assert_eq!(m.hidden.len(), 4);
        assert!(m.hidden.iter().all(|(p, _)| p.starts_with("hidden/")));
    }

    #[test]
    fn a_second_call_reuses_the_repository_and_its_head() {
        let root = scratch("reuse");
        let first = materialize(&root, "0123456789ab").expect("first");
        let head = tree::head_sha(&first.repo).expect("head");
        let second = materialize(&root, "0123456789ab").expect("second");
        assert_eq!(tree::head_sha(&second.repo).expect("head"), head);
        tree::assert_clean(&second.repo).expect("still clean");
    }
}
