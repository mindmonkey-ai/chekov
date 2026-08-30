//! Honest summaries of noisy throughput measurements.
//!
//! Median with p10/p90, never mean +/- stddev: decode rate is right-skewed by
//! thermal events, and a mean flatters a run that hit one stall. The first
//! repetition is dropped as warmup and the drop is recorded rather than
//! quietly absorbed.

/// A summarised measurement series.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub median: f64,
    pub p10: f64,
    pub p90: f64,
    /// Samples that contributed, after the warmup drop.
    pub n: usize,
    pub warmup_dropped: usize,
}

/// The verdict when two configurations are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// Distinguishable: the first is faster.
    Faster,
    /// Distinguishable: the first is slower.
    Slower,
    /// Not distinguishable by this run. Printed, never resolved into a winner.
    NoSignificantDifference,
}

/// Summarise, dropping the first sample as warmup.
#[must_use]
pub fn summarize(samples: &[f64]) -> Option<Summary> {
    let kept = samples.get(1..)?;
    if kept.is_empty() {
        return None;
    }
    let mut sorted = kept.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(Summary {
        median: percentile(&sorted, 0.50),
        p10: percentile(&sorted, 0.10),
        p90: percentile(&sorted, 0.90),
        n: sorted.len(),
        warmup_dropped: 1,
    })
}

/// Linear-interpolated percentile over an ascending slice.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    let last = sorted.len() - 1;
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample counts here are single digits"
    )]
    let pos = q * last as f64;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "pos is bounded by the slice length"
    )]
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(last);
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample counts here are single digits"
    )]
    let frac = pos - lo as f64;
    (sorted[hi] - sorted[lo]).mul_add(frac, sorted[lo])
}

/// Compare two summaries.
///
/// Indistinguishable when the p10-p90 intervals overlap AND the medians differ
/// by less than `significance_pct`. Both conditions, so a tight-but-shifted
/// pair is still called, and a wide-but-close pair is not.
#[must_use]
pub fn compare(a: &Summary, b: &Summary, significance_pct: f64) -> Comparison {
    let overlap = a.p10 <= b.p90 && b.p10 <= a.p90;
    let larger = a.median.abs().max(b.median.abs()).max(f64::MIN_POSITIVE);
    let delta_pct = (a.median - b.median).abs() / larger * 100.0;
    if overlap || delta_pct < significance_pct {
        return Comparison::NoSignificantDifference;
    }
    if a.median > b.median {
        Comparison::Faster
    } else {
        Comparison::Slower
    }
}

/// Whether enough distinct depths were measured to fit a decay curve.
///
/// Two points define a line through any two values; calling that a curve and
/// extrapolating from it is how a benchmark invents numbers.
#[must_use]
pub const fn can_fit_curve(distinct_depths: usize) -> bool {
    distinct_depths >= 3
}

#[cfg(test)]
mod tests {
    use super::{Comparison, can_fit_curve, compare, summarize};

    #[test]
    fn the_first_sample_is_dropped_as_warmup_and_the_drop_is_recorded() {
        // A cold first run is the classic outlier; 5.0 must not drag the median.
        let s = summarize(&[5.0, 20.0, 21.0, 22.0, 23.0]).expect("a summary");
        assert_eq!(s.warmup_dropped, 1);
        assert_eq!(s.n, 4, "four samples contributed");
        assert!(
            (s.median - 21.5).abs() < 1e-9,
            "median of the surviving four, got {}",
            s.median
        );
    }

    #[test]
    fn a_single_sample_cannot_be_summarised_at_all() {
        assert_eq!(summarize(&[20.0]), None, "nothing survives the warmup drop");
        assert_eq!(summarize(&[]), None);
    }

    #[test]
    fn the_summary_is_median_and_percentiles_not_a_mean() {
        // One thermal stall. A mean would report ~16.5 and flatter nothing;
        // the median must stay near the honest middle.
        let s = summarize(&[99.0, 22.0, 22.0, 1.0, 22.0, 22.0]).expect("a summary");
        assert!((s.median - 22.0).abs() < 1e-9, "got {}", s.median);
        assert!(s.p10 <= s.median && s.median <= s.p90);
    }

    #[test]
    fn overlapping_intervals_with_close_medians_are_not_resolved_into_a_winner() {
        let a = summarize(&[0.0, 22.9, 22.5, 23.3, 22.9]).expect("a");
        let b = summarize(&[0.0, 23.4, 23.0, 23.8, 23.4]).expect("b");
        assert_eq!(
            compare(&a, &b, 5.0),
            Comparison::NoSignificantDifference,
            "2% apart with overlapping spread is not a result"
        );
    }

    #[test]
    fn a_clear_difference_is_still_called() {
        let fast = summarize(&[0.0, 40.0, 41.0, 40.5, 40.2]).expect("fast");
        let slow = summarize(&[0.0, 20.0, 20.5, 19.8, 20.1]).expect("slow");
        assert_eq!(compare(&fast, &slow, 5.0), Comparison::Faster);
        assert_eq!(compare(&slow, &fast, 5.0), Comparison::Slower);
    }

    #[test]
    fn a_big_median_gap_still_needs_separated_intervals() {
        // Medians differ by more than 5%, but the spread is enormous and the
        // intervals overlap — this run cannot tell them apart.
        let a = summarize(&[0.0, 5.0, 40.0, 20.0, 35.0]).expect("a");
        let b = summarize(&[0.0, 6.0, 42.0, 24.0, 30.0]).expect("b");
        assert_eq!(compare(&a, &b, 5.0), Comparison::NoSignificantDifference);
    }

    #[test]
    fn two_depths_are_not_enough_to_fit_a_curve() {
        assert!(!can_fit_curve(2), "two points define a line, not a decay");
        assert!(!can_fit_curve(1));
        assert!(can_fit_curve(3));
    }
}
