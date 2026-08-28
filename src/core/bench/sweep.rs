//! Depth × repetition sweep, summarised by `core::stats` — the raw samples
//! ride along so every summary can be audited back to what was measured.

use crate::core::bench::probes;
use crate::core::bench::runner::ProbeArtifact;
use crate::core::config::BenchSection;
use crate::core::proxy::http::HttpRequest;
use crate::core::stats::{self, Summary};
use crate::error::ChekovError;

pub struct SweepPlan {
    pub depths: Vec<u32>,
    pub repetitions: u32,
    pub max_tokens: u32,
}

impl From<&BenchSection> for SweepPlan {
    fn from(bench: &BenchSection) -> Self {
        Self {
            depths: bench.depths.clone(),
            repetitions: bench.repetitions,
            max_tokens: bench.max_tokens,
        }
    }
}

/// One depth's measurements: raw samples plus their summaries.
pub struct DepthResult {
    pub depth: u32,
    /// Measured prompt depth (`timings.prompt_n`) — the honest x-axis.
    pub prompt_n: u64,
    /// Max prompt tokens served from cache across the repetitions.
    pub cache_n: u64,
    pub decode_samples: Vec<f64>,
    pub prefill_samples: Vec<f64>,
    pub decode: Option<Summary>,
    pub prefill: Option<Summary>,
}

/// The probe executor — `runner::cross` in production, canned in tests.
pub type ProbeExec<'a> = dyn FnMut(&HttpRequest) -> Result<ProbeArtifact, ChekovError> + 'a;

pub fn run_sweep(plan: &SweepPlan, exec: &mut ProbeExec) -> Result<Vec<DepthResult>, ChekovError> {
    plan.depths
        .iter()
        .map(|&depth| measure_depth(plan, depth, exec))
        .collect()
}

/// One depth's repetitions. Public so the CLI can append each depth's row to
/// the run log as it completes (`--resume` loses at most one task).
pub fn measure_depth(
    plan: &SweepPlan,
    depth: u32,
    exec: &mut ProbeExec,
) -> Result<DepthResult, ChekovError> {
    let mut decode_samples = Vec::new();
    let mut prefill_samples = Vec::new();
    let mut prompt_n = 0_u64;
    let mut cache_n = 0_u64;
    for _ in 0..plan.repetitions {
        let artifact = exec(&probes::throughput_probe(depth, plan.max_tokens))?;
        decode_samples.push(artifact.timings.predicted_per_second);
        prefill_samples.push(artifact.timings.prompt_per_second);
        prompt_n = prompt_n.max(artifact.timings.prompt_n);
        cache_n = cache_n.max(artifact.timings.cache_n);
    }
    Ok(DepthResult {
        depth,
        prompt_n,
        cache_n,
        decode: stats::summarize(&decode_samples),
        prefill: stats::summarize(&prefill_samples),
        decode_samples,
        prefill_samples,
    })
}

/// The refusal the spec pins: two points define a line, and extrapolating
/// from it is how a benchmark invents numbers.
#[must_use]
pub fn curve_note(distinct_depths: usize) -> Option<String> {
    (!stats::can_fit_curve(distinct_depths)).then(|| {
        format!("insufficient depths to fit a curve — measure at least 3 (got {distinct_depths})")
    })
}

#[cfg(test)]
mod tests {
    use super::{SweepPlan, curve_note, run_sweep};
    use crate::core::bench::runner::{ProbeArtifact, Timings};
    use crate::error::ChekovError;

    fn artifact(decode_tps: f64) -> ProbeArtifact {
        ProbeArtifact {
            anthropic_body: r#"{"type":"message","content":[{"type":"text","text":"1"}]}"#.into(),
            timings: Timings {
                prompt_n: 1000,
                prompt_per_second: 400.0,
                predicted_n: 128,
                predicted_per_second: decode_tps,
                cache_n: 64,
            },
        }
    }

    #[test]
    fn a_sweep_summarises_each_depth_and_keeps_the_raw_samples() {
        let plan = SweepPlan {
            depths: vec![100, 200],
            repetitions: 3,
            max_tokens: 16,
        };
        let mut tick = 0_u32;
        let results = run_sweep(&plan, &mut |_req| {
            tick += 1;
            Ok(artifact(20.0 + f64::from(tick)))
        })
        .expect("sweep");
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].decode_samples.len(),
            3,
            "raw samples are auditable"
        );
        let summary = results[0].decode.as_ref().expect("three samples summarise");
        assert_eq!(summary.warmup_dropped, 1);
        assert_eq!(
            results[0].prompt_n, 1000,
            "the honest depth is the measured one"
        );
        assert_eq!(results[0].cache_n, 64, "prefix-cache reuse rides along");
    }

    #[test]
    fn a_failed_probe_fails_the_sweep_loudly() {
        let plan = SweepPlan {
            depths: vec![100],
            repetitions: 2,
            max_tokens: 16,
        };
        let result = run_sweep(&plan, &mut |_req| Err(ChekovError::BenchNoTimings));
        assert!(
            result.is_err(),
            "a mid-sweep failure must not yield a partial run"
        );
    }

    #[test]
    fn fewer_than_three_depths_refuse_a_curve_in_the_stated_words() {
        let note = curve_note(2).expect("two depths cannot fit a curve");
        assert!(
            note.contains("insufficient depths to fit a curve"),
            "{note}"
        );
        assert!(note.contains("(got 2)"), "{note}");
        assert_eq!(curve_note(3), None);
    }
}
