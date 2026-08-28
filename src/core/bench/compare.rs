//! Comparison of two stored runs — same engine only. A cross-engine
//! comparison attributes the engine's change to the model and is refused.

use crate::core::bench::store::RunRecord;
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
    pub a: &'a RunRecord,
    pub b: &'a RunRecord,
}

pub fn compare_runs(
    a: &RunRecord,
    b: &RunRecord,
    significance_pct: f64,
) -> Result<Vec<DepthComparison>, ChekovError> {
    assert_same_engine(a, b)?;
    let mut rows = Vec::new();
    for depth_a in &a.depths {
        let Some(depth_b) = b.depths.iter().find(|d| d.depth == depth_a.depth) else {
            continue;
        };
        let (Some(sum_a), Some(sum_b)) = (
            stats::summarize(&depth_a.decode_samples),
            stats::summarize(&depth_b.decode_samples),
        ) else {
            continue;
        };
        let verdict = stats::compare(&sum_a, &sum_b, significance_pct);
        rows.push(DepthComparison {
            depth: depth_a.depth,
            a: sum_a,
            b: sum_b,
            verdict,
        });
    }
    Ok(rows)
}

/// `engine.build_commit` must be recorded AND equal on both sides — an
/// unrecorded engine cannot be attested to be the same one.
fn assert_same_engine(a: &RunRecord, b: &RunRecord) -> Result<(), ChekovError> {
    match (&a.engine_build_commit, &b.engine_build_commit) {
        (Some(commit_a), Some(commit_b)) if commit_a == commit_b => Ok(()),
        (commit_a, commit_b) => Err(ChekovError::BenchEngineMismatch {
            a: commit_a.clone().unwrap_or_else(|| "unrecorded".to_owned()),
            b: commit_b.clone().unwrap_or_else(|| "unrecorded".to_owned()),
        }),
    }
}

#[must_use]
pub fn render_comparison(pair: &RunPair, rows: &[DepthComparison]) -> String {
    let header = format!(
        "compare {} vs {}  (engine {})\n",
        pair.a.model,
        pair.b.model,
        pair.a
            .engine_build_commit
            .as_deref()
            .unwrap_or("unrecorded"),
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
                row.depth, pair.a.model
            )
        }
        Comparison::Slower => {
            format!(
                "depth {:>6}: {} is faster — {numbers}\n",
                row.depth, pair.b.model
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
    use crate::core::bench::store::{DepthRecord, MachineRecord, RunRecord};
    use crate::core::stats::Comparison;
    use crate::error::ChekovError;

    fn run(model: &str, engine: Option<&str>, decode: &[f64]) -> RunRecord {
        RunRecord {
            schema_version: 1,
            created_utc: "20260827T120000Z".into(),
            model: model.into(),
            ctx: 131_072,
            launch_args: vec![],
            engine_build_commit: engine.map(str::to_owned),
            machine: MachineRecord {
                chip: None,
                memsize_bytes: None,
                gpu_budget_mib: None,
                budget_provenance: None,
            },
            depths: vec![DepthRecord {
                depth: 1024,
                prompt_n: 1000,
                decode_samples: decode.to_vec(),
                prefill_samples: decode.to_vec(),
            }],
            fixture: vec![],
        }
    }

    #[test]
    fn differing_engines_are_refused_naming_the_field() {
        let a = run("m1", Some("79aac7d9"), &[19.0, 21.0, 22.0]);
        let b = run("m2", Some("00c0ffee"), &[19.0, 21.0, 22.0]);
        let err = compare_runs(&a, &b, 5.0).expect_err("cross-engine");
        assert!(matches!(err, ChekovError::BenchEngineMismatch { .. }));
        assert!(err.to_string().contains("engine.build_commit"), "{err}");
    }

    #[test]
    fn an_unknown_engine_on_either_side_is_also_refused() {
        // "Same engine" cannot be attested when one side never recorded it.
        let a = run("m1", None, &[19.0, 21.0, 22.0]);
        let b = run("m2", Some("79aac7d9"), &[19.0, 21.0, 22.0]);
        assert!(compare_runs(&a, &b, 5.0).is_err());
    }

    #[test]
    fn overlapping_intervals_print_no_significant_difference() {
        let a = run("m1", Some("79aac7d9"), &[19.0, 20.0, 21.0, 22.0]);
        let b = run("m2", Some("79aac7d9"), &[19.5, 20.5, 21.0, 21.5]);
        let rows = compare_runs(&a, &b, 5.0).expect("same engine");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verdict, Comparison::NoSignificantDifference);
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &rows);
        assert!(rendered.contains("no significant difference"), "{rendered}");
    }

    #[test]
    fn only_depths_present_in_both_runs_are_compared() {
        let mut a = run("m1", Some("79aac7d9"), &[19.0, 21.0, 22.0]);
        a.depths.push(DepthRecord {
            depth: 4096,
            prompt_n: 4100,
            decode_samples: vec![15.0, 16.0, 17.0],
            prefill_samples: vec![15.0, 16.0, 17.0],
        });
        let b = run("m2", Some("79aac7d9"), &[30.0, 40.0, 41.0]);
        let rows = compare_runs(&a, &b, 5.0).expect("same engine");
        assert_eq!(rows.len(), 1, "depth 4096 exists only in one run");
        assert_eq!(rows[0].depth, 1024);
    }

    #[test]
    fn a_clear_gap_is_called() {
        let a = run("m1", Some("79aac7d9"), &[38.0, 40.0, 41.0, 40.5]);
        let b = run("m2", Some("79aac7d9"), &[19.0, 20.0, 21.0, 20.5]);
        let rows = compare_runs(&a, &b, 5.0).expect("same engine");
        assert_eq!(rows[0].verdict, Comparison::Faster);
    }
}
