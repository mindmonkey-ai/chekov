//! Comparison of two stored runs.
//!
//! The stamp refuses when the ENVIRONMENT differs; the model fields
//! (`weights_revision`, `quant`) are the comparison's subject, not its
//! precondition — comparing two models is the point, comparing two
//! environments is a category error the stamp exists to prevent.

use crate::core::bench::stamp;
use crate::core::bench::store::{RunLog, TaskRow};
use crate::core::stats::{self, Comparison, Summary};
use crate::error::ChekovError;

#[derive(Debug)]
pub struct DepthComparison {
    pub depth: u32,
    pub a: Summary,
    pub b: Summary,
    pub verdict: Comparison,
}

pub struct RunPair<'a> {
    pub a: &'a RunLog,
    pub b: &'a RunLog,
}

pub fn compare_runs(
    a: &RunLog,
    b: &RunLog,
    significance_pct: f64,
) -> Result<Vec<DepthComparison>, ChekovError> {
    assert_same_environment(a, b)?;
    let mut rows = Vec::new();
    for row_a in throughput_rows(a) {
        let Some(depth) = depth_of(row_a) else {
            continue;
        };
        let Some(row_b) = throughput_rows(b).find(|r| depth_of(r) == Some(depth)) else {
            continue;
        };
        let (Some(sum_a), Some(sum_b)) = (
            stats::summarize(&row_a.measure.decode_samples),
            stats::summarize(&row_b.measure.decode_samples),
        ) else {
            continue;
        };
        let verdict = stats::compare(&sum_a, &sum_b, significance_pct);
        rows.push(DepthComparison {
            depth,
            a: sum_a,
            b: sum_b,
            verdict,
        });
    }
    Ok(rows)
}

fn throughput_rows(log: &RunLog) -> impl Iterator<Item = &TaskRow> {
    log.rows.iter().filter(|r| r.suite == "throughput")
}

fn depth_of(row: &TaskRow) -> Option<u32> {
    row.task_id.strip_prefix("depth-")?.parse().ok()
}

/// Refuse on the first differing stamp field, with the subject fields
/// (`weights_revision`, `quant`) masked equal — they are what is being
/// compared, not what must match.
fn assert_same_environment(a: &RunLog, b: &RunLog) -> Result<(), ChekovError> {
    let mut b_env = b.head.stamp.clone();
    b_env
        .weights_revision
        .clone_from(&a.head.stamp.weights_revision);
    b_env.quant.clone_from(&a.head.stamp.quant);
    stamp::mismatch_error(&a.head.stamp, &b_env).map_or(Ok(()), Err)
}

#[must_use]
pub fn render_comparison(pair: &RunPair, rows: &[DepthComparison]) -> String {
    let header = format!(
        "compare {} vs {}  (engine {})\n",
        pair.a.head.model, pair.b.head.model, pair.a.head.stamp.engine_build_commit
    );
    if rows.is_empty() {
        return format!("{header}no depth measured in both runs — nothing to compare\n");
    }
    let lines: String = rows.iter().map(|row| verdict_line(pair, row)).collect();
    format!("{header}{lines}")
}

fn verdict_line(pair: &RunPair, row: &DepthComparison) -> String {
    let numbers = format!(
        "{:.1} vs {:.1} tok/s (p10-p90 [{:.1}..{:.1}] vs [{:.1}..{:.1}])",
        row.a.median, row.b.median, row.a.p10, row.a.p90, row.b.p10, row.b.p90
    );
    match row.verdict {
        Comparison::Faster => {
            format!(
                "depth {:>6}: {} is faster — {numbers}\n",
                row.depth, pair.a.head.model
            )
        }
        Comparison::Slower => {
            format!(
                "depth {:>6}: {} is faster — {numbers}\n",
                row.depth, pair.b.head.model
            )
        }
        Comparison::NoSignificantDifference => {
            format!(
                "depth {:>6}: no significant difference — {numbers}\n",
                row.depth
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RunPair, compare_runs, render_comparison};
    use crate::core::bench::stamp::Stamp;
    use crate::core::bench::store::{Measure, RunHead, RunLog, TaskRow};
    use crate::core::stats::Comparison;
    use crate::error::ChekovError;

    fn stamp(engine: &str, weights: &str) -> Stamp {
        Stamp {
            machine_id: "8d41f0c2a917".into(),
            engine_build_commit: engine.into(),
            weights_revision: weights.into(),
            quant: "Q8_0".into(),
            ctx: 131_072,
            n_parallel: 1,
            kv_unified: "engine-default".into(),
            n_batch: "engine-default".into(),
            n_ubatch: "engine-default".into(),
            type_k: "q8_0".into(),
            type_v: "q8_0".into(),
            flash_attn: "on".into(),
            seed: 42,
            temperature_milli: 0,
            chekov_version: "0.1.0".into(),
            prompt_set_hash: "e19a".into(),
            corpus_id: "throughput-v1".into(),
        }
    }

    fn run(model: &str, stamp: Stamp, decode: &[f64]) -> RunLog {
        RunLog {
            head: RunHead {
                model: model.into(),
                machine_brand: None,
                launch_args: vec![],
                forced_reasoning_format: None,
                stamp,
            },
            rows: vec![TaskRow {
                schema: 1,
                run_id: "r".into(),
                seq: 0,
                suite: "throughput".into(),
                task_id: "depth-1024".into(),
                transport: crate::core::bench::store::Transport::Buffered,
                measure: Measure {
                    prompt_n: 1000,
                    decode_samples: decode.to_vec(),
                    prefill_samples: decode.to_vec(),
                    warmup_dropped: 1,
                    cache_n: 0,
                },
                grade: None,
            }],
        }
    }

    #[test]
    fn a_differing_environment_is_refused_naming_the_first_field() {
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let mut changed = stamp("dda1b0d67", "r2/s2");
        changed.seed = 7;
        changed.type_k = "f16".into();
        let b = run("m2", changed, &[19.0, 21.0, 22.0]);
        let err = compare_runs(&a, &b, 5.0).expect_err("environment differs");
        match err {
            ChekovError::BenchStampMismatch { field, .. } => {
                assert_eq!(field, "type_k", "type_k precedes seed in declaration order");
            }
            other => panic!("expected stamp mismatch, got {other}"),
        }
    }

    #[test]
    fn differing_models_under_one_environment_compare_fine() {
        // weights_revision and quant are the SUBJECT of the comparison.
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 20.0, 21.0, 22.0]);
        let mut other = stamp("dda1b0d67", "r2/s2");
        other.quant = "UD-Q6_K_XL".into();
        let b = run("m2", other, &[19.5, 20.5, 21.0, 21.5]);
        let rows = compare_runs(&a, &b, 5.0).expect("same environment");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verdict, Comparison::NoSignificantDifference);
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &rows);
        assert!(rendered.contains("no significant difference"), "{rendered}");
    }

    #[test]
    fn a_differing_prompt_set_is_refused() {
        // You cannot compare runs of different task sets.
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let mut other = stamp("dda1b0d67", "r1/s1");
        other.prompt_set_hash = "ffff".into();
        let b = run("m2", other, &[19.0, 21.0, 22.0]);
        assert!(compare_runs(&a, &b, 5.0).is_err());
    }

    #[test]
    fn only_depths_present_in_both_runs_are_compared() {
        let mut a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        a.rows.push(TaskRow {
            schema: 1,
            run_id: "r".into(),
            seq: 1,
            suite: "throughput".into(),
            task_id: "depth-4096".into(),
            transport: crate::core::bench::store::Transport::Buffered,
            measure: Measure {
                prompt_n: 4100,
                decode_samples: vec![15.0, 16.0, 17.0],
                prefill_samples: vec![15.0, 16.0, 17.0],
                warmup_dropped: 1,
                cache_n: 0,
            },
            grade: None,
        });
        let b = run("m2", stamp("dda1b0d67", "r2/s2"), &[30.0, 40.0, 41.0]);
        let rows = compare_runs(&a, &b, 5.0).expect("same environment");
        assert_eq!(rows.len(), 1, "depth 4096 exists only in one run");
        assert_eq!(rows[0].depth, 1024);
    }

    #[test]
    fn a_clear_gap_is_called() {
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[38.0, 40.0, 41.0, 40.5]);
        let b = run("m2", stamp("dda1b0d67", "r2/s2"), &[19.0, 20.0, 21.0, 20.5]);
        let rows = compare_runs(&a, &b, 5.0).expect("same environment");
        assert_eq!(rows[0].verdict, Comparison::Faster);
    }
}
