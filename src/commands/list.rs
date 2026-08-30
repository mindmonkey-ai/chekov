//! `chekov list` — registered models: name, quant, size on disk, revision,
//! role, active marker.

use std::path::Path;
use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::registry::{ModelRole, Registry};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct ListCmd {}

/// Rows for the list table. Pure over the registry + a disk-size probe.
#[must_use]
pub fn list_rows(reg: &Registry, root: &Path) -> Vec<Vec<String>> {
    reg.models
        .iter()
        .map(|(name, entry)| {
            let marker = if reg.active.as_deref() == Some(name) {
                "*"
            } else {
                ""
            };
            vec![
                marker.to_owned(),
                name.clone(),
                entry.quant.clone(),
                human_bytes(dir_size(&root.join(&entry.path))),
                entry.revision.chars().take(12).collect(),
                role_cell(entry.role).to_owned(),
            ]
        })
        .collect()
}

/// `role = "judge"` is what `capability bench --judge` requires, so the table
/// says which entry carries it. A model that is only served has no role.
const fn role_cell(role: Option<ModelRole>) -> &'static str {
    match role {
        Some(ModelRole::Judge) => "judge",
        None => "",
    }
}

/// Recursive on-disk size of a model directory (0 when absent).
#[must_use]
pub fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let child = entry.path();
            if child.is_dir() {
                dir_size(&child)
            } else {
                entry.metadata().map_or(0, |m| m.len())
            }
        })
        .sum()
}

/// `163_840` bytes → `160.0 KiB`-style human form.
// Display-only conversion; precision loss past 2^53 bytes is irrelevant here.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < 4 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", ["KiB", "MiB", "GiB", "TiB"][unit - 1])
}

impl Command for ListCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let reg = ctx.registry()?;
        if reg.models.is_empty() {
            println!("no models registered — add one with `chekov pull <org/repo:QUANT>`");
            return Ok(ExitCode::SUCCESS);
        }
        let rows = list_rows(&reg, &ctx.config.root);
        println!(
            "{}",
            super::render_table(&["", "NAME", "QUANT", "SIZE", "REVISION", "ROLE"], &rows)
        );
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::{dir_size, human_bytes, list_rows};
    use crate::core::registry::{ModelEntry, ModelRole, Registry};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chekov-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(160 * 1024 * 1024 * 1024), "160.0 GiB");
    }

    #[test]
    fn dir_size_sums_nested_files() {
        let dir = scratch("list-dirsize");
        std::fs::write(dir.join("a"), vec![0u8; 100]).expect("write");
        std::fs::create_dir_all(dir.join("sub")).expect("mkdir");
        std::fs::write(dir.join("sub/b"), vec![0u8; 50]).expect("write");
        assert_eq!(dir_size(&dir), 150);
    }

    fn entry(role: Option<ModelRole>) -> ModelEntry {
        ModelEntry {
            repo: "org/repo".into(),
            quant: "Q8_0".into(),
            revision: "0123456789abcdef0123".into(),
            path: "models/m@0123456789ab".into(),
            first_shard: "m.gguf".into(),
            hermes_ok: false,
            ctx_size: None,
            extra_flags: vec![],
            role,
        }
    }

    /// `--judge` needs a `role = "judge"` entry, so the table has to say which
    /// one carries it — a field set by hand is a field the user must be able
    /// to read back.
    #[test]
    fn a_judge_entry_is_marked_and_a_plain_one_is_not() {
        let root = scratch("list-roles");
        let mut reg = Registry::default();
        reg.models.insert("j".into(), entry(Some(ModelRole::Judge)));
        reg.models.insert("plain".into(), entry(None));
        let rows = list_rows(&reg, &root);
        let cells = |name: &str| {
            rows.iter()
                .find(|r| r[1] == name)
                .cloned()
                .unwrap_or_default()
        };
        assert_eq!(cells("j").last().map(String::as_str), Some("judge"));
        assert_eq!(cells("plain").last().map(String::as_str), Some(""));
    }

    #[test]
    fn rows_mark_active_and_shorten_revision() {
        let root = scratch("list-rows");
        let mut reg = Registry::default();
        reg.models.insert("m".into(), entry(None));
        reg.active = Some("m".into());
        let rows = list_rows(&reg, &root);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains(&"0123456789ab".to_owned()), "{rows:?}");
        assert!(
            rows[0].iter().any(|c| c.contains('*')),
            "no active marker: {rows:?}"
        );
    }
}
