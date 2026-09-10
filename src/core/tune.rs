//! `chekov tune` stages, candidate lists, and argv rewriting (spec §4, §10).
//!
//! Pure, side-effect-free helpers: naming the five tune stages in their fixed
//! run order, listing the launch-flag values each stage tries, and rewriting
//! a launch argv to carry one candidate value under either flag spelling —
//! or, for the spec stage's `off`, to carry none.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::bench::sweep::{self, DepthResult, ProbeExec, SweepPlan};
use crate::core::clock::utc_compact_now;
use crate::core::config::TuneSection;
use crate::core::machine::pmset_therm;
use crate::core::stats::{Comparison, Summary, compare};
use crate::error::ChekovError;

/// llama-server's default `--batch-size` when the flag is absent, per
/// `llama-server --help` on this machine.
pub(crate) const ENGINE_DEFAULT_BATCH: u32 = 2048;

/// llama-server's default `--spec-draft-n-max` when the flag is absent, per
/// `llama-server --help`.
///
/// The draft length the 2026-09-01 spike found to be a net loss on a
/// 3B-active mixture-of-experts model.
pub(crate) const ENGINE_DEFAULT_SPEC_DRAFT_N_MAX: u32 = 3;

/// One dimension `chekov tune` sweeps, in the fixed order they run.
///
/// `Spec` runs first because the other four tune the kernel and batch
/// geometry around whatever decode path is active (spec-stage design §3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Spec,
    Fa,
    Kv,
    Batch,
    Ubatch,
}

impl Stage {
    pub const ORDER: [Self; 5] = [Self::Spec, Self::Fa, Self::Kv, Self::Batch, Self::Ubatch];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Spec => "spec",
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
            Self::Spec | Self::Fa | Self::Kv => Metric::Decode,
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
    SpecType,
    SpecDraftNMax,
}

impl Flag {
    /// Every spelling the engine accepts, short first; the LAST entry is the
    /// long spelling `rewrite` appends under.
    #[must_use]
    pub const fn names(self) -> &'static [&'static str] {
        match self {
            Self::FlashAttn => &["-fa", "--flash-attn"],
            Self::CacheTypeK => &["-ctk", "--cache-type-k"],
            Self::CacheTypeV => &["-ctv", "--cache-type-v"],
            Self::BatchSize => &["-b", "--batch-size"],
            Self::UbatchSize => &["-ub", "--ubatch-size"],
            Self::SpecType => &["--spec-type"],
            Self::SpecDraftNMax => &["--spec-draft-n-max"],
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
        out.extend(names.last().map(|name| (*name).to_owned()));
        out.push(value.to_owned());
    }
    out
}

/// `argv` without `flag` and its value, wherever and however often it appears.
///
/// The one tune rewrite that removes a flag, because "no speculative
/// decoding" is the absence of `--spec-type`, not a value of it.
#[must_use]
pub fn strip(argv: &[String], flag: Flag) -> Vec<String> {
    let names = flag.names();
    let mut out = Vec::with_capacity(argv.len());
    let mut index = 0;
    while index < argv.len() {
        if names.contains(&argv[index].as_str()) {
            index += if argv.get(index + 1).is_some() { 2 } else { 1 };
            continue;
        }
        out.push(argv[index].clone());
        index += 1;
    }
    out
}

/// One of the engine's history-based drafters (`--spec-type ngram-*`).
///
/// No head, no draft file — it proposes runs of tokens the model already
/// saw (n-gram spec-stage design §3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NgramType {
    Simple,
    MapK,
    MapK4v,
    Mod,
    Cache,
}

impl NgramType {
    pub const ALL: [Self; 5] = [
        Self::Simple,
        Self::MapK,
        Self::MapK4v,
        Self::Mod,
        Self::Cache,
    ];

    /// The engine's own spelling, as `--spec-type` takes it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Simple => "ngram-simple",
            Self::MapK => "ngram-map-k",
            Self::MapK4v => "ngram-map-k4v",
            Self::Mod => "ngram-mod",
            Self::Cache => "ngram-cache",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.label() == name)
    }

    /// Whether the drafter keeps its table across requests (design §13).
    ///
    /// `ngram-mod` shares one table over every sequence and resets it only
    /// on occupancy; `ngram-cache`'s per-request reset is a no-op. On a probe
    /// that repeats one prompt, both replay the first reply.
    #[must_use]
    pub const fn keeps_memory(self) -> bool {
        matches!(self, Self::Mod | Self::Cache)
    }
}

/// One `[tune] spec_drafts` entry, parsed at the plan boundary (spec-stage
/// design §3; n-gram design §3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpecDraft {
    Off,
    Mtp(u32),
    Ngram(NgramType),
}

impl SpecDraft {
    /// `off`, `mtp:<n>` with `n ≥ 1`, or `ngram:<type>` naming one of the
    /// engine's five history-based drafters; anything else is the named error.
    pub fn parse(value: &str) -> Result<Self, ChekovError> {
        let bad = || ChekovError::TuneBadSpecCandidate {
            value: value.to_owned(),
        };
        if value == "off" {
            return Ok(Self::Off);
        }
        if let Some(name) = value.strip_prefix("ngram:") {
            return NgramType::parse(name).map(Self::Ngram).ok_or_else(bad);
        }
        let length = value.strip_prefix("mtp:").ok_or_else(bad)?;
        match length.parse::<u32>() {
            Ok(n) if n >= 1 => Ok(Self::Mtp(n)),
            _ => Err(bad()),
        }
    }

    /// The spelling `stage_line` and the record carry.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::Mtp(n) => format!("mtp:{n}"),
            Self::Ngram(t) => format!("ngram:{}", t.label()),
        }
    }
}

/// Every `[tune] spec_drafts` entry parsed, or the first bad one named.
pub fn spec_values(cfg: &TuneSection) -> Result<Vec<SpecDraft>, ChekovError> {
    cfg.spec_drafts
        .iter()
        .map(|v| SpecDraft::parse(v))
        .collect()
}

/// `incumbent` carrying `value` for the spec stage: both flags rewritten
/// together for `mtp:<n>`, both stripped for `off` (spec-stage design §3),
/// the type written and the length stripped for `ngram:<type>` — the
/// length is the head's, inert for a history-based drafter (n-gram §3).
fn apply_spec(incumbent: &[String], value: &str) -> Vec<String> {
    match SpecDraft::parse(value) {
        Ok(SpecDraft::Mtp(n)) => {
            let typed = rewrite(incumbent, Flag::SpecType, "draft-mtp");
            rewrite(&typed, Flag::SpecDraftNMax, &n.to_string())
        }
        Ok(SpecDraft::Ngram(t)) => {
            let typed = rewrite(incumbent, Flag::SpecType, t.label());
            strip(&typed, Flag::SpecDraftNMax)
        }
        // `off`, or a value `spec_values` already refused at plan time.
        Ok(SpecDraft::Off) | Err(_) => strip_spec(incumbent),
    }
}

/// `argv` without either speculative flag.
fn strip_spec(argv: &[String]) -> Vec<String> {
    strip(&strip(argv, Flag::SpecType), Flag::SpecDraftNMax)
}

/// The value `flag` currently carries in `argv`, under either spelling.
#[must_use]
pub fn value_of(argv: &[String], flag: Flag) -> Option<String> {
    let names = flag.names();
    let position = argv.iter().position(|a| names.contains(&a.as_str()))?;
    argv.get(position + 1).cloned()
}

/// The flags `applied_extra_flags` copies from a winner.
const APPLIED_FLAGS: [Flag; 7] = [
    Flag::FlashAttn,
    Flag::CacheTypeK,
    Flag::CacheTypeV,
    Flag::BatchSize,
    Flag::UbatchSize,
    Flag::SpecType,
    Flag::SpecDraftNMax,
];

/// `current` with every flag the winner carries rewritten to the winner's
/// value; a flag the winner does not carry is left untouched (spec §8).
///
/// Except the speculative pair: a winner without `--spec-type` is one the
/// spec stage switched off, and the current flags must lose theirs too
/// (spec-stage design §5).
#[must_use]
pub fn applied_extra_flags(current: &[String], winner: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    for flag in APPLIED_FLAGS {
        if let Some(value) = value_of(winner, flag) {
            out = rewrite(&out, flag, &value);
        }
    }
    if value_of(winner, Flag::SpecType).is_none() {
        out = strip_spec(&out);
    } else if value_of(winner, Flag::SpecDraftNMax).is_none() {
        // The winner is a full argv derived from `current`: a length it
        // lacks is one the stage removed (an n-gram winner), never one it
        // forgot (n-gram design §13).
        out = strip(&out, Flag::SpecDraftNMax);
    }
    out
}

/// TOML-style double-quoted, comma-space separated array literal.
fn quoted_array(values: &[String]) -> String {
    let quoted: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
    format!("[{}]", quoted.join(", "))
}

/// The `models.toml [models.<name>]` diff `--apply` prints before writing
/// (spec §8): `extra_flags` before, then after.
#[must_use]
pub fn apply_diff(name: &str, before: &[String], after: &[String]) -> String {
    format!(
        "models.toml [models.{name}]\n- extra_flags = {}\n+ extra_flags = {}\n",
        quoted_array(before),
        quoted_array(after)
    )
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
        Stage::Spec => cfg.spec_drafts.clone(),
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
/// rewrites K and V together, `Spec` rewrites or strips its pair.
fn apply(stage: Stage, incumbent: &[String], value: &str) -> Vec<String> {
    match stage {
        Stage::Spec => apply_spec(incumbent, value),
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
    pub decode: Summary,
    pub prefill: Summary,
    pub prompt_n: u64,
    /// Draft tokens proposed and accepted over every repetition, the warmup
    /// included — zero-both when nothing drafted (n-gram design §13).
    pub draft_n: u64,
    pub draft_n_accepted: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthMeasurement {
    pub depth: u32,
    pub measured: Measured,
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
    Measured(Vec<DepthMeasurement>),
    Incomplete {
        measurements: Vec<DepthMeasurement>,
        reason: String,
    },
    Degenerate(String),
    Skipped(String),
}

impl Outcome {
    #[must_use]
    pub fn measurements(&self) -> &[DepthMeasurement] {
        match self {
            Self::Measured(measurements) | Self::Incomplete { measurements, .. } => measurements,
            Self::Degenerate(_) | Self::Skipped(_) => &[],
        }
    }
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
    Outcome::Measured(vec![DepthMeasurement {
        depth,
        measured: Measured {
            decode: decode.clone(),
            prefill: prefill.clone(),
            prompt_n: result.prompt_n,
            draft_n: result.draft_n,
            draft_n_accepted: result.draft_n_accepted,
        },
    }])
}

pub fn validate_probe(plan: &SweepPlan, ctx_size: u32) -> Result<(), String> {
    if plan.depths.is_empty()
        || plan.depths.contains(&0)
        || plan.depths.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err("set [tune] depths to strictly increasing positive token counts".into());
    }
    if plan.repetitions < 3 {
        return Err(
            "set [bench] repetitions to at least 3: one warmup and two measured samples per depth"
                .into(),
        );
    }
    if plan.max_tokens == 0 {
        return Err("set [bench] max_tokens to a positive completion budget".into());
    }
    if let Some(depth) = plan
        .depths
        .iter()
        .find(|&&depth| u64::from(depth) + u64::from(plan.max_tokens) > u64::from(ctx_size))
    {
        return Err(format!(
            "probe depth {depth} plus {} generated tokens exceeds ctx {ctx_size}; \
             reduce [tune] depths or increase this model's ctx_size",
            plan.max_tokens
        ));
    }
    Ok(())
}

pub fn measure_depths(plan: &SweepPlan, exec: &mut ProbeExec) -> Outcome {
    if plan.depths.is_empty() {
        return Outcome::Degenerate("no probe depths; configure [tune] depths".into());
    }
    let mut measurements = Vec::new();
    for &depth in &plan.depths {
        let outcome = sweep::measure_depth(plan, depth, exec).map_or_else(
            |error| Outcome::Degenerate(error.to_string()),
            |result| classify(&result, depth),
        );
        match outcome {
            Outcome::Measured(mut measured) => measurements.append(&mut measured),
            Outcome::Degenerate(reason)
            | Outcome::Skipped(reason)
            | Outcome::Incomplete { reason, .. } => {
                return Outcome::Incomplete {
                    measurements,
                    reason: format!("depth {depth}: {reason}"),
                };
            }
        }
    }
    Outcome::Measured(measurements)
}

/// Whether a candidate replaces the incumbent, and the report phrase either
/// way (spec §4).
pub struct Verdict {
    pub wins: bool,
    pub phrase: String,
}

/// What the other metric did, and what the guard allows it to do: the
/// incumbent's primary median (for the no-difference phrase), the candidate's
/// loss on the other metric in percent of the incumbent's median (negative
/// when it lost), and the tolerance the run was configured with.
struct Guard {
    incumbent_median: f64,
    other_delta_pct: f64,
    tolerance_pct: f64,
}

/// The verdict phrase for one stage's primary/other comparison, and whether
/// it wins. `comparisons` is `(primary, other)` — bundled so the helper stays
/// at this crate's clippy argument floor (`clippy.toml`, §3.4).
fn phrase(comparisons: (Comparison, Comparison), stage: Stage, guard: &Guard) -> (bool, String) {
    let (primary, other) = comparisons;
    let primary_label = stage.metric().label();
    let other_label = stage.metric().other().label();
    let lost = guard.other_delta_pct;
    let tolerance = guard.tolerance_pct;
    match primary {
        Comparison::Faster if other != Comparison::Slower => (
            true,
            format!("faster on {primary_label}, {other_label} not slower — new incumbent"),
        ),
        Comparison::Faster if -lost <= tolerance => (
            true,
            format!(
                "faster on {primary_label}, {other_label} {lost:+.0}% is within the \
                 {tolerance:.0}% guard — new incumbent"
            ),
        ),
        Comparison::Faster => (
            false,
            format!(
                "faster on {primary_label} but {other_label} {lost:+.0}% is beyond the \
                 {tolerance:.0}% guard — incumbent kept"
            ),
        ),
        Comparison::Slower => (false, format!("slower on {primary_label} — incumbent kept")),
        Comparison::NoSignificantDifference => (
            false,
            format!(
                "no significant difference vs {:.1} — incumbent kept",
                guard.incumbent_median
            ),
        ),
    }
}

/// `stage` plus the two thresholds `judge` compares under: the significance
/// a difference must reach, and how much of the other metric a winner may
/// give up.
///
/// Bundled so `judge` stays at this crate's clippy argument floor
/// (`clippy.toml`, §3.4) despite the spec's logically independent inputs.
#[derive(Clone, Copy)]
pub struct JudgeCriteria {
    pub stage: Stage,
    pub significance_pct: f64,
    pub guard_tolerance_pct: f64,
}

/// Judges `candidate` against `incumbent` on `criteria.stage`'s primary
/// metric (spec §4).
///
/// It wins when the other metric does not regress — or regresses by no more
/// than the guard tolerance (2026-09-03).
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
    let guard = Guard {
        incumbent_median: incumbent.by_metric(metric).median,
        other_delta_pct: delta_pct(
            candidate.by_metric(other).median,
            incumbent.by_metric(other).median,
        ),
        tolerance_pct: criteria.guard_tolerance_pct,
    };
    let (wins, text) = phrase((primary_cmp, other_cmp), criteria.stage, &guard);
    Verdict { wins, phrase: text }
}

#[must_use]
pub fn judge_depths(
    candidate: &[DepthMeasurement],
    incumbent: &[DepthMeasurement],
    criteria: JudgeCriteria,
) -> Verdict {
    let Some((first, baseline)) = candidate.first().zip(incumbent.first()) else {
        return depth_refusal("missing probe depths");
    };
    if candidate.len() != incumbent.len()
        || candidate
            .iter()
            .zip(incumbent)
            .any(|(a, b)| a.depth != b.depth)
    {
        return depth_refusal("probe depths differ");
    }
    let primary = judge(&first.measured, &baseline.measured, criteria);
    if candidate.len() == 1 {
        return primary;
    }
    let shallow = primary
        .phrase
        .trim_end_matches(" — new incumbent")
        .trim_end_matches(" — incumbent kept");
    let mut notes = vec![format!("depth {}: {shallow}", first.depth)];
    let mut wins = primary.wins;
    for pair in candidate.iter().zip(incumbent).skip(1) {
        let (passed, note) = guard_depth(pair, &criteria);
        wins &= passed;
        notes.push(note);
    }
    let decision = if wins {
        "new incumbent"
    } else {
        "incumbent kept"
    };
    Verdict {
        wins,
        phrase: format!("{} — {decision}", notes.join("; ")),
    }
}

fn depth_refusal(reason: &str) -> Verdict {
    Verdict {
        wins: false,
        phrase: format!("{reason} — incumbent kept"),
    }
}

fn guard_depth(
    pair: (&DepthMeasurement, &DepthMeasurement),
    criteria: &JudgeCriteria,
) -> (bool, String) {
    let checks: Vec<_> = [Metric::Decode, Metric::Prefill]
        .into_iter()
        .map(|metric| {
            let summaries = (
                pair.0.measured.by_metric(metric),
                pair.1.measured.by_metric(metric),
            );
            guard_metric(summaries, metric, criteria)
        })
        .collect();
    let passed = checks.iter().all(|(passed, _)| *passed);
    let notes: Vec<_> = checks.into_iter().map(|(_, note)| note).collect();
    (
        passed,
        format!("depth {}: {}", pair.0.depth, notes.join(", ")),
    )
}

fn guard_metric(
    pair: (&Summary, &Summary),
    metric: Metric,
    criteria: &JudgeCriteria,
) -> (bool, String) {
    let label = metric.label();
    if compare(pair.0, pair.1, criteria.significance_pct) != Comparison::Slower {
        return (true, format!("{label} not slower"));
    }
    let delta = delta_pct(pair.0.median, pair.1.median);
    let tolerance = criteria.guard_tolerance_pct;
    let passed = -delta <= tolerance;
    let relation = if passed { "within" } else { "beyond" };
    (
        passed,
        format!("{label} {delta:+.0}% is {relation} the {tolerance:.0}% guard"),
    )
}

/// `candidate` relative to `incumbent` in percent, negative for a loss; an
/// incumbent median of zero has no percentage and reads as no loss.
fn delta_pct(candidate: f64, incumbent: f64) -> f64 {
    if incumbent <= 0.0 {
        return 0.0;
    }
    (candidate - incumbent) / incumbent * 100.0
}

/// The stage's winning candidate: the highest primary median among those
/// that beat the incumbent, ties kept at the earlier candidate.
#[must_use]
pub fn pick_winner(scored: &[ScoredTrial]) -> Option<&ScoredTrial> {
    let mut best: Option<(&ScoredTrial, f64)> = None;
    for entry @ (candidate, measured, verdict) in scored {
        if !verdict.wins {
            continue;
        }
        let Some(shallow) = measured.first() else {
            continue;
        };
        let median = shallow.measured.by_metric(candidate.stage.metric()).median;
        match best {
            Some((_, best_median)) if median <= best_median => {}
            _ => best = Some((entry, median)),
        }
    }
    best.map(|(entry, _)| entry)
}

pub type ScoredTrial = (Candidate, Vec<DepthMeasurement>, Verdict);

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

pub(crate) fn depth_lines(label: &CandidateLabel, depths: &[DepthMeasurement]) -> String {
    depths
        .iter()
        .map(|entry| {
            format!(
                "\n    depth {}: {}{}",
                entry.depth,
                measured_cells(&entry.measured),
                accept_note(label, &entry.measured)
            )
        })
        .collect::<Vec<_>>()
        .concat()
}

/// The dirty-clock note appended to a report line, or empty when the clock
/// was not dirty.
fn dirty_note(dirty: Option<u32>) -> String {
    dirty.map_or_else(String::new, |pct| {
        format!("   clock was dirty (CPU_Speed_Limit {pct}%)")
    })
}

/// `   acceptance 63% (189 of 300 drafted)` for a trial that drafted.
///
/// `   no drafts` for a spec-stage candidate that did not — the expected
/// reading of a history-based drafter on a prompt with nothing to match
/// (n-gram design §13); nothing for any other trial.
#[must_use]
pub fn accept_note(label: &CandidateLabel, measured: &Measured) -> String {
    if measured.draft_n > 0 {
        let pct = u128::from(measured.draft_n_accepted) * 200 / u128::from(measured.draft_n);
        return format!(
            "   acceptance {}% ({} of {} drafted)",
            pct.div_ceil(2).min(100),
            measured.draft_n_accepted,
            measured.draft_n
        );
    }
    let speculative = label.stage == Stage::Spec && label.value != "off";
    if speculative {
        "   no drafts".to_owned()
    } else {
        String::new()
    }
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
        Outcome::Measured(depths) => {
            let Some(first) = depths.first() else {
                return format!("  {stage:<10} {value:<8} degenerate: no probe depths");
            };
            let cells = measured_cells(&first.measured);
            let phrase = context.verdict.map_or("", |v| v.phrase.as_str());
            let note = dirty_note(context.dirty);
            let accept = accept_note(label, &first.measured);
            let extra = if depths.len() > 1 {
                depth_lines(label, depths)
            } else {
                String::new()
            };
            format!("  {stage:<10} {value:<8} {cells}   {phrase}{note}{accept}{extra}")
        }
        Outcome::Incomplete {
            measurements,
            reason,
        } => format!(
            "  {stage:<10} {value:<8} degenerate: {reason}{}",
            depth_lines(label, measurements)
        ),
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

/// The probe geometry a run measured under (spec §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub depth: u32,
    #[serde(default)]
    pub depths: Vec<u32>,
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
    /// The bench stamp's flag set, read off `argv` the same way `build_head`
    /// reads a run's, so a trial and a run describe a configuration in the
    /// same words (spec §8).
    pub stamp: crate::core::bench::stamp::LaunchFlags,
    pub outcome: String,
    pub decode: Option<Summary>,
    pub prefill: Option<Summary>,
    pub prompt_n: Option<u64>,
    #[serde(default)]
    pub depths: Vec<DepthMeasurement>,
    /// Draft tokens proposed and accepted over the probe's repetitions
    /// (n-gram design §13). Records from before the fields load as zero-both.
    #[serde(default)]
    pub draft_n: u64,
    #[serde(default)]
    pub draft_n_accepted: u64,
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
    /// The `[tune] guard_tolerance_pct` this run judged under — how much of
    /// the other metric a winner was allowed to give up. A record from before
    /// the knob was judged on the strict rule, which is what `0` says.
    #[serde(default)]
    pub guard_tolerance_pct: f64,
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
    use super::{
        Candidate, Flag, Metric, NgramType, SpecDraft, Stage, candidates, rewrite, spec_values,
        stages, strip, value_of,
    };
    use crate::core::config::TuneSection;
    use crate::error::ChekovError;

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
        let appended = rewrite(&argv(&["-fa", "on"]), Flag::SpecType, "draft-mtp");
        assert_eq!(
            appended,
            argv(&["-fa", "on", "--spec-type", "draft-mtp"]),
            "a one-spelling flag appends itself"
        );
    }

    #[test]
    fn apply_rewrites_only_the_flags_the_winner_carries_and_shows_the_diff() {
        let current = argv(&["--reasoning-format", "none", "--batch-size", "2048"]);
        let winner = argv(&[
            "--flash-attn",
            "on",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q8_0",
            "--batch-size",
            "4096",
            "--ubatch-size",
            "1024",
        ]);
        let after = super::applied_extra_flags(&current, &winner);
        assert_eq!(
            after,
            argv(&[
                "--reasoning-format",
                "none",
                "--batch-size",
                "4096",
                "--flash-attn",
                "on",
                "--cache-type-k",
                "q8_0",
                "--cache-type-v",
                "q8_0",
                "--ubatch-size",
                "1024",
            ])
        );
        let diff = super::apply_diff("m", &current, &after);
        assert!(
            diff.starts_with(
                "models.toml [models.m]\n\
                 - extra_flags = [\"--reasoning-format\", \"none\", \"--batch-size\", \"2048\"]\n\
                 + extra_flags = ["
            ),
            "{diff}"
        );
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
            Stage::ORDER,
            [
                Stage::Spec,
                Stage::Fa,
                Stage::Kv,
                Stage::Batch,
                Stage::Ubatch
            ]
        );
        assert_eq!(Stage::parse("spec"), Some(Stage::Spec));
        assert_eq!(
            (Stage::Spec.metric(), Stage::Fa.metric(), Stage::Kv.metric()),
            (Metric::Decode, Metric::Decode, Metric::Decode)
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
            draft_n: 0,
            draft_n_accepted: 0,
        }
    }

    fn single_depth(measured: super::Measured) -> Vec<super::DepthMeasurement> {
        vec![super::DepthMeasurement {
            depth: 4096,
            measured,
        }]
    }

    fn depth_measurements(values: &[(u32, f64, f64)]) -> Vec<super::DepthMeasurement> {
        values
            .iter()
            .map(|&(depth, decode, prefill)| super::DepthMeasurement {
                depth,
                measured: super::Measured {
                    prompt_n: u64::from(depth),
                    ..measured(decode, prefill)
                },
            })
            .collect()
    }

    fn probe_artifact(depth: u32) -> crate::core::bench::runner::ProbeArtifact {
        crate::core::bench::runner::ProbeArtifact {
            anthropic_body: "{}".into(),
            timings: crate::core::bench::runner::Timings {
                prompt_n: u64::from(depth),
                prompt_per_second: 300.0,
                predicted_n: 32,
                predicted_per_second: 50.0,
                cache_n: 0,
                draft_n: 10,
                draft_n_accepted: 6,
                thinking_chars: 0,
                answer_chars: 32,
            },
        }
    }

    #[test]
    fn multi_depth_tune_measures_full_repetitions_and_drops_each_depths_warmup() {
        let plan = super::SweepPlan {
            depths: vec![4096, 65536],
            repetitions: 5,
            max_tokens: 32,
        };
        let mut requests = Vec::new();
        let outcome = super::measure_depths(&plan, &mut |request| {
            let depth = if requests.len() < 5 { 4096 } else { 65536 };
            requests.push(request.body.len());
            Ok(probe_artifact(depth))
        });
        let super::Outcome::Measured(depths) = outcome else {
            panic!("both depths measured")
        };
        assert_eq!(requests.len(), 10);
        assert!(
            requests[5] > requests[0] * 8,
            "the deep request contains the deeper prompt"
        );
        assert_eq!(
            depths.iter().map(|entry| entry.depth).collect::<Vec<_>>(),
            plan.depths
        );
        for entry in depths {
            assert_eq!((entry.measured.decode.n, entry.measured.prefill.n), (4, 4));
            assert_eq!(
                (
                    entry.measured.decode.warmup_dropped,
                    entry.measured.prefill.warmup_dropped
                ),
                (1, 1)
            );
            assert_eq!(
                (entry.measured.draft_n, entry.measured.draft_n_accepted),
                (50, 30)
            );
        }
    }

    #[test]
    fn multi_depth_tune_preserves_shallow_results_when_a_deep_probe_fails() {
        let plan = super::SweepPlan {
            depths: vec![4096, 65536],
            repetitions: 5,
            max_tokens: 32,
        };
        let mut calls = 0;
        let outcome = super::measure_depths(&plan, &mut |_| {
            calls += 1;
            if calls == 6 {
                return Err(crate::error::ChekovError::TuneBaselineDegenerate {
                    name: "fake".into(),
                    reason: "upstream stopped".into(),
                });
            }
            Ok(probe_artifact(4096))
        });
        assert_eq!(calls, 6);
        let super::Outcome::Incomplete {
            measurements,
            reason,
        } = outcome
        else {
            panic!("a partial run cannot become a measured candidate")
        };
        assert_eq!(measurements.len(), 1);
        assert_eq!(measurements[0].measured.decode.n, 4);
        assert!(
            reason.contains("65536") && reason.contains("upstream stopped"),
            "{reason}"
        );
    }

    fn depth_verdict(values: &[(u32, f64, f64)], tolerance: f64) -> super::Verdict {
        let incumbent = depth_measurements(&[(4096, 100.0, 400.0), (65536, 50.0, 200.0)]);
        super::judge_depths(
            &depth_measurements(values),
            &incumbent,
            super::JudgeCriteria {
                stage: super::Stage::Fa,
                significance_pct: 5.0,
                guard_tolerance_pct: tolerance,
            },
        )
    }

    #[test]
    fn multi_depth_tune_rejects_a_shallow_win_with_a_deep_decode_regression() {
        let verdict = depth_verdict(&[(4096, 120.0, 400.0), (65536, 30.0, 200.0)], 15.0);
        assert!(!verdict.wins);
        assert!(verdict.phrase.contains("65536"), "{}", verdict.phrase);
        assert!(verdict.phrase.contains("decode -40%"), "{}", verdict.phrase);
        assert!(
            !verdict.phrase.contains("new incumbent"),
            "{}",
            verdict.phrase
        );
    }

    #[test]
    fn multi_depth_tune_guards_both_metrics_at_depth() {
        let verdict = depth_verdict(&[(4096, 120.0, 400.0), (65536, 50.0, 140.0)], 15.0);
        assert!(!verdict.wins);
        assert!(
            verdict.phrase.contains("prefill -30%"),
            "{}",
            verdict.phrase
        );
    }

    #[test]
    fn multi_depth_tune_accepts_a_deep_loss_only_within_the_configured_guard() {
        let values = [(4096, 120.0, 400.0), (65536, 45.0, 180.0)];
        let tolerated = depth_verdict(&values, 15.0);
        assert!(tolerated.wins, "{}", tolerated.phrase);
        assert!(tolerated.phrase.contains("4096"), "{}", tolerated.phrase);
        assert!(tolerated.phrase.contains("65536"), "{}", tolerated.phrase);
        assert!(tolerated.phrase.contains("within"), "{}", tolerated.phrase);
        assert!(!depth_verdict(&values, 0.0).wins);
    }

    #[test]
    fn multi_depth_tune_requires_a_shallow_win_even_when_the_deep_probe_wins() {
        let verdict = depth_verdict(&[(4096, 100.0, 400.0), (65536, 80.0, 250.0)], 15.0);
        assert!(!verdict.wins);
        assert!(verdict.phrase.contains("no significant difference"));
    }

    #[test]
    fn multi_depth_tune_refuses_missing_mismatched_or_empty_depth_sets() {
        for values in [
            vec![(4096, 120.0, 400.0)],
            vec![(4096, 120.0, 400.0), (32768, 50.0, 200.0)],
            vec![],
        ] {
            let verdict = depth_verdict(&values, 15.0);
            assert!(!verdict.wins);
            assert!(verdict.phrase.contains("depth"), "{}", verdict.phrase);
        }
    }

    #[test]
    fn multi_depth_tune_checks_every_guard_depth() {
        let incumbent = depth_measurements(&[
            (4096, 100.0, 400.0),
            (32768, 70.0, 300.0),
            (65536, 50.0, 200.0),
        ]);
        let candidate = depth_measurements(&[
            (4096, 120.0, 400.0),
            (32768, 70.0, 300.0),
            (65536, 20.0, 200.0),
        ]);
        let verdict = super::judge_depths(
            &candidate,
            &incumbent,
            super::JudgeCriteria {
                stage: super::Stage::Fa,
                significance_pct: 5.0,
                guard_tolerance_pct: 15.0,
            },
        );
        assert!(!verdict.wins);
        assert!(verdict.phrase.contains("65536"), "{}", verdict.phrase);
    }

    #[test]
    fn multi_depth_tune_selects_only_a_winner_that_passes_the_deep_guard() {
        let mut trials = Vec::new();
        for (value, shallow, deep) in [("safe", 120.0, 50.0), ("unsafe", 150.0, 20.0)] {
            let values = [(4096, shallow, 400.0), (65536, deep, 200.0)];
            trials.push((
                super::Candidate {
                    stage: super::Stage::Fa,
                    value: value.into(),
                    argv: vec![],
                },
                depth_measurements(&values),
                depth_verdict(&values, 15.0),
            ));
        }
        let winner = super::pick_winner(&trials).expect("the safe candidate wins");
        assert_eq!(winner.0.value, "safe");
        assert!((winner.1[1].measured.decode.median - 50.0).abs() < f64::EPSILON);
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
                    guard_tolerance_pct: 15.0,
                },
            )
        };
        let faster_prefill = judge(&measured(31.1, 466.0), super::Stage::Batch, 5.0);
        assert!(faster_prefill.wins);
        assert_eq!(
            faster_prefill.phrase,
            "faster on prefill, decode not slower — new incumbent"
        );
        let slower = judge(&measured(24.9, 397.0), super::Stage::Fa, 5.0);
        assert_eq!(
            (slower.wins, slower.phrase.as_str()),
            (false, "slower on decode — incumbent kept")
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

    /// A loss on the other metric inside the guard is a trade the stage may
    /// make; beyond it the incumbent is kept — and the phrase says how much
    /// was lost against which tolerance either way.
    #[test]
    fn a_loss_on_the_other_metric_is_judged_against_the_guard() {
        let inc = measured(31.2, 402.0);
        let judge = |candidate: &super::Measured| {
            super::judge(
                candidate,
                &inc,
                super::JudgeCriteria {
                    stage: super::Stage::Batch,
                    significance_pct: 5.0,
                    guard_tolerance_pct: 15.0,
                },
            )
        };
        let costs_decode = judge(&measured(24.0, 466.0));
        assert!(!costs_decode.wins);
        assert_eq!(
            costs_decode.phrase,
            "faster on prefill but decode -23% is beyond the 15% guard — incumbent kept"
        );
        let within = judge(&measured(28.0, 466.0));
        assert!(
            within.wins,
            "a loss inside the guard is a trade the stage may make"
        );
        assert_eq!(
            within.phrase,
            "faster on prefill, decode -10% is within the 15% guard — new incumbent"
        );
    }

    /// `guard_tolerance_pct = 0` is the strict guard the stage shipped with:
    /// any significant loss on the other metric keeps the incumbent, and the
    /// phrase still says how much was lost.
    #[test]
    fn a_zero_guard_is_the_strict_rule() {
        let inc = measured(31.2, 402.0);
        let strict = super::judge(
            &measured(28.0, 466.0),
            &inc,
            super::JudgeCriteria {
                stage: super::Stage::Batch,
                significance_pct: 5.0,
                guard_tolerance_pct: 0.0,
            },
        );
        assert!(!strict.wins);
        assert_eq!(
            strict.phrase,
            "faster on prefill but decode -10% is beyond the 0% guard — incumbent kept"
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
            phrase: "slower on prefill — incumbent kept".into(),
        };
        let scored = vec![
            (cand("512"), single_depth(measured(31.0, 288.0)), lose),
            (cand("1024"), single_depth(measured(31.0, 466.0)), win("w")),
            (cand("4096"), single_depth(measured(31.0, 466.0)), win("w")),
            (cand("2048"), single_depth(measured(31.0, 431.0)), win("w")),
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
            draft_n: 0,
            draft_n_accepted: 0,
            thinking_chars: 0,
            answer_chars: 0,
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
            &super::Outcome::Measured(single_depth(m)),
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
    fn a_stage_line_renders_the_fa_off_quantized_kv_skip() {
        let skipped = super::stage_line(
            &super::CandidateLabel {
                stage: super::Stage::Fa,
                value: "off",
            },
            &super::Outcome::Skipped(
                "fa off requires unquantized KV — llama.cpp refuses the combination; \
                 skipped under a q8_0 incumbent"
                    .into(),
            ),
            &super::LineContext {
                verdict: None,
                dirty: None,
            },
        );
        assert_eq!(
            skipped,
            "  fa         off      skipped: fa off requires unquantized KV — llama.cpp refuses the combination; skipped under a q8_0 incumbent"
        );
    }

    #[test]
    fn a_stage_line_names_the_degenerate_branch() {
        let degenerate = super::stage_line(
            &super::CandidateLabel {
                stage: super::Stage::Fa,
                value: "off",
            },
            &super::Outcome::Degenerate("prompt_n 1900 short of depth 4096".into()),
            &super::LineContext {
                verdict: None,
                dirty: None,
            },
        );
        assert_eq!(
            degenerate,
            "  fa         off      degenerate: prompt_n 1900 short of depth 4096"
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

    fn sample_record(
        argv: Vec<String>,
        stamp: crate::core::bench::stamp::LaunchFlags,
    ) -> super::Record {
        super::Record {
            model: "m".into(),
            quant: "Q8_0".into(),
            revision: "abc123def456".into(),
            machine_id: "8d41f0c2a917".into(),
            engine_build_commit: "0f194b907".into(),
            chekov_version: "0.1.0".into(),
            probe: super::Probe {
                depth: 4096,
                depths: Vec::new(),
                repetitions: 5,
                max_tokens: 128,
            },
            significance_pct: 5.0,
            guard_tolerance_pct: 15.0,
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
                depths: Vec::new(),
                draft_n: 0,
                draft_n_accepted: 0,
                speed_limit_pct: [None, Some(87)],
                reason: None,
                verdict: None,
            }],
            winner: None,
            verdict: super::DEFAULTS_WON.into(),
        }
    }

    #[test]
    fn a_record_round_trips_and_names_its_launch_flags() {
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
        let flags = crate::core::bench::stamp::launch_flags(&argv);
        assert_eq!(
            (
                flags.n_batch.as_str(),
                flags.n_ubatch.as_str(),
                flags.type_k.as_str(),
                flags.flash_attn.as_str(),
                flags.spec_type.as_str()
            ),
            ("4096", "engine-default", "q8_0", "on", "engine-default")
        );
        let record = sample_record(argv, flags);
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
        assert!((back.guard_tolerance_pct - 15.0).abs() < f64::EPSILON);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_trials_stamp_reads_the_reasoning_flags_off_its_argv() {
        let argv: Vec<String> = ["--reasoning-effort", "low", "--reasoning-budget", "-1"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let flags = crate::core::bench::stamp::launch_flags(&argv);
        let record = sample_record(argv, flags);
        let json = serde_json::to_string(&record).expect("ser");
        let back: super::Record = serde_json::from_str(&json).expect("a record round-trips");
        let stamp = &back.trials[0].stamp;
        assert_eq!(
            (
                stamp.reasoning_effort.as_str(),
                stamp.reasoning_budget.as_str()
            ),
            ("low", "-1")
        );
        assert_eq!(stamp.reasoning_format, "engine-default");
        assert!(
            json.contains("\"reasoning_budget\":\"-1\""),
            "the record on disk carries the field: {json}"
        );
    }

    /// A record written before the guard knob existed was judged on the
    /// strict rule, and reads as exactly that.
    #[test]
    fn a_record_from_before_the_guard_knob_reads_as_strict() {
        let record = sample_record(argv(&[]), crate::core::bench::stamp::launch_flags(&[]));
        let json = serde_json::to_string(&record).expect("serialize");
        assert!(json.contains("\"guard_tolerance_pct\":15.0,"), "{json}");
        let pre_knob = json.replace("\"guard_tolerance_pct\":15.0,", "");
        let old: super::Record = serde_json::from_str(&pre_knob).expect("a pre-knob record loads");
        assert!(old.guard_tolerance_pct.abs() < f64::EPSILON);
    }

    #[test]
    fn the_spec_grammar_is_off_or_mtp_n() {
        assert_eq!(SpecDraft::parse("off").expect("off"), SpecDraft::Off);
        assert_eq!(SpecDraft::parse("mtp:1").expect("mtp:1"), SpecDraft::Mtp(1));
        assert_eq!(SpecDraft::Mtp(2).label(), "mtp:2");
        assert_eq!(SpecDraft::Off.label(), "off");
        for bad in ["mtp", "mtp:0", "mtp:x", "ngram", "on", ""] {
            let err = SpecDraft::parse(bad).expect_err(bad);
            assert!(
                matches!(&err, ChekovError::TuneBadSpecCandidate { value } if value == bad),
                "{bad}: {err}"
            );
        }
        let cfg = TuneSection {
            spec_drafts: vec!["off".into(), "mtp:1".into()],
            ..TuneSection::default()
        };
        assert_eq!(
            spec_values(&cfg).expect("valid"),
            vec![SpecDraft::Off, SpecDraft::Mtp(1)]
        );
        let bad = TuneSection {
            spec_drafts: vec!["mtp:1".into(), "eagle".into()],
            ..TuneSection::default()
        };
        assert!(spec_values(&bad).is_err());
    }

    #[test]
    fn mtp_candidates_rewrite_both_flags_and_off_strips_them() {
        let cfg = TuneSection::default();
        let plain = argv(&["--flash-attn", "on", "--cache-type-k", "q8_0"]);
        let spec = candidates(Stage::Spec, &plain, &cfg);
        let values: Vec<&str> = spec.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(
            values,
            vec!["mtp:1", "mtp:2", "mtp:3"],
            "off IS the incumbent"
        );
        let mut drafted_one = plain;
        drafted_one.extend(argv(&[
            "--spec-type",
            "draft-mtp",
            "--spec-draft-n-max",
            "1",
        ]));
        assert_eq!(spec[0].argv, drafted_one);
    }

    #[test]
    fn off_strips_both_flags_wherever_they_sit() {
        let cfg = TuneSection::default();
        let plain = argv(&["--flash-attn", "on", "--cache-type-k", "q8_0"]);
        let drafted = argv(&[
            "--flash-attn",
            "on",
            "--spec-type",
            "draft-mtp",
            "--spec-draft-n-max",
            "3",
            "--cache-type-k",
            "q8_0",
        ]);
        let spec = candidates(Stage::Spec, &drafted, &cfg);
        let values: Vec<&str> = spec.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(
            values,
            vec!["off", "mtp:1", "mtp:2"],
            "mtp:3 IS the incumbent"
        );
        assert_eq!(
            spec[0].argv, plain,
            "off strips both flags wherever they sit"
        );
        let mut rewritten = drafted;
        rewritten[5] = "1".into();
        assert_eq!(
            spec[1].argv, rewritten,
            "a rewrite keeps the flags where they were"
        );
        assert_eq!(
            strip(&plain, Flag::SpecType),
            plain,
            "nothing to strip returns the argv"
        );
        let stray = argv(&["--spec-draft-n-max", "2"]);
        assert_eq!(strip(&stray, Flag::SpecDraftNMax), argv(&[]));
    }

    #[test]
    fn apply_strips_the_spec_flags_when_the_winner_dropped_them() {
        let current = argv(&[
            "--temp",
            "0.6",
            "--spec-type",
            "draft-mtp",
            "--spec-draft-n-max",
            "3",
        ]);
        let off_won = argv(&["--temp", "0.6", "--flash-attn", "on"]);
        assert_eq!(
            super::applied_extra_flags(&current, &off_won),
            argv(&["--temp", "0.6", "--flash-attn", "on"])
        );
        let mut one_won = current.clone();
        one_won[5] = "1".into();
        assert_eq!(super::applied_extra_flags(&current, &one_won), one_won);
        let untouched = argv(&["--temp", "0.6"]);
        assert_eq!(
            super::applied_extra_flags(&untouched, &off_won),
            argv(&["--temp", "0.6", "--flash-attn", "on"]),
            "nothing to strip, nothing stripped"
        );
    }

    #[test]
    fn the_spec_grammar_accepts_the_five_ngram_types_and_refuses_the_rest() {
        for (spelling, expected) in [
            ("ngram:ngram-simple", NgramType::Simple),
            ("ngram:ngram-map-k", NgramType::MapK),
            ("ngram:ngram-map-k4v", NgramType::MapK4v),
            ("ngram:ngram-mod", NgramType::Mod),
            ("ngram:ngram-cache", NgramType::Cache),
        ] {
            let parsed = SpecDraft::parse(spelling).expect(spelling);
            assert_eq!(parsed, SpecDraft::Ngram(expected));
            assert_eq!(parsed.label(), spelling, "the label is the spelling");
            assert_eq!(expected.label(), &spelling["ngram:".len()..]);
        }
        for bad in [
            "ngram:",
            "ngram:mtp",
            "ngram:ngram-simple,ngram-mod",
            "ngram:ngram-map-k4",
        ] {
            let err = SpecDraft::parse(bad).expect_err(bad);
            assert!(
                matches!(&err, ChekovError::TuneBadSpecCandidate { value } if value == bad),
                "{bad}: {err}"
            );
        }
        assert!(NgramType::Mod.keeps_memory() && NgramType::Cache.keeps_memory());
        assert!(!NgramType::Simple.keeps_memory());
        assert_eq!(NgramType::ALL.len(), 5);
    }

    #[test]
    fn an_ngram_candidate_writes_the_type_and_strips_the_draft_length_only() {
        let cfg = TuneSection {
            spec_drafts: vec!["off".into(), "ngram:ngram-mod".into()],
            ..TuneSection::default()
        };
        let drafted = argv(&[
            "--spec-type",
            "draft-mtp",
            "--spec-draft-n-max",
            "3",
            "--spec-ngram-mod-n-match",
            "24",
        ]);
        let spec = candidates(Stage::Spec, &drafted, &cfg);
        let values: Vec<&str> = spec.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["off", "ngram:ngram-mod"]);
        assert_eq!(
            spec[1].argv,
            argv(&["--spec-type", "ngram-mod", "--spec-ngram-mod-n-match", "24"]),
            "the type is rewritten in place, the length stripped, the engine's own knob kept"
        );
    }

    #[test]
    fn apply_strips_a_stale_draft_length_behind_an_ngram_winner() {
        let current = argv(&[
            "--temp",
            "0.6",
            "--spec-type",
            "draft-mtp",
            "--spec-draft-n-max",
            "3",
        ]);
        let ngram_won = argv(&["--temp", "0.6", "--spec-type", "ngram-mod"]);
        assert_eq!(
            super::applied_extra_flags(&current, &ngram_won),
            argv(&["--temp", "0.6", "--spec-type", "ngram-mod"]),
            "a winner without the length lost it in the stage; the current flags lose it too"
        );
        let mtp_won = argv(&[
            "--temp",
            "0.6",
            "--spec-type",
            "draft-mtp",
            "--spec-draft-n-max",
            "1",
        ]);
        assert_eq!(super::applied_extra_flags(&current, &mtp_won), mtp_won);
    }

    /// The good `DepthResult` of the degenerate test, with the draft counts
    /// the caller sets.
    fn drafted_result(
        draft_n: u64,
        draft_n_accepted: u64,
    ) -> crate::core::bench::sweep::DepthResult {
        crate::core::bench::sweep::DepthResult {
            depth: 4096,
            prompt_n: 4101,
            cache_n: 0,
            draft_n,
            draft_n_accepted,
            thinking_chars: 0,
            answer_chars: 0,
            decode_samples: vec![30.0, 31.0, 31.2],
            prefill_samples: vec![400.0, 402.0, 401.0],
            decode: crate::core::stats::summarize(&[30.0, 31.0, 31.2]),
            prefill: crate::core::stats::summarize(&[400.0, 402.0, 401.0]),
        }
    }

    #[test]
    fn fresh_prefill_records_zero_cached_tokens() {
        let outcome = super::classify(&drafted_result(0, 0), 4096);
        let json = serde_json::to_value(outcome.measurements()).unwrap();
        assert_eq!(json[0]["measured"]["cache_n"], 0);
    }

    #[test]
    fn legacy_measurements_keep_the_cache_count_unknown() {
        let mut json = serde_json::to_value(measured(30.0, 400.0)).unwrap();
        json.as_object_mut().unwrap().remove("cache_n");
        let legacy: super::Measured = serde_json::from_value(json).unwrap();
        assert!(serde_json::to_value(legacy).unwrap()["cache_n"].is_null());
    }

    fn quiet() -> super::LineContext<'static> {
        super::LineContext {
            verdict: None,
            dirty: None,
        }
    }

    #[test]
    fn a_stage_line_prints_the_acceptance_when_the_trial_drafted() {
        let drafted = super::Measured {
            draft_n: 300,
            draft_n_accepted: 189,
            ..measured(74.7, 126.0)
        };
        let line = super::stage_line(
            &super::CandidateLabel {
                stage: super::Stage::Fa,
                value: "off",
            },
            &super::Outcome::Measured(single_depth(drafted)),
            &quiet(),
        );
        assert!(
            line.ends_with("   acceptance 63% (189 of 300 drafted)"),
            "any drafting trial says so, whatever its stage: {line}"
        );
    }

    #[test]
    fn no_drafts_is_said_only_on_a_spec_candidate_that_drafted_nothing() {
        let dry = || super::Outcome::Measured(single_depth(measured(60.0, 140.0)));
        let line = |stage, value| {
            super::stage_line(&super::CandidateLabel { stage, value }, &dry(), &quiet())
        };
        assert!(
            line(super::Stage::Spec, "ngram:ngram-simple").ends_with("   no drafts"),
            "{}",
            line(super::Stage::Spec, "ngram:ngram-simple")
        );
        assert!(line(super::Stage::Spec, "mtp:1").ends_with("   no drafts"));
        assert!(
            !line(super::Stage::Spec, "off").contains("drafts"),
            "off never drafts by design"
        );
        assert!(!line(super::Stage::Kv, "f16").contains("drafts"));
    }

    #[test]
    fn classify_carries_the_draft_counts_and_a_record_from_before_them_loads_with_zeros() {
        let super::Outcome::Measured(m) = super::classify(&drafted_result(300, 189), 4096) else {
            panic!("measured");
        };
        assert_eq!(
            (m[0].measured.draft_n, m[0].measured.draft_n_accepted),
            (300, 189)
        );
        let record = sample_record(argv(&[]), crate::core::bench::stamp::launch_flags(&[]));
        let json = serde_json::to_string(&record).expect("ser");
        assert!(json.contains("\"draft_n\":0,"), "{json}");
        let old = json
            .replace("\"draft_n\":0,", "")
            .replace("\"draft_n_accepted\":0,", "");
        let back: super::Record = serde_json::from_str(&old).expect("a pre-count record loads");
        assert_eq!(back.trials[0].draft_n, 0);
    }
}
