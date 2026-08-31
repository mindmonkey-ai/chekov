//! `chekov tune` stages, candidate lists, and argv rewriting (spec §4, §10).
//!
//! Pure, side-effect-free helpers: naming the four tune stages in their fixed
//! run order, listing the launch-flag values each stage tries, and rewriting
//! a launch argv to carry one candidate value under either flag spelling.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::bench::stamp::flag_value_either;
use crate::core::bench::sweep::DepthResult;
use crate::core::clock::utc_compact_now;
use crate::core::config::TuneSection;
use crate::core::machine::pmset_therm;
use crate::core::stats::{Comparison, Summary, compare};
use crate::error::ChekovError;

/// llama-server's default `--batch-size` when the flag is absent, per
/// `llama-server --help` on this machine.
const ENGINE_DEFAULT_BATCH: u32 = 2048;

/// One dimension `chekov tune` sweeps, in the fixed order they run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Fa,
    Kv,
    Batch,
    Ubatch,
}

impl Stage {
    pub const ORDER: [Self; 4] = [Self::Fa, Self::Kv, Self::Batch, Self::Ubatch];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fa => "fa",
            Self::Kv => "kv",
            Self::Batch => "batch",
            Self::Ubatch => "ubatch",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ORDER.into_iter().find(|stage| stage.label() == s)
    }

    #[must_use]
    pub const fn metric(self) -> Metric {
        match self {
            Self::Fa | Self::Kv => Metric::Decode,
            Self::Batch | Self::Ubatch => Metric::Prefill,
        }
    }
}

/// The throughput measurement a stage's candidates are judged on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metric {
    Decode,
    Prefill,
}

impl Metric {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Prefill => "prefill",
        }
    }

    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Decode => Self::Prefill,
            Self::Prefill => Self::Decode,
        }
    }
}

/// A launch-flag variant tried by one tune stage, with the rewritten argv it
/// produces.
pub struct Candidate {
    pub stage: Stage,
    pub value: String,
    pub argv: Vec<String>,
}

/// A launch flag `chekov tune` rewrites, with its short and long spellings.
#[derive(Clone, Copy)]
pub enum Flag {
    FlashAttn,
    CacheTypeK,
    CacheTypeV,
    BatchSize,
    UbatchSize,
}

impl Flag {
    #[must_use]
    pub const fn names(self) -> [&'static str; 2] {
        match self {
            Self::FlashAttn => ["-fa", "--flash-attn"],
            Self::CacheTypeK => ["-ctk", "--cache-type-k"],
            Self::CacheTypeV => ["-ctv", "--cache-type-v"],
            Self::BatchSize => ["-b", "--batch-size"],
            Self::UbatchSize => ["-ub", "--ubatch-size"],
        }
    }
}

/// Rewrites `argv` so `flag` carries `value`: the first occurrence of either
/// spelling is replaced in place, later duplicates are dropped, and an
/// absent flag is appended under its long spelling.
#[must_use]
pub fn rewrite(argv: &[String], flag: Flag, value: &str) -> Vec<String> {
    let names = flag.names();
    let mut out = Vec::with_capacity(argv.len() + 2);
    let mut replaced = false;
    let mut index = 0;
    while index < argv.len() {
        let token = &argv[index];
        if !names.contains(&token.as_str()) {
            out.push(token.clone());
            index += 1;
            continue;
        }
        if !replaced {
            out.push(token.clone());
            out.push(value.to_owned());
            replaced = true;
        }
        let skip_value = argv.get(index + 1).is_some();
        index += if skip_value { 2 } else { 1 };
    }
    if !replaced {
        out.push(names[1].to_owned());
        out.push(value.to_owned());
    }
    out
}

/// The value `flag` currently carries in `argv`, under either spelling.
#[must_use]
pub fn value_of(argv: &[String], flag: Flag) -> Option<String> {
    let names = flag.names();
    let position = argv.iter().position(|a| names.contains(&a.as_str()))?;
    argv.get(position + 1).cloned()
}

fn incumbent_batch(incumbent: &[String]) -> u32 {
    value_of(incumbent, Flag::BatchSize)
        .and_then(|value| value.parse().ok())
        .unwrap_or(ENGINE_DEFAULT_BATCH)
}

/// The candidate values a stage tries, per the `[tune]` config (spec §4).
///
/// The plan line counts these (`N candidates`) while `candidates` returns the
/// ones that are not the incumbent already — the two numbers differ by the
/// incumbent, which is what the plan says out loud.
pub(crate) fn values_for(stage: Stage, incumbent: &[String], cfg: &TuneSection) -> Vec<String> {
    match stage {
        Stage::Fa => cfg.flash_attn.clone(),
        Stage::Kv => cfg.cache_types.clone(),
        Stage::Batch => cfg.batch_sizes.iter().map(u32::to_string).collect(),
        Stage::Ubatch => {
            let batch = incumbent_batch(incumbent);
            cfg.ubatch_sizes
                .iter()
                .filter(|&&value| value <= batch)
                .map(u32::to_string)
                .collect()
        }
    }
}

/// `incumbent` rewritten to carry `value` for `stage`'s flag(s); `Kv`
/// rewrites K and V together.
fn apply(stage: Stage, incumbent: &[String], value: &str) -> Vec<String> {
    match stage {
        Stage::Fa => rewrite(incumbent, Flag::FlashAttn, value),
        Stage::Kv => {
            let with_k = rewrite(incumbent, Flag::CacheTypeK, value);
            rewrite(&with_k, Flag::CacheTypeV, value)
        }
        Stage::Batch => rewrite(incumbent, Flag::BatchSize, value),
        Stage::Ubatch => rewrite(incumbent, Flag::UbatchSize, value),
    }
}

/// Every candidate `stage` tries against `incumbent`, excluding the
/// incumbent's own argv (it is not a candidate against itself).
#[must_use]
pub fn candidates(stage: Stage, incumbent: &[String], cfg: &TuneSection) -> Vec<Candidate> {
    values_for(stage, incumbent, cfg)
        .into_iter()
        .filter_map(|value| {
            let argv = apply(stage, incumbent, &value);
            (argv != incumbent).then_some(Candidate { stage, value, argv })
        })
        .collect()
}

/// The stages to run, in the fixed order, filtered to `requested` labels
/// when given (duplicates collapse); an unrecognized label is an error.
pub fn stages(requested: Option<&[String]>) -> Result<Vec<Stage>, ChekovError> {
    let Some(labels) = requested else {
        return Ok(Stage::ORDER.to_vec());
    };
    let mut parsed = Vec::with_capacity(labels.len());
    for label in labels {
        let stage = Stage::parse(label).ok_or_else(|| ChekovError::TuneUnknownStage {
            stage: label.clone(),
        })?;
        parsed.push(stage);
    }
    Ok(Stage::ORDER
        .into_iter()
        .filter(|stage| parsed.contains(stage))
        .collect())
}

/// One depth's measurement, promoted out of a raw `DepthResult` once it has
/// enough samples to be judged (spec §5).
pub struct Measured {
    pub decode: Summary,
    pub prefill: Summary,
    pub prompt_n: u64,
}

impl Measured {
    const fn by_metric(&self, metric: Metric) -> &Summary {
        match metric {
            Metric::Decode => &self.decode,
            Metric::Prefill => &self.prefill,
        }
    }
}

/// What a depth's raw result becomes once classified (spec §5).
pub enum Outcome {
    Measured(Measured),
    Degenerate(String),
    Skipped(String),
}

/// A trial too thin to trust: too few samples to summarise after the warmup
/// drop, or a prompt that never reached half the requested depth (spec §5).
#[must_use]
pub fn classify(result: &DepthResult, depth: u32) -> Outcome {
    let (Some(decode), Some(prefill)) = (&result.decode, &result.prefill) else {
        return Outcome::Degenerate("fewer than 2 samples after the warmup drop".into());
    };
    if result.prompt_n * 2 < u64::from(depth) {
        return Outcome::Degenerate(format!(
            "prompt_n {} is below half the requested depth {depth}",
            result.prompt_n
        ));
    }
    Outcome::Measured(Measured {
        decode: decode.clone(),
        prefill: prefill.clone(),
        prompt_n: result.prompt_n,
    })
}

/// Whether a candidate replaces the incumbent, and the report phrase either
/// way (spec §4).
pub struct Verdict {
    pub wins: bool,
    pub phrase: String,
}

/// The verdict phrase for one stage's primary/other comparison, and whether
/// it wins. `comparisons` is `(primary, other)` — bundled so the helper stays
/// at this crate's clippy argument floor (`clippy.toml`, §3.4).
fn phrase(
    comparisons: (Comparison, Comparison),
    stage: Stage,
    incumbent_median: f64,
) -> (bool, String) {
    let (primary, other) = comparisons;
    let primary_label = stage.metric().label();
    let other_label = stage.metric().other().label();
    match primary {
        Comparison::Faster if other != Comparison::Slower => (
            true,
            format!("faster on {primary_label}, {other_label} not slower — new incumbent"),
        ),
        Comparison::Faster => (
            false,
            format!("faster on {primary_label} but slower on {other_label} — incumbent kept"),
        ),
        Comparison::Slower => (false, format!("slower on {primary_label}")),
        Comparison::NoSignificantDifference => (
            false,
            format!("no significant difference vs {incumbent_median:.1} — incumbent kept"),
        ),
    }
}

/// `stage` plus the significance threshold `judge` compares under.
///
/// Bundled so `judge` stays at this crate's clippy argument floor
/// (`clippy.toml`, §3.4) despite the spec's four logically independent
/// inputs.
#[derive(Clone, Copy)]
pub struct JudgeCriteria {
    pub stage: Stage,
    pub significance_pct: f64,
}

/// Judges `candidate` against `incumbent` on `criteria.stage`'s primary
/// metric, winning only when the other metric does not regress (spec §4).
#[must_use]
pub fn judge(candidate: &Measured, incumbent: &Measured, criteria: JudgeCriteria) -> Verdict {
    let metric = criteria.stage.metric();
    let other = metric.other();
    let primary_cmp = compare(
        candidate.by_metric(metric),
        incumbent.by_metric(metric),
        criteria.significance_pct,
    );
    let other_cmp = compare(
        candidate.by_metric(other),
        incumbent.by_metric(other),
        criteria.significance_pct,
    );
    let incumbent_median = incumbent.by_metric(metric).median;
    let (wins, text) = phrase((primary_cmp, other_cmp), criteria.stage, incumbent_median);
    Verdict { wins, phrase: text }
}

/// The stage's winning candidate: the highest primary median among those
/// that beat the incumbent, ties kept at the earlier candidate.
#[must_use]
pub fn pick_winner<'a>(
    scored: &'a [(Candidate, Measured, Verdict)],
) -> Option<&'a (Candidate, Measured, Verdict)> {
    let mut best: Option<(&'a (Candidate, Measured, Verdict), f64)> = None;
    for entry @ (candidate, measured, verdict) in scored {
        if !verdict.wins {
            continue;
        }
        let median = measured.by_metric(candidate.stage.metric()).median;
        match best {
            Some((_, best_median)) if median <= best_median => {}
            _ => best = Some((entry, median)),
        }
    }
    best.map(|(entry, _)| entry)
}

/// The two metric cells of a stage-tuning report line (spec §9).
pub(crate) fn measured_cells(measured: &Measured) -> String {
    format!(
        "decode {:.1} [{:.1}..{:.1}]  prefill {:.0} [{:.0}..{:.0}]",
        measured.decode.median,
        measured.decode.p10,
        measured.decode.p90,
        measured.prefill.median,
        measured.prefill.p10,
        measured.prefill.p90,
    )
}

/// The dirty-clock note appended to a report line, or empty when the clock
/// was not dirty.
fn dirty_note(dirty: Option<u32>) -> String {
    dirty.map_or_else(String::new, |pct| {
        format!("   clock was dirty (CPU_Speed_Limit {pct}%)")
    })
}

/// Which candidate a report line names — bundled with `LineContext` so
/// `stage_line` stays at this crate's clippy argument floor (`clippy.toml`,
/// §3.4) despite the spec's five logically independent inputs.
pub struct CandidateLabel<'a> {
    pub stage: Stage,
    pub value: &'a str,
}

/// The judged context for a measured candidate's report line; both fields
/// are `None` for a skipped or degenerate outcome.
pub struct LineContext<'a> {
    pub verdict: Option<&'a Verdict>,
    pub dirty: Option<u32>,
}

/// One candidate's stage-tuning report line (spec §9).
#[must_use]
pub fn stage_line(label: &CandidateLabel, outcome: &Outcome, context: &LineContext) -> String {
    let stage = label.stage.label();
    let value = label.value;
    match outcome {
        Outcome::Measured(measured) => {
            let cells = measured_cells(measured);
            let phrase = context.verdict.map_or("", |v| v.phrase.as_str());
            let note = dirty_note(context.dirty);
            format!("  {stage:<10} {value:<8} {cells}   {phrase}{note}")
        }
        Outcome::Skipped(reason) => format!("  {stage:<10} {value:<8} skipped: {reason}"),
        Outcome::Degenerate(reason) => format!("  {stage:<10} {value:<8} degenerate: {reason}"),
    }
}

/// Where a trial's thermal readings came from (spec §6).
///
/// Named in the record so a reader knows the limitation: macOS exposes the
/// real pressure level only through a C notification API this crate does
/// not link.
pub const THERMAL_SOURCE: &str = "pmset -g therm";

/// A run where the baseline's own flags could not be beaten (spec §8) —
/// the record still exists, but there is nothing to `--apply`.
pub const DEFAULTS_WON: &str = "defaults won";

/// `pmset -g therm`'s `CPU_Speed_Limit = N` line, parsed without a regex
/// dependency for two call sites: find the line, split on `=`, trim, parse.
#[must_use]
pub fn parse_therm(pmset_output: &str) -> Option<u32> {
    let line = pmset_output
        .lines()
        .find(|line| line.contains("CPU_Speed_Limit"))?;
    let (_, value) = line.split_once('=')?;
    value.trim().parse().ok()
}

/// `pmset -g therm`, read and parsed. `None` when the command failed or the
/// output carried no speed-limit line (the nominal case).
#[must_use]
pub fn read_therm() -> Option<u32> {
    parse_therm(&pmset_therm()?)
}

/// The lower of two thermal readings, but only when one is actually
/// throttled (below 100) — two nominal readings carry no note.
#[must_use]
pub fn thermal_note(before: Option<u32>, after: Option<u32>) -> Option<u32> {
    [before, after]
        .into_iter()
        .flatten()
        .filter(|&pct| pct < 100)
        .min()
}

/// The bench stamp's flag sextet, read straight off a trial's argv so a
/// tune trial and a bench run describe a configuration in the same words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlagSextet {
    pub kv_unified: String,
    pub n_batch: String,
    pub n_ubatch: String,
    pub type_k: String,
    pub type_v: String,
    pub flash_attn: String,
}

/// Read the flag sextet off a launch argv (spec §8 — same name pairs as
/// `build_head`).
#[must_use]
pub fn sextet(argv: &[String]) -> FlagSextet {
    FlagSextet {
        kv_unified: flag_value_either(argv, &["-kvu", "--kv-unified"]),
        n_batch: flag_value_either(argv, &["-b", "--batch-size"]),
        n_ubatch: flag_value_either(argv, &["-ub", "--ubatch-size"]),
        type_k: flag_value_either(argv, &["-ctk", "--cache-type-k"]),
        type_v: flag_value_either(argv, &["-ctv", "--cache-type-v"]),
        flash_attn: flag_value_either(argv, &["-fa", "--flash-attn"]),
    }
}

/// The probe geometry a run measured under (spec §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub depth: u32,
    pub repetitions: u32,
    pub max_tokens: u32,
}

/// One launch of a tune run — the baseline or one stage's candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trial {
    pub stage: String,
    pub value: Option<String>,
    pub argv: Vec<String>,
    pub stamp: FlagSextet,
    pub outcome: String,
    pub decode: Option<Summary>,
    pub prefill: Option<Summary>,
    pub prompt_n: Option<u64>,
    /// Speed limit before and after the probe (spec §6); either below 100
    /// marks the trial's clock as dirty without voiding it.
    pub speed_limit_pct: [Option<u32>; 2],
    pub reason: Option<String>,
    pub verdict: Option<String>,
}

/// A completed `chekov tune` run (spec §8), written after every trial so a
/// crash leaves the trials so far on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub model: String,
    pub quant: String,
    pub revision: String,
    pub machine_id: String,
    pub engine_build_commit: String,
    pub chekov_version: String,
    pub probe: Probe,
    /// The `[bench] significance_pct` this run judged at. Stamped beside the
    /// probe geometry because a verdict is only as strong as its threshold,
    /// and the report reads it rather than restating a default.
    pub significance_pct: f64,
    pub thermal_source: String,
    pub trials: Vec<Trial>,
    /// The final incumbent's argv, when it beat the baseline; `None` means
    /// `DEFAULTS_WON`.
    pub winner: Option<Vec<String>>,
    pub verdict: String,
}

/// `<dir>/<utc_compact_now>-<model>.json`, computed once per run.
#[must_use]
pub fn record_path(dir: &Path, model: &str) -> PathBuf {
    dir.join(format!("{}-{model}.json", utc_compact_now()))
}

/// Pretty-print `record` to `path`, creating the parent directory first.
pub fn write_record(path: &Path, record: &Record) -> Result<(), ChekovError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    let text = serde_json::to_string_pretty(record)
        .map_err(|e| ChekovError::io(format!("serializing {}", path.display()), e.into()))?;
    std::fs::write(path, text)
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))
}

#[cfg(test)]
mod tests {
    use super::{Candidate, Flag, Metric, Stage, candidates, rewrite, stages, value_of};
    use crate::core::config::TuneSection;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn a_flag_is_rewritten_under_either_spelling_or_appended_when_absent() {
        let long = argv(&["--flash-attn", "on", "--cache-type-k", "q8_0"]);
        assert_eq!(
            rewrite(&long, Flag::FlashAttn, "off"),
            argv(&["--flash-attn", "off", "--cache-type-k", "q8_0"])
        );
        let short = argv(&["-fa", "on", "-b", "2048"]);
        assert_eq!(
            rewrite(&short, Flag::BatchSize, "4096"),
            argv(&["-fa", "on", "-b", "4096"])
        );
        assert_eq!(
            rewrite(&short, Flag::UbatchSize, "1024"),
            argv(&["-fa", "on", "-b", "2048", "--ubatch-size", "1024"])
        );
        let twice = argv(&["-b", "512", "--batch-size", "1024"]);
        assert_eq!(
            rewrite(&twice, Flag::BatchSize, "2048"),
            argv(&["-b", "2048"]),
            "later duplicates are removed"
        );
        assert_eq!(value_of(&short, Flag::BatchSize).as_deref(), Some("2048"));
        assert_eq!(value_of(&short, Flag::UbatchSize), None);
    }

    #[test]
    fn stages_run_in_the_fixed_order_and_name_their_metric() {
        assert_eq!(stages(None).expect("all"), Stage::ORDER.to_vec());
        let picked = stages(Some(&argv(&["ubatch", "fa"]))).expect("subset");
        assert_eq!(
            picked,
            vec![Stage::Fa, Stage::Ubatch],
            "the argument's order does not matter"
        );
        assert!(stages(Some(&argv(&["threads"]))).is_err());
        assert_eq!(
            (Stage::Fa.metric(), Stage::Kv.metric()),
            (Metric::Decode, Metric::Decode)
        );
        assert_eq!(
            (Stage::Batch.metric(), Stage::Ubatch.metric()),
            (Metric::Prefill, Metric::Prefill)
        );
        assert_eq!(Metric::Decode.other(), Metric::Prefill);
    }

    #[test]
    fn kv_candidates_rewrite_k_and_v_together_and_the_incumbent_is_not_a_candidate() {
        let cfg = TuneSection::default();
        let incumbent = argv(&[
            "--flash-attn",
            "on",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q8_0",
        ]);
        let kv = candidates(Stage::Kv, &incumbent, &cfg);
        assert_eq!(
            kv.len(),
            1,
            "q8_0 is the incumbent; only f16 is a candidate"
        );
        assert_eq!(kv[0].value, "f16");
        assert_eq!(
            kv[0].argv,
            argv(&[
                "--flash-attn",
                "on",
                "--cache-type-k",
                "f16",
                "--cache-type-v",
                "f16"
            ])
        );
        let fa = candidates(Stage::Fa, &incumbent, &cfg);
        assert_eq!(
            fa.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
            vec!["off"]
        );
    }

    #[test]
    fn ubatch_candidates_never_exceed_the_incumbent_batch() {
        let cfg = TuneSection::default();
        let with_batch = argv(&["--batch-size", "1024"]);
        let ubatch_with_batch = candidates(Stage::Ubatch, &with_batch, &cfg);
        let values: Vec<&str> = ubatch_with_batch.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["256", "512", "1024"]);
        let ubatch_no_batch = candidates(Stage::Ubatch, &[], &cfg);
        let engine_default: Vec<&str> = ubatch_no_batch.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(
            engine_default,
            vec!["256", "512", "1024", "2048"],
            "no batch flag means the engine's 2048"
        );
        let batch_candidates = candidates(Stage::Batch, &with_batch, &cfg);
        let batch: Vec<&str> = batch_candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(batch, vec!["512", "2048", "4096"]);
    }

    #[test]
    fn a_candidate_carries_its_stage_and_value() {
        let c = Candidate {
            stage: Stage::Batch,
            value: "4096".into(),
            argv: argv(&["-b", "4096"]),
        };
        assert_eq!((c.stage.label(), c.value.as_str()), ("batch", "4096"));
    }

    fn summary(median: f64, spread: f64) -> crate::core::stats::Summary {
        crate::core::stats::Summary {
            median,
            p10: median - spread,
            p90: median + spread,
            n: 4,
            warmup_dropped: 1,
        }
    }
    fn measured(decode: f64, prefill: f64) -> super::Measured {
        super::Measured {
            decode: summary(decode, 0.3),
            prefill: summary(prefill, 3.0),
            prompt_n: 4101,
        }
    }

    #[test]
    fn a_candidate_wins_its_stage_only_on_its_own_metric_without_losing_the_other() {
        let inc = measured(31.2, 402.0);
        let judge = |candidate: &super::Measured, stage, significance_pct| {
            super::judge(
                candidate,
                &inc,
                super::JudgeCriteria {
                    stage,
                    significance_pct,
                },
            )
        };
        let faster_prefill = judge(&measured(31.1, 466.0), super::Stage::Batch, 5.0);
        assert!(faster_prefill.wins);
        assert_eq!(
            faster_prefill.phrase,
            "faster on prefill, decode not slower — new incumbent"
        );
        let costs_decode = judge(&measured(24.0, 466.0), super::Stage::Batch, 5.0);
        assert!(!costs_decode.wins);
        assert_eq!(
            costs_decode.phrase,
            "faster on prefill but slower on decode — incumbent kept"
        );
        let slower = judge(&measured(24.9, 397.0), super::Stage::Fa, 5.0);
        assert_eq!(
            (slower.wins, slower.phrase.as_str()),
            (false, "slower on decode")
        );
        let close = judge(&measured(31.3, 402.0), super::Stage::Fa, 5.0);
        assert_eq!(
            (close.wins, close.phrase.as_str()),
            (false, "no significant difference vs 31.2 — incumbent kept")
        );
        let batch_on_decode = judge(&measured(40.0, 402.0), super::Stage::Batch, 5.0);
        assert!(
            !batch_on_decode.wins,
            "a decode gain does not win a prefill stage"
        );
    }

    #[test]
    fn the_stage_winner_is_the_best_primary_median_among_winners_and_ties_keep_the_earlier() {
        let cand = |v: &str| super::Candidate {
            stage: super::Stage::Batch,
            value: v.into(),
            argv: vec![],
        };
        let win = |phrase: &str| super::Verdict {
            wins: true,
            phrase: phrase.into(),
        };
        let lose = super::Verdict {
            wins: false,
            phrase: "slower on prefill".into(),
        };
        let scored = vec![
            (cand("512"), measured(31.0, 288.0), lose),
            (cand("1024"), measured(31.0, 466.0), win("w")),
            (cand("4096"), measured(31.0, 466.0), win("w")),
            (cand("2048"), measured(31.0, 431.0), win("w")),
        ];
        let winner = super::pick_winner(&scored).expect("two winners");
        assert_eq!(winner.0.value, "1024");
        assert!(super::pick_winner(&scored[..1]).is_none());
    }

    #[test]
    fn a_trial_that_did_not_reach_the_depth_or_kept_too_few_samples_is_degenerate() {
        use crate::core::bench::sweep::DepthResult;
        let good = DepthResult {
            depth: 4096,
            prompt_n: 4101,
            cache_n: 0,
            decode_samples: vec![30.0, 31.0, 31.2],
            prefill_samples: vec![400.0, 402.0, 401.0],
            decode: crate::core::stats::summarize(&[30.0, 31.0, 31.2]),
            prefill: crate::core::stats::summarize(&[400.0, 402.0, 401.0]),
        };
        assert!(matches!(
            super::classify(&good, 4096),
            super::Outcome::Measured(_)
        ));
        let shallow = DepthResult {
            prompt_n: 1900,
            ..good.clone()
        };
        assert!(matches!(
            super::classify(&shallow, 4096),
            super::Outcome::Degenerate(r) if r.contains("1900") && r.contains("4096")
        ));
        let thin = DepthResult {
            decode: None,
            ..good
        };
        assert!(matches!(
            super::classify(&thin, 4096),
            super::Outcome::Degenerate(r) if r.contains("fewer than 2 samples")
        ));
    }

    #[test]
    fn a_stage_line_carries_the_cells_the_phrase_and_the_dirty_clock() {
        let m = measured(31.1, 466.0);
        let v = super::Verdict {
            wins: true,
            phrase: "faster on prefill, decode not slower — new incumbent".into(),
        };
        let line = super::stage_line(
            &super::CandidateLabel {
                stage: super::Stage::Ubatch,
                value: "1024",
            },
            &super::Outcome::Measured(m),
            &super::LineContext {
                verdict: Some(&v),
                dirty: Some(87),
            },
        );
        assert_eq!(
            line,
            "  ubatch     1024     decode 31.1 [30.8..31.4]  prefill 466 [463..469]   faster on prefill, decode not slower — new incumbent   clock was dirty (CPU_Speed_Limit 87%)"
        );
        let skipped = super::stage_line(
            &super::CandidateLabel {
                stage: super::Stage::Kv,
                value: "f16",
            },
            &super::Outcome::Skipped("exceeds the GPU budget by 4120 MiB".into()),
            &super::LineContext {
                verdict: None,
                dirty: None,
            },
        );
        assert_eq!(
            skipped,
            "  kv         f16      skipped: exceeds the GPU budget by 4120 MiB"
        );
    }

    #[test]
    fn the_thermal_readout_is_the_speed_limit_when_throttled_and_none_when_nominal() {
        assert_eq!(
            super::parse_therm("CPU_Speed_Limit \t= 87\nCPU_Available_CPUs = 24\n"),
            Some(87)
        );
        assert_eq!(
            super::parse_therm("Note: No thermal warning level has been recorded\n"),
            None
        );
        assert_eq!(super::parse_therm(""), None);
        assert_eq!(super::thermal_note(None, None), None);
        assert_eq!(super::thermal_note(Some(100), Some(87)), Some(87));
        assert_eq!(super::thermal_note(Some(100), Some(100)), None);
    }

    fn sample_record(argv: Vec<String>, stamp: super::FlagSextet) -> super::Record {
        super::Record {
            model: "m".into(),
            quant: "Q8_0".into(),
            revision: "abc123def456".into(),
            machine_id: "8d41f0c2a917".into(),
            engine_build_commit: "0f194b907".into(),
            chekov_version: "0.1.0".into(),
            probe: super::Probe {
                depth: 4096,
                repetitions: 5,
                max_tokens: 128,
            },
            significance_pct: 5.0,
            thermal_source: super::THERMAL_SOURCE.into(),
            trials: vec![super::Trial {
                stage: "baseline".into(),
                value: None,
                argv,
                stamp,
                outcome: "measured".into(),
                decode: Some(summary(31.2, 0.3)),
                prefill: Some(summary(402.0, 3.0)),
                prompt_n: Some(4101),
                speed_limit_pct: [None, Some(87)],
                reason: None,
                verdict: None,
            }],
            winner: None,
            verdict: super::DEFAULTS_WON.into(),
        }
    }

    #[test]
    fn a_record_round_trips_and_names_its_flag_sextet() {
        let argv = argv(&[
            "--flash-attn",
            "on",
            "-ctk",
            "q8_0",
            "--cache-type-v",
            "q8_0",
            "--batch-size",
            "4096",
        ]);
        let sextet = super::sextet(&argv);
        assert_eq!(
            (
                sextet.n_batch.as_str(),
                sextet.n_ubatch.as_str(),
                sextet.type_k.as_str(),
                sextet.flash_attn.as_str()
            ),
            ("4096", "engine-default", "q8_0", "on")
        );
        let record = sample_record(argv, sextet);
        let dir = std::env::temp_dir().join(format!("chekov-tune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = super::record_path(&dir, "m");
        assert!(
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("-m.json")),
            "{}",
            path.display()
        );
        super::write_record(&path, &record).expect("written");
        let back: super::Record =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(back.verdict, "defaults won");
        assert_eq!(back.trials[0].speed_limit_pct, [None, Some(87)]);
        assert!(back.winner.is_none());
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
