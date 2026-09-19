//! Embeds `fixtures/fixture-v1/` into the binary as a `(path, text)` table.
//!
//! No build dependency: the content hash is computed at run time from the
//! same table (`core::bench::fixture::manifest::content_hash`). This script
//! never touches the crate's own CLI (AGENTS.md §12 override 3 still holds).

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURE_DIR: &str = "fixtures/fixture-v1";
const SKIPPED_DIRS: [&str; 2] = ["target", ".git"];
const SKIPPED_FILES: [&str; 2] = ["README.md", ".gitignore"];

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join(FIXTURE_DIR);
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &root, &mut files)?;
    files.sort();
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("fixture_v1_files.rs");
    fs::write(&out, table(&root, &files))?;
    Ok(())
}

/// Every embedded file under `dir`, as a fixture-relative `/`-separated path.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                walk(root, &path, out)?;
            }
        } else if !SKIPPED_FILES.contains(&name.as_str()) {
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(relative);
        }
    }
    Ok(())
}

/// `pub const FILES: &[(&str, &str)] = &[ ("Cargo.toml", include_str!("…")), … ];`
fn table(root: &Path, files: &[String]) -> String {
    let mut text = String::from("pub const FILES: &[(&str, &str)] = &[\n");
    for relative in files {
        let absolute = root.join(relative).display().to_string();
        text.push_str(&format!(
            "    ({relative:?}, include_str!({absolute:?})),\n"
        ));
    }
    text.push_str("];\n");
    text
}
