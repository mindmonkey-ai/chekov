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

/// Which task sets a bench run measures (spec §2.1 `--suite`).
///
/// Defaults to `throughput` — a DEVIATION from the spec's `agentic` default,
/// held until the agentic set reaches the spec's full case counts; defaulting
/// to a partial set would misrepresent what "bench" measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Suite {
    Throughput,
    Agentic,
    All,
}

impl Suite {
    #[must_use]
    pub const fn runs_throughput(self) -> bool {
        matches!(self, Self::Throughput | Self::All)
    }

    #[must_use]
    pub const fn runs_agentic(self) -> bool {
        matches!(self, Self::Agentic | Self::All)
    }
}

/// One candidate's place in the run, as data — printed by `--dry-run`,
/// estimated for the confirm gate, executed sequentially.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchStep {
    pub model: String,
    pub action: StepAction,
    /// Weights on disk, for the load estimate. `None` renders `?` — an
    /// unknown size is stated, never invented.
    pub weights_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepAction {
    /// The server already serves this model; measure and leave it up.
    UseRunning,
    /// Bench launches it, tears it down, and verifies the budget released.
    Launch,
    /// The judge, loaded once after every candidate is down (spec C §3).
    Judge,
}

/// Reference rates from the spec's measured inputs (§7.6): load ≈ 4 s/GiB,
/// prefill ≈ 150 tok/s, decode ≈ 60 tok/s. An estimate, never a promise.
const LOAD_MS_PER_GIB: u64 = 4_000;
const PREFILL_TOK_S: u64 = 150;
const DECODE_TOK_S: u64 = 60;
const GIB: u64 = 1024 * 1024 * 1024;

/// Rough wall-clock seconds for the whole plan.
///
/// Every candidate's load and sweep, plus the judge step's own load. What the
/// judge then SPENDS on verdicts is `judge_estimate_secs`, added by the
/// caller. Integer milliseconds throughout — an estimate needs no float.
#[must_use]
pub fn estimate_secs(steps: &[BenchStep], plan: &crate::core::bench::sweep::SweepPlan) -> u64 {
    let sweep_ms: u64 = plan
        .depths
        .iter()
        .map(|&d| {
            u64::from(plan.repetitions)
                * (u64::from(d) * 1000 / PREFILL_TOK_S
                    + u64::from(plan.max_tokens) * 1000 / DECODE_TOK_S)
        })
        .sum();
    let total_ms: u64 = steps.iter().map(|step| step_ms(step, sweep_ms)).sum();
    total_ms.div_ceil(1000)
}

/// One step's wall-clock cost: a load for anything launched, a sweep for
/// anything measured against the model once it's up. The judge is loaded
/// but never swept — its cost is `judge_estimate_secs`, counted elsewhere.
fn step_ms(step: &BenchStep, sweep_ms: u64) -> u64 {
    let load_ms = || step.weights_bytes.unwrap_or(0) / GIB * LOAD_MS_PER_GIB;
    match step.action {
        StepAction::Launch => load_ms() + sweep_ms,
        StepAction::Judge => load_ms(),
        StepAction::UseRunning => sweep_ms,
    }
}

/// Any launch is a real side effect (a model load, a teardown) — those runs
/// confirm, and the judge's own step is such a launch. A pure
/// reuse-the-running-server run stays gate-free.
#[must_use]
pub fn needs_confirm(steps: &[BenchStep]) -> bool {
    steps
        .iter()
        .any(|s| matches!(s.action, StepAction::Launch | StepAction::Judge))
}

/// Two seconds a verdict on the 2026-08-30 probe (1.06 s gpt-oss-20b,
/// 0.78 s Gemma), rounded up; two orders per crossing.
pub const JUDGE_SECS_PER_VERDICT: u64 = 2;

#[must_use]
pub const fn judge_estimate_secs(crossings: u64) -> u64 {
    crossings * 2 * JUDGE_SECS_PER_VERDICT
}

/// The plan a human reads before agreeing to it.
#[must_use]
pub fn render_plan(steps: &[BenchStep], estimate_s: u64) -> String {
    let lines: String = steps.iter().map(step_line).collect();
    format!(
        "bench plan:\n{lines}~{} min estimated\n",
        estimate_s.div_ceil(60)
    )
}

fn step_line(step: &BenchStep) -> String {
    let action = match step.action {
        StepAction::UseRunning => "use running server",
        StepAction::Launch => "launch + teardown",
        StepAction::Judge => "judge: launch + teardown",
    };
    let weights = step
        .weights_bytes
        .map_or_else(|| "?".to_owned(), render_gib);
    format!("  {}  {action}  weights {weights}\n", step.model)
}

/// One-decimal GiB without touching floats.
fn render_gib(bytes: u64) -> String {
    let tenths = bytes * 10 / GIB;
    format!("{}.{} GiB", tenths / 10, tenths % 10)
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

    use super::{
        BenchStep, GIB, StepAction, estimate_secs, judge_estimate_secs, needs_confirm, render_plan,
    };
    use crate::core::bench::sweep::SweepPlan;

    fn plan() -> SweepPlan {
        SweepPlan {
            depths: vec![1024, 4096, 16384],
            repetitions: 5,
            max_tokens: 128,
        }
    }

    fn step(model: &str, action: StepAction, gib: Option<u64>) -> BenchStep {
        BenchStep {
            model: model.into(),
            action,
            weights_bytes: gib.map(|g| g * 1024 * 1024 * 1024),
        }
    }

    #[test]
    fn a_pure_reuse_run_needs_no_confirm_and_any_launch_does() {
        assert!(!needs_confirm(&[step("m", StepAction::UseRunning, None)]));
        assert!(needs_confirm(&[
            step("m", StepAction::UseRunning, None),
            step("n", StepAction::Launch, Some(35)),
        ]));
    }

    #[test]
    fn the_estimate_grows_with_launches_and_depths() {
        let reuse = estimate_secs(&[step("m", StepAction::UseRunning, None)], &plan());
        let launch = estimate_secs(&[step("m", StepAction::Launch, Some(35))], &plan());
        assert!(launch > reuse, "a load is not free: {launch} vs {reuse}");
        let mut deeper = plan();
        deeper.depths.push(65_536);
        assert!(estimate_secs(&[step("m", StepAction::UseRunning, None)], &deeper) > reuse);
    }

    #[test]
    fn the_rendered_plan_names_every_step_and_the_total() {
        let steps = [
            step("qwen3.8-27b", StepAction::Launch, Some(24)),
            step("gpt-oss-120b", StepAction::Launch, None),
        ];
        let rendered = render_plan(&steps, estimate_secs(&steps, &plan()));
        assert!(rendered.contains("qwen3.8-27b  launch + teardown  weights 24.0 GiB"));
        assert!(
            rendered.contains("gpt-oss-120b  launch + teardown  weights ?"),
            "an unknown size is stated, never invented: {rendered}"
        );
        assert!(rendered.contains("min estimated"), "{rendered}");
    }

    #[test]
    fn a_judge_step_confirms_loads_and_never_sweeps() {
        let steps = [
            BenchStep {
                model: "ornith-1.5-35b-a3b".into(),
                action: StepAction::Launch,
                weights_bytes: Some(GIB),
            },
            BenchStep {
                model: "gpt-oss-20b".into(),
                action: StepAction::Judge,
                weights_bytes: Some(GIB),
            },
        ];
        let plan = crate::core::bench::sweep::SweepPlan {
            depths: vec![1024],
            repetitions: 1,
            max_tokens: 60,
        };
        let one = estimate_secs(&steps[..1], &plan);
        let both = estimate_secs(&steps, &plan);
        assert_eq!(
            both - one,
            4,
            "a judge step costs its load (4 s/GiB) and no sweep"
        );
        assert!(needs_confirm(&steps[1..]));
        assert!(
            render_plan(&steps, both)
                .contains("  gpt-oss-20b  judge: launch + teardown  weights 1.0 GiB\n")
        );
        assert_eq!(judge_estimate_secs(6), 24);
    }
}
