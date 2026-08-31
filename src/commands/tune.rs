//! `chekov tune [NAME]` — measure which launch flags beat the current ones on
//! this machine, and say so honestly (spec §2 surface, §7 plan, §9 report).

/// `chekov tune` — the four-stage descent over launch flags (spec §2).
#[derive(Debug, clap::Args)]
pub struct TuneCmd {
    /// Model to tune (defaults to the active model).
    pub name: Option<String>,
    /// Print the stage plan, the launch count and the estimate; launch nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Pre-approve the confirm gate (every trial is a launch).
    #[arg(long)]
    pub yes: bool,
    /// Write the winning flags into the model's `extra_flags`.
    #[arg(long)]
    pub apply: bool,
    /// Restrict the descent to these stages: fa, kv, batch, ubatch.
    #[arg(long, value_delimiter = ',')]
    pub stages: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::core::bench::sweep::SweepPlan;
    use crate::core::config::TuneSection;
    use crate::core::registry::{Effective, ModelEntry};
    use crate::core::stats::Summary;
    use crate::core::tune::{DEFAULTS_WON, Probe, Record, Stage, THERMAL_SOURCE, Trial, sextet};
    use crate::error::ChekovError;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    fn effective(flags: &[&str]) -> Effective {
        Effective {
            name: "m".into(),
            ctx_size: 4096,
            flags: argv(flags),
            entry: ModelEntry {
                repo: "o/r".into(),
                quant: "Q8_0".into(),
                revision: "abc123def456".into(),
                path: "models/m@abc123def456".into(),
                first_shard: "m.gguf".into(),
                hermes_ok: false,
                ctx_size: None,
                extra_flags: vec![],
                role: None,
            },
        }
    }

    fn plan_for<'a>(tune: &'a TuneSection, flags: &[&str]) -> super::Plan<'a> {
        super::Plan {
            eff: effective(flags),
            stages: Stage::ORDER.to_vec(),
            tune,
            sweep: SweepPlan {
                depths: vec![4096],
                repetitions: 5,
                max_tokens: 128,
            },
            significance_pct: 5.0,
        }
    }

    fn summary(median: f64, spread: f64) -> Summary {
        Summary {
            median,
            p10: median - spread,
            p90: median + spread,
            n: 4,
            warmup_dropped: 1,
        }
    }

    fn measured_trial(stage: &str, value: Option<&str>, argv: Vec<String>) -> Trial {
        Trial {
            stage: stage.to_owned(),
            value: value.map(str::to_owned),
            stamp: sextet(&argv),
            argv,
            outcome: "measured".into(),
            decode: Some(summary(31.2, 0.4)),
            prefill: Some(summary(402.0, 4.0)),
            prompt_n: Some(4101),
            speed_limit_pct: [None, None],
            reason: None,
            verdict: None,
        }
    }

    fn record_of(trials: Vec<Trial>, winner: Option<Vec<String>>) -> Record {
        Record {
            model: "m".into(),
            quant: "Q8_0".into(),
            revision: "abc123def456".into(),
            machine_id: "8d41f0c2a917".into(),
            engine_build_commit: "0f194b907".into(),
            chekov_version: "0.1.0".into(),
            probe: Probe {
                depth: 4096,
                repetitions: 5,
                max_tokens: 128,
            },
            thermal_source: THERMAL_SOURCE.into(),
            verdict: if winner.is_some() {
                super::CANDIDATE_WON.into()
            } else {
                DEFAULTS_WON.into()
            },
            trials,
            winner,
        }
    }

    fn baseline_argv() -> Vec<String> {
        argv(&[
            "--flash-attn",
            "on",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q8_0",
        ])
    }

    fn defaults_won_fixture() -> (Record, Vec<String>) {
        let record = record_of(vec![measured_trial("baseline", None, baseline_argv())], None);
        (record, vec!["  fa         off      slower on decode".to_owned()])
    }

    fn winner_fixture() -> (Record, Vec<String>) {
        let mut winner = baseline_argv();
        winner.extend(argv(&["--batch-size", "4096", "--ubatch-size", "1024"]));
        let mut won = measured_trial("ubatch", Some("1024"), winner.clone());
        won.prefill = Some(summary(466.0, 4.0));
        won.decode = Some(summary(31.1, 0.4));
        let record = record_of(
            vec![
                measured_trial("baseline", None, baseline_argv()),
                won,
            ],
            Some(winner),
        );
        (record, vec!["  ubatch     1024     faster on prefill".to_owned()])
    }

    #[test]
    fn the_plan_counts_launches_as_an_upper_bound_and_prints_the_estimate() {
        let tune = TuneSection::default();
        let plan = plan_for(
            &tune,
            &[
                "--flash-attn",
                "on",
                "--cache-type-k",
                "q8_0",
                "--cache-type-v",
                "q8_0",
            ],
        );
        assert_eq!(
            super::max_launches(&plan),
            9,
            "baseline + 1 + 1 + 3 + 3 with the default lists"
        );
        let text = super::plan_text(&plan, Some(35 * 1024 * 1024 * 1024), None);
        assert!(
            text.starts_with("tune m @ ctx 4096, probe depth 4096 × 5 reps\n"),
            "{text}"
        );
        assert!(
            text.contains("  fa         2 candidates   (1 is the incumbent)\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "  ubatch     4 candidates   (1 is the incumbent; values ≤ the incumbent batch)\n"
            ),
            "{text}"
        );
        assert!(text.contains("  ≤ 9 launches, ~"), "{text}");
        let with_running = super::plan_text(&plan, None, Some("m"));
        assert!(
            with_running.contains("will stop the running 'm' first"),
            "{with_running}"
        );
    }

    #[test]
    fn the_report_ends_with_the_record_and_how_to_apply_or_says_defaults_won() {
        let (record, lines) = defaults_won_fixture();
        let out = super::report(&record, &lines, Path::new("tune/x-m.json"));
        assert!(
            out.contains(
                "\n  defaults won — no candidate beat the current flags at p < 5% on its metric\n"
            ),
            "{out}"
        );
        assert!(out.ends_with("  record     tune/x-m.json\n"), "{out}");
        let (record, lines) = winner_fixture();
        let out = super::report(&record, &lines, Path::new("tune/x-m.json"));
        assert!(
            out.contains(
                "  winner     --flash-attn on --cache-type-k q8_0 --cache-type-v q8_0 \
                 --batch-size 4096 --ubatch-size 1024\n"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "  apply with: chekov tune m --apply   (or add the flags to extra_flags by hand)\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn a_launch_refusal_over_the_budget_is_a_skipped_trial_not_an_error() {
        let skipped = super::skip_reason(&ChekovError::ModelExceedsBudget {
            name: "m".into(),
            need_mib: 28_696,
            budget_mib: 24_576,
            ctx: 4096,
        });
        assert_eq!(
            skipped.as_deref(),
            Some("exceeds the GPU budget by 4120 MiB")
        );
        assert!(super::skip_reason(&ChekovError::ServerNotRunning).is_none());
    }
}
