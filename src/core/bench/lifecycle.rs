//! Per-candidate server lifecycle pieces (spec §7.3) — the pure parts.
//!
//! The orchestration (who launches, who tears down) lives in the bench
//! command; this module holds what can be tested without a process: flag
//! hygiene against the binary's own `--help`, the Metal residency env, the
//! budget-release policy, and the plan-as-data the confirm gate prints.

use std::path::Path;

/// Argv flags the built binary's own `--help` does not mention.
///
/// chekov tracks tip-of-master with no pin, and upstream REMOVES flags behind
/// an `arg_removed()` handler that terminates startup — this catches that
/// before a spawn, not as a cryptic exit in the server log. Value tokens
/// (paths, `q8_0`, numbers) are never flagged: only `-`-prefixed tokens are.
#[must_use]
pub fn unknown_flags(argv: &[String], help_text: &str) -> Vec<String> {
    argv.iter()
        .filter(|token| token.starts_with('-'))
        .filter(|flag| !help_text.contains(flag.as_str()))
        .cloned()
        .collect()
}

/// The binary's own `--help`, for `unknown_flags`. `None` when it cannot be
/// captured — the caller states that loudly rather than trusting or refusing
/// an unverifiable argv.
#[must_use]
pub fn server_help(engine_dir: &Path) -> Option<String> {
    let binary = crate::core::engine::server_binary(engine_dir);
    let out = std::process::Command::new(binary)
        .arg("--help")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The child env every bench-managed spawn carries (spec §7.3.3).
///
/// Metal keeps GPU memory wired for 3 MINUTES after use by default, which
/// makes a sequential sweep OOM nondeterministically on the second model —
/// and the failure vanishes when runs are spaced out.
pub const METAL_RESIDENCY: (&str, &str) = ("GGML_METAL_RESIDENCY_KEEP_ALIVE_S", "5");

/// How long teardown waits for the budget to come back, from `[bench]`.
#[derive(Debug, Clone, Copy)]
pub struct ReleasePolicy {
    pub total_mib: u64,
    pub release_pct: u32,
    pub max_polls: u32,
    pub interval: std::time::Duration,
}

impl ReleasePolicy {
    #[must_use]
    pub const fn want_mib(&self) -> u64 {
        self.total_mib * self.release_pct as u64 / 100
    }
}

/// Wait until the engine reports at least `release_pct` of the budget free.
///
/// `read_free` is `machine::live_gpu_free` in production, canned in tests.
/// A reader that stops answering is loud — an unverifiable release must not
/// read as a verified one.
pub fn wait_budget_released(
    policy: ReleasePolicy,
    read_free: &mut dyn FnMut() -> Option<u64>,
) -> Result<u64, crate::error::ChekovError> {
    let want = policy.want_mib();
    let mut last_seen = 0;
    for _ in 0..policy.max_polls {
        let free = read_free().ok_or(crate::error::ChekovError::BenchBudgetNotReleased {
            free_mib: last_seen,
            want_mib: want,
        })?;
        if free >= want {
            return Ok(free);
        }
        last_seen = free;
        std::thread::sleep(policy.interval);
    }
    Err(crate::error::ChekovError::BenchBudgetNotReleased {
        free_mib: last_seen,
        want_mib: want,
    })
}

#[cfg(test)]
mod tests {
    use super::unknown_flags;

    /// Shape of real `llama-server --help` output: short+long pairs, values.
    const HELP: &str = "\
  -m,    --model FNAME                 model path\n\
  -c,    --ctx-size N                  size of the prompt context\n\
  -np,   --parallel N                  number of parallel sequences\n\
  -fa,   --flash-attn [on|off|auto]    set Flash Attention use\n\
  -ctk,  --cache-type-k TYPE           KV cache data type for K\n\
  -ctv,  --cache-type-v TYPE           KV cache data type for V\n\
         --jinja                       use jinja template for chat\n\
         --reasoning-format FORMAT     controls thought tags\n\
         --host HOST                   ip address to listen on\n\
         --port PORT                   port to listen on\n\
         --api-key KEY                 API key to use for authentication\n\
         --temp N                      temperature\n\
         --top-p N                     top-p sampling\n\
         --top-k N                     top-k sampling\n";

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| (*t).to_owned()).collect()
    }

    #[test]
    fn a_clean_argv_raises_nothing() {
        let args = argv(&[
            "-m",
            "model.gguf",
            "--ctx-size",
            "262144",
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
            "--api-key",
            "k",
            "--jinja",
            "--flash-attn",
            "on",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q8_0",
            "-np",
            "1",
            "--reasoning-format",
            "none",
            "--temp",
            "0.6",
            "--top-p",
            "0.95",
            "--top-k",
            "20",
        ]);
        assert_eq!(unknown_flags(&args, HELP), Vec::<String>::new());
    }

    #[test]
    fn an_upstream_removed_flag_is_caught_before_the_spawn() {
        // --draft-max was REMOVED upstream behind arg_removed(), which
        // terminates startup — the failure this check exists to front-run.
        let args = argv(&["-m", "model.gguf", "--draft-max", "16"]);
        assert_eq!(unknown_flags(&args, HELP), vec!["--draft-max".to_owned()]);
    }

    #[test]
    fn values_and_paths_are_never_flagged() {
        // q8_0, file paths, numbers — only `-`-prefixed tokens are flags.
        let args = argv(&["--cache-type-k", "q8_0", "-m", "/x/-weird-dir/m.gguf"]);
        assert_eq!(unknown_flags(&args, HELP), Vec::<String>::new());
    }

    use super::{ReleasePolicy, wait_budget_released};
    use crate::error::ChekovError;

    fn instant_policy(max_polls: u32) -> ReleasePolicy {
        ReleasePolicy {
            total_mib: 228_065,
            release_pct: 80,
            max_polls,
            interval: std::time::Duration::ZERO,
        }
    }

    #[test]
    fn a_recovering_budget_succeeds_when_it_crosses_the_threshold() {
        let mut readings = [10_000_u64, 100_000, 190_000].into_iter();
        let free = wait_budget_released(instant_policy(5), &mut || readings.next())
            .expect("released on the third poll");
        assert_eq!(free, 190_000, "80% of 228065 is 182452 — 190000 crosses it");
    }

    #[test]
    fn a_budget_that_never_recovers_is_loud_with_both_numbers() {
        let err =
            wait_budget_released(instant_policy(3), &mut || Some(50_000)).expect_err("still wired");
        match err {
            ChekovError::BenchBudgetNotReleased { free_mib, want_mib } => {
                assert_eq!(free_mib, 50_000);
                assert_eq!(want_mib, 182_452);
            }
            other => panic!("expected the release refusal, got {other}"),
        }
    }

    #[test]
    fn a_reader_that_stops_answering_is_loud_not_a_verified_release() {
        assert!(wait_budget_released(instant_policy(3), &mut || None).is_err());
    }
}
