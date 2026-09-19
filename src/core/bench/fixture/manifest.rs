//! The fixture-v1 grading contract (`manifest.toml`), read strictly.

use std::path::PathBuf;

use serde::Deserialize;

use crate::core::hash::sha256_hex;
use crate::error::ChekovError;

/// What this chekov knows how to read.
pub const MANIFEST_VERSION: u32 = 1;
/// The manifest's path inside the fixture — the one embedded file the
/// content hash leaves out, so writing the hash in never changes it.
pub const MANIFEST_PATH: &str = "manifest.toml";
const HIDDEN_DIR: &str = "hidden/";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    pub content_hash: String,
    /// Reserve slots (capability-spec §9) — reported, never enforced here.
    pub task_slots: u32,
    /// Defaults to empty so a manifest with no `[[tasks]]` reaches the
    /// business-rule check below instead of failing as a missing TOML key.
    #[serde(default)]
    pub tasks: Vec<ManifestTask>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestTask {
    pub id: String,
    pub device: String,
    /// The file holding the masked body.
    pub source: String,
    /// The held-out test, under `hidden/`; never in a prompt.
    pub hidden: String,
    /// The scoring tier this device is reported on: 6 (compile) or 7 (test).
    pub tier: u32,
    /// The masked fn: `name`, or `Owner::name` when the name repeats in the file.
    pub symbol: String,
}

#[must_use]
pub fn invalid(reason: String) -> ChekovError {
    ChekovError::FixtureInvalid {
        path: PathBuf::from(format!("fixture-v1/{MANIFEST_PATH}")),
        reason,
    }
}

/// SHA-256 over every embedded file but the manifest, as `path\0text\0` in
/// table order — an edit to any fixture file without a manifest bump is a
/// refused run, not a silently different corpus.
#[must_use]
pub fn content_hash(files: &[(&str, &str)]) -> String {
    let mut bytes = Vec::new();
    for (path, text) in files.iter().filter(|(p, _)| *p != MANIFEST_PATH) {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
    }
    sha256_hex(&bytes)
}

pub fn parse(text: &str, files: &[(&str, &str)]) -> Result<Manifest, ChekovError> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    if manifest.version != MANIFEST_VERSION {
        return Err(invalid(format!(
            "version {} — this chekov reads manifest version {MANIFEST_VERSION}",
            manifest.version
        )));
    }
    let computed = content_hash(files);
    if manifest.content_hash != computed {
        return Err(invalid(format!(
            "content hash {computed} does not match manifest {}",
            manifest.content_hash
        )));
    }
    validate_tasks(&manifest.tasks, files).map_err(invalid)?;
    Ok(manifest)
}

fn validate_tasks(tasks: &[ManifestTask], files: &[(&str, &str)]) -> Result<(), String> {
    if tasks.is_empty() {
        return Err("no tasks — a manifest with nothing to grade".to_owned());
    }
    let mut ids = std::collections::BTreeSet::new();
    for task in tasks {
        if !ids.insert(task.id.as_str()) {
            return Err(format!("task {} is listed twice", task.id));
        }
        validate_task(task, files)?;
    }
    Ok(())
}

fn validate_task(task: &ManifestTask, files: &[(&str, &str)]) -> Result<(), String> {
    let has = |path: &str| files.iter().any(|(p, _)| *p == path);
    if !has(&task.source) {
        return Err(format!(
            "task {}: source {} is not in the fixture",
            task.id, task.source
        ));
    }
    if !task.hidden.starts_with(HIDDEN_DIR) || !has(&task.hidden) {
        return Err(format!(
            "task {}: hidden {} must be an embedded file under {HIDDEN_DIR}",
            task.id, task.hidden
        ));
    }
    if !matches!(task.tier, 6 | 7) {
        return Err(format!(
            "task {}: tier {} — only 6 (compile) and 7 (test) are graded",
            task.id, task.tier
        ));
    }
    if task.symbol.trim().is_empty() {
        return Err(format!("task {}: symbol is empty", task.id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Manifest, content_hash, parse};

    const SRC: &str = "pub fn a() -> u32 {\n    1\n}\n";
    const HIDDEN: &str = "#[test]\nfn t() {}\n";

    /// A three-file fixture whose manifest declares the hash of the other two.
    fn files(manifest_body: &str) -> Vec<(String, String)> {
        let content = [("src/a.rs", SRC), ("hidden/t.rs", HIDDEN)];
        let hash = content_hash(&content);
        let manifest = format!(
            "version = 1\nid = \"fixture-v1\"\ncontent_hash = \"{hash}\"\ntask_slots = 10\n{manifest_body}"
        );
        let mut out: Vec<(String, String)> = content
            .iter()
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect();
        out.push(("manifest.toml".to_owned(), manifest));
        out
    }

    fn borrowed(files: &[(String, String)]) -> Vec<(&str, &str)> {
        files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect()
    }

    fn parse_body(body: &str) -> Result<Manifest, String> {
        let files = files(body);
        let refs = borrowed(&files);
        let text = refs
            .iter()
            .find(|(p, _)| *p == "manifest.toml")
            .map(|(_, t)| *t)
            .expect("manifest");
        parse(text, &refs).map_err(|e| e.to_string())
    }

    const ONE_TASK: &str = "[[tasks]]\nid = \"d1\"\ndevice = \"x\"\nsource = \"src/a.rs\"\nhidden = \"hidden/t.rs\"\ntier = 7\nsymbol = \"a\"\n";

    #[test]
    fn a_valid_manifest_parses_and_the_hash_excludes_the_manifest_itself() {
        let manifest = parse_body(ONE_TASK).expect("parses");
        assert_eq!(manifest.tasks.len(), 1);
        assert_eq!(manifest.tasks[0].symbol, "a");
        let with = content_hash(&[
            ("src/a.rs", SRC),
            ("hidden/t.rs", HIDDEN),
            ("manifest.toml", "anything"),
        ]);
        let without = content_hash(&[("src/a.rs", SRC), ("hidden/t.rs", HIDDEN)]);
        assert_eq!(
            with, without,
            "editing the manifest never changes the content hash"
        );
    }

    #[test]
    fn a_wrong_hash_is_refused_naming_both_values() {
        let files = files(ONE_TASK);
        let mut refs = borrowed(&files);
        refs.push(("src/extra.rs", "pub fn extra() {}\n"));
        let text = refs
            .iter()
            .find(|(p, _)| *p == "manifest.toml")
            .map(|(_, t)| *t)
            .expect("manifest");
        let err = parse(text, &refs).expect_err("refused").to_string();
        assert!(
            err.contains("content hash") && err.contains("does not match"),
            "{err}"
        );
    }

    #[test]
    fn an_unknown_key_a_stray_hidden_a_duplicate_id_and_a_bad_tier_are_refused() {
        let unknown = format!("{ONE_TASK}extra = 1\n");
        assert!(
            parse_body(&unknown)
                .expect_err("unknown key")
                .contains("extra")
        );
        let stray = ONE_TASK.replace("hidden/t.rs", "src/a.rs");
        assert!(
            parse_body(&stray)
                .expect_err("hidden outside hidden/")
                .contains("hidden/")
        );
        let twice = format!("{ONE_TASK}{ONE_TASK}");
        assert!(
            parse_body(&twice)
                .expect_err("duplicate")
                .contains("listed twice")
        );
        let five = ONE_TASK.replace("tier = 7", "tier = 5");
        assert!(parse_body(&five).expect_err("tier").contains("tier 5"));
        let empty = ONE_TASK.replace("symbol = \"a\"", "symbol = \"\"");
        assert!(
            parse_body(&empty)
                .expect_err("symbol")
                .contains("symbol is empty")
        );
        assert!(parse_body("").expect_err("no tasks").contains("no tasks"));
    }

    #[test]
    fn a_newer_version_is_refused_naming_what_this_chekov_reads() {
        let files = files(ONE_TASK);
        let refs = borrowed(&files);
        let text = refs
            .iter()
            .find(|(p, _)| *p == "manifest.toml")
            .map(|(_, t)| *t)
            .expect("manifest");
        let bumped = text.replace("version = 1", "version = 2");
        let err = parse(&bumped, &refs).expect_err("refused").to_string();
        assert!(
            err.contains("version 2") && err.contains("manifest version 1"),
            "{err}"
        );
    }
}
