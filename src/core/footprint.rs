//! What one model will occupy, and whether this machine can hold it.
//!
//! The `run` gate, `recommend` and `graph` all size a model the same way —
//! weights on disk plus the KV cache at its context — so the arithmetic lives
//! once, here, and the three can never disagree about what fits.

#[cfg(test)]
mod tests {
    use super::{Decision, decide, weights_on_disk};
    use crate::core::registry::ModelEntry;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn a_footprint_is_judged_against_the_budget_and_an_unknown_one_is_unverified() {
        assert_eq!(decide(None, 1_000), Decision::Unverified);
        assert_eq!(decide(Some(500 * MIB), 1_000), Decision::Proceed);
        assert_eq!(decide(Some(900 * MIB), 1_000), Decision::Tight { pct: 90 });
        assert_eq!(
            decide(Some(1_001 * MIB), 1_000),
            Decision::Exceeds { need_mib: 1_001 },
            "one MiB over is over — never rounded into fitting"
        );
    }

    fn entry(path: &str) -> ModelEntry {
        ModelEntry {
            repo: "org/repo-GGUF".into(),
            quant: "Q4_K_M".into(),
            revision: "abc123def4567890".into(),
            path: path.into(),
            first_shard: "a.gguf".into(),
            hermes_ok: false,
            ctx_size: None,
            extra_flags: vec![],
            role: None,
        }
    }

    #[test]
    fn weights_on_disk_sum_the_gguf_files_one_level_down_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("chekov-footprint-{}", std::process::id()));
        let dir = root.join("models/m@abc123def456");
        std::fs::create_dir_all(dir.join("sub")).expect("mkdir");
        std::fs::write(dir.join("a.gguf"), [0u8; 10]).expect("write");
        std::fs::write(dir.join("sub/b.gguf"), [0u8; 5]).expect("write");
        std::fs::write(dir.join("notes.txt"), [0u8; 100]).expect("write");
        assert_eq!(
            weights_on_disk(&root, &entry("models/m@abc123def456")),
            Some(15)
        );
        std::fs::create_dir_all(root.join("models/empty")).expect("mkdir");
        assert_eq!(
            weights_on_disk(&root, &entry("models/empty")),
            None,
            "nothing on disk is absent, not zero"
        );
        assert_eq!(weights_on_disk(&root, &entry("models/missing")), None);
        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
