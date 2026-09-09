//! User-supplied graded probe sets (TOML).
//!
//! Deliberately NO compiled-in fixture: fixture-v1 carries a release gate —
//! it does not ship until measured against three models of clearly different
//! capability with the spread published — so until that campaign happens,
//! `--fixture <path>` is the only source of graded probes.

use std::path::Path;

use serde::Deserialize;

use crate::error::ChekovError;

/// What this chekov knows how to read.
const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub version: u32,
    pub probes: Vec<FixtureProbe>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureProbe {
    pub id: String,
    pub prompt: String,
    pub max_tokens: u32,
    /// Substrings the reply must contain (all of them), case-insensitive.
    #[serde(default)]
    pub expect_contains: Vec<String>,
}

pub fn load(path: &Path) -> Result<Fixture, ChekovError> {
    let invalid = |reason: String| ChekovError::FixtureInvalid {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|e| invalid(e.to_string()))?;
    let fixture: Fixture = toml::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    if fixture.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "version {} — this chekov reads version {SUPPORTED_VERSION}",
            fixture.version
        )));
    }
    if fixture.probes.is_empty() {
        return Err(invalid(
            "no probes — a fixture with nothing to grade".to_owned(),
        ));
    }
    Ok(fixture)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    fn write_scratch(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chekov-test-fixture");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write fixture");
        path
    }

    #[test]
    fn a_valid_fixture_parses() {
        let path = write_scratch(
            "ok.toml",
            r#"
version = 1

[[probes]]
id = "greeting"
prompt = "Say hello."
max_tokens = 32
expect_contains = ["hello"]
"#,
        );
        let fixture = super::load(&path).expect("valid fixture");
        assert_eq!(fixture.probes.len(), 1);
        assert_eq!(fixture.probes[0].id, "greeting");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let path = write_scratch("typo.toml", "version = 1\nprobes = []\ntypo = 1\n");
        assert!(super::load(&path).is_err(), "deny_unknown_fields");
    }

    #[test]
    fn a_newer_version_is_refused_naming_what_this_chekov_reads() {
        let path = write_scratch("v2.toml", "version = 2\nprobes = []\n");
        let err = super::load(&path).expect_err("too new");
        assert!(err.to_string().contains("version 1"), "{err}");
    }

    #[test]
    fn an_empty_probe_list_is_refused() {
        let path = write_scratch("empty.toml", "version = 1\nprobes = []\n");
        assert!(
            super::load(&path).is_err(),
            "a fixture with nothing to grade is a mistake"
        );
    }
}
