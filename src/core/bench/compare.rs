//! Comparison of two stored runs.
//!
//! The stamp refuses when the ENVIRONMENT differs; the model fields
//! (`weights_revision`, `quant`) are the comparison's subject, not its
//! precondition — comparing two models is the point, comparing two
//! environments is a category error the stamp exists to prevent.

use crate::core::bench::codebase::TaskTier;
use crate::core::bench::codebase::ladder::{Score, Tier, as_f64};
use crate::core::bench::stamp;
use crate::core::bench::stamp::Stamp;
use crate::core::bench::store::{
    self, CodebaseRow, RunLog, Tally, TaskRow, Transport, door_tag, is_unavailable,
};
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

/// Everything two runs can be held against each other on.
#[derive(Debug)]
pub struct RunComparison {
    pub depths: Vec<DepthComparison>,
    pub agentic: AgenticComparison,
    pub codebase: CodebaseComparison,
}

/// Which of the two runs recorded a section at all.
///
/// A section only one run measured is named as absent. Dropping it instead
/// would read as a section where nothing differed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Both,
    OnlyA,
    OnlyB,
    Neither,
}

impl Presence {
    const fn of(a: bool, b: bool) -> Self {
        match (a, b) {
            (true, true) => Self::Both,
            (true, false) => Self::OnlyA,
            (false, true) => Self::OnlyB,
            (false, false) => Self::Neither,
        }
    }
}

/// One counted figure with the two runs side by side (`8/10` vs `7/10`).
#[derive(Debug)]
pub struct SuiteTotals {
    pub label: String,
    pub a: String,
    pub b: String,
}

/// A case exactly one of the two runs passed — the finding a comparison
/// exists to surface.
#[derive(Debug)]
pub struct CaseDelta {
    pub suite: String,
    pub task_id: String,
    pub transport: Transport,
    pub a_pass: bool,
    pub b_pass: bool,
    pub a_reason: Option<String>,
    pub b_reason: Option<String>,
}

/// A case one run graded and the other never did. `which` is `"a"` or `"b"`.
#[derive(Debug)]
pub struct OnlyIn {
    pub which: &'static str,
    pub suite: String,
    pub task_id: String,
    pub transport: Transport,
}

#[derive(Debug)]
pub struct AgenticComparison {
    pub presence: Presence,
    pub totals: Vec<SuiteTotals>,
    pub disagreements: Vec<CaseDelta>,
    pub only_in: Vec<OnlyIn>,
    /// Graded-but-unmeasurable rows kept out of every count, `(a, b)`.
    pub unavailable: (usize, usize),
}

/// One ladder tier over one task-tier group, both runs at once.
#[derive(Debug)]
pub struct TierDelta {
    pub group: String,
    pub tier: String,
    pub mean_a: f64,
    pub mean_b: f64,
    pub a_better: usize,
    pub b_better: usize,
    pub ties: usize,
    pub verdict: Comparison,
}

/// Tasks a group lost, and why: one run could not answer them, or — for the
/// lift — one run measured only one of the two arms.
#[derive(Debug)]
pub struct GroupDrop {
    pub group: String,
    pub dropped: usize,
    pub reason: &'static str,
}

const DROP_UNAVAILABLE: &str = "unavailable in one run";
const DROP_ONE_ARM: &str = "measured on one arm only in one run";
const LIFT_GROUP: &str = "context lift";

#[derive(Debug)]
pub struct CodebaseComparison {
    pub presence: Presence,
    pub tiers: Vec<TierDelta>,
    pub dropped: Vec<GroupDrop>,
    /// Why the `equiv` row is absent: the two runs' judges were not the same
    /// instrument, or they were and no crossing reached a verdict in both.
    /// `None` when there is nothing to explain — the row is present, or
    /// neither run used a judge.
    pub judge_note: Option<String>,
}

/// How a comparison runs: the significance threshold, and whether the
/// runtime allow-list (spec 2026-08-31 §7) is masked.
pub struct CompareOpts {
    pub significance_pct: f64,
    pub cross_runtime: bool,
}

/// The fields `--cross-runtime` permits to differ, and no others.
const CROSS_RUNTIME_ALLOWED: [&str; 14] = [
    "runtime",
    "timing_source",
    "engine_build_commit",
    "ctx",
    "n_parallel",
    "kv_unified",
    "n_batch",
    "n_ubatch",
    "type_k",
    "type_v",
    "flash_attn",
    "spec_type",
    "spec_draft_n_max",
    "prompt_set_hash",
];

pub fn compare_runs(
    a: &RunLog,
    b: &RunLog,
    opts: &CompareOpts,
) -> Result<RunComparison, ChekovError> {
    let pair = RunPair { a, b };
    assert_same_environment(&pair, opts)?;
    Ok(RunComparison {
        depths: depth_comparisons(&pair, opts.significance_pct),
        agentic: compare_agentic(&pair),
        codebase: compare_codebase(&pair),
    })
}

fn depth_comparisons(pair: &RunPair, significance_pct: f64) -> Vec<DepthComparison> {
    let mut rows = Vec::new();
    for row_a in throughput_rows(pair.a) {
        let Some(depth) = depth_of(row_a) else {
            continue;
        };
        let Some(row_b) = throughput_rows(pair.b).find(|r| depth_of(r) == Some(depth)) else {
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
    rows
}

fn throughput_rows(log: &RunLog) -> impl Iterator<Item = &TaskRow> {
    log.rows.iter().filter(|r| r.suite == "throughput")
}

fn depth_of(row: &TaskRow) -> Option<u32> {
    row.task_id.strip_prefix("depth-")?.parse().ok()
}

/// Refuse on the first differing stamp field, with the subject fields
/// (`weights_revision`, `quant`, `judge`) masked equal — they are what is
/// being compared, not what must match. `--cross-runtime` additionally
/// masks the runtime allow-list (spec §7).
fn assert_same_environment(pair: &RunPair, opts: &CompareOpts) -> Result<(), ChekovError> {
    let a = &pair.a.head.stamp;
    let mut b_env = pair.b.head.stamp.clone();
    b_env.weights_revision.clone_from(&a.weights_revision);
    b_env.quant.clone_from(&a.quant);
    b_env.judge.clone_from(&a.judge);
    if opts.cross_runtime {
        mask_cross_runtime(&mut b_env, a);
    }
    stamp::mismatch_error(a, &b_env).map_or(Ok(()), Err)
}

/// Masks exactly the 14-entry `--cross-runtime` allow-list (spec §7) onto
/// `b_env` — the fields a foreign runtime is permitted to differ on, and no
/// others. A foreign server's speculative flags are as unobservable as its
/// KV flags, which is why the two ride along.
fn mask_cross_runtime(b_env: &mut Stamp, a: &Stamp) {
    b_env.runtime.clone_from(&a.runtime);
    b_env.timing_source.clone_from(&a.timing_source);
    b_env.engine_build_commit.clone_from(&a.engine_build_commit);
    b_env.ctx = a.ctx;
    b_env.n_parallel = a.n_parallel;
    b_env.kv_unified.clone_from(&a.kv_unified);
    b_env.n_batch.clone_from(&a.n_batch);
    b_env.n_ubatch.clone_from(&a.n_ubatch);
    b_env.type_k.clone_from(&a.type_k);
    b_env.type_v.clone_from(&a.type_v);
    b_env.flash_attn.clone_from(&a.flash_attn);
    b_env.spec_type.clone_from(&a.spec_type);
    b_env.spec_draft_n_max.clone_from(&a.spec_draft_n_max);
    b_env.prompt_set_hash.clone_from(&a.prompt_set_hash);
}

/// The `--cross-runtime` banner: both runtimes, the warning, one line per
/// allow-listed field that differs, the closing sentence.
#[must_use]
pub fn cross_runtime_banner(a: &Stamp, b: &Stamp) -> String {
    let mut lines = vec![
        format!("cross-runtime comparison: {} vs {}", a.runtime, b.runtime),
        "determinism does not hold across runtimes; differing fields:".to_owned(),
    ];
    let (ja, jb) = (json_of(a), json_of(b));
    for field in CROSS_RUNTIME_ALLOWED {
        let (va, vb) = (&ja[field], &jb[field]);
        if va != vb {
            lines.push(format!("{field}: {va} vs {vb}"));
        }
    }
    lines.push("this measures the runtimes, not the model.".to_owned());
    lines.join("\n") + "\n"
}

fn json_of(s: &Stamp) -> serde_json::Value {
    serde_json::to_value(s).unwrap_or_default()
}

// ---------------------------------------------------------------- agentic

/// One graded agentic case as both runs recorded it.
struct AgenticPair<'a> {
    a: &'a TaskRow,
    b: &'a TaskRow,
}

/// The two runs' rows for the cases they both graded.
struct Sides<'a> {
    a: &'a [&'a TaskRow],
    b: &'a [&'a TaskRow],
}

fn graded_agentic(log: &RunLog) -> impl Iterator<Item = &TaskRow> {
    log.rows
        .iter()
        .filter(|r| store::AGENTIC.contains(&r.suite.as_str()) && r.grade.is_some())
}

/// The identity of a crossing: the case AND the door it took.
fn same_case(x: &TaskRow, y: &TaskRow) -> bool {
    x.suite == y.suite && x.task_id == y.task_id && x.transport == y.transport
}

fn compare_agentic<'a>(pair: &RunPair<'a>) -> AgenticComparison {
    let presence = Presence::of(
        graded_agentic(pair.a).next().is_some(),
        graded_agentic(pair.b).next().is_some(),
    );
    let pairs: Vec<AgenticPair<'a>> = graded_agentic(pair.a)
        .filter_map(|row_a| {
            let row_b = graded_agentic(pair.b).find(|r| same_case(row_a, r))?;
            Some(AgenticPair { a: row_a, b: row_b })
        })
        .collect();
    AgenticComparison {
        presence,
        totals: agentic_totals(&pairs),
        disagreements: pairs.iter().filter_map(case_delta).collect(),
        only_in: unpaired_cases(pair),
        unavailable: (
            pairs.iter().filter(|p| is_unavailable(p.a)).count(),
            pairs.iter().filter(|p| is_unavailable(p.b)).count(),
        ),
    }
}

/// Cases graded in one run and not the other. They are named rather than
/// dropped: a case only one run ran is not a case the two runs agreed on.
fn unpaired_cases(pair: &RunPair) -> Vec<OnlyIn> {
    let mut out = only_in(pair.a, pair.b, "a");
    out.extend(only_in(pair.b, pair.a, "b"));
    out
}

fn only_in(from: &RunLog, other: &RunLog, which: &'static str) -> Vec<OnlyIn> {
    graded_agentic(from)
        .filter(|row| !graded_agentic(other).any(|r| same_case(row, r)))
        .map(|row| OnlyIn {
            which,
            suite: row.suite.clone(),
            task_id: row.task_id.clone(),
            transport: row.transport,
        })
        .collect()
}

/// Only the cases that separate the two runs: exactly one side passed.
///
/// Both-pass and both-fail are agreement and say nothing. An unavailable case
/// on either side is not a disagreement either — nobody measured it.
fn case_delta(pair: &AgenticPair) -> Option<CaseDelta> {
    if is_unavailable(pair.a) || is_unavailable(pair.b) {
        return None;
    }
    let (a_pass, b_pass) = (graded_pass(pair.a), graded_pass(pair.b));
    if a_pass == b_pass {
        return None;
    }
    Some(CaseDelta {
        suite: pair.a.suite.clone(),
        task_id: pair.a.task_id.clone(),
        transport: pair.a.transport,
        a_pass,
        b_pass,
        a_reason: reason_of(pair.a),
        b_reason: reason_of(pair.b),
    })
}

fn graded_pass(row: &TaskRow) -> bool {
    row.grade.as_ref().is_some_and(|g| g.pass)
}

fn reason_of(row: &TaskRow) -> Option<String> {
    row.grade.as_ref().and_then(|g| g.reason.clone())
}

/// The report's own figures, counted over the cases both runs graded.
///
/// The counting is the store's, not a second copy of it: a comparison that
/// counted differently from the report would make one of them a liar.
fn agentic_totals(pairs: &[AgenticPair]) -> Vec<SuiteTotals> {
    let a: Vec<&TaskRow> = pairs.iter().map(|p| p.a).collect();
    let b: Vec<&TaskRow> = pairs.iter().map(|p| p.b).collect();
    let sides = Sides { a: &a, b: &b };
    let mut out = Vec::new();
    out.extend(tool_emit_totals(&sides, Transport::Buffered));
    out.extend(grammar_gap_totals(&sides));
    out.extend(instruction_totals(&sides, Transport::Buffered));
    out.extend(tool_emit_totals(&sides, Transport::Streamed));
    out.extend(instruction_totals(&sides, Transport::Streamed));
    out
}

fn rows_in<'a>(rows: &[&'a TaskRow], suite: &str) -> Vec<&'a TaskRow> {
    rows.iter().filter(|r| r.suite == suite).copied().collect()
}

fn rows_via<'a>(rows: &[&'a TaskRow], suite: &str, transport: Transport) -> Vec<&'a TaskRow> {
    rows.iter()
        .filter(|r| r.suite == suite && r.transport == transport)
        .copied()
        .collect()
}

fn tool_emit_totals(sides: &Sides, transport: Transport) -> Option<SuiteTotals> {
    let a = rows_via(sides.a, "tool_emit", transport);
    if a.is_empty() {
        return None;
    }
    let b = rows_via(sides.b, "tool_emit", transport);
    Some(SuiteTotals {
        label: format!("tool_emit{}", door_tag(transport)),
        a: Tally::of(&a).cell(),
        b: Tally::of(&b).cell(),
    })
}

/// Both arms of the §7.2 gap, so the reader can see the gap move rather than
/// a single number that hides which arm shifted.
fn grammar_gap_totals(sides: &Sides) -> Vec<SuiteTotals> {
    let forced_a = rows_in(sides.a, "grammar_gap");
    if forced_a.is_empty() {
        return Vec::new();
    }
    let forced_b = rows_in(sides.b, "grammar_gap");
    let emit_a = rows_in(sides.a, "tool_emit");
    let emit_b = rows_in(sides.b, "tool_emit");
    vec![
        SuiteTotals {
            label: "grammar_gap forced".to_owned(),
            a: Tally::of(&forced_a).cell(),
            b: Tally::of(&forced_b).cell(),
        },
        SuiteTotals {
            label: "grammar_gap unconstrained".to_owned(),
            a: Tally::of(&store::unconstrained_arm(&forced_a, &emit_a)).cell(),
            b: Tally::of(&store::unconstrained_arm(&forced_b, &emit_b)).cell(),
        },
    ]
}

fn instruction_totals(sides: &Sides, transport: Transport) -> Vec<SuiteTotals> {
    let a = rows_via(sides.a, "instruction", transport);
    if a.is_empty() {
        return Vec::new();
    }
    let b = rows_via(sides.b, "instruction", transport);
    let door = door_tag(transport);
    vec![
        SuiteTotals {
            label: format!("instruction strict{door}"),
            a: Tally::of(&a).cell(),
            b: Tally::of(&b).cell(),
        },
        SuiteTotals {
            label: format!("instruction loose{door}"),
            a: Tally::loose(&a).cell(),
            b: Tally::loose(&b).cell(),
        },
    ]
}

// --------------------------------------------------------------- codebase

/// A codebase task both runs answered.
struct CodePair<'a> {
    a: &'a CodebaseRow,
    b: &'a CodebaseRow,
}

/// Per-task wins on one tier: how often each run scored higher, and how often
/// they landed on the same value.
#[derive(Debug, Clone, Copy, Default)]
struct Wins {
    a: usize,
    b: usize,
    ties: usize,
}

/// Two ladder scores that differ only in the last bits of a division are the
/// same answer, not a win.
const TIE: f64 = 1e-9;

/// The exact test's reach: `u128` holds `2ⁿ` and every intermediate binomial
/// well past this, and no corpus comes near it.
const MAX_SIGN_TEST_N: u32 = 100;

fn codebase_rows(log: &RunLog) -> impl Iterator<Item = &TaskRow> {
    log.rows
        .iter()
        .filter(|r| r.suite == "codebase" && r.codebase.is_some())
}

fn compare_codebase(pair: &RunPair) -> CodebaseComparison {
    let presence = Presence::of(
        codebase_rows(pair.a).next().is_some(),
        codebase_rows(pair.b).next().is_some(),
    );
    let mut tiers = Vec::new();
    let mut dropped = Vec::new();
    for group in tier_groups(pair) {
        let (pairs, lost) = group_pairs(pair, group);
        dropped.push(GroupDrop {
            group: group.label(),
            dropped: lost,
            reason: DROP_UNAVAILABLE,
        });
        tiers.extend(group_tiers(&pairs, group));
    }
    let (lifts, lost) = lift_pairs(pair);
    tiers.extend(lift_deltas(&lifts));
    dropped.push(GroupDrop {
        group: LIFT_GROUP.to_owned(),
        dropped: lost,
        reason: DROP_ONE_ARM,
    });
    let (equiv, judge_note) = compare_judge(pair);
    tiers.extend(equiv);
    CodebaseComparison {
        presence,
        tiers,
        dropped,
        judge_note,
    }
}

/// One judged verdict per task id, as `1.0`/`0.0` — abstentions and skips are
/// absent, so a pair exists only where both runs reached a verdict.
fn judge_values(log: &RunLog) -> std::collections::BTreeMap<&str, f64> {
    store::judge_rows(log)
        .into_iter()
        .filter_map(|r| Some((r.task_id.as_str(), r.judge.as_ref()?.equivalent?)))
        .map(|(id, e)| (id, if e { 1.0 } else { 0.0 }))
        .collect()
}

fn judge_label(stamp: &Stamp) -> String {
    stamp.judge.as_ref().map_or_else(
        || "none".to_owned(),
        |j| format!("{}@{}/{}", j.model, j.revision, j.rubric_hash),
    )
}

/// The `equiv` row's numbers, once both runs are known to share a judge.
fn equiv_delta(pair: &RunPair) -> Option<TierDelta> {
    let (va, vb) = (judge_values(pair.a), judge_values(pair.b));
    let values: Vec<(f64, f64)> = va
        .iter()
        .filter_map(|(id, a)| Some((*a, *vb.get(id)?)))
        .collect();
    if values.is_empty() {
        return None;
    }
    let wins = win_counts(&values);
    Some(TierDelta {
        group: TaskTier::FunctionBody.label().to_owned(),
        tier: "equiv".to_owned(),
        mean_a: mean(values.iter().map(|v| v.0)),
        mean_b: mean(values.iter().map(|v| v.1)),
        a_better: wins.a,
        b_better: wins.b,
        ties: wins.ties,
        verdict: sign_test(wins.a, wins.b),
    })
}

/// The `equiv` row when both runs were judged by the same instrument, or
/// the note saying why they were not compared. Silence would read as "the
/// judge had nothing to say", so a shared judge with no shared verdict says
/// which of the two it was.
fn compare_judge(pair: &RunPair) -> (Option<TierDelta>, Option<String>) {
    let (ja, jb) = (&pair.a.head.stamp.judge, &pair.b.head.stamp.judge);
    match (ja, jb) {
        (None, None) => (None, None),
        (Some(a), Some(b)) if a == b => equiv_delta(pair).map_or_else(
            || {
                (
                    None,
                    Some("equiv: not compared (no crossing judged in both runs)".to_owned()),
                )
            },
            |delta| (Some(delta), None),
        ),
        _ => (
            None,
            Some(format!(
                "equiv: not compared (judge differs: a={} b={})",
                judge_label(&pair.a.head.stamp),
                judge_label(&pair.b.head.stamp)
            )),
        ),
    }
}

/// One line of the codebase section: a task tier and, for the cross-file
/// tier, which arm — the two arms are two measurements, and a mean over both
/// would hide exactly the number B1 exists to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeGroup {
    tier: TaskTier,
    with_extra: bool,
}

impl CodeGroup {
    fn of(row: &CodebaseRow) -> Self {
        use crate::core::bench::codebase::run::WITH_EXTRA;
        Self {
            tier: row.tier,
            with_extra: row.tier == TaskTier::CrossFileFirst
                && row.arm.as_deref() == Some(WITH_EXTRA),
        }
    }

    /// `cross_file_first+extra` for the arm that was shown the defining file;
    /// the tier's own label everywhere else — the report's spelling.
    fn label(self) -> String {
        if self.with_extra {
            format!(
                "{}{}",
                self.tier.label(),
                crate::core::bench::codebase::ARM_EXTRA_SUFFIX
            )
        } else {
            self.tier.label().to_owned()
        }
    }
}

/// Every group the runs actually recorded, in the order it first appears.
/// The taxonomy can grow, so nothing here names two of them.
fn tier_groups(pair: &RunPair) -> Vec<CodeGroup> {
    let mut groups: Vec<CodeGroup> = Vec::new();
    let seen = codebase_rows(pair.a)
        .chain(codebase_rows(pair.b))
        .filter_map(|r| r.codebase.as_ref())
        .map(CodeGroup::of);
    for group in seen {
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    groups
}

/// One group's tasks as both runs answered them, and how many were dropped
/// because one run or the other could not answer.
fn group_pairs<'a>(pair: &RunPair<'a>, group: CodeGroup) -> (Vec<CodePair<'a>>, usize) {
    let mut pairs = Vec::new();
    let mut dropped = 0;
    for row_a in codebase_rows(pair.a).filter(|r| in_group(r, group)) {
        let Some(row_b) = codebase_rows(pair.b).find(|r| r.task_id == row_a.task_id) else {
            continue;
        };
        if is_unavailable(row_a) || is_unavailable(row_b) {
            dropped += 1;
        } else if let (Some(a), Some(b)) = (row_a.codebase.as_ref(), row_b.codebase.as_ref()) {
            pairs.push(CodePair { a, b });
        }
    }
    (pairs, dropped)
}

fn in_group(row: &TaskRow, group: CodeGroup) -> bool {
    row.codebase
        .as_ref()
        .is_some_and(|c| CodeGroup::of(c) == group)
}

/// One delta per ladder tier that has a value on both sides.
fn group_tiers(pairs: &[CodePair], group: CodeGroup) -> Vec<TierDelta> {
    [
        Tier::Exact,
        Tier::EditSim,
        Tier::IdentF1,
        Tier::Parse,
        Tier::Symbols,
    ]
    .into_iter()
    .filter_map(|tier| tier_delta(pairs, group, tier))
    .collect()
}

fn tier_delta(pairs: &[CodePair], group: CodeGroup, tier: Tier) -> Option<TierDelta> {
    let values: Vec<(f64, f64)> = pairs.iter().filter_map(|p| tier_values(p, tier)).collect();
    delta_over(group.label(), tier.label(), &values)
}

/// The paired sign test over one column of `(a, b)` values, as one report
/// row — or `None` when nothing was measured on both sides.
fn delta_over(group: String, tier: &str, values: &[(f64, f64)]) -> Option<TierDelta> {
    if values.is_empty() {
        return None;
    }
    let wins = win_counts(values);
    Some(TierDelta {
        group,
        tier: tier.to_owned(),
        mean_a: mean(values.iter().map(|v| v.0)),
        mean_b: mean(values.iter().map(|v| v.1)),
        a_better: wins.a,
        b_better: wins.b,
        ties: wins.ties,
        verdict: sign_test(wins.a, wins.b),
    })
}

// ----------------------------------------------------------- context lift

/// One cross-file task's two arms, `(no_extra, extra)`, as one run answered
/// them.
type Arms<'a> = (&'a CodebaseRow, &'a CodebaseRow);

/// One cross-file task measured on both arms in both runs.
struct LiftPair<'a> {
    a: Arms<'a>,
    b: Arms<'a>,
}

/// A column of the lift row: the ladder tiers recomputed from stored text,
/// tier 5 from its stored score, and the exec tiers from theirs.
#[derive(Clone, Copy)]
enum LiftTier {
    Ladder(Tier),
    Compile,
    Test,
}

impl LiftTier {
    const ALL: [Self; 7] = [
        Self::Ladder(Tier::Exact),
        Self::Ladder(Tier::EditSim),
        Self::Ladder(Tier::IdentF1),
        Self::Ladder(Tier::Parse),
        Self::Ladder(Tier::Symbols),
        Self::Compile,
        Self::Test,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Ladder(tier) => tier.label(),
            Self::Compile => Tier::Compile.label(),
            Self::Test => Tier::Test.label(),
        }
    }

    /// This tier's value on one row, or `None` where the row has none.
    fn value(self, row: &CodebaseRow) -> Option<f64> {
        use crate::core::bench::store::ExecScore;
        let exec_value = |score: &ExecScore| match score {
            ExecScore::Value(v) => Some(*v),
            ExecScore::Skipped(_) => None,
        };
        match self {
            Self::Ladder(Tier::Symbols) => row.symbols_score,
            Self::Ladder(tier) => value_of(row, tier),
            Self::Compile => exec_value(&row.exec.as_ref()?.compile),
            Self::Test => exec_value(&row.exec.as_ref()?.test),
        }
    }

    /// `extra − no_extra` on one task, or `None` when either arm lacks a value.
    fn lift(self, arms: Arms) -> Option<f64> {
        Some(self.value(arms.1)? - self.value(arms.0)?)
    }
}

/// The cross-file rows one run answered, keyed by task: the arms it has.
fn arms_of(
    log: &RunLog,
) -> std::collections::BTreeMap<&str, (Option<&CodebaseRow>, Option<&CodebaseRow>)> {
    use crate::core::bench::codebase::base_id;
    use crate::core::bench::codebase::run::WITH_EXTRA;
    let mut arms: std::collections::BTreeMap<&str, (Option<&CodebaseRow>, Option<&CodebaseRow>)> =
        std::collections::BTreeMap::new();
    for row in codebase_rows(log).filter(|r| !is_unavailable(r)) {
        let Some(code) = row.codebase.as_ref() else {
            continue;
        };
        if code.tier != TaskTier::CrossFileFirst {
            continue;
        }
        let entry = arms.entry(base_id(&row.task_id)).or_default();
        if code.arm.as_deref() == Some(WITH_EXTRA) {
            entry.1 = Some(code);
        } else {
            entry.0 = Some(code);
        }
    }
    arms
}

/// Every task measured on both arms in both runs, and how many tasks both
/// runs touched but one of them measured on one arm only.
fn lift_pairs<'a>(pair: &RunPair<'a>) -> (Vec<LiftPair<'a>>, usize) {
    let (arms_a, arms_b) = (arms_of(pair.a), arms_of(pair.b));
    let mut pairs = Vec::new();
    let mut dropped = 0;
    for (id, a) in &arms_a {
        let Some(b) = arms_b.get(id) else { continue };
        match (a, b) {
            ((Some(a0), Some(a1)), (Some(b0), Some(b1))) => pairs.push(LiftPair {
                a: (a0, a1),
                b: (b0, b1),
            }),
            _ => dropped += 1,
        }
    }
    (pairs, dropped)
}

/// One `context lift` row per tier with a lift on both sides: A's per-task
/// lifts against B's, through the same paired sign test as every tier.
fn lift_deltas(pairs: &[LiftPair]) -> Vec<TierDelta> {
    LiftTier::ALL
        .into_iter()
        .filter_map(|tier| {
            let values: Vec<(f64, f64)> = pairs
                .iter()
                .filter_map(|p| Some((tier.lift(p.a)?, tier.lift(p.b)?)))
                .collect();
            delta_over(LIFT_GROUP.to_owned(), tier.label(), &values)
        })
        .collect()
}

/// One task's pair of values on this tier, or `None` when either side skips
/// it: tiers 1-2 skip function bodies, and tier 5 has no score until the
/// worktree scored it.
fn tier_values(pair: &CodePair, tier: Tier) -> Option<(f64, f64)> {
    if tier == Tier::Symbols {
        return Some((pair.a.symbols_score?, pair.b.symbols_score?));
    }
    Some((value_of(pair.a, tier)?, value_of(pair.b, tier)?))
}

fn value_of(row: &CodebaseRow, tier: Tier) -> Option<f64> {
    match store::recompute(row, tier) {
        Score::Value(v) => Some(v),
        Score::Skipped(_) => None,
    }
}

fn win_counts(values: &[(f64, f64)]) -> Wins {
    let mut wins = Wins::default();
    for (a, b) in values {
        if (a - b).abs() <= TIE {
            wins.ties += 1;
        } else if a > b {
            wins.a += 1;
        } else {
            wins.b += 1;
        }
    }
    wins
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let collected: Vec<f64> = values.collect();
    if collected.is_empty() {
        return 0.0;
    }
    collected.iter().sum::<f64>() / as_f64(collected.len())
}

/// An exact two-sided binomial sign test at p < 0.05.
///
/// `n = a_better + b_better`, `k = min`, `p = 2·Σ_{i≤k} C(n,i) / 2ⁿ`, with
/// integer coefficients — a normal approximation lies at the small n a task
/// suite actually has, and the whole point of the line is that a 3-vs-3 split
/// is not a finding.
///
/// Above `MAX_SIGN_TEST_N` the exact test is out of `u128`'s reach and the
/// answer is the conservative one: a difference nobody demonstrated is
/// reported as none, never as a win.
fn sign_test(a_better: usize, b_better: usize) -> Comparison {
    let n = u32::try_from(a_better + b_better).unwrap_or(u32::MAX);
    if n == 0 || n > MAX_SIGN_TEST_N {
        return Comparison::NoSignificantDifference;
    }
    let k = u32::try_from(a_better.min(b_better)).unwrap_or(u32::MAX);
    let tail: u128 = (0..=k).map(|i| binomial(n, i)).sum();
    // p < 0.05  ⇔  2·tail / 2ⁿ < 1/20  ⇔  40·tail < 2ⁿ. An overflow on the
    // left can only mean it is the larger: 2ⁿ never reaches 2¹²⁸ here.
    if tail
        .checked_mul(40)
        .is_none_or(|scaled| scaled >= 1u128 << n)
    {
        return Comparison::NoSignificantDifference;
    }
    if a_better > b_better {
        Comparison::Faster
    } else {
        Comparison::Slower
    }
}

/// `C(n, k)` exactly, by the multiplicative recurrence — no factorial to
/// overflow long before the coefficient does.
fn binomial(n: u32, k: u32) -> u128 {
    (0..k).fold(1_u128, |c, i| {
        c.saturating_mul(u128::from(n - i)) / u128::from(i + 1)
    })
}

// ----------------------------------------------------------------- render

#[must_use]
pub fn render_comparison(pair: &RunPair, comparison: &RunComparison) -> String {
    let mut out = format!(
        "compare {} vs {}  (engine {})\n",
        pair.a.head.model, pair.b.head.model, pair.a.head.stamp.engine_build_commit
    );
    if comparison.depths.is_empty() {
        out.push_str("no depth measured in both runs — nothing to compare\n");
    } else {
        out.extend(comparison.depths.iter().map(|row| verdict_line(pair, row)));
    }
    out.push_str(&render_agentic(pair, &comparison.agentic));
    out.push_str(&render_codebase(pair, &comparison.codebase));
    out
}

/// A section one run never measured is named as absent. A section that simply
/// vanished would read as a section where nothing differed.
fn absent_line(pair: &RunPair, section: &str, presence: Presence) -> Option<String> {
    let missing = match presence {
        Presence::Both => return None,
        Presence::OnlyA => pair.b.head.model.clone(),
        Presence::OnlyB => pair.a.head.model.clone(),
        Presence::Neither => format!("{} or {}", pair.a.head.model, pair.b.head.model),
    };
    Some(format!("{section}: not measured in {missing}\n"))
}

/// The label column, wide enough for the longest label in this section and
/// never narrower than `floor`, so the numbers line up down the block.
fn column_width(lengths: impl Iterator<Item = usize>, floor: usize) -> usize {
    lengths.max().unwrap_or(0).max(floor) + 2
}

fn render_agentic(pair: &RunPair, agentic: &AgenticComparison) -> String {
    if let Some(line) = absent_line(pair, "agentic", agentic.presence) {
        return line;
    }
    if agentic.totals.is_empty() {
        return "agentic\n  no case graded in both runs\n".to_owned();
    }
    let width = column_width(agentic.totals.iter().map(|t| t.label.len()), 22);
    let mut out = String::from("agentic\n");
    out.extend(agentic.totals.iter().map(|total| totals_line(total, width)));
    out.push_str(&disagreement_block(pair, &agentic.disagreements));
    out.push_str(&only_in_block(pair, &agentic.only_in));
    out.push_str(&unavailable_line(agentic.unavailable));
    out
}

fn totals_line(total: &SuiteTotals, width: usize) -> String {
    format!("  {:<width$}{} vs {}\n", total.label, total.a, total.b)
}

/// Always printed, an honest zero included: a count silently left out is
/// indistinguishable from a count of nothing.
fn unavailable_line(counts: (usize, usize)) -> String {
    format!("  unavailable excluded: {} vs {}\n", counts.0, counts.1)
}

fn disagreement_block(pair: &RunPair, cases: &[CaseDelta]) -> String {
    if cases.is_empty() {
        return "  disagreements: none — the two runs answer every shared case alike\n".to_owned();
    }
    let width = column_width(cases.iter().map(|c| case_id(c).len()), 16);
    let mut out = String::from("  disagreements (cases that separate the two):\n");
    out.extend(
        cases
            .iter()
            .map(|case| disagreement_line(pair, case, width)),
    );
    out
}

fn disagreement_line(pair: &RunPair, case: &CaseDelta, width: usize) -> String {
    format!(
        "    {:<width$}{}   |   {}\n",
        case_id(case),
        case_side(&pair.a.head.model, case.a_pass, case.a_reason.as_deref()),
        case_side(&pair.b.head.model, case.b_pass, case.b_reason.as_deref()),
    )
}

fn case_id(case: &CaseDelta) -> String {
    format!("{}{}", case.task_id, door_tag(case.transport))
}

fn case_side(model: &str, pass: bool, reason: Option<&str>) -> String {
    if pass {
        return format!("{model} pass");
    }
    reason.map_or_else(
        || format!("{model} FAIL"),
        |why| format!("{model} FAIL — {why}"),
    )
}

fn only_in_block(pair: &RunPair, cases: &[OnlyIn]) -> String {
    [("a", &pair.a.head.model), ("b", &pair.b.head.model)]
        .into_iter()
        .filter_map(|(which, model)| only_in_line(cases, which, model))
        .collect()
}

fn only_in_line(cases: &[OnlyIn], which: &str, model: &str) -> Option<String> {
    let ids: Vec<String> = cases
        .iter()
        .filter(|c| c.which == which)
        .map(|c| format!("{}{}", c.task_id, door_tag(c.transport)))
        .collect();
    if ids.is_empty() {
        return None;
    }
    Some(format!("  only in {model}: {}\n", ids.join(", ")))
}

/// The two column widths the codebase block lines up on.
#[derive(Debug, Clone, Copy)]
struct Columns {
    group: usize,
    tier: usize,
}

fn render_codebase(pair: &RunPair, codebase: &CodebaseComparison) -> String {
    if let Some(line) = absent_line(pair, "codebase", codebase.presence) {
        return line;
    }
    if codebase.tiers.is_empty() {
        return "codebase\n  no task scored in both runs\n".to_owned();
    }
    let widths = Columns {
        group: column_width(codebase.tiers.iter().map(|t| t.group.len()), 13),
        tier: column_width(codebase.tiers.iter().map(|t| t.tier.len()), 8),
    };
    let mut out = String::from("codebase\n");
    out.extend(
        codebase
            .tiers
            .iter()
            .map(|delta| tier_delta_line(pair, delta, widths)),
    );
    if let Some(note) = &codebase.judge_note {
        out.push_str("  ");
        out.push_str(note);
        out.push('\n');
    }
    out.push_str(&dropped_block(&codebase.dropped));
    out
}

fn tier_delta_line(pair: &RunPair, delta: &TierDelta, widths: Columns) -> String {
    format!(
        "  {:<group$}{:<tier$}{:.2} vs {:.2}  (Δ {:+.2})   A better {}, B better {}, tie {} — {}\n",
        delta.group,
        delta.tier,
        delta.mean_a,
        delta.mean_b,
        delta.mean_a - delta.mean_b,
        delta.a_better,
        delta.b_better,
        delta.ties,
        delta_phrase((&pair.a.head.model, &pair.b.head.model), delta),
        group = widths.group,
        tier = widths.tier,
    )
}

/// The verdict names the model. A verdict the reader has to decode against
/// the header ("A is better") is a verdict about nothing — and a split the
/// exact test could not reach says so, rather than posing as "no difference".
fn delta_phrase(models: (&str, &str), delta: &TierDelta) -> String {
    let untied = delta.a_better + delta.b_better;
    if untied > MAX_SIGN_TEST_N as usize {
        return format!("sign test not run (n = {untied} > {MAX_SIGN_TEST_N})");
    }
    match delta.verdict {
        Comparison::Faster => format!("{} is better", models.0),
        Comparison::Slower => format!("{} is better", models.1),
        Comparison::NoSignificantDifference => "no significant difference".to_owned(),
    }
}

/// Tasks a group lost because one run or the other could not answer them —
/// printed per group, because a silently smaller denominator is its own
/// dishonesty.
fn dropped_block(dropped: &[GroupDrop]) -> String {
    dropped
        .iter()
        .filter(|d| d.dropped > 0)
        .map(drop_line)
        .collect()
}

fn drop_line(drop: &GroupDrop) -> String {
    format!(
        "  {} task(s) dropped from {}: {}\n",
        drop.dropped, drop.group, drop.reason
    )
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
    use super::{
        CompareOpts, Presence, RunPair, SuiteTotals, TierDelta, binomial, compare_runs,
        cross_runtime_banner, render_comparison, sign_test,
    };
    use crate::core::bench::codebase::{Excluded, TaskTier};
    use crate::core::bench::stamp::{JudgeStamp, Stamp};
    use crate::core::bench::store::{
        CodebaseRow, DecidedBy, GradeRow, JUDGE_SUITE, JudgeRow, Measure, RunHead, RunLog, TaskRow,
        Transport,
    };
    use crate::core::stats::Comparison;
    use crate::error::ChekovError;

    fn stamp(engine: &str, weights: &str) -> Stamp {
        Stamp {
            machine_id: "8d41f0c2a917".into(),
            runtime: crate::core::bench::stamp::RUNTIME_LLAMA_CPP.to_owned(),
            timing_source: crate::core::bench::stamp::TIMING_SERVER.to_owned(),
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
            spec_type: "engine-default".into(),
            spec_draft_n_max: "engine-default".into(),
            allow_exec: false,
            cargo_version: None,
            exec_target: "none".into(),
            seed: 42,
            temperature_milli: 0,
            chekov_version: "0.1.0".into(),
            prompt_set_hash: "e19a".into(),
            corpus_id: "throughput-v1".into(),
            judge: None,
        }
    }

    fn head_of(model: &str, stamp: Stamp) -> RunHead {
        RunHead {
            model: model.into(),
            machine_brand: None,
            launch_args: vec![],
            forced_reasoning_format: None,
            stamp,
        }
    }

    fn head(model: &str) -> RunHead {
        head_of(model, stamp("dda1b0d67", "r1/s1"))
    }

    fn run(model: &str, stamp: Stamp, decode: &[f64]) -> RunLog {
        RunLog {
            head: head_of(model, stamp),
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
                codebase: None,
                judge: None,
            }],
        }
    }

    fn opts(significance_pct: f64) -> CompareOpts {
        CompareOpts {
            significance_pct,
            cross_runtime: false,
            cross_flags: false,
        }
    }

    /// Two llama.cpp runs that differ only on launch flags are the flag
    /// experiment `--cross-flags` exists for: exactly the eight flag fields
    /// are masked, the banner names the ones that differ and says what is
    /// being measured, and everything else still refuses (spec-stage
    /// follow-up (b), 2026-09-03).
    #[test]
    fn cross_flags_masks_exactly_the_eight_launch_flags_and_names_them() {
        assert_eq!(super::CROSS_FLAGS_ALLOWED.len(), 8);
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let mut flagged = stamp("dda1b0d67", "r1/s1");
        flagged.n_batch = "4096".into();
        flagged.spec_type = "draft-mtp".into();
        flagged.spec_draft_n_max = "1".into();
        let b = run("m2", flagged.clone(), &[19.0, 21.0, 22.0]);
        let err = compare_runs(&a, &b, &opts(5.0)).expect_err("flags differ");
        match err {
            ChekovError::BenchStampMismatch { field, .. } => assert_eq!(field, "n_batch"),
            other => panic!("expected stamp mismatch, got {other}"),
        }
        let cross = CompareOpts {
            cross_flags: true,
            ..opts(5.0)
        };
        assert!(compare_runs(&a, &b, &cross).is_ok());
        let banner = cross_flags_banner(&a.head.stamp, &b.head.stamp);
        assert!(banner.starts_with("launch-flag comparison: m1 vs m2\n"), "{banner}");
        assert!(banner.contains("n_batch: \"engine-default\" vs \"4096\"\n"), "{banner}");
        assert!(banner.contains("spec_type: \"engine-default\" vs \"draft-mtp\"\n"), "{banner}");
        assert!(!banner.contains("flash_attn:"), "an equal flag is not listed: {banner}");
        assert!(banner.ends_with("this measures the launch flags, not the model.\n"), "{banner}");

        let mut foreign = flagged;
        foreign.runtime = "mtplx 0.4.1".into();
        let c = run("m2", foreign, &[19.0, 21.0, 22.0]);
        let err = compare_runs(&a, &c, &cross).expect_err("runtime is not a flag");
        match err {
            ChekovError::BenchStampMismatch { field, .. } => assert_eq!(field, "runtime"),
            other => panic!("expected stamp mismatch, got {other}"),
        }
    }

    #[test]
    fn a_differing_environment_is_refused_naming_the_first_field() {
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let mut changed = stamp("dda1b0d67", "r2/s2");
        changed.seed = 7;
        changed.type_k = "f16".into();
        let b = run("m2", changed, &[19.0, 21.0, 22.0]);
        let err = compare_runs(&a, &b, &opts(5.0)).expect_err("environment differs");
        match err {
            ChekovError::BenchStampMismatch { field, .. } => {
                assert_eq!(field, "type_k", "type_k precedes seed in declaration order");
            }
            other => panic!("expected stamp mismatch, got {other}"),
        }
    }

    #[test]
    fn a_differing_judge_does_not_refuse_the_comparison() {
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let mut b = run("m2", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        b.head.stamp.judge = Some(crate::core::bench::stamp::JudgeStamp {
            model: "gpt-oss-20b".into(),
            quant: "F16".into(),
            revision: "d449b42d93e1".into(),
            arch: "gpt-oss".into(),
            rubric_hash: "9f8e7d6c5b4a".into(),
            max_tokens: 512,
            reasoning_effort: "low".into(),
            min_consistency_pct: 70,
        });
        assert!(super::assert_same_environment(&RunPair { a: &a, b: &b }, &opts(5.0)).is_ok());
    }

    #[test]
    fn cross_runtime_masks_exactly_the_allow_list() {
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let foreign_stamp = |corpus_id: &str| {
            let mut s = stamp("dda1b0d67", "r1/s1");
            s.runtime = "mtplx 0.4.1".into();
            s.engine_build_commit = "abc123".into();
            s.ctx = 65_536;
            s.n_parallel = 4;
            s.kv_unified = "on".into();
            s.n_batch = "2048".into();
            s.n_ubatch = "512".into();
            s.type_k = "f16".into();
            s.type_v = "f16".into();
            s.flash_attn = "off".into();
            s.prompt_set_hash = "ffff".into();
            s.corpus_id = corpus_id.into();
            s
        };
        let cross = opts(5.0);
        let cross = CompareOpts {
            cross_runtime: true,
            ..cross
        };

        let b = run("m2", foreign_stamp("throughput-v1"), &[19.0, 21.0, 22.0]);
        assert!(compare_runs(&a, &b, &cross).is_ok());

        let b_bad_corpus = run("m2", foreign_stamp("other"), &[19.0, 21.0, 22.0]);
        let err = compare_runs(&a, &b_bad_corpus, &cross).expect_err("corpus_id must still match");
        match err {
            ChekovError::BenchStampMismatch { field, .. } => assert_eq!(field, "corpus_id"),
            other => panic!("expected stamp mismatch, got {other}"),
        }

        let err = compare_runs(&a, &b, &opts(5.0)).expect_err("runtime refused without the flag");
        match err {
            ChekovError::BenchStampMismatch { field, .. } => assert_eq!(field, "runtime"),
            other => panic!("expected stamp mismatch, got {other}"),
        }
    }

    #[test]
    fn timing_source_is_allow_listed_only_under_cross_runtime() {
        assert_eq!(super::CROSS_RUNTIME_ALLOWED.len(), 14);

        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let mut foreign = stamp("dda1b0d67", "r1/s1");
        foreign.timing_source = crate::core::bench::stamp::TIMING_CHEKOV_STREAMED.to_owned();
        let b = run("m2", foreign, &[19.0, 21.0, 22.0]);

        let err =
            compare_runs(&a, &b, &opts(5.0)).expect_err("timing_source refused without the flag");
        match err {
            ChekovError::BenchStampMismatch { field, .. } => assert_eq!(field, "timing_source"),
            other => panic!("expected stamp mismatch, got {other}"),
        }

        let cross = CompareOpts {
            cross_runtime: true,
            ..opts(5.0)
        };
        assert!(compare_runs(&a, &b, &cross).is_ok());

        let banner = cross_runtime_banner(&a.head.stamp, &b.head.stamp);
        assert!(
            banner
                .lines()
                .any(|line| line.starts_with("timing_source: ")),
            "banner missing timing_source: {banner}"
        );
    }

    /// A run decoded with the MTP head and one decoded without it are
    /// different environments: refused by name, masked only under
    /// `--cross-runtime`, and named in the banner (spec-stage design §6).
    #[test]
    fn a_differing_spec_type_is_refused_and_allow_listed_only_under_cross_runtime() {
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let mut drafted = stamp("dda1b0d67", "r1/s1");
        drafted.spec_type = "draft-mtp".into();
        drafted.spec_draft_n_max = "1".into();
        let b = run("m2", drafted, &[19.0, 21.0, 22.0]);
        let err = compare_runs(&a, &b, &opts(5.0)).expect_err("the head is an environment");
        match err {
            ChekovError::BenchStampMismatch { field, a, b } => {
                assert_eq!(field, "spec_type");
                assert_eq!(
                    (a.as_str(), b.as_str()),
                    ("\"engine-default\"", "\"draft-mtp\"")
                );
            }
            other => panic!("expected stamp mismatch, got {other}"),
        }
        let cross = CompareOpts {
            cross_runtime: true,
            ..opts(5.0)
        };
        assert!(compare_runs(&a, &b, &cross).is_ok());
        let banner = cross_runtime_banner(&a.head.stamp, &b.head.stamp);
        assert!(
            banner.contains("spec_type: \"engine-default\" vs \"draft-mtp\""),
            "{banner}"
        );
        assert!(
            banner.contains("spec_draft_n_max: \"engine-default\" vs \"1\""),
            "{banner}"
        );
    }

    #[test]
    fn the_banner_names_each_differing_allow_listed_field_and_only_those() {
        let a = stamp("dda1b0d67", "r1/s1");
        let mut b = stamp("dda1b0d67", "r1/s1");
        b.runtime = "mtplx 0.4.1".into();
        b.engine_build_commit = "def456".into();
        b.flash_attn = "off".into();
        b.prompt_set_hash = "ffff".into();

        let banner = cross_runtime_banner(&a, &b);
        assert!(banner.starts_with("cross-runtime comparison: llama.cpp vs mtplx 0.4.1\n"));
        assert!(banner.contains("determinism does not hold across runtimes"));
        for needle in [
            "runtime: ",
            "engine_build_commit: ",
            "flash_attn: ",
            "prompt_set_hash: ",
        ] {
            assert!(banner.contains(needle), "missing {needle}");
        }
        assert!(!banner.contains("corpus_id"));
        assert!(
            banner
                .trim_end()
                .ends_with("this measures the runtimes, not the model.")
        );
    }

    #[test]
    fn same_runtime_cross_runtime_banners_with_no_field_lines() {
        let a = stamp("dda1b0d67", "r1/s1");
        let b = stamp("dda1b0d67", "r1/s1");
        let banner = cross_runtime_banner(&a, &b);
        let lines: Vec<&str> = banner.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "cross-runtime comparison: llama.cpp vs llama.cpp");
        assert!(lines[1].contains("determinism does not hold across runtimes"));
        assert_eq!(lines[2], "this measures the runtimes, not the model.");
    }

    #[test]
    fn differing_models_under_one_environment_compare_fine() {
        // weights_revision and quant are the SUBJECT of the comparison.
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 20.0, 21.0, 22.0]);
        let mut other = stamp("dda1b0d67", "r2/s2");
        other.quant = "UD-Q6_K_XL".into();
        let b = run("m2", other, &[19.5, 20.5, 21.0, 21.5]);
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        assert_eq!(compared.depths.len(), 1);
        assert_eq!(
            compared.depths[0].verdict,
            Comparison::NoSignificantDifference
        );
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(rendered.contains("no significant difference"), "{rendered}");
    }

    #[test]
    fn a_differing_prompt_set_is_refused() {
        // You cannot compare runs of different task sets.
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[19.0, 21.0, 22.0]);
        let mut other = stamp("dda1b0d67", "r1/s1");
        other.prompt_set_hash = "ffff".into();
        let b = run("m2", other, &[19.0, 21.0, 22.0]);
        assert!(compare_runs(&a, &b, &opts(5.0)).is_err());
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
            codebase: None,
            judge: None,
        });
        let b = run("m2", stamp("dda1b0d67", "r2/s2"), &[30.0, 40.0, 41.0]);
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        assert_eq!(
            compared.depths.len(),
            1,
            "depth 4096 exists only in one run"
        );
        assert_eq!(compared.depths[0].depth, 1024);
    }

    #[test]
    fn a_clear_gap_is_called() {
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[38.0, 40.0, 41.0, 40.5]);
        let b = run("m2", stamp("dda1b0d67", "r2/s2"), &[19.0, 20.0, 21.0, 20.5]);
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        assert_eq!(compared.depths[0].verdict, Comparison::Faster);
    }

    // ------------------------------------------------------ agentic fixtures

    /// One graded case: the suite, the id, and how it went.
    struct Case {
        suite: &'static str,
        task_id: &'static str,
        grade: GradeRow,
    }

    impl Case {
        fn pass(suite: &'static str, task_id: &'static str) -> Self {
            Self {
                suite,
                task_id,
                grade: GradeRow::pass(),
            }
        }

        fn fail(suite: &'static str, task_id: &'static str, why: &str) -> Self {
            Self {
                suite,
                task_id,
                grade: GradeRow::fail(why.to_owned()),
            }
        }

        fn na(suite: &'static str, task_id: &'static str) -> Self {
            Self {
                suite,
                task_id,
                grade: GradeRow::unavailable("the engine refused".to_owned()),
            }
        }
    }

    fn empty_measure() -> Measure {
        Measure {
            prompt_n: 0,
            decode_samples: vec![],
            prefill_samples: vec![],
            warmup_dropped: 0,
            cache_n: 0,
        }
    }

    fn graded_row(seq: usize, case: Case) -> TaskRow {
        TaskRow {
            schema: 1,
            run_id: "r".into(),
            seq: u32::try_from(seq).unwrap_or(0),
            suite: case.suite.into(),
            task_id: case.task_id.into(),
            transport: Transport::Buffered,
            measure: empty_measure(),
            grade: Some(case.grade),
            codebase: None,
            judge: None,
        }
    }

    fn agentic_run(model: &str, cases: Vec<Case>) -> RunLog {
        RunLog {
            head: head(model),
            rows: cases
                .into_iter()
                .enumerate()
                .map(|(seq, case)| graded_row(seq, case))
                .collect(),
        }
    }

    fn cells(totals: &[SuiteTotals], label: &str) -> (String, String) {
        let found = totals
            .iter()
            .find(|t| t.label == label)
            .unwrap_or_else(|| panic!("no total labelled {label}"));
        (found.a.clone(), found.b.clone())
    }

    // ----------------------------------------------------- codebase fixtures

    const GOLD: &str = "let total = sum(values);";
    const OTHER: &str = "let other = product(items);";

    /// One codebase task, as one run answered it.
    struct CodeCase {
        task_id: String,
        tier: TaskTier,
        prediction: &'static str,
        /// The cross-file arm this row is — `None` on the same-file tiers.
        arm: Option<&'static str>,
    }

    fn codebase_row(seq: usize, case: CodeCase) -> TaskRow {
        TaskRow {
            schema: 1,
            run_id: "r".into(),
            seq: u32::try_from(seq).unwrap_or(0),
            suite: "codebase".into(),
            task_id: case.task_id,
            transport: Transport::Buffered,
            measure: empty_measure(),
            grade: Some(GradeRow::pass()),
            codebase: Some(CodebaseRow {
                tier: case.tier,
                file: "src/lib.rs".into(),
                line: 1,
                label: "fn f".into(),
                gold: GOLD.into(),
                prediction: case.prediction.into(),
                prefix: "fn f() {".into(),
                suffix: "}".into(),
                excluded: Excluded {
                    doc_comment: 0,
                    cross_file: "slice A".into(),
                    cfg_test_lines: 0,
                    cross_file_withheld: 0,
                },
                symbols_score: None,
                unsupported: false,
                arm: case.arm.map(str::to_owned),
                extra: None,
                also_first_uses: Vec::new(),
                name: None,
                n_predict: None,
                exec: None,
            }),
            judge: None,
        }
    }

    /// Six tasks of one group: A answers the first `a_wins` exactly and B
    /// answers the rest, so the split is `a_wins` to `6 - a_wins`.
    fn code_cases(tier: TaskTier, a_wins: usize) -> (Vec<CodeCase>, Vec<CodeCase>) {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for index in 0..6 {
            let task_id = format!("{}-{index}", tier.label());
            let a_right = index < a_wins;
            a.push(CodeCase {
                task_id: task_id.clone(),
                tier,
                prediction: if a_right { GOLD } else { OTHER },
                arm: None,
            });
            b.push(CodeCase {
                task_id,
                tier,
                prediction: if a_right { OTHER } else { GOLD },
                arm: None,
            });
        }
        (a, b)
    }

    fn codebase_run(model: &str, cases: Vec<CodeCase>) -> RunLog {
        RunLog {
            head: head(model),
            rows: cases
                .into_iter()
                .enumerate()
                .map(|(seq, case)| codebase_row(seq, case))
                .collect(),
        }
    }

    /// `in_file` split six-nil, `function_body` split four-two: one group the
    /// sign test can call and one it cannot.
    fn codebase_pair() -> (RunLog, RunLog) {
        let (mut a, mut b) = code_cases(TaskTier::InFile, 6);
        let (body_a, body_b) = code_cases(TaskTier::FunctionBody, 4);
        a.extend(body_a);
        b.extend(body_b);
        (codebase_run("m1", a), codebase_run("m2", b))
    }

    fn tier_of<'a>(tiers: &'a [TierDelta], group: &str, tier: &str) -> &'a TierDelta {
        tiers
            .iter()
            .find(|t| t.group == group && t.tier == tier)
            .unwrap_or_else(|| panic!("no {group}/{tier} delta"))
    }

    /// `float_cmp` is on, and it is right to be: these are means of divisions.
    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // ------------------------------------------------- cross-file fixtures

    /// One cross-file task's two arms for one run: what the model answered
    /// without the defining file, then with it.
    fn cross_arms(index: usize, without: &'static str, with: &'static str) -> Vec<CodeCase> {
        use crate::core::bench::codebase::ARM_EXTRA_SUFFIX;
        use crate::core::bench::codebase::run::{NO_EXTRA, WITH_EXTRA};
        let base = format!("cff-{index}");
        vec![
            CodeCase {
                task_id: base.clone(),
                tier: TaskTier::CrossFileFirst,
                prediction: without,
                arm: Some(NO_EXTRA),
            },
            CodeCase {
                task_id: format!("{base}{ARM_EXTRA_SUFFIX}"),
                tier: TaskTier::CrossFileFirst,
                prediction: with,
                arm: Some(WITH_EXTRA),
            },
        ]
    }

    /// Six cross-file tasks. A is wrong without the defining file and right
    /// with it (a lift of +1 on `exact`, every task); B is the mirror image
    /// (−1). The blended group would read 0.50 vs 0.50 and hide all of it.
    fn cross_file_pair() -> (RunLog, RunLog) {
        let a = (0..6).flat_map(|i| cross_arms(i, OTHER, GOLD)).collect();
        let b = (0..6).flat_map(|i| cross_arms(i, GOLD, OTHER)).collect();
        (codebase_run("m1", a), codebase_run("m2", b))
    }

    #[test]
    fn the_two_cross_file_arms_are_compared_apart_not_blended() {
        let (a, b) = cross_file_pair();
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        let without = tier_of(&compared.codebase.tiers, "cross_file_first", "exact");
        assert!(
            approx(without.mean_a, 0.0) && approx(without.mean_b, 1.0),
            "{without:?}"
        );
        let with = tier_of(&compared.codebase.tiers, "cross_file_first+extra", "exact");
        assert!(
            approx(with.mean_a, 1.0) && approx(with.mean_b, 0.0),
            "{with:?}"
        );
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(
            rendered
                .lines()
                .any(|l| l.trim_start().starts_with("cross_file_first+extra")),
            "{rendered}"
        );
    }

    #[test]
    fn the_context_lift_is_compared_per_task_with_the_paired_sign_test() {
        let (a, b) = cross_file_pair();
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        let lift = tier_of(&compared.codebase.tiers, "context lift", "exact");
        assert!(
            approx(lift.mean_a, 1.0) && approx(lift.mean_b, -1.0),
            "{lift:?}"
        );
        assert_eq!((lift.a_better, lift.b_better, lift.ties), (6, 0, 0));
        assert_eq!(lift.verdict, Comparison::Faster, "six of six is a finding");
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(
            rendered
                .lines()
                .any(|l| l.trim_start().starts_with("context lift")
                    && l.contains("1.00 vs -1.00  (Δ +2.00)")
                    && l.ends_with("m1 is better")),
            "{rendered}"
        );
    }

    #[test]
    fn a_task_measured_on_one_arm_only_leaves_the_lift_and_is_counted() {
        let (a, mut b) = cross_file_pair();
        b.rows.retain(|r| r.task_id != "cff-5+extra");
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        let lift = tier_of(&compared.codebase.tiers, "context lift", "exact");
        assert_eq!(
            lift.a_better + lift.b_better + lift.ties,
            5,
            "five tasks have both arms in both runs"
        );
        let drop = compared
            .codebase
            .dropped
            .iter()
            .find(|d| d.group == "context lift")
            .expect("the lift reports what it lost");
        assert_eq!(drop.dropped, 1);
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(
            rendered.contains(
                "1 task(s) dropped from context lift: measured on one arm only in one run\n"
            ),
            "{rendered}"
        );
    }

    // --------------------------------------------------------------- agentic

    /// Every agentic suite, both doors closed, with each run losing a
    /// different case — so no two totals happen to agree by accident.
    fn mixed_agentic_pair() -> (RunLog, RunLog) {
        let a = agentic_run(
            "m1",
            vec![
                Case::pass("tool_emit", "te-001"),
                Case::fail("tool_emit", "te-002", "no call emitted"),
                Case::pass("grammar_gap", "gg-te-001"),
                Case::pass("grammar_gap", "gg-te-002"),
                Case::pass("instruction", "if-001"),
                Case::fail("instruction", "if-002", "loose:pass; trailing prose"),
            ],
        );
        let b = agentic_run(
            "m2",
            vec![
                Case::pass("tool_emit", "te-001"),
                Case::pass("tool_emit", "te-002"),
                Case::pass("grammar_gap", "gg-te-001"),
                Case::fail("grammar_gap", "gg-te-002", "the grammar was refused"),
                Case::pass("instruction", "if-001"),
                Case::pass("instruction", "if-002"),
            ],
        );
        (a, b)
    }

    #[test]
    fn the_agentic_totals_stand_side_by_side_in_the_reports_own_counting() {
        let (a, b) = mixed_agentic_pair();
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        let totals = &compared.agentic.totals;
        assert_eq!(cells(totals, "tool_emit"), ("1/2".into(), "2/2".into()));
        assert_eq!(
            cells(totals, "grammar_gap forced"),
            ("2/2".into(), "1/2".into())
        );
        assert_eq!(
            cells(totals, "grammar_gap unconstrained"),
            ("1/2".into(), "2/2".into())
        );
        assert_eq!(
            cells(totals, "instruction strict"),
            ("1/2".into(), "2/2".into())
        );
        assert_eq!(
            cells(totals, "instruction loose"),
            ("2/2".into(), "2/2".into()),
            "the loose arm rescues the case whose reason records loose:pass"
        );
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        let line = |label: &str| {
            rendered
                .lines()
                .find(|l| l.trim_start().starts_with(label))
                .unwrap_or_else(|| panic!("no {label} line in {rendered}"))
        };
        assert!(
            line("grammar_gap forced").ends_with("2/2 vs 1/2"),
            "{rendered}"
        );
        assert_eq!(
            line("tool_emit").find(" vs "),
            line("instruction loose").find(" vs "),
            "one column width down the whole section: {rendered}"
        );
    }

    #[test]
    fn a_case_only_one_run_passes_is_listed_and_a_case_both_fail_is_not() {
        let a = agentic_run(
            "m1",
            vec![
                Case::fail("instruction", "if-012", "failed 'max_lines:8'"),
                Case::fail("tool_emit", "te-020", "no call emitted"),
                Case::pass("tool_emit", "te-021"),
            ],
        );
        let b = agentic_run(
            "m2",
            vec![
                Case::pass("instruction", "if-012"),
                Case::fail("tool_emit", "te-020", "no call emitted"),
                Case::pass("tool_emit", "te-021"),
            ],
        );
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        let cases = &compared.agentic.disagreements;
        assert_eq!(cases.len(), 1, "only if-012 separates the two runs");
        assert_eq!(cases[0].task_id, "if-012");
        assert!(!cases[0].a_pass && cases[0].b_pass);
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(
            rendered.contains("m1 FAIL — failed 'max_lines:8'   |   m2 pass"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("te-020"),
            "a case both runs fail separates nothing: {rendered}"
        );
        assert!(
            !rendered.contains("te-021"),
            "a case both runs pass separates nothing: {rendered}"
        );
    }

    #[test]
    fn a_case_graded_in_one_run_only_is_named_never_dropped() {
        let a = agentic_run(
            "m1",
            vec![
                Case::pass("tool_emit", "te-010"),
                Case::pass("tool_emit", "te-011"),
            ],
        );
        let b = agentic_run("m2", vec![Case::pass("tool_emit", "te-010")]);
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        assert_eq!(compared.agentic.only_in.len(), 1);
        assert_eq!(compared.agentic.only_in[0].which, "a");
        assert_eq!(compared.agentic.only_in[0].task_id, "te-011");
        assert_eq!(
            cells(&compared.agentic.totals, "tool_emit"),
            ("1/1".into(), "1/1".into()),
            "the totals count the cases both runs graded, and te-011 is named apart"
        );
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(rendered.contains("only in m1: te-011"), "{rendered}");
    }

    #[test]
    fn an_unavailable_case_leaves_every_count_and_is_reported_as_excluded() {
        let a = agentic_run(
            "m1",
            vec![
                Case::pass("tool_emit", "te-030"),
                Case::na("tool_emit", "te-031"),
            ],
        );
        let b = agentic_run(
            "m2",
            vec![
                Case::pass("tool_emit", "te-030"),
                Case::pass("tool_emit", "te-031"),
            ],
        );
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        assert_eq!(compared.agentic.unavailable, (1, 0));
        assert!(
            compared.agentic.disagreements.is_empty(),
            "a case nobody measured on one side is not a disagreement"
        );
        assert_eq!(
            cells(&compared.agentic.totals, "tool_emit"),
            ("1/1".into(), "2/2".into()),
            "the unavailable case leaves the numerator AND the denominator"
        );
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(
            rendered.contains("unavailable excluded: 1 vs 0"),
            "{rendered}"
        );
    }

    #[test]
    fn a_section_one_run_never_measured_is_named_not_dropped() {
        let a = agentic_run("m1", vec![Case::pass("tool_emit", "te-040")]);
        let b = RunLog {
            head: head("m2"),
            rows: vec![],
        };
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        assert_eq!(compared.agentic.presence, Presence::OnlyA);
        assert_eq!(compared.codebase.presence, Presence::Neither);
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(
            rendered.contains("agentic: not measured in m2"),
            "{rendered}"
        );
        assert!(
            rendered.contains("codebase: not measured in m1 or m2"),
            "{rendered}"
        );
    }

    // -------------------------------------------------------------- codebase

    #[test]
    fn codebase_tasks_pair_by_id_and_only_a_clear_group_is_called() {
        let (a, b) = codebase_pair();
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        let exact = tier_of(&compared.codebase.tiers, "in_file", "exact");
        assert_eq!((exact.a_better, exact.b_better, exact.ties), (6, 0, 0));
        assert!(approx(exact.mean_a, 1.0) && approx(exact.mean_b, 0.0));
        assert_eq!(exact.verdict, Comparison::Faster, "six-nil is p = 0.031");
        let ident = tier_of(&compared.codebase.tiers, "function_body", "ident_f1");
        assert_eq!((ident.a_better, ident.b_better, ident.ties), (4, 2, 0));
        assert_eq!(
            ident.verdict,
            Comparison::NoSignificantDifference,
            "four-two is p = 0.69"
        );
        assert!(
            !compared
                .codebase
                .tiers
                .iter()
                .any(|t| t.group == "function_body" && t.tier == "exact"),
            "tiers 1-2 skip function bodies, exactly as the report skips them"
        );
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(rendered.contains("in_file        exact"), "{rendered}");
        assert!(rendered.contains("m1 is better"), "{rendered}");
        assert!(rendered.contains("(Δ +1.00)"), "{rendered}");
    }

    #[test]
    fn a_codebase_task_unavailable_in_either_run_is_dropped_and_counted() {
        let (mut a, b) = codebase_pair();
        a.rows[0].grade = Some(GradeRow::unavailable("the engine refused".to_owned()));
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        let dropped = compared
            .codebase
            .dropped
            .iter()
            .find(|d| d.group == "in_file")
            .expect("the in_file group is present");
        assert_eq!(dropped.dropped, 1);
        let exact = tier_of(&compared.codebase.tiers, "in_file", "exact");
        assert_eq!((exact.a_better, exact.b_better), (5, 0));
        let rendered = render_comparison(&RunPair { a: &a, b: &b }, &compared);
        assert!(
            rendered.contains("1 task(s) dropped from in_file"),
            "{rendered}"
        );
    }

    // -------------------------------------------------------------- judge

    fn judge_stamp() -> JudgeStamp {
        JudgeStamp {
            model: "gpt-oss-20b".into(),
            quant: "F16".into(),
            revision: "1a2b3c4d5e6f".into(),
            arch: "gpt-oss".into(),
            rubric_hash: "9f8e7d6c5b4a".into(),
            max_tokens: 512,
            reasoning_effort: "low".into(),
            min_consistency_pct: 70,
        }
    }

    fn verdict(equivalent: Option<bool>) -> JudgeRow {
        JudgeRow {
            equivalent,
            gold_first: None,
            prediction_first: None,
            decided_by: if equivalent.is_some() {
                DecidedBy::SwapAgreement
            } else {
                DecidedBy::SwapDisagreement
            },
            skipped: None,
            judge_secs: 1.0,
        }
    }

    fn judge_row(seq: usize, task_id: &str, row: JudgeRow) -> TaskRow {
        TaskRow {
            schema: 1,
            run_id: "r".into(),
            seq: u32::try_from(seq).unwrap_or(0),
            suite: JUDGE_SUITE.into(),
            task_id: task_id.into(),
            transport: Transport::Buffered,
            measure: empty_measure(),
            grade: None,
            codebase: None,
            judge: Some(row),
        }
    }

    fn function_body_rows(ids: [&str; 3]) -> Vec<TaskRow> {
        ids.into_iter()
            .enumerate()
            .map(|(seq, id)| {
                codebase_row(
                    seq,
                    CodeCase {
                        task_id: id.into(),
                        tier: TaskTier::FunctionBody,
                        prediction: GOLD,
                        arm: None,
                    },
                )
            })
            .collect()
    }

    /// Three `function_body` tasks, both answered and both judged by the same
    /// instrument: a's judge calls fb-1 equivalent, fb-2 not, fb-3
    /// equivalent; b's calls fb-1 not, fb-2 not, and abstains on fb-3.
    fn judged_pair() -> (RunLog, RunLog) {
        let ids = ["fb-1", "fb-2", "fb-3"];
        let mut a_stamp = stamp("dda1b0d67", "r1/s1");
        a_stamp.judge = Some(judge_stamp());
        let mut b_stamp = stamp("dda1b0d67", "r2/s2");
        b_stamp.judge = Some(judge_stamp());
        let mut a_rows = function_body_rows(ids);
        a_rows.extend([
            judge_row(3, "fb-1", verdict(Some(true))),
            judge_row(4, "fb-2", verdict(Some(false))),
            judge_row(5, "fb-3", verdict(Some(true))),
        ]);
        let mut b_rows = function_body_rows(ids);
        b_rows.extend([
            judge_row(3, "fb-1", verdict(Some(false))),
            judge_row(4, "fb-2", verdict(Some(false))),
            judge_row(5, "fb-3", verdict(None)),
        ]);
        (
            RunLog {
                head: head_of("m1", a_stamp),
                rows: a_rows,
            },
            RunLog {
                head: head_of("m2", b_stamp),
                rows: b_rows,
            },
        )
    }

    #[test]
    fn a_shared_judge_yields_an_equiv_row_with_the_paired_sign_test() {
        // a: fb-1 true, fb-2 false, fb-3 true ; b: fb-1 false, fb-2 false, fb-3 undecided
        let (a, b) = judged_pair();
        let cmp = super::compare_runs(&a, &b, &opts(5.0)).expect("compare");
        let equiv = cmp
            .codebase
            .tiers
            .iter()
            .find(|t| t.tier == "equiv")
            .expect("equiv row");
        assert_eq!(equiv.group, "function_body");
        assert_eq!(
            (equiv.a_better, equiv.b_better, equiv.ties),
            (1, 0, 1),
            "fb-3 is not paired: b abstained"
        );
        assert!(cmp.codebase.judge_note.is_none());
        let out = super::render_comparison(&RunPair { a: &a, b: &b }, &cmp);
        // Two spaces, not one: the group column's floor (13) is exactly
        // `"function_body".len()`, and `column_width` always adds 2 — the
        // same convention that pads "in_file" out to 15 columns elsewhere
        // in this file (see `codebase_tasks_pair_by_id_and_only_a_clear_group_is_called`).
        assert!(out.contains("function_body  equiv"), "{out}");
    }

    #[test]
    fn a_differing_or_absent_judge_prints_a_note_and_leaves_the_tiers_alone() {
        let (a, mut b) = judged_pair();
        b.head.stamp.judge = None;
        let cmp = super::compare_runs(&a, &b, &opts(5.0)).expect("compare");
        assert!(cmp.codebase.tiers.iter().all(|t| t.tier != "equiv"));
        assert_eq!(
            cmp.codebase.judge_note.as_deref(),
            Some(
                "equiv: not compared (judge differs: a=gpt-oss-20b@1a2b3c4d5e6f/9f8e7d6c5b4a b=none)"
            )
        );
        let out = super::render_comparison(&RunPair { a: &a, b: &b }, &cmp);
        assert!(
            out.contains("  equiv: not compared (judge differs: a=gpt-oss-20b@1a2b3c4d5e6f/9f8e7d6c5b4a b=none)\n"),
            "{out}"
        );
    }

    /// One judge, both runs, and not one crossing decided in both of them:
    /// the absent `equiv` row is a fact about the overlap, not silence.
    #[test]
    fn a_shared_judge_with_no_shared_verdict_says_why_there_is_no_equiv_row() {
        let (mut a, mut b) = judged_pair();
        // a decides fb-1 and fb-2; b decides only fb-3 — no id is in both.
        a.rows
            .retain(|r| r.suite != JUDGE_SUITE || r.task_id != "fb-3");
        b.rows.retain(|r| r.suite != JUDGE_SUITE);
        b.rows.push(judge_row(3, "fb-3", verdict(Some(true))));
        let cmp = super::compare_runs(&a, &b, &opts(5.0)).expect("compare");
        assert!(cmp.codebase.tiers.iter().all(|t| t.tier != "equiv"));
        assert_eq!(
            cmp.codebase.judge_note.as_deref(),
            Some("equiv: not compared (no crossing judged in both runs)")
        );
        let out = super::render_comparison(&RunPair { a: &a, b: &b }, &cmp);
        assert!(
            out.contains("  equiv: not compared (no crossing judged in both runs)\n"),
            "{out}"
        );
    }

    // ------------------------------------------------------------- sign test

    #[test]
    fn the_sign_test_calls_six_nil_and_refuses_four_two_and_an_empty_split() {
        // 2·C(6,0)/2⁶ = 0.031 — under 0.05.
        assert_eq!(sign_test(6, 0), Comparison::Faster);
        assert_eq!(sign_test(0, 6), Comparison::Slower);
        // 2·(C(6,0)+C(6,1)+C(6,2))/2⁶ = 0.688 — nowhere near.
        assert_eq!(sign_test(4, 2), Comparison::NoSignificantDifference);
        // Nothing to test: no difference, never a winner by default.
        assert_eq!(sign_test(0, 0), Comparison::NoSignificantDifference);
    }

    #[test]
    fn the_sign_test_is_exact_at_the_boundary_an_approximation_would_miss() {
        // 2·C(5,0)/2⁵ = 0.0625 — a five-nil sweep is NOT significant, which a
        // normal approximation at this n would happily claim it was.
        assert_eq!(sign_test(5, 0), Comparison::NoSignificantDifference);
        assert_eq!(sign_test(6, 0), Comparison::Faster);
        assert_eq!(binomial(6, 0), 1);
        assert_eq!(binomial(6, 2), 15);
        assert_eq!(binomial(52, 26), 495_918_532_948_104);
    }

    #[test]
    fn a_split_past_the_exact_tests_reach_says_so_instead_of_posing_as_no_difference() {
        let delta = |a_better: usize, b_better: usize, verdict: Comparison| super::TierDelta {
            group: "in_file".into(),
            tier: "parse".into(),
            mean_a: 0.9,
            mean_b: 0.7,
            a_better,
            b_better,
            ties: 0,
            verdict,
        };
        let names = ("ornith", "qwen");
        assert_eq!(
            super::delta_phrase(names, &delta(101, 0, Comparison::NoSignificantDifference)),
            "sign test not run (n = 101 > 100)"
        );
        assert_eq!(
            super::delta_phrase(names, &delta(6, 0, Comparison::Faster)),
            "ornith is better"
        );
        assert_eq!(
            super::delta_phrase(names, &delta(0, 6, Comparison::Slower)),
            "qwen is better"
        );
        assert_eq!(
            super::delta_phrase(names, &delta(4, 2, Comparison::NoSignificantDifference)),
            "no significant difference"
        );
    }
}
