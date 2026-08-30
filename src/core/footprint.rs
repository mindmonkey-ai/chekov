//! What one model will occupy, and whether this machine can hold it.
//!
//! The `run` gate, `recommend` and `graph` all size a model the same way —
//! weights on disk plus the KV cache at its context — so the arithmetic lives
//! once, here, and the three can never disagree about what fits.

use std::path::Path;

use crate::core::config::Config;
use crate::core::frontier::{Fit, fit_for};
use crate::core::registry::{Effective, ModelEntry};

const MIB: u64 = 1024 * 1024;

/// What the gate does with one footprint against one budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Proceed,
    /// Fits, but past the tight fraction — said out loud, never refused.
    Tight {
        pct: u64,
    },
    Exceeds {
        need_mib: u64,
    },
    /// An input was unknown; the gate proceeds and says it did not check.
    Unverified,
}

/// `fit_for`'s verdict with the numbers the gate has to print.
#[must_use]
pub const fn decide(total_bytes: Option<u64>, budget_mib: u64) -> Decision {
    match (fit_for(total_bytes, budget_mib), total_bytes) {
        (Fit::Fits, _) => Decision::Proceed,
        (Fit::Tight, Some(total)) => Decision::Tight {
            pct: total * 100 / (budget_mib * MIB),
        },
        (Fit::Exceeds, Some(total)) => Decision::Exceeds {
            need_mib: total.div_ceil(MIB),
        },
        _ => Decision::Unverified,
    }
}

/// Weights plus KV cache at the model's effective context, or `None` when
/// either is unknown — an unknown must never be summed as zero.
#[must_use]
pub fn predicted_total(cfg: &Config, eff: &Effective) -> Option<u64> {
    let weights = weights_on_disk(&cfg.root, &eff.entry)?;
    let shard = crate::core::server::shard_path(cfg, eff);
    let geometry = crate::core::gguf::read_geometry(&shard).ok()?;
    let q8 = eff.flags.iter().any(|f| f == "q8_0");
    let kv = crate::core::gguf::kv_bytes(&geometry, eff.ctx_size, q8)?;
    Some(weights + kv)
}

/// Bytes actually on disk for a model directory, or `None` when it is absent.
///
/// Walks one level of subdirectories: a repo like `unsloth/MiniMax-M2.7-GGUF`
/// keeps its shards under a quant folder (`UD-Q5_K_XL/…`), so a top-level-only
/// scan reports a fully downloaded 158 GiB model as absent.
#[must_use]
pub fn weights_on_disk(root: &Path, entry: &ModelEntry) -> Option<u64> {
    let dir = root.join(&entry.path);
    let total = gguf_bytes_in(&dir) + subdir_gguf_bytes(&dir);
    (total > 0).then_some(total)
}

fn gguf_bytes_in(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "gguf"))
        .filter_map(|e| e.metadata().ok().map(|m| m.len()))
        .sum()
}

fn subdir_gguf_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| gguf_bytes_in(&e.path()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{Decision, OVERHEAD_BYTES, decide, sized, wants_q8, weights_on_disk};
    use crate::core::registry::ModelEntry;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn the_total_is_weights_plus_kv_plus_the_compute_overhead_graph_reserves() {
        assert_eq!(sized(10, 5), 15 + OVERHEAD_BYTES);
        assert_eq!(
            OVERHEAD_BYTES,
            3 * 1024 * MIB,
            "the 3 GiB every frontier cell reserves"
        );
    }

    #[test]
    fn q8_is_read_from_the_effective_flags_wherever_it_appears() {
        let flags = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(wants_q8(&flags(&["--cache-type-k", "q8_0"])));
        assert!(!wants_q8(&flags(&["--flash-attn", "on"])));
        assert!(!wants_q8(&flags(&[])));
    }

    #[test]
    fn a_footprint_is_judged_against_the_budget_and_an_unknown_one_is_unverified() {
        assert_eq!(decide(None, 1_000), Decision::Unverified);
        assert_eq!(
            decide(Some(MIB), 0),
            Decision::Unverified,
            "a zero budget is an unreadable one, not one everything exceeds"
        );
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
        // A failed run leaves the tree behind; start clean so `empty` is empty.
        let _ = std::fs::remove_dir_all(&root);
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
