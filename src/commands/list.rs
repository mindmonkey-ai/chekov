//! `chekov list` — registered models: name, quant, size on disk, revision,
//! active marker.

use std::path::Path;
use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::registry::Registry;
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct ListCmd {}

/// Rows for the list table. Pure over the registry + a disk-size probe.
#[must_use]
pub fn list_rows(reg: &Registry, root: &Path) -> Vec<Vec<String>> {
    let _ = (reg, root);
    todo!("cycle 5a red")
}

/// Recursive on-disk size of a model directory (0 when absent).
#[must_use]
pub fn dir_size(path: &Path) -> u64 {
    let _ = path;
    todo!("cycle 5a red")
}

/// `163_840` bytes → `160.0 KiB`-style human form.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    let _ = bytes;
    todo!("cycle 5a red")
}

impl Command for ListCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5a red")
    }
}

#[cfg(test)]
mod tests {
    use super::{dir_size, human_bytes, list_rows};
    use crate::core::registry::{ModelEntry, Registry};

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

    #[test]
    fn rows_mark_active_and_shorten_revision() {
        let root = scratch("list-rows");
        let mut reg = Registry::default();
        reg.models.insert(
            "m".into(),
            ModelEntry {
                repo: "org/repo".into(),
                quant: "Q8_0".into(),
                revision: "0123456789abcdef0123".into(),
                path: "models/m@0123456789ab".into(),
                first_shard: "m.gguf".into(),
                hermes_ok: false,
                ctx_size: None,
                extra_flags: vec![],
            },
        );
        reg.active = Some("m".into());
        let rows = list_rows(&reg, &root);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains(&"0123456789ab".to_owned()), "{rows:?}");
        assert!(rows[0].iter().any(|c| c.contains('*')), "no active marker: {rows:?}");
    }
}
