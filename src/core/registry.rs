//! `models.toml` registry (§4.3 of the bootstrap prompt): defaults + per-model
//! tables, where flag arrays CONCATENATE (defaults first, then `extra_flags`)
//! rather than replace. `deny_unknown_fields` everywhere (§C.7).
//!
//! TOML ordering note: `active` is a top-level key and therefore serialized
//! before the `[defaults]` table — top-level keys after a table header would
//! belong to that table.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ChekovError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub models: BTreeMap<String, ModelEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    pub ctx_size: u32,
    pub flags: Vec<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            ctx_size: 98_304,
            flags: [
                "--jinja",
                "--flash-attn",
                "on",
                "--cache-type-k",
                "q8_0",
                "--cache-type-v",
                "q8_0",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub repo: String,
    pub quant: String,
    pub revision: String,
    /// Model directory relative to the chekov root: `models/<name>@<rev12>`.
    pub path: String,
    pub first_shard: String,
    #[serde(default)]
    pub hermes_ok: bool,
    /// Overrides `defaults.ctx_size` when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_size: Option<u32>,
    /// Appended AFTER the default flags — concatenation, not replacement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_flags: Vec<String>,
}

/// Fully resolved launch parameters for one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub name: String,
    pub ctx_size: u32,
    pub flags: Vec<String>,
    pub entry: ModelEntry,
}

impl Registry {
    /// Load from `path`; a missing file is an empty registry, a malformed one
    /// is `RegistryCorrupt` (§C.2 — never silently reset).
    pub fn load(path: &Path) -> Result<Self, ChekovError> {
        let _ = path;
        todo!("cycle 2 red")
    }

    /// Atomic save: write `<path>.tmp`, then rename over `path`.
    pub fn save(&self, path: &Path) -> Result<(), ChekovError> {
        let _ = path;
        todo!("cycle 2 red")
    }

    /// Resolve a model's effective config: defaults ⊕ model entry, flags
    /// concatenated (defaults first, then `extra_flags`).
    pub fn effective(&self, name: &str) -> Result<Effective, ChekovError> {
        let _ = name;
        todo!("cycle 2 red")
    }

    /// The active model's name, or a loud error naming `chekov use`.
    pub fn active_name(&self) -> Result<&str, ChekovError> {
        todo!("cycle 2 red")
    }

    /// Set the active model; the name must be registered.
    pub fn set_active(&mut self, name: &str) -> Result<(), ChekovError> {
        let _ = name;
        todo!("cycle 2 red")
    }

    /// Remove a model; refuses to remove the active one.
    pub fn remove(&mut self, name: &str) -> Result<ModelEntry, ChekovError> {
        let _ = name;
        todo!("cycle 2 red")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ModelEntry, Registry};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chekov-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn sample_entry() -> ModelEntry {
        ModelEntry {
            repo: "unsloth/MiniMax-M2.7-GGUF".into(),
            quant: "UD-Q5_K_XL".into(),
            revision: "abc123def4567890".into(),
            path: "models/minimax-m2.7@abc123def456".into(),
            first_shard: "MiniMax-M2.7-UD-Q5_K_XL-00001-of-00004.gguf".into(),
            hermes_ok: true,
            ctx_size: None,
            extra_flags: vec!["--reasoning-format".into(), "none".into()],
        }
    }

    #[test]
    fn roundtrips_through_disk() {
        let path = scratch("reg-roundtrip").join("models.toml");
        let mut reg = Registry::default();
        reg.models.insert("minimax-m2.7".into(), sample_entry());
        reg.active = Some("minimax-m2.7".into());
        reg.save(&path).expect("save");
        let loaded = Registry::load(&path).expect("load");
        assert_eq!(loaded.active.as_deref(), Some("minimax-m2.7"));
        assert_eq!(loaded.models["minimax-m2.7"], sample_entry());
    }

    #[test]
    fn missing_file_is_empty_registry() {
        let path = scratch("reg-missing").join("models.toml");
        let reg = Registry::load(&path).expect("empty default");
        assert!(reg.models.is_empty());
        assert_eq!(reg.active, None);
    }

    #[test]
    fn corrupt_file_names_path_and_remediation() {
        let path = scratch("reg-corrupt").join("models.toml");
        std::fs::write(&path, "not = [valid").expect("write");
        let msg = Registry::load(&path).expect_err("must reject").to_string();
        assert!(msg.contains("models.toml"), "no path in: {msg}");
        assert!(msg.contains("chekov pull"), "no remediation in: {msg}");
    }

    #[test]
    fn unknown_registry_key_is_corrupt() {
        let path = scratch("reg-unknown").join("models.toml");
        std::fs::write(&path, "activ = \"x\"\n").expect("write");
        assert!(Registry::load(&path).is_err(), "unknown key must not pass");
    }

    #[test]
    fn effective_flags_concatenate_defaults_then_extra() {
        let mut reg = Registry::default();
        reg.models.insert("m".into(), sample_entry());
        let eff = reg.effective("m").expect("registered");
        let flags = eff.flags.join(" ");
        assert!(flags.starts_with("--jinja --flash-attn on"), "defaults not first: {flags}");
        assert!(flags.ends_with("--reasoning-format none"), "extras not appended: {flags}");
    }

    #[test]
    fn effective_ctx_prefers_model_override() {
        let mut reg = Registry::default();
        let mut entry = sample_entry();
        entry.ctx_size = Some(32_768);
        reg.models.insert("m".into(), entry);
        assert_eq!(reg.effective("m").expect("registered").ctx_size, 32_768);
        let mut plain = Registry::default();
        plain.models.insert("p".into(), sample_entry());
        assert_eq!(plain.effective("p").expect("registered").ctx_size, 98_304);
    }

    #[test]
    fn set_active_requires_registered_model() {
        let mut reg = Registry::default();
        let msg = reg.set_active("ghost").expect_err("unknown").to_string();
        assert!(msg.contains("chekov list"), "no remediation in: {msg}");
    }

    #[test]
    fn remove_refuses_active_model() {
        let mut reg = Registry::default();
        reg.models.insert("m".into(), sample_entry());
        reg.active = Some("m".into());
        let msg = reg.remove("m").expect_err("active is protected").to_string();
        assert!(msg.contains("chekov use"), "no remediation in: {msg}");
    }
}
