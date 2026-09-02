//! One bench run on disk (spec §7.5).
//!
//! `$CHEKOV_HOME/eval/<run_id>/` holds `stamp.json` (the pinned configuration
//! plus the exact launch argv) and `results.jsonl` — one object per task,
//! appended in a single flushed write, so a crash loses at most one task and
//! `--resume` picks up from the rest.
//!
//! Rows store raw samples only; summaries are recomputed on read, so a stored
//! median can never drift from what was measured.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::bench::codebase::ladder::{self, Score, Tier, as_f64};
use crate::core::bench::codebase::{Excluded, ExtraFile, TaskTier};
use crate::core::bench::stamp::{Stamp, first_mismatch, mismatch_error};
use crate::core::bench::sweep::curve_note;
use crate::core::stats;
use crate::error::ChekovError;

/// What this chekov writes and reads.
pub const SCHEMA_VERSION: u32 = 1;

pub use crate::core::bench::runner::Transport;

/// The suites whose rows are graded per case.
pub(crate) const AGENTIC: [&str; 3] = ["tool_emit", "grammar_gap", "instruction"];

/// The suites crossed through both doors, so a case can disagree with itself.
const PAIRED: [&str; 2] = ["tool_emit", "instruction"];

/// Everything `stamp.json` records about a run, once.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunHead {
    pub model: String,
    /// Human-readable beside the hashed `machine_id`.
    pub machine_brand: Option<String>,
    /// The exact argv the measured server was launched with (flag hygiene).
    pub launch_args: Vec<String>,
    /// The `reasoning_format` the forced (grammar) arm asked the engine for,
    /// when the run had one — the one way that arm differs from the
    /// unconstrained arm beyond the grammar itself. Absent on runs recorded
    /// before the field, and on runs without a forced pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_reasoning_format: Option<String>,
    pub stamp: Stamp,
}

/// One task's row — one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRow {
    pub schema: u32,
    pub run_id: String,
    pub seq: u32,
    pub suite: String,
    pub task_id: String,
    /// Which door the probe took. Rows written before the field exist only
    /// from the buffered door, so absent means buffered.
    #[serde(default)]
    pub transport: Transport,
    pub measure: Measure,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<GradeRow>,
    /// Present on `codebase` rows only: what the model saw, what it answered,
    /// and the gold. Tiers 1–4 are recomputed from these on read; tier 5
    /// needs the worktree and is scored at run time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codebase: Option<CodebaseRow>,
    /// Present on `judge` rows only: the verdict for the codebase row with
    /// the same `task_id`. Rows written before slice C load as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measure {
    pub prompt_n: u64,
    pub decode_samples: Vec<f64>,
    pub prefill_samples: Vec<f64>,
    /// Recorded per §7.4 even though `summarize` re-derives it — auditable.
    pub warmup_dropped: u32,
    /// Max prompt tokens served from the KV cache across the repetitions —
    /// the reason a warm rerun's `prompt_n` shrinks. Default so rows written
    /// before this field load as zero-cached.
    #[serde(default)]
    pub cache_n: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradeRow {
    pub pass: bool,
    pub reason: Option<String>,
    /// The task could not be measured at all — the engine refused, the
    /// capability is absent. NEVER a failure: a model is not wrong because
    /// something outside it would not run. An unavailable axis reports N/A
    /// with its reason (spec §7.5), never a zero.
    #[serde(default)]
    pub unavailable: bool,
}

impl GradeRow {
    #[must_use]
    pub const fn pass() -> Self {
        Self {
            pass: true,
            reason: None,
            unavailable: false,
        }
    }

    #[must_use]
    pub const fn fail(reason: String) -> Self {
        Self {
            pass: false,
            reason: Some(reason),
            unavailable: false,
        }
    }

    #[must_use]
    pub const fn unavailable(reason: String) -> Self {
        Self {
            pass: false,
            reason: Some(reason),
            unavailable: true,
        }
    }
}

/// Tier 6 or tier 7's outcome for one crossing.
///
/// Not `ladder::Score`: that one is `Copy` over a `&'static str`, and these
/// reasons carry cargo's own words. It is also serialised, and `Score` is not.
/// Widening `Score` would cost the four text tiers their `Copy` for a reason
/// none of them has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExecScore {
    Value(f64),
    Skipped(String),
}

/// Tiers 6-7 for one crossing, measured at run time and never recomputed.
///
/// A compile result cannot be re-derived from stored text the way tiers 1-4
/// can — the toolchain, the worktree and the rest of the workspace all went
/// into it — so this is the one part of a codebase row that is a stored score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRow {
    pub compile: ExecScore,
    /// `<file>:<line>: <message>` from the first `error` diagnostic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compile_error: Option<String>,
    /// The covering tests actually run, in file order, at most five.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
    pub test: ExecScore,
    /// `<test>: <cargo's text>` for the first candidate that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_failure: Option<String>,
    pub check_secs: f64,
    pub test_secs: f64,
}

impl ExecRow {
    /// Both tiers skipped for one reason, nothing measured — what a crossing
    /// records when the machine could not have run either of them.
    #[must_use]
    pub fn skipped(reason: &str) -> Self {
        Self {
            compile: ExecScore::Skipped(reason.to_owned()),
            compile_error: None,
            tests: Vec::new(),
            test: ExecScore::Skipped(reason.to_owned()),
            test_failure: None,
            check_secs: 0.0,
            test_secs: 0.0,
        }
    }
}

/// The suite a verdict row is recorded under — its own suite, because the
/// codebase row it judges was flushed long before the judge phase ran.
pub const JUDGE_SUITE: &str = "judge";

/// How a judge row was settled (spec C §5). "Not at all" is one of the ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    SwapAgreement,
    SwapDisagreement,
    Identical,
    Skipped,
}

/// One crossing's verdict: the two raw answers, what they settle to, and why
/// there is none when there is none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeRow {
    /// `None` = the two orders disagreed, or the crossing was skipped.
    pub equivalent: Option<bool>,
    /// The raw answer with the gold shown as A; `None` when that order was skipped.
    pub gold_first: Option<bool>,
    /// The raw answer with the prediction shown as A.
    pub prediction_first: Option<bool>,
    pub decided_by: DecidedBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    pub judge_secs: f64,
}

/// A codebase task's record (spec §8, slice A). Raw text in, scores out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodebaseRow {
    pub tier: TaskTier,
    pub file: String,
    pub line: usize,
    pub label: String,
    pub gold: String,
    pub prediction: String,
    pub prefix: String,
    pub suffix: String,
    pub excluded: Excluded,
    /// Tier 5 against the worktree's symbol set, scored at run time — and
    /// absent when the task was never answered. A task nobody asked has no
    /// score, and `0.0` would read as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbols_score: Option<f64>,
    /// The engine said this model cannot infill at all — recorded where the
    /// outcome is known, so the report never has to guess from an error's
    /// wording. Rows written before the field load as `false`: an outage was
    /// the commoner case, and claiming a capability gap is the worse error.
    #[serde(default)]
    pub unsupported: bool,
    /// Which arm of a cross-file crossing this row is — `"no_extra"` or
    /// `"extra"`. `None` on the same-file tiers, which have one arm and so
    /// no arm to name. Slice-A rows load as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm: Option<String>,
    /// The file the `extra` arm sent, and how much of it. `None` everywhere
    /// else — including the `no_extra` arm, which sent nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<ExtraFile>,
    /// Other names whose first use in the file also falls in this span.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_first_uses: Vec<String>,
    /// The symbol a cross-file crossing is keyed on — which name the model
    /// was asked to recover. `None` on the same-file tiers, which key on a
    /// span and not on a name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The `n_predict` this crossing actually sent, so a reader can tell a
    /// short fill from one the budget cut off. `None` on rows written before
    /// the field, where the number is unrecorded rather than zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_predict: Option<u32>,
    /// Tiers 6-7, when `--allow-exec` was given. `None` when it was not, and
    /// on every row written before B2 — which is what those runs were: runs
    /// that executed nothing, not runs that failed to compile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecRow>,
}

/// One task to append: its identity plus what was measured.
pub struct Task {
    pub suite: String,
    pub task_id: String,
    pub measure: Measure,
    pub grade: Option<GradeRow>,
    pub transport: Transport,
    /// Present on `codebase` rows only — see `TaskRow::codebase`.
    pub codebase: Option<CodebaseRow>,
    /// Present on `judge` rows only — see `TaskRow::judge`.
    pub judge: Option<JudgeRow>,
}

/// An open run directory being written.
#[derive(Debug)]
pub struct RunWriter {
    dir: PathBuf,
    run_id: String,
    file: std::fs::File,
    seq: u32,
}

impl RunWriter {
    /// Create `<eval>/<run_id>/`, write `stamp.json`, open the results file.
    pub fn create(eval_dir: &Path, run_id: &str, head: &RunHead) -> Result<Self, ChekovError> {
        let dir = eval_dir.join(run_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| ChekovError::io(format!("creating {}", dir.display()), e))?;
        let stamp_path = dir.join("stamp.json");
        let json = serde_json::to_string_pretty(head).map_err(|e| invalid(&stamp_path, e))?;
        std::fs::write(&stamp_path, json)
            .map_err(|e| ChekovError::io(format!("writing {}", stamp_path.display()), e))?;
        let file = open_append(&dir)?;
        Ok(Self {
            dir,
            run_id: run_id.to_owned(),
            file,
            seq: 0,
        })
    }

    /// Reopen an existing run for `--resume`. A differing stamp is refused —
    /// resuming under a changed configuration would mix two runs into one.
    pub fn resume(
        eval_dir: &Path,
        run_id: &str,
        head: &RunHead,
    ) -> Result<(Self, RunLog), ChekovError> {
        let log = RunLog::load(&eval_dir.join(run_id))?;
        if let Some(refusal) = mismatch_error(&log.head.stamp, &head.stamp) {
            return Err(refusal);
        }
        let dir = eval_dir.join(run_id);
        let file = open_append(&dir)?;
        let seq = u32::try_from(log.rows.len()).unwrap_or(u32::MAX);
        Ok((
            Self {
                dir,
                run_id: run_id.to_owned(),
                file,
                seq,
            },
            log,
        ))
    }

    /// One row, one flushed `O_APPEND` write — a crash loses at most this task.
    pub fn append(&mut self, task: Task) -> Result<(), ChekovError> {
        let row = TaskRow {
            schema: SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            seq: self.seq,
            suite: task.suite,
            task_id: task.task_id,
            transport: task.transport,
            measure: task.measure,
            grade: task.grade,
            codebase: task.codebase,
            judge: task.judge,
        };
        let results = self.dir.join("results.jsonl");
        let mut line = serde_json::to_string(&row).map_err(|e| invalid(&results, e))?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.flush())
            .map_err(|e| ChekovError::io(format!("appending {}", results.display()), e))?;
        self.seq += 1;
        Ok(())
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn open_append(dir: &Path) -> Result<std::fs::File, ChekovError> {
    let path = dir.join("results.jsonl");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| ChekovError::io(format!("opening {}", path.display()), e))
}

fn invalid(path: &Path, e: impl std::fmt::Display) -> ChekovError {
    ChekovError::BenchRunInvalid {
        path: path.to_path_buf(),
        reason: e.to_string(),
    }
}

/// A loaded run: head plus every recorded row.
#[derive(Debug)]
pub struct RunLog {
    pub head: RunHead,
    pub rows: Vec<TaskRow>,
}

impl RunLog {
    pub fn load(run_dir: &Path) -> Result<Self, ChekovError> {
        let stamp_path = run_dir.join("stamp.json");
        let head_text =
            std::fs::read_to_string(&stamp_path).map_err(|e| invalid(&stamp_path, e))?;
        let head: RunHead =
            serde_json::from_str(&head_text).map_err(|e| invalid(&stamp_path, e))?;
        let results = run_dir.join("results.jsonl");
        let rows = if results.exists() {
            read_rows(&results)?
        } else {
            Vec::new()
        };
        Ok(Self { head, rows })
    }

    /// Whether a task is already recorded through this door (the `--resume`
    /// skip test) — the other door's crossing of the same case is still owed.
    #[must_use]
    pub fn is_done(&self, key: &TaskKey) -> bool {
        self.rows.iter().any(|r| {
            r.suite == key.suite && r.task_id == key.task_id && r.transport == key.transport
        })
    }
}

/// A run recorded before it had a judge takes the judge on resume.
///
/// The head is rewritten with the judge added and nothing else changed — and
/// only when every other field of `incoming` already matches, so a resume the
/// next `RunWriter::resume` is about to refuse leaves the stamp exactly as it
/// found it. A run that already names a judge is left alone: `resume` compares
/// that judge like any other field.
pub fn adopt_judge(run_dir: &Path, incoming: &Stamp) -> Result<(), ChekovError> {
    if incoming.judge.is_none() {
        return Ok(());
    }
    let mut log = RunLog::load(run_dir)?;
    if log.head.stamp.judge.is_some() {
        return Ok(());
    }
    let mut candidate = log.head.stamp.clone();
    candidate.judge.clone_from(&incoming.judge);
    if first_mismatch(&candidate, incoming).is_some() {
        return Ok(());
    }
    log.head.stamp = candidate;
    let stamp_path = run_dir.join("stamp.json");
    let json = serde_json::to_string_pretty(&log.head).map_err(|e| invalid(&stamp_path, e))?;
    std::fs::write(&stamp_path, json)
        .map_err(|e| ChekovError::io(format!("writing {}", stamp_path.display()), e))
}

/// What identifies one recorded crossing: the case and the door it took.
#[derive(Debug, Clone, Copy)]
pub struct TaskKey<'a> {
    pub suite: &'a str,
    pub task_id: &'a str,
    pub transport: Transport,
}

impl<'a> TaskKey<'a> {
    #[must_use]
    pub const fn buffered(suite: &'a str, task_id: &'a str) -> Self {
        Self {
            suite,
            task_id,
            transport: Transport::Buffered,
        }
    }

    #[must_use]
    pub const fn streamed(suite: &'a str, task_id: &'a str) -> Self {
        Self {
            suite,
            task_id,
            transport: Transport::Streamed,
        }
    }
}

/// Every line parses or the load is loud, naming the line — a skipped corrupt
/// row would silently shrink a measurement set.
fn read_rows(path: &Path) -> Result<Vec<TaskRow>, ChekovError> {
    let text = std::fs::read_to_string(path).map_err(|e| invalid(path, e))?;
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: TaskRow = serde_json::from_str(line)
            .map_err(|e| invalid(path, format!("line {}: {e}", index + 1)))?;
        if row.schema != SCHEMA_VERSION {
            return Err(invalid(
                path,
                format!(
                    "line {}: schema {} — this chekov reads {SCHEMA_VERSION}",
                    index + 1,
                    row.schema
                ),
            ));
        }
        rows.push(row);
    }
    Ok(rows)
}

/// The run as a table, summaries recomputed from the samples.
#[must_use]
pub fn render_run(log: &RunLog) -> String {
    let stamp = &log.head.stamp;
    let mut out = format!(
        "bench {}  ctx {}  engine {}  machine {}\n",
        log.head.model, stamp.ctx, stamp.engine_build_commit, stamp.machine_id
    );
    out.push_str(&timing_source_line(stamp));
    out.push_str(&speculative_line(stamp));
    out.push_str(&throughput_table(log));
    let probes: String = log
        .rows
        .iter()
        .filter(|r| r.suite == "fixture")
        .map(probe_line)
        .collect();
    out.push_str(&probes);
    out.push_str(&suite_summaries(log));
    out.push_str(&render_codebase(log));
    out
}

/// The depth table, and only when throughput was actually measured.
///
/// Printing "insufficient depths to fit a curve" for a suite that never ran
/// reports a failure to fit a curve nobody asked for.
fn throughput_table(log: &RunLog) -> String {
    let rows: Vec<&TaskRow> = rows_of(log, "throughput").collect();
    if rows.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("depth  prompt_n  decode tok/s (median [p10..p90])  prefill tok/s  n\n");
    for row in &rows {
        out.push_str(&depth_line(row));
    }
    let summarisable = rows
        .iter()
        .filter(|r| stats::summarize(&r.measure.decode_samples).is_some())
        .count();
    if let Some(note) = curve_note(summarisable) {
        out.push_str(&note);
        out.push('\n');
    }
    out
}

/// Per-suite summary lines for the agentic suites: counts always printed,
/// failures listed individually, passes counted — no silent caps.
fn suite_summaries(log: &RunLog) -> String {
    let mut out = String::new();
    let failures: String = log
        .rows
        .iter()
        .filter(|r| AGENTIC.contains(&r.suite.as_str()))
        .filter(|r| r.grade.as_ref().is_some_and(|g| !g.pass && !g.unavailable))
        .map(agentic_fail_line)
        .collect();
    out.push_str(&failures);
    out.extend(tool_emit_line(log, Transport::Buffered));
    out.extend(grammar_gap_line(log));
    out.extend(instruction_line(log, Transport::Buffered));
    out.extend(tool_emit_line(log, Transport::Streamed));
    out.extend(instruction_line(log, Transport::Streamed));
    out.push_str(&asymmetry_lines(log));
    out
}

fn agentic_fail_line(row: &TaskRow) -> String {
    let reason = row
        .grade
        .as_ref()
        .and_then(|g| g.reason.as_deref())
        .unwrap_or("");
    format!(
        "{} FAIL {}{}  {reason}\n",
        row.suite,
        row.task_id,
        door_tag(row.transport)
    )
}

/// The buffered door is the unmarked one — every earlier run went through it.
pub(crate) const fn door_tag(transport: Transport) -> &'static str {
    match transport {
        Transport::Buffered => "",
        Transport::Streamed => " [streamed]",
    }
}

const fn door_label(transport: Transport) -> &'static str {
    match transport {
        Transport::Buffered => "",
        Transport::Streamed => "streamed ",
    }
}

/// The same case answered differently through the two doors — the finding
/// the streamed pass exists to surface. Only cases measured both ways
/// compare; a run that never streamed says nothing here.
fn asymmetry_lines(log: &RunLog) -> String {
    if !log.rows.iter().any(|r| r.transport == Transport::Streamed) {
        return String::new();
    }
    let lines: String = PAIRED
        .iter()
        .flat_map(|suite| both_ways(log, suite))
        .filter_map(disagreement)
        .collect();
    if lines.is_empty() {
        return "asymmetry    none — buffered and streamed agree on every case measured both \
                ways\n"
            .to_owned();
    }
    lines
}

/// (buffered, streamed) rows of the cases measured through both doors.
fn both_ways<'a>(
    log: &'a RunLog,
    suite: &'a str,
) -> impl Iterator<Item = (&'a TaskRow, &'a TaskRow)> {
    rows_via(log, suite, Transport::Streamed)
        .filter(|row| !is_unavailable(row))
        .filter_map(move |streamed| {
            let buffered = rows_via(log, suite, Transport::Buffered)
                .find(|row| row.task_id == streamed.task_id && !is_unavailable(row))?;
            Some((buffered, streamed))
        })
}

fn disagreement((buffered, streamed): (&TaskRow, &TaskRow)) -> Option<String> {
    let passed_one = |row: &TaskRow| row.grade.as_ref().is_some_and(|g| g.pass);
    let word = |pass: bool| if pass { "PASS" } else { "FAIL" };
    let (b, s) = (passed_one(buffered), passed_one(streamed));
    if b == s {
        return None;
    }
    let failing = if b { streamed } else { buffered };
    let reason = failing
        .grade
        .as_ref()
        .and_then(|g| g.reason.as_deref())
        .unwrap_or("");
    Some(format!(
        "asymmetry    {} {}: buffered {}, streamed {} — {reason}\n",
        buffered.suite,
        buffered.task_id,
        word(b),
        word(s)
    ))
}

fn rows_of<'a>(log: &'a RunLog, suite: &'a str) -> impl Iterator<Item = &'a TaskRow> {
    log.rows.iter().filter(move |r| r.suite == suite)
}

/// Every verdict row, in file order — the report's source for the judge
/// clause and trailer.
#[must_use]
pub(crate) fn judge_rows(log: &RunLog) -> Vec<&TaskRow> {
    rows_of(log, JUDGE_SUITE)
        .filter(|r| r.judge.is_some())
        .collect()
}

/// The verdicts themselves — what the `equiv` cell, the judge clause and the
/// judge trailer all read, without each of them unwrapping the task row.
fn verdicts(log: &RunLog) -> Vec<&JudgeRow> {
    rows_of(log, JUDGE_SUITE)
        .filter_map(|r| r.judge.as_ref())
        .collect()
}

fn rows_via<'a>(
    log: &'a RunLog,
    suite: &'a str,
    transport: Transport,
) -> impl Iterator<Item = &'a TaskRow> {
    rows_of(log, suite).filter(move |r| r.transport == transport)
}

fn passed(rows: &[&TaskRow]) -> usize {
    rows.iter()
        .filter(|r| r.grade.as_ref().is_some_and(|g| g.pass))
        .count()
}

pub(crate) fn is_unavailable(row: &TaskRow) -> bool {
    row.grade.as_ref().is_some_and(|g| g.unavailable)
}

/// A strict pass, or a failure whose reason records that the loose grader
/// would have let it through.
fn loosely_passed(row: &TaskRow) -> bool {
    row.grade.as_ref().is_some_and(|g| {
        g.pass
            || g.reason
                .as_deref()
                .is_some_and(|s| s.contains("loose:pass"))
    })
}

/// What a graded suite's rows count to, once the unavailable ones are out.
///
/// The report and `capability compare` print the same figure, so they count it
/// the same way — one helper rather than two that can drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tally {
    pub passed: usize,
    pub total: usize,
    pub excluded: usize,
}

impl Tally {
    /// The strict count: a row passes only when its grade says so.
    pub(crate) fn of(rows: &[&TaskRow]) -> Self {
        let (kept, excluded) = measured(rows);
        Self {
            passed: passed(&kept),
            total: kept.len(),
            excluded,
        }
    }

    /// The chattiness-tolerant count (`instruction`'s loose arm).
    pub(crate) fn loose(rows: &[&TaskRow]) -> Self {
        let (kept, excluded) = measured(rows);
        Self {
            passed: kept.iter().filter(|r| loosely_passed(r)).count(),
            total: kept.len(),
            excluded,
        }
    }

    /// `8/10` — the cell both the report and the comparison print.
    pub(crate) fn cell(&self) -> String {
        format!("{}/{}", self.passed, self.total)
    }
}

/// The unconstrained arm of the §7.2 gap: the buffered `tool_emit` rows whose
/// case the forced pass actually ran (`gg-<id>` names case `<id>`).
///
/// "The same cases" means the ones actually forced — an unavailable case has
/// no forced result to compare against, so including its unconstrained result
/// on one side of the gap invents the difference. The forced pass is buffered,
/// so it pairs against buffered rows only; adding the streamed crossings to
/// one side would invent a gap too.
pub(crate) fn unconstrained_arm<'a>(
    forced: &[&TaskRow],
    tool_emit: &[&'a TaskRow],
) -> Vec<&'a TaskRow> {
    let (kept, _) = measured(forced);
    let base_ids: Vec<&str> = kept
        .iter()
        .filter_map(|r| r.task_id.strip_prefix("gg-"))
        .collect();
    tool_emit
        .iter()
        .filter(|r| r.transport == Transport::Buffered)
        .filter(|r| base_ids.contains(&r.task_id.as_str()))
        .copied()
        .collect()
}

/// The rows that were actually measured, and how many were not.
///
/// An unmeasured task belongs in NEITHER the numerator nor the denominator:
/// leaving it in the denominator scores the model down for a question it was
/// never asked. The count rides along so the exclusion is always printed —
/// a silently smaller denominator is its own dishonesty.
fn measured<'a>(rows: &[&'a TaskRow]) -> (Vec<&'a TaskRow>, usize) {
    let kept: Vec<&TaskRow> = rows
        .iter()
        .filter(|r| !is_unavailable(r))
        .copied()
        .collect();
    let excluded = rows.len() - kept.len();
    (kept, excluded)
}

/// `; tests elided: L lines in F files`, or nothing when no row's file gave
/// any up.
///
/// The rows carry a per-file count, so the sum is over distinct files, not
/// over rows — three tasks from one file elided its tests once. A run that
/// cut nothing omits the clause: an honest zero here is only noise.
fn elided_note(rows: &[&TaskRow]) -> String {
    let by_file: std::collections::BTreeMap<&str, usize> = rows
        .iter()
        .filter_map(|r| r.codebase.as_ref())
        .map(|c| (c.file.as_str(), c.excluded.cfg_test_lines))
        .collect();
    let lines: usize = by_file.values().sum();
    if lines == 0 {
        return String::new();
    }
    let files = by_file.values().filter(|n| **n > 0).count();
    format!("; tests elided: {lines} lines in {files} files")
}

/// ` (N unavailable, excluded)`, or nothing when everything was measured.
fn excluded_note(excluded: usize) -> String {
    if excluded == 0 {
        String::new()
    } else {
        format!(" ({excluded} unavailable, excluded)")
    }
}

/// The reason the first unavailable row recorded, for an N/A line.
fn unavailable_reason(rows: &[&TaskRow]) -> String {
    rows.iter()
        .find(|r| is_unavailable(r))
        .and_then(|r| r.grade.as_ref())
        .and_then(|g| g.reason.as_deref())
        .unwrap_or("reason unrecorded")
        .to_owned()
}

/// How many distinct files the rows were drawn from.
fn distinct_files(rows: &[&TaskRow]) -> usize {
    rows.iter()
        .filter_map(|r| r.codebase.as_ref())
        .map(|c| c.file.as_str())
        .collect::<std::collections::BTreeSet<&str>>()
        .len()
}

/// The codebase block: counts and labels, then one line per tier group —
/// two of them for `cross_file_first`, one per arm — and the lift between
/// the arms.
///
/// The header says `engine window ≤ n_batch` because llama.cpp's `/infill`
/// caps the prefix at ~¾·`n_batch` tokens and the suffix at ~¼·`n_batch`;
/// `extra from ctx` because the extra chunk is bounded by the context, not
/// by the batch. chekov sends whole files and grades over whole files, but a
/// long file reaches the model only in part.
#[must_use]
pub fn render_codebase(log: &RunLog) -> String {
    let rows: Vec<&TaskRow> = rows_of(log, "codebase")
        .filter(|r| r.codebase.is_some())
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let (kept, excluded) = measured(&rows);
    if kept.is_empty() {
        return codebase_na_line(&rows);
    }
    let mut out = codebase_header(&Header {
        kept: &kept,
        excluded,
        log,
    });
    out.push_str(&fim_transport_line(&log.head.stamp));
    out.push_str(&scores_line(
        "in_file",
        &group(&kept, TaskTier::InFile, None),
        None,
    ));
    let judge = judge_cell(&kept, log);
    out.push_str(&scores_line(
        "function_body",
        &group(&kept, TaskTier::FunctionBody, None),
        judge.as_deref(),
    ));
    out.push_str(&cross_lines(&rows, &kept));
    out.push_str(&exec_trailer(&kept, &log.head.stamp));
    out.push_str(&judge_trailer(&kept, log));
    out
}

/// What the header line reads (§4 — three parameters).
struct Header<'a> {
    kept: &'a [&'a TaskRow],
    excluded: usize,
    log: &'a RunLog,
}

fn codebase_header(header: &Header) -> String {
    let kept = header.kept;
    let counts = crate::core::bench::codebase::Counts {
        in_file: tier_tasks(kept, TaskTier::InFile),
        function_body: tier_tasks(kept, TaskTier::FunctionBody),
        cross_file_first: tier_tasks(kept, TaskTier::CrossFileFirst),
    };
    format!(
        "codebase     {} tasks, {} crossings, from {} files ({}) — {}; context: same-file, \
         plus the defining file for cross_file_first (engine window ≤ n_batch; extra from \
         ctx); tiers 1-4 score the first gold_lines lines of each fill{}{}{}{}\n",
        distinct_tasks(kept),
        kept.len(),
        distinct_files(kept),
        crate::core::bench::codebase::tier_counts_clause(counts),
        crate::core::bench::codebase::MASK_LABEL,
        elided_note(kept),
        exec_clause(&header.log.head.stamp),
        judge_clause(header.log),
        excluded_note(header.excluded),
    )
}

/// Which wire filled the codebase rows (spec §6): llama.cpp's native
/// `/infill` for every run chekov launches itself, or the chat-completions
/// fallback for a foreign runtime with no FIM endpoint.
fn fim_transport_line(stamp: &crate::core::bench::stamp::Stamp) -> String {
    let transport = if stamp.runtime == crate::core::bench::stamp::RUNTIME_LLAMA_CPP {
        "/infill"
    } else {
        "chat"
    };
    format!("fim transport: {transport}\n")
}

/// Whose clock produced the run's numbers, said out loud exactly when it was
/// not the server's own (spec §6).
///
/// A llama.cpp run reads its timings off the server and prints nothing here —
/// its render is what it always was. A run chekov timed itself carries the
/// caveat with the claim: the windows include wire overhead.
fn timing_source_line(stamp: &crate::core::bench::stamp::Stamp) -> String {
    if stamp.timing_source == crate::core::bench::stamp::TIMING_SERVER {
        return String::new();
    }
    format!(
        "timing source: {} (client wall-clock over SSE; includes wire overhead)\n",
        stamp.timing_source
    )
}

/// How the run decoded, said out loud exactly when it was not plain
/// autoregressive decoding (spec-stage design §6): the speculative type and
/// its draft length. A run without `--spec-type` prints nothing here.
fn speculative_line(stamp: &crate::core::bench::stamp::Stamp) -> String {
    if stamp.spec_type == crate::core::bench::stamp::FLAG_ENGINE_DEFAULT {
        return String::new();
    }
    format!(
        "speculative: {}, draft length {}\n",
        stamp.spec_type, stamp.spec_draft_n_max
    )
}

/// `; exec: cargo 1.95.0 (…), offline, scratch target` — what the exec tiers
/// ran under, once per run, and nothing at all when they did not run.
fn exec_clause(stamp: &crate::core::bench::stamp::Stamp) -> String {
    match (stamp.allow_exec, stamp.cargo_version.as_deref()) {
        (true, Some(version)) => format!("; exec: {version}, offline, scratch target"),
        _ => String::new(),
    }
}

/// The cross-file tier's two arm lines and the lift — or one line saying
/// why there are none, told apart by the run's OWN rows.
///
/// "None sampled" is a claim about the repository, and only the rows before
/// the unavailable ones were dropped can support it: an outage that took
/// every crossing away leaves the same empty groups behind, and printing
/// "no unambiguous cross-file first use in this repository" for it blames
/// the repository for the server.
fn cross_lines(rows: &[&TaskRow], kept: &[&TaskRow]) -> String {
    use crate::core::bench::codebase::run::{NO_EXTRA, WITH_EXTRA};
    let sampled: Vec<&TaskRow> = rows.iter().filter(|r| is_cross(r)).copied().collect();
    if sampled.is_empty() {
        return "             cross_file_first        none sampled — no unambiguous \
                cross-file first use in this repository\n"
            .to_owned();
    }
    let without = group(kept, TaskTier::CrossFileFirst, Some(NO_EXTRA));
    let with = group(kept, TaskTier::CrossFileFirst, Some(WITH_EXTRA));
    if without.is_empty() && with.is_empty() {
        return format!(
            "             {:<24}all {} crossings unavailable — {}\n",
            "cross_file_first",
            sampled.len(),
            unavailable_reason(&sampled)
        );
    }
    format!(
        "{}{}{}",
        scores_line("cross_file_first", &without, None),
        scores_line("cross_file_first+extra", &with, None),
        lift_line(kept)
    )
}

fn is_cross(row: &TaskRow) -> bool {
    row.codebase
        .as_ref()
        .is_some_and(|c| c.tier == TaskTier::CrossFileFirst)
}

use crate::core::bench::codebase::base_id;

/// Distinct tasks behind these rows: the header counts tasks, the crossings
/// count is `rows.len()`.
fn distinct_tasks(rows: &[&TaskRow]) -> usize {
    rows.iter()
        .map(|r| base_id(&r.task_id))
        .collect::<std::collections::BTreeSet<&str>>()
        .len()
}

fn tier_tasks(rows: &[&TaskRow], tier: TaskTier) -> usize {
    rows.iter()
        .filter(|r| r.codebase.as_ref().is_some_and(|c| c.tier == tier))
        .map(|r| base_id(&r.task_id))
        .collect::<std::collections::BTreeSet<&str>>()
        .len()
}

/// One tier's rows, optionally restricted to one arm.
fn group<'a>(rows: &[&'a TaskRow], tier: TaskTier, arm: Option<&str>) -> Vec<&'a CodebaseRow> {
    rows.iter()
        .filter_map(|r| r.codebase.as_ref())
        .filter(|c| c.tier == tier && arm.is_none_or(|a| c.arm.as_deref() == Some(a)))
        .collect()
}

/// The whole-suite N/A line, which only a run where NOTHING was answered
/// earns.
///
/// A missing FIM capability is named as such only when every row was recorded
/// as one at the crossing. Nothing here reads the reason: a refusal's own
/// words carry the URL it was refused at, and that URL ends in `/infill`, so
/// a dead server would otherwise be reported as a model that cannot infill.
fn codebase_na_line(rows: &[&TaskRow]) -> String {
    let reason = unavailable_reason(rows);
    let unsupported = rows
        .iter()
        .all(|r| r.codebase.as_ref().is_some_and(|c| c.unsupported));
    if unsupported {
        format!("codebase     N/A — infill unsupported by this model ({reason})\n")
    } else {
        format!("codebase     N/A — {reason}\n")
    }
}

/// One line of tier means for a group, at the 24-wide label column every
/// line of the block shares.
fn scores_line(label: &str, group: &[&CodebaseRow], judge: Option<&str>) -> String {
    if group.is_empty() {
        return String::new();
    }
    let mut cells = Vec::new();
    for t in [Tier::Exact, Tier::EditSim, Tier::IdentF1, Tier::Parse] {
        if let Some(mean) = tier_mean(group, t) {
            cells.push(format!("{} {mean:.2}", t.label()));
        }
    }
    cells.push(symbols_cell(group));
    cells.extend(exec_cells(group));
    if let Some(j) = judge {
        cells.push(j.to_owned());
    }
    format!(
        "             {label:<24}{}   (n={})\n",
        cells.join("   "),
        group.len()
    )
}

/// The per-tier difference of arm means over the tasks measured in BOTH arms.
///
/// A task unavailable in either arm never reaches `kept`, so it is excluded
/// here by construction — a difference against a missing arm is not a
/// measurement, and would read as a lift of exactly the arm that answered.
fn lift_line(kept: &[&TaskRow]) -> String {
    let pairs = arm_pairs(kept);
    if pairs.is_empty() {
        return String::new();
    }
    let mut cells = Vec::new();
    for t in [Tier::Exact, Tier::EditSim, Tier::IdentF1, Tier::Parse] {
        if let Some(delta) = tier_delta(&pairs, t) {
            cells.push(format!("{} {delta:+.2}", t.label()));
        }
    }
    if let Some(delta) = symbols_delta(&pairs) {
        cells.push(format!("symbols {delta:+.2}"));
    }
    cells.extend(exec_delta_cells(&pairs));
    format!(
        "             {:<24}{}   ({})\n",
        "context lift",
        cells.join("  "),
        lift_note(kept, &pairs)
    )
}

/// `(no_extra, extra)` for every cross-file task measured in both arms.
fn arm_pairs<'a>(kept: &[&'a TaskRow]) -> Vec<(&'a CodebaseRow, &'a CodebaseRow)> {
    use crate::core::bench::codebase::run::{NO_EXTRA, WITH_EXTRA};
    let with = arm_map(kept, WITH_EXTRA);
    arm_map(kept, NO_EXTRA)
        .into_iter()
        .filter_map(|(id, without)| with.get(id).map(|w| (without, *w)))
        .collect()
}

/// The cross-file rows of one arm, keyed by the task they belong to.
fn arm_map<'a>(
    kept: &[&'a TaskRow],
    arm: &str,
) -> std::collections::BTreeMap<&'a str, &'a CodebaseRow> {
    kept.iter()
        .filter_map(|r| Some((base_id(&r.task_id), r.codebase.as_ref()?)))
        .filter(|(_, c)| c.tier == TaskTier::CrossFileFirst && c.arm.as_deref() == Some(arm))
        .collect()
}

/// The mean of `with − without` for the tiers recomputed from stored text.
fn tier_delta(pairs: &[(&CodebaseRow, &CodebaseRow)], tier: Tier) -> Option<f64> {
    let deltas: Vec<f64> = pairs
        .iter()
        .filter_map(|(a, b)| match (recompute(a, tier), recompute(b, tier)) {
            (Score::Value(x), Score::Value(y)) => Some(y - x),
            _ => None,
        })
        .collect();
    if deltas.is_empty() {
        return None;
    }
    Some(deltas.iter().sum::<f64>() / as_f64(deltas.len()))
}

/// Tier 5's lift, from the scores stored at run time on both arms.
fn symbols_delta(pairs: &[(&CodebaseRow, &CodebaseRow)]) -> Option<f64> {
    let deltas: Vec<f64> = pairs
        .iter()
        .filter_map(|(a, b)| Some(b.symbols_score? - a.symbols_score?))
        .collect();
    if deltas.is_empty() {
        return None;
    }
    Some(deltas.iter().sum::<f64>() / as_f64(deltas.len()))
}

/// `6 files sent, 41.2 KiB, 1 truncated; 2 withheld`, prefixed `n=k of N; `
/// when a task was measured on one arm only — the lift never runs quietly
/// over fewer tasks than the tier has.
///
/// `files sent` counts ROWS, not distinct paths: six tasks that all cross
/// for a symbol declared in the same G are six files sent, and `KiB` is the
/// six-way sum. It is what the run put on the wire, which is the number the
/// lift was bought with; a distinct-path count would understate the cost.
fn lift_note(kept: &[&TaskRow], pairs: &[(&CodebaseRow, &CodebaseRow)]) -> String {
    let total = tier_tasks(kept, TaskTier::CrossFileFirst);
    let sent: Vec<&crate::core::bench::codebase::ExtraFile> =
        pairs.iter().filter_map(|(_, b)| b.extra.as_ref()).collect();
    let bytes: usize = sent
        .iter()
        .map(|e| usize::try_from(e.bytes).unwrap_or(0))
        .sum();
    let truncated = sent.iter().filter(|e| e.truncated).count();
    let withheld: u32 = pairs
        .iter()
        .map(|(_, b)| b.excluded.cross_file_withheld)
        .sum();
    let scope = if pairs.len() == total {
        String::new()
    } else {
        format!("n={} of {total}; ", pairs.len())
    };
    format!(
        "{scope}{} files sent, {:.1} KiB, {truncated} truncated; {withheld} withheld",
        sent.len(),
        as_f64(bytes) / 1024.0,
    )
}

/// Tier 5's cell: the mean of the rows that carry a score, or `n/a` when none
/// of them does — an unscored group is not a zero-scoring one.
fn symbols_cell(group: &[&CodebaseRow]) -> String {
    let scored: Vec<f64> = group.iter().filter_map(|c| c.symbols_score).collect();
    if scored.is_empty() {
        return "symbols n/a (scored at run time)".to_owned();
    }
    let mean = scored.iter().sum::<f64>() / as_f64(scored.len());
    format!("symbols {mean:.2} (scored at run time)")
}

/// `compile 0.83 (n=12)` and `test 1.00 (n=3 of 12 had a covering test)`, or
/// nothing at all when this run never ran the exec tiers.
///
/// `compile`'s `n` counts crossings with a VERDICT: a skip is excluded from
/// the mean and counted in the trailer by reason, because averaging a skip in
/// as a zero would score the model down for a question the machine could not
/// ask. `test`'s parenthetical always says how many crossings had a covering
/// test at all — the number that makes the mean readable.
fn exec_cells(group: &[&CodebaseRow]) -> Vec<String> {
    let execs: Vec<&ExecRow> = group.iter().filter_map(|c| c.exec.as_ref()).collect();
    if execs.is_empty() {
        return Vec::new();
    }
    let covered = execs.iter().filter(|e| !e.tests.is_empty()).count();
    let total = execs.len();
    let compile = match exec_mean(execs.iter().map(|e| &e.compile)) {
        Some((mean, n)) => format!("compile {mean:.2} (n={n})"),
        None => "compile n/a".to_owned(),
    };
    let test = match exec_mean(execs.iter().map(|e| &e.test)) {
        Some((mean, n)) => format!("test {mean:.2} (n={n} of {total} had a covering test)"),
        None => format!("test n/a ({covered} of {total} had a covering test)"),
    };
    vec![compile, test]
}

/// The mean of the scored values and how many there were — `None` when every
/// one of them was skipped.
fn exec_mean<'a>(scores: impl Iterator<Item = &'a ExecScore>) -> Option<(f64, usize)> {
    let values: Vec<f64> = scores
        .filter_map(|s| match s {
            ExecScore::Value(v) => Some(*v),
            ExecScore::Skipped(_) => None,
        })
        .collect();
    if values.is_empty() {
        return None;
    }
    Some((
        values.iter().sum::<f64>() / as_f64(values.len()),
        values.len(),
    ))
}

/// The exec tiers' lift: `compile +0.33` and `test n/a`.
///
/// A pair contributes only when BOTH arms produced a verdict for that tier —
/// a difference against a skip is not a measurement, and would read as a lift
/// of exactly the arm that ran. Nothing at all when neither arm ever ran the
/// exec tiers.
fn exec_delta_cells(pairs: &[(&CodebaseRow, &CodebaseRow)]) -> Vec<String> {
    if !pairs
        .iter()
        .any(|(a, b)| a.exec.is_some() || b.exec.is_some())
    {
        return Vec::new();
    }
    [
        ("compile", exec_delta(pairs, |e| &e.compile)),
        ("test", exec_delta(pairs, |e| &e.test)),
    ]
    .into_iter()
    .map(|(label, delta)| {
        delta.map_or_else(
            || format!("{label} n/a"),
            |value| format!("{label} {value:+.2}"),
        )
    })
    .collect()
}

fn exec_delta(
    pairs: &[(&CodebaseRow, &CodebaseRow)],
    pick: fn(&ExecRow) -> &ExecScore,
) -> Option<f64> {
    let deltas: Vec<f64> = pairs
        .iter()
        .filter_map(
            |(a, b)| match (pick(a.exec.as_ref()?), pick(b.exec.as_ref()?)) {
                (ExecScore::Value(x), ExecScore::Value(y)) => Some(y - x),
                _ => None,
            },
        )
        .collect();
    if deltas.is_empty() {
        return None;
    }
    Some(deltas.iter().sum::<f64>() / as_f64(deltas.len()))
}

/// The block's last line: what the exec tiers cost, or why there are none.
///
/// Three shapes, and the rows decide which: no exec half anywhere is a run
/// that was never given the flag; every crossing skipped for one reason is
/// that reason, said once; anything else is the timing plus the skips
/// counted by reason.
fn exec_trailer(rows: &[&TaskRow], stamp: &crate::core::bench::stamp::Stamp) -> String {
    let execs: Vec<&ExecRow> = rows
        .iter()
        .filter_map(|r| r.codebase.as_ref()?.exec.as_ref())
        .collect();
    if execs.is_empty() || !stamp.allow_exec {
        return "             tiers 6-7 skipped: --allow-exec not given\n".to_owned();
    }
    let checks: Vec<f64> = execs
        .iter()
        .filter(|e| matches!(e.compile, ExecScore::Value(_)))
        .map(|e| e.check_secs)
        .collect();
    let skips = skip_tally(&execs);
    if checks.is_empty() {
        return format!(
            "             tiers 6-7 skipped: {}\n",
            one_reason(&skips).unwrap_or_else(|| "no crossing produced a verdict".to_owned())
        );
    }
    format!(
        "             tiers 6-7: cold check {:.0} s, then {:.0} s median per crossing{}\n",
        checks[0],
        median(&checks[1..]).unwrap_or(checks[0]),
        skip_note(&skips),
    )
}

/// `equiv 0.60 (n=5 judged of 6; 1 undecided)`, or the void, or `None`
/// when the run has no judge rows at all.
fn judge_cell(kept: &[&TaskRow], log: &RunLog) -> Option<String> {
    let stamp = log.head.stamp.judge.as_ref()?;
    let rows = verdicts(log);
    if rows.is_empty() {
        return None;
    }
    let total = tier_tasks(kept, TaskTier::FunctionBody);
    if let Some(rate) = crate::core::bench::judge::consistency_pct(&rows)
        && rate < stamp.min_consistency_pct
    {
        return Some(format!(
            "equiv voided (swap consistency {rate}% < {}%)",
            stamp.min_consistency_pct
        ));
    }
    let judged: Vec<bool> = rows.iter().filter_map(|r| r.equivalent).collect();
    let undecided = rows
        .iter()
        .filter(|r| r.decided_by == DecidedBy::SwapDisagreement)
        .count();
    if judged.is_empty() {
        return Some(format!(
            "equiv n/a (0 judged of {total}; {undecided} undecided)"
        ));
    }
    let agreed = judged.iter().filter(|&&e| e).count();
    let mean = as_f64(agreed) / as_f64(judged.len());
    Some(format!(
        "equiv {mean:.2} (n={} judged of {total}; {undecided} undecided)",
        judged.len()
    ))
}

/// `; judge: <model> (<quant>@<rev12>, <arch>, effort <e>) rubric <hash>, swap consistency N% (k of n)`.
fn judge_clause(log: &RunLog) -> String {
    let Some(j) = log.head.stamp.judge.as_ref() else {
        return String::new();
    };
    let rows = verdicts(log);
    let answered = rows
        .iter()
        .filter(|r| r.gold_first.is_some() && r.prediction_first.is_some())
        .count();
    let agreed = rows
        .iter()
        .filter(|r| r.gold_first.is_some() && r.gold_first == r.prediction_first)
        .count();
    let consistency = crate::core::bench::judge::consistency_pct(&rows).map_or_else(
        || "n/a".to_owned(),
        |rate| format!("{rate}% ({agreed} of {answered})"),
    );
    format!(
        "; judge: {} ({}@{}, {}, effort {}) rubric {}, swap consistency {consistency}",
        j.model, j.quant, j.revision, j.arch, j.reasoning_effort, j.rubric_hash
    )
}

/// The judge's own trailer line: called under `--judge`, resuming, or
/// skipped outright — and a second line naming any off-schema replies.
fn judge_trailer(kept: &[&TaskRow], log: &RunLog) -> String {
    let Some(stamp) = log.head.stamp.judge.as_ref() else {
        return "             judge skipped: --judge not given\n".to_owned();
    };
    let rows = verdicts(log);
    let total = tier_tasks(kept, TaskTier::FunctionBody);
    if rows.is_empty() {
        return unjudged_trailer(total, &stamp.model);
    }
    let count = |d: DecidedBy| rows.iter().filter(|r| r.decided_by == d).count();
    let called = rows.iter().filter(|r| was_called(r)).count();
    let secs: Vec<f64> = rows
        .iter()
        .filter(|r| was_called(r))
        .map(|r| r.judge_secs)
        .collect();
    let mut out = format!(
        "             judge: {} identical, {called} called, {} undecided, {} skipped; {:.1} s \
         median per verdict\n",
        count(DecidedBy::Identical),
        count(DecidedBy::SwapDisagreement),
        count(DecidedBy::Skipped),
        median(&secs).unwrap_or(0.0)
    );
    if let Some(warning) = schema_warning(&rows) {
        out.push_str(&warning);
    }
    out
}

/// Whether the judge was actually asked about this crossing.
///
/// Not "did it answer": an off-schema or truncated reply is a request that was
/// sent and paid for, and it belongs in both the count and the median. The two
/// pre-call settlements — an identical pair, and a `did not compile` skip —
/// never reach the wire, and record `0.0` seconds because nothing was spent.
fn was_called(row: &JudgeRow) -> bool {
    row.decided_by != DecidedBy::Identical && row.judge_secs > 0.0
}

/// A run under a judge that holds no verdict.
///
/// With no `function_body` crossing there was never anything to ask about and
/// the phase was skipped (spec C §7); otherwise the crossings are still owed
/// and the run is resumable.
fn unjudged_trailer(total: usize, model: &str) -> String {
    if total == 0 {
        return "             judge: nothing eligible\n".to_owned();
    }
    format!("             judge: 0 of {total} crossings judged — resume with --judge {model}\n")
}

/// `             warning: N reply was not the schema — …`, or `None` when
/// every reply the judge returned parsed as the schema.
fn schema_warning(rows: &[&JudgeRow]) -> Option<String> {
    let not_schema = rows
        .iter()
        .filter(|r| {
            r.skipped
                .as_deref()
                .is_some_and(|s| s.starts_with("reply was not the schema"))
        })
        .count();
    (not_schema > 0).then(|| {
        format!(
            "             warning: {not_schema} reply was not the schema — the grammar was \
             not enforced for this judge and the column is measuring the prompt alone\n"
        )
    })
}

/// Every skip reason with its count, most frequent first and ties by reason —
/// a stable order, so the line is testable.
fn skip_tally(execs: &[&ExecRow]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for exec in execs {
        if let ExecScore::Skipped(reason) = &exec.compile {
            *counts.entry(reason.as_str()).or_default() += 1;
        }
    }
    let mut tally: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(reason, n)| (reason.to_owned(), n))
        .collect();
    tally.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    tally
}

/// The one reason every crossing was skipped for, when there is only one.
fn one_reason(skips: &[(String, usize)]) -> Option<String> {
    match skips {
        [(reason, _)] => Some(reason.clone()),
        _ => None,
    }
}

/// `; 3 skipped (2 check timed out after 120 s, 1 needs network)`, or nothing
/// when nothing was skipped.
fn skip_note(skips: &[(String, usize)]) -> String {
    let total: usize = skips.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return String::new();
    }
    let parts: Vec<String> = skips
        .iter()
        .map(|(reason, n)| format!("{n} {reason}"))
        .collect();
    format!("; {total} skipped ({})", parts.join(", "))
}

/// The middle value of a sorted copy — the upper of the two on an even count.
fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[sorted.len() / 2])
}

/// The mean of the values that tier recomputes for this group — `None` when
/// every task in the group skips that tier.
fn tier_mean(group: &[&CodebaseRow], tier: Tier) -> Option<f64> {
    let values: Vec<f64> = group
        .iter()
        .filter_map(|c| match recompute(c, tier) {
            Score::Value(v) => Some(v),
            Score::Skipped(_) => None,
        })
        .collect();
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / as_f64(values.len()))
}

/// Tiers 1–4 from the stored text — a stored score can never drift. The
/// ladder's own function does the scoring, so the run and the re-read cannot
/// disagree about which tier a task skipped, or say it differently.
pub(crate) fn recompute(c: &CodebaseRow, tier: Tier) -> Score {
    ladder::stored_tier(
        tier,
        &ladder::StoredText {
            tier: c.tier,
            gold: &c.gold,
            prediction: &c.prediction,
            prefix: &c.prefix,
            suffix: &c.suffix,
        },
    )
}

fn tool_emit_line(log: &RunLog, transport: Transport) -> Option<String> {
    let rows: Vec<&TaskRow> = rows_via(log, "tool_emit", transport).collect();
    if rows.is_empty() {
        return None;
    }
    let label = door_label(transport);
    let tally = Tally::of(&rows);
    if tally.total == 0 {
        return Some(format!(
            "tool_emit    {label}N/A — nothing was measured ({})\n",
            unavailable_reason(&rows)
        ));
    }
    Some(format!(
        "tool_emit    {label}{}{}\n",
        tally.cell(),
        excluded_note(tally.excluded)
    ))
}

/// The §7.2 anti-self-deception line: forced vs unconstrained ON THE SAME
/// CASES — a large gap means "works only with a babysitter".
///
/// When the forced pass could not run at all, the axis is N/A with the
/// engine's own reason. Reporting 0/N there would publish a gap the model
/// never earned.
fn grammar_gap_line(log: &RunLog) -> Option<String> {
    let forced: Vec<&TaskRow> = rows_of(log, "grammar_gap").collect();
    if forced.is_empty() {
        return None;
    }
    let arm = Tally::of(&forced);
    if arm.total == 0 {
        return Some(format!(
            "grammar_gap  N/A — the forced pass could not run ({}); \
             no gap is reported because none was measured\n",
            unavailable_reason(&forced)
        ));
    }
    let tool_emit: Vec<&TaskRow> = rows_of(log, "tool_emit").collect();
    let paired = Tally::of(&unconstrained_arm(&forced, &tool_emit));
    if paired.total == 0 {
        return Some(format!(
            "grammar_gap  {} forced — no unconstrained result to compare against{}\n",
            arm.cell(),
            excluded_note(arm.excluded)
        ));
    }
    let gap = gap_pct(arm, paired);
    Some(format!(
        "grammar_gap  {} forced — unconstrained on the same cases {} (gap {gap:+}%){}{}\n",
        arm.cell(),
        paired.cell(),
        excluded_note(arm.excluded),
        forced_mode_note(log),
    ))
}

/// Forced minus unconstrained, in whole percentage points.
fn gap_pct(forced: Tally, unconstrained: Tally) -> i64 {
    let pct = |t: Tally| i64::try_from(t.passed * 100 / t.total.max(1)).unwrap_or(0);
    pct(forced) - pct(unconstrained)
}

/// `; forced pass ran with reasoning extracted (<mode>)` when the forced arm
/// asked the engine to extract reasoning — the one extra difference from the
/// unconstrained arm, printed rather than hidden.
fn forced_mode_note(log: &RunLog) -> String {
    log.head
        .forced_reasoning_format
        .as_deref()
        .map_or_else(String::new, |mode| {
            format!("; forced pass ran with reasoning extracted ({mode})")
        })
}

fn instruction_line(log: &RunLog, transport: Transport) -> Option<String> {
    let rows: Vec<&TaskRow> = rows_via(log, "instruction", transport).collect();
    if rows.is_empty() {
        return None;
    }
    let label = door_label(transport);
    let strict = Tally::of(&rows);
    if strict.total == 0 {
        return Some(format!(
            "instruction  {label}N/A — nothing was measured ({})\n",
            unavailable_reason(&rows)
        ));
    }
    let loose = Tally::loose(&rows);
    Some(format!(
        "instruction  {label}strict {}, loose {} (chattiness gap {}){}\n",
        strict.cell(),
        loose.cell(),
        loose.passed.saturating_sub(strict.passed),
        excluded_note(strict.excluded)
    ))
}

fn depth_line(row: &TaskRow) -> String {
    let decode = stats::summarize(&row.measure.decode_samples);
    let prefill = stats::summarize(&row.measure.prefill_samples);
    let depth = row.task_id.strip_prefix("depth-").unwrap_or(&row.task_id);
    // A hot prefix cache must be visible next to the prompt_n it shrank.
    let cached = if row.measure.cache_n > 0 {
        format!("  cache_n {}", row.measure.cache_n)
    } else {
        String::new()
    };
    match (decode, prefill) {
        (Some(d), Some(p)) => format!(
            "{:>5}  {:>8}  {:.1} [{:.1}..{:.1}]  {:.1}  {} ({} warmup dropped){cached}\n",
            depth, row.measure.prompt_n, d.median, d.p10, d.p90, p.median, d.n, d.warmup_dropped
        ),
        _ => format!(
            "{:>5}  {:>8}  too few samples to summarise{cached}\n",
            depth, row.measure.prompt_n
        ),
    }
}

fn probe_line(row: &TaskRow) -> String {
    let Some(grade) = &row.grade else {
        return format!("fixture ??   {}  (no grade recorded)\n", row.task_id);
    };
    let verdict = if grade.pass { "PASS" } else { "FAIL" };
    let reason = grade.reason.as_deref().unwrap_or("");
    format!("fixture {verdict} {}  {reason}\n", row.task_id)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        CodebaseRow, DecidedBy, GradeRow, JudgeRow, Measure, RunHead, RunLog, RunWriter, Task,
        TaskKey, TaskRow, Transport, render_codebase, render_run,
    };
    use crate::core::bench::codebase::{Excluded, ExtraFile, TaskTier};
    use crate::core::bench::stamp::{JudgeStamp, Stamp};
    use crate::error::ChekovError;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-bench-jsonl")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn stamp() -> Stamp {
        Stamp {
            machine_id: "8d41f0c2a917".into(),
            runtime: crate::core::bench::stamp::RUNTIME_LLAMA_CPP.to_owned(),
            timing_source: crate::core::bench::stamp::TIMING_SERVER.to_owned(),
            engine_build_commit: "dda1b0d67".into(),
            weights_revision: "fbbaed45c2f0/model-00001.gguf".into(),
            quant: "Q8_0".into(),
            ctx: 262_144,
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

    fn head() -> RunHead {
        RunHead {
            model: "ornith-1.5-35b-a3b".into(),
            machine_brand: Some("Apple M3 Ultra".into()),
            launch_args: vec!["-m".into(), "model.gguf".into()],
            forced_reasoning_format: None,
            stamp: stamp(),
        }
    }

    fn measure(decode: &[f64]) -> Measure {
        Measure {
            prompt_n: 1055,
            decode_samples: decode.to_vec(),
            prefill_samples: decode.to_vec(),
            warmup_dropped: 1,
            cache_n: 0,
        }
    }

    #[test]
    fn a_row_written_before_cache_n_loads_as_zero_cached() {
        let row = r#"{"schema":1,"run_id":"r","seq":0,"suite":"throughput","task_id":"depth-1024","measure":{"prompt_n":10,"decode_samples":[1.0,2.0],"prefill_samples":[1.0,2.0],"warmup_dropped":1}}"#;
        let parsed: super::TaskRow = serde_json::from_str(row).expect("old row loads");
        assert_eq!(parsed.measure.cache_n, 0);
    }

    /// Unconstrained 1/2 on the call cases, forced 2/2 (gap +50%);
    /// instruction strict 1/2, loose 2/2 (chattiness gap 1).
    fn graded_run(eval: &std::path::Path) -> RunWriter {
        let mut writer = RunWriter::create(eval, "r7-model", &head()).expect("create");
        let rows: [(&str, &str, bool, Option<&str>); 6] = [
            ("tool_emit", "te-001", true, None),
            (
                "tool_emit",
                "te-002",
                false,
                Some("called 'read_file', expected 'grep'"),
            ),
            ("grammar_gap", "gg-te-001", true, None),
            ("grammar_gap", "gg-te-002", true, None),
            ("instruction", "if-001", true, Some("loose:pass")),
            (
                "instruction",
                "if-002",
                false,
                Some("failed 'fenced_rust_only'; loose:pass"),
            ),
        ];
        for (suite, id, pass, reason) in rows {
            writer
                .append(Task {
                    suite: suite.into(),
                    task_id: id.into(),
                    measure: measure(&[20.0, 20.0]),
                    grade: Some(GradeRow {
                        pass,
                        reason: reason.map(str::to_owned),
                        unavailable: false,
                    }),
                    transport: Transport::Buffered,
                    codebase: None,
                    judge: None,
                })
                .expect("append");
        }
        writer
    }

    #[test]
    fn an_unmeasurable_forced_pass_reports_na_not_a_zero_score() {
        // The engine refusing to constrain the model is not the model failing.
        // Reporting 0/2 here would publish a gap nobody measured.
        let eval = scratch("forced-na");
        let mut writer = RunWriter::create(&eval, "r8-model", &head()).expect("create");
        let refused = || GradeRow::unavailable("http status: 400".to_owned());
        let rows = [
            ("tool_emit", "te-001", GradeRow::pass()),
            ("tool_emit", "te-002", GradeRow::pass()),
            ("grammar_gap", "gg-te-001", refused()),
            ("grammar_gap", "gg-te-002", refused()),
        ];
        for (suite, id, grade) in rows {
            writer.append(graded(suite, id, grade)).expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("grammar_gap  N/A"),
            "an unmeasurable axis is N/A: {rendered}"
        );
        assert!(
            rendered.contains("http status: 400"),
            "with its reason: {rendered}"
        );
        assert!(
            !rendered.contains("(gap"),
            "no gap may be published: {rendered}"
        );
        assert!(
            !rendered.contains("forced —"),
            "no forced score may be published: {rendered}"
        );
        assert!(
            !rendered.contains("grammar_gap FAIL"),
            "unavailable is never listed as a failure: {rendered}"
        );
    }

    #[test]
    fn a_partly_unavailable_axis_scores_only_what_was_measured() {
        // Reachable through --resume, which mixes previously-recorded
        // unavailable rows with freshly graded ones. Leaving the unavailable
        // ones in the denominator scores the model down for questions it was
        // never asked — the same fabrication as the all-unavailable case.
        let eval = scratch("mixed-availability");
        let mut writer = RunWriter::create(&eval, "r9-model", &head()).expect("create");
        let rows = [
            ("tool_emit", "te-001", GradeRow::pass()),
            ("tool_emit", "te-002", GradeRow::pass()),
            ("grammar_gap", "gg-te-001", GradeRow::pass()),
            (
                "grammar_gap",
                "gg-te-002",
                GradeRow::unavailable("http status: 400".to_owned()),
            ),
        ];
        for (suite, id, grade) in rows {
            writer.append(graded(suite, id, grade)).expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("grammar_gap  1/1 forced"),
            "only the measured case counts: {rendered}"
        );
        assert!(
            rendered.contains("(1 unavailable, excluded)"),
            "the exclusion is always printed: {rendered}"
        );
        assert!(
            rendered.contains("unconstrained on the same cases 1/1"),
            "the comparison set is the forced-measured cases only: {rendered}"
        );
        assert!(
            rendered.contains("(gap +0%)"),
            "1/1 vs 1/1 is no gap, not -50%: {rendered}"
        );
    }

    #[test]
    fn an_agentic_only_run_does_not_claim_a_failed_curve_fit() {
        // The throughput suite never ran; "insufficient depths" would report a
        // failure to fit a curve nobody asked for.
        let eval = scratch("agentic-only");
        let writer = graded_run(&eval);
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            !rendered.contains("insufficient depths"),
            "no throughput rows means no curve claim: {rendered}"
        );
        assert!(!rendered.contains("decode tok/s"), "{rendered}");
        assert!(rendered.contains("tool_emit    1/2"), "{rendered}");
    }

    #[test]
    fn suite_summaries_carry_both_gaps_and_list_failures() {
        let eval = scratch("summaries");
        let writer = graded_run(&eval);
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(rendered.contains("tool_emit    1/2"), "{rendered}");
        assert!(
            rendered.contains(
                "grammar_gap  2/2 forced — unconstrained on the same cases 1/2 (gap +50%)"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("instruction  strict 1/2, loose 2/2 (chattiness gap 1)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("tool_emit FAIL te-002  called 'read_file'"),
            "failures are listed individually: {rendered}"
        );
    }

    fn graded(suite: &str, id: &str, grade: GradeRow) -> Task {
        Task {
            suite: suite.into(),
            task_id: id.into(),
            measure: measure(&[20.0, 20.0]),
            grade: Some(grade),
            transport: Transport::Buffered,
            codebase: None,
            judge: None,
        }
    }

    fn streamed(suite: &str, id: &str, grade: GradeRow) -> Task {
        Task {
            transport: Transport::Streamed,
            ..graded(suite, id, grade)
        }
    }

    /// `graded_run` plus the streamed crossings of the same cases: te-001
    /// agrees (PASS), te-002 flips to PASS, if-001 flips to FAIL, if-002
    /// fails both ways.
    fn with_streamed(name: &str) -> String {
        let eval = scratch(name);
        let mut writer = graded_run(&eval);
        for task in [
            streamed("tool_emit", "te-001", GradeRow::pass()),
            streamed("tool_emit", "te-002", GradeRow::pass()),
            streamed(
                "instruction",
                "if-001",
                GradeRow::fail("failed 'max_lines'; loose:pass".to_owned()),
            ),
            streamed(
                "instruction",
                "if-002",
                GradeRow::fail("failed 'fenced_rust_only'; loose:pass".to_owned()),
            ),
        ] {
            writer.append(task).expect("append");
        }
        render_run(&RunLog::load(writer.dir()).expect("load"))
    }

    #[test]
    fn a_row_written_before_transport_loads_as_buffered() {
        let line = r#"{"schema":1,"run_id":"r","seq":0,"suite":"tool_emit","task_id":"te-001",
            "measure":{"prompt_n":4,"decode_samples":[1.0],"prefill_samples":[1.0],"warmup_dropped":0}}"#;
        let row: TaskRow = serde_json::from_str(line).expect("an old row still loads");
        assert_eq!(row.transport, Transport::Buffered);
    }

    /// A real `codebase` row from before slice C — no `judge` key at all.
    const PRE_C_ROW: &str = r#"{"schema":1,"run_id":"20260830T072140Z-qwen3.8-flash-next","seq":64,"suite":"codebase","task_id":"in_file-1142dc-L35","transport":"buffered","measure":{"prompt_n":659,"decode_samples":[31.383755425823352],"prefill_samples":[280.5818219595511],"warmup_dropped":0,"cache_n":0},"codebase":{"tier":"in_file","file":"crates/pushkin-cli/src/verbs/list.rs","line":35,"label":"boundary-scanned (not AST)","gold":"    let entries = state::registry();","prediction":"    let entries = match state::entries() {\n        Ok(entries) => entries,\n        Err(error) => {\n            eprintln!(\"pushkin: cannot read the registry: {error}\");\n            return 1;\n        }\n    };\n","prefix":"//! `pushkin list [--prune]`: every daemon the per-user registry knows, from\n//! any directory (ADR-0010 step 1b). One line per repository: running with\n//! its pid, or stale (registered, nothing answering). `--prune` removes the\n//! stale entries' dirs; a dir with no root record is reported as unknown and\n//! never pruned — a thing we cannot name is not ours to delete.\n\nuse super::state;\nuse pushkin_daemon::protocol::{Request, Response, PROTOCOL_VERSION};\nuse pushkin_daemon::server;\n\npub struct ListArgs {\n    pub prune: bool,\n}\n\nenum Health {\n    Running(u32),\n    Stale,\n}\n\nfn health(entry: &state::Entry) -> Health {\n    match server::request_at(\n        &entry.socket(),\n        &Request::Ping {\n            v: PROTOCOL_VERSION,\n        },\n    ) {\n        Ok(Response::Pong { info }) => Health::Running(info.pid),\n        _ => Health::Stale,\n    }\n}\n\n/// Exit 0 always: an empty or stale registry is a state, not a failure.\n#[must_use]\npub fn run(args: &ListArgs) -> i32 {\n","suffix":"\n    if entries.is_empty() {\n        println!(\"pushkin: no daemons registered\");\n        return 0;\n    }\n    let mut running = 0;\n    let mut stale = 0;\n    for entry in entries {\n        match report(&entry, args.prune) {\n            Some(Health::Running(_)) => running += 1,\n            Some(Health::Stale) => stale += 1,\n            None => {}\n        }\n    }\n    // The live count is the named trigger for ADR-0012's option A: a shared\n    // process is worth revisiting only when this number gets large.\n    println!(\"{running} running, {stale} stale\");\n    0\n}\n\nfn report(entry: &state::Entry, prune: bool) -> Option<Health> {\n    let Some(root) = entry.root.as_deref() else {\n        println!(\n            \"{}  unknown (no root record; not pruned)\",\n            entry.dir.display()\n        );\n        return None;\n    };\n    let health = health(entry);\n    match health {\n        Health::Running(pid) => println!(\"{}  running (pid {pid})\", root.display()),\n        Health::Stale if prune => match std::fs::remove_dir_all(&entry.dir) {\n            Ok(()) => println!(\"{}  stale — pruned\", root.display()),\n            Err(error) => println!(\"{}  stale — prune failed: {error}\", root.display()),\n        },\n        Health::Stale => println!(\"{}  stale (no daemon answering)\", root.display()),\n    }\n    Some(health)\n}\n","excluded":{"doc_comment":0,"cross_file":"n/a: same-file","cfg_test_lines":0,"cross_file_withheld":0},"symbols_score":1.0,"unsupported":false,"n_predict":64}}"#;

    fn judge_task(id: &str, verdict: JudgeRow) -> Task {
        Task {
            suite: super::JUDGE_SUITE.into(),
            task_id: id.into(),
            measure: crate::core::bench::codebase::run::empty_measure(),
            grade: None,
            transport: Transport::Buffered,
            codebase: None,
            judge: Some(verdict),
        }
    }

    #[test]
    fn a_judge_row_round_trips_and_an_old_row_loads_without_one() {
        let eval = scratch("judge-row");
        let mut writer = RunWriter::create(&eval, "r-judge", &head()).expect("create");
        writer
            .append(judge_task(
                "function_body-abc123-L10",
                JudgeRow {
                    equivalent: None,
                    gold_first: Some(true),
                    prediction_first: Some(false),
                    decided_by: DecidedBy::SwapDisagreement,
                    skipped: None,
                    judge_secs: 2.5,
                },
            ))
            .expect("append");
        let log = RunLog::load(&eval.join("r-judge")).expect("load");
        let row = &log.rows[0];
        assert_eq!(row.suite, "judge");
        let judge = row.judge.as_ref().expect("judge row");
        assert_eq!(judge.decided_by, DecidedBy::SwapDisagreement);
        assert_eq!(
            (judge.gold_first, judge.prediction_first),
            (Some(true), Some(false))
        );
        let text = std::fs::read_to_string(eval.join("r-judge/results.jsonl")).expect("read");
        assert!(
            text.contains("\"decided_by\":\"swap_disagreement\""),
            "{text}"
        );
        assert_eq!(super::judge_rows(&log).len(), 1, "judge_rows finds the row");
        assert!(log.is_done(&TaskKey::buffered("judge", "function_body-abc123-L10")));
        assert!(
            serde_json::from_str::<TaskRow>(PRE_C_ROW)
                .expect("a row without the field parses")
                .judge
                .is_none()
        );
    }

    #[test]
    fn is_done_is_transport_aware() {
        let eval = scratch("done-transport");
        let writer = graded_run(&eval);
        let log = RunLog::load(writer.dir()).expect("load");
        assert!(log.is_done(&super::TaskKey::buffered("tool_emit", "te-001")));
        assert!(
            !log.is_done(&super::TaskKey::streamed("tool_emit", "te-001")),
            "the streamed crossing of the same case is still owed"
        );
    }

    #[test]
    fn streamed_rows_render_beside_buffered_and_disagreements_are_named() {
        let rendered = with_streamed("asymmetry");
        // The buffered lines keep their shape; streamed ones sit beside them.
        assert!(rendered.contains("tool_emit    1/2\n"), "{rendered}");
        assert!(
            rendered.contains("tool_emit    streamed 2/2\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("instruction  streamed strict 0/2, loose 2/2 (chattiness gap 2)"),
            "{rendered}"
        );
        // The finding: the same case answered differently through the two doors.
        assert!(
            rendered.contains("asymmetry    tool_emit te-002: buffered FAIL, streamed PASS"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "asymmetry    instruction if-001: buffered PASS, streamed FAIL — failed 'max_lines'"
            ),
            "{rendered}"
        );
        assert!(
            !rendered.contains("asymmetry    instruction if-002"),
            "a case that fails both ways is not an asymmetry: {rendered}"
        );
        // Individual failure lines say which door.
        assert!(
            rendered.contains("instruction FAIL if-001 [streamed]  failed 'max_lines'"),
            "{rendered}"
        );
        // The grammar gap pairs the forced pass against BUFFERED unconstrained
        // rows only — the forced pass is buffered, and doubling the other side
        // would invent a gap.
        assert!(
            rendered.contains("unconstrained on the same cases 1/2 (gap +50%)"),
            "{rendered}"
        );
    }

    #[test]
    fn agreeing_transports_say_so_and_a_buffered_only_run_says_nothing() {
        let eval = scratch("agree");
        let mut writer = graded_run(&eval);
        let buffered_only = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(!buffered_only.contains("asymmetry"), "{buffered_only}");
        writer
            .append(streamed("tool_emit", "te-001", GradeRow::pass()))
            .expect("append");
        writer
            .append(streamed(
                "tool_emit",
                "te-002",
                GradeRow::fail("called 'read_file', expected 'grep'".to_owned()),
            ))
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains(
                "asymmetry    none — buffered and streamed agree on every case measured both ways"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn the_grammar_gap_line_names_the_forced_arms_reasoning_mode() {
        // The forced arm differs from the unconstrained one in one more way
        // when the engine extracted reasoning for it — printed, never hidden.
        let eval = scratch("forced-mode");
        let mut with_mode = head();
        with_mode.forced_reasoning_format = Some("deepseek".into());
        let mut writer = RunWriter::create(&eval, "r9-model", &with_mode).expect("create");
        for (suite, id) in [
            ("tool_emit", "te-001"),
            ("tool_emit", "te-002"),
            ("grammar_gap", "gg-te-001"),
            ("grammar_gap", "gg-te-002"),
        ] {
            writer
                .append(graded(suite, id, GradeRow::pass()))
                .expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains(
                "grammar_gap  2/2 forced — unconstrained on the same cases 2/2 (gap +0%); \
                 forced pass ran with reasoning extracted (deepseek)"
            ),
            "{rendered}"
        );

        // A run recorded before the field, or without extraction, says nothing.
        let plain =
            render_run(&RunLog::load(graded_run(&scratch("forced-plain")).dir()).expect("load"));
        assert!(!plain.contains("reasoning extracted"), "{plain}");
        let old_head = r#"{"model":"m","machine_brand":null,"launch_args":[],"stamp":"#;
        let old = format!(
            "{old_head}{}}}",
            serde_json::to_string(&stamp()).expect("stamp")
        );
        let loaded: RunHead = serde_json::from_str(&old).expect("an old stamp.json still loads");
        assert_eq!(loaded.forced_reasoning_format, None);
    }

    /// Bundled so `codebase_task` stays within the 3-argument limit.
    #[derive(Clone, Copy)]
    struct CodebaseFixture<'a> {
        id: &'a str,
        tier: TaskTier,
        gold: &'a str,
        prediction: &'a str,
    }

    /// The three tasks `codebase_rows_round_trip_and_the_block_recomputes_tiers_from_stored_text`
    /// exercises: two in-file (one exact, one a miss) and one function body.
    fn codebase_fixtures() -> [CodebaseFixture<'static>; 3] {
        [
            CodebaseFixture {
                id: "in_file-abc123-L7",
                tier: TaskTier::InFile,
                gold: "let a = 1;",
                prediction: "let a = 1;",
            },
            CodebaseFixture {
                id: "in_file-abc123-L9",
                tier: TaskTier::InFile,
                gold: "let b = 2;",
                prediction: "let c = 3;",
            },
            CodebaseFixture {
                id: "function_body-abc123-L20",
                tier: TaskTier::FunctionBody,
                gold: "let x = 1;\n    x",
                prediction: "x",
            },
        ]
    }

    fn codebase_task(fixture: CodebaseFixture) -> Task {
        Task {
            suite: "codebase".into(),
            task_id: fixture.id.into(),
            measure: measure(&[20.0, 20.0]),
            grade: None,
            transport: Transport::Buffered,
            codebase: Some(CodebaseRow {
                tier: fixture.tier,
                file: "src/a.rs".into(),
                line: 7,
                label: "boundary-scanned (not AST)".into(),
                gold: fixture.gold.into(),
                prediction: fixture.prediction.into(),
                prefix: "fn f() {\n".into(),
                suffix: "\n}\n".into(),
                excluded: Excluded {
                    doc_comment: 0,
                    cross_file: "n/a: same-file".into(),
                    cfg_test_lines: 0,
                    cross_file_withheld: 0,
                },
                symbols_score: Some(1.0),
                unsupported: false,
                arm: None,
                extra: None,
                also_first_uses: Vec::new(),
                name: None,
                n_predict: Some(64),
                exec: None,
            }),
            judge: None,
        }
    }

    /// The same task with its file's inline tests counted as elided — the
    /// header clause reads this and nothing else.
    fn elided_codebase_task(id: &str, file: &str, cfg_test_lines: usize) -> Task {
        let mut task = codebase_task(CodebaseFixture {
            id,
            tier: TaskTier::InFile,
            gold: "let a = 1;",
            prediction: "let a = 1;",
        });
        task.task_id = id.into();
        if let Some(row) = task.codebase.as_mut() {
            row.file = file.into();
            row.excluded.cfg_test_lines = cfg_test_lines;
        }
        task
    }

    /// One arm of a cross-file task: same span, same gold, different context
    /// and so a different prediction.
    fn cross_arm(id: &str, arm: &str, prediction: &str) -> Task {
        let mut task = codebase_task(CodebaseFixture {
            id,
            tier: TaskTier::InFile,
            gold: "let a = build(1);",
            prediction,
        });
        task.task_id = id.into();
        if let Some(row) = task.codebase.as_mut() {
            row.tier = TaskTier::CrossFileFirst;
            row.arm = Some(arm.into());
            row.also_first_uses = vec!["Widget".into()];
            row.symbols_score = Some(if arm == "extra" { 1.0 } else { 0.5 });
            if arm == "extra" {
                row.extra = Some(ExtraFile {
                    path: "src/defs.rs".into(),
                    bytes: 2048,
                    truncated: false,
                });
                row.excluded.cross_file =
                    "sent src/defs.rs (2.0 KiB); withheld 2 (contain the answer)".into();
                row.excluded.cross_file_withheld = 2;
            }
        }
        task
    }

    /// A codebase row nobody could answer: unavailable, with no tier-5 score
    /// and the crossing's own verdict on whether the model can infill at all.
    fn unavailable_codebase_task(id: &str, reason: &str, unsupported: bool) -> Task {
        let mut task = codebase_task(CodebaseFixture {
            id,
            tier: TaskTier::InFile,
            gold: "let a = 1;",
            prediction: "",
        });
        task.task_id = id.into();
        if let Some(row) = task.codebase.as_mut() {
            row.symbols_score = None;
            row.unsupported = unsupported;
        }
        task.grade = Some(GradeRow::unavailable(reason.into()));
        task
    }

    /// A row written by slice A knows nothing of the arms or the withheld
    /// count. It must still load — `deny_unknown_fields` guards the other
    /// direction, a typo in a NEW field, and never rejects an old run.
    #[test]
    fn a_slice_a_codebase_row_loads_without_the_slice_b_fields() {
        let row = r#"{"schema":1,"run_id":"r","seq":0,"suite":"codebase","task_id":"in_file-abc-L7","measure":{"prompt_n":10,"decode_samples":[20.0],"prefill_samples":[400.0],"warmup_dropped":0,"cache_n":0},"transport":"buffered","codebase":{"tier":"in_file","file":"src/a.rs","line":7,"label":"boundary-scanned (not AST)","gold":"let a = 1;","prediction":"let a = 1;","prefix":"fn f() {\n","suffix":"\n}\n","excluded":{"doc_comment":0,"cross_file":"n/a: same-file"}}}"#;
        let parsed: super::TaskRow = serde_json::from_str(row).expect("a slice-A row loads");
        let codebase = parsed.codebase.expect("a codebase row");
        assert_eq!(codebase.arm, None);
        assert_eq!(codebase.extra, None);
        assert!(codebase.also_first_uses.is_empty());
        assert_eq!(codebase.excluded.cross_file_withheld, 0);
        assert_eq!(codebase.excluded.cfg_test_lines, 0);
    }

    /// The row a run with `--allow-exec` writes, and the one a run without it
    /// writes, both round-trip — and a pre-B2 row loads as the second.
    #[test]
    fn an_exec_row_round_trips_and_a_pre_b2_row_loads_without_one() {
        let mut task = codebase_task(CodebaseFixture {
            id: "in_file-abc123-L7",
            tier: TaskTier::InFile,
            gold: "let a = 1;",
            prediction: "let a = 1;",
        });
        if let Some(row) = task.codebase.as_mut() {
            row.exec = Some(super::ExecRow {
                compile: super::ExecScore::Value(1.0),
                compile_error: None,
                tests: vec!["covers_alpha".into()],
                test: super::ExecScore::Skipped("did not compile".into()),
                test_failure: None,
                check_secs: 6.25,
                test_secs: 0.0,
            });
        }
        let row = task.codebase.expect("a codebase row");
        let text = serde_json::to_string(&row).expect("serialise");
        let back: super::CodebaseRow = serde_json::from_str(&text).expect("deserialise");
        let exec = back.exec.expect("the exec half survives the round trip");
        assert_eq!(exec.compile, super::ExecScore::Value(1.0));
        assert_eq!(
            exec.test,
            super::ExecScore::Skipped("did not compile".into())
        );
        assert_eq!(exec.tests, vec!["covers_alpha".to_owned()]);

        let pre_b2 = r#"{"tier":"in_file","file":"src/a.rs","line":7,
            "label":"boundary-scanned (not AST)","gold":"let a = 1;","prediction":"let a = 1;",
            "prefix":"fn f() {\n","suffix":"\n}\n",
            "excluded":{"doc_comment":0,"cross_file":"n/a: same-file"}}"#;
        let old: super::CodebaseRow = serde_json::from_str(pre_b2).expect("a pre-B2 row loads");
        assert!(
            old.exec.is_none(),
            "a run that never executed has no exec half"
        );
    }

    /// A skip is a reason, never a zero: `ExecRow::skipped` measures nothing
    /// and scores nothing.
    #[test]
    fn a_wholly_skipped_exec_row_carries_the_reason_on_both_tiers() {
        let row = super::ExecRow::skipped("no Rust toolchain: cargo not on PATH");
        assert_eq!(
            row.compile,
            super::ExecScore::Skipped("no Rust toolchain: cargo not on PATH".into())
        );
        assert_eq!(row.compile, row.test, "one reason covers both tiers");
        assert!(row.tests.is_empty());
        assert!(row.compile_error.is_none() && row.test_failure.is_none());
    }

    #[test]
    fn the_block_reports_both_arms_and_the_lift_between_them() {
        let eval = scratch("codebase-arms");
        let mut writer = RunWriter::create(&eval, "r20-model", &head()).expect("create");
        for task in codebase_fixtures().into_iter().take(2).map(codebase_task) {
            writer.append(task).expect("append");
        }
        for (id, arm, prediction) in [
            (
                "cross_file_first-abc123-L4",
                "no_extra",
                "let a = guess(1);",
            ),
            (
                "cross_file_first-abc123-L4+extra",
                "extra",
                "let a = build(1);",
            ),
        ] {
            writer
                .append(cross_arm(id, arm, prediction))
                .expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        for line in [
            "codebase     3 tasks, 4 crossings, from 1 files (2 in_file, 0 function_body, \
             1 cross_file_first × 2 arms) — boundary-scanned (not AST); context: same-file, \
             plus the defining file for cross_file_first (engine window ≤ n_batch; extra from ctx)",
            "             cross_file_first        exact 0.00",
            "             cross_file_first+extra  exact 1.00",
            "             context lift            exact +1.00",
            "(1 files sent, 2.0 KiB, 0 truncated; 2 withheld)",
            "             tiers 6-7 skipped: --allow-exec not given",
        ] {
            assert!(rendered.contains(line), "{line}\n---\n{rendered}");
        }
        assert!(
            rendered.contains("symbols +0.50"),
            "tier 5's lift comes from the stored scores: {rendered}"
        );
    }

    /// A cross-file task answered on only one arm cannot contribute a
    /// difference, so it leaves the lift — and the line says so.
    #[test]
    fn a_task_measured_on_one_arm_only_is_excluded_from_the_lift() {
        let eval = scratch("codebase-half-arm");
        let mut writer = RunWriter::create(&eval, "r21-model", &head()).expect("create");
        for (id, arm, prediction) in [
            (
                "cross_file_first-abc123-L4",
                "no_extra",
                "let a = guess(1);",
            ),
            (
                "cross_file_first-abc123-L4+extra",
                "extra",
                "let a = build(1);",
            ),
            (
                "cross_file_first-abc123-L9",
                "no_extra",
                "let a = guess(2);",
            ),
        ] {
            writer
                .append(cross_arm(id, arm, prediction))
                .expect("append");
        }
        writer
            .append(unavailable_codebase_task(
                "cross_file_first-abc123-L9+extra",
                "the server stopped answering",
                false,
            ))
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("(n=1 of 2; 1 files sent, 2.0 KiB, 0 truncated; 2 withheld)"),
            "{rendered}"
        );
        assert!(rendered.contains("(1 unavailable, excluded)"), "{rendered}");
    }

    #[test]
    fn a_run_without_cross_file_tasks_says_so_instead_of_printing_three_empty_lines() {
        let eval = scratch("codebase-no-cross");
        let mut writer = RunWriter::create(&eval, "r22-model", &head()).expect("create");
        for task in codebase_fixtures().map(codebase_task) {
            writer.append(task).expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("(2 in_file, 1 function_body, 0 cross_file_first)"),
            "no arms to announce when there are no cross tasks: {rendered}"
        );
        assert!(
            rendered.contains(
                "             cross_file_first        none sampled — no unambiguous \
                 cross-file first use in this repository"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("context lift"), "{rendered}");
        assert!(!rendered.contains("cross_file_first+extra"), "{rendered}");
    }

    /// An outage that took every crossing away is not a repository without
    /// cross-file first uses — the run sampled six, and the line says which
    /// of the two happened.
    #[test]
    fn a_cross_lane_lost_to_an_outage_says_so_and_does_not_blame_the_repository() {
        let eval = scratch("codebase-cross-outage");
        let mut writer = RunWriter::create(&eval, "r23-model", &head()).expect("create");
        for task in codebase_fixtures().into_iter().take(2).map(codebase_task) {
            writer.append(task).expect("append");
        }
        for id in [
            "cross_file_first-abc123-L4",
            "cross_file_first-abc123-L4+extra",
        ] {
            let mut task = unavailable_codebase_task(id, "the server stopped answering", false);
            if let Some(row) = task.codebase.as_mut() {
                row.tier = TaskTier::CrossFileFirst;
            }
            writer.append(task).expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains(
                "             cross_file_first        all 2 crossings unavailable — \
                 the server stopped answering"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("none sampled"), "{rendered}");
        assert!(!rendered.contains("context lift"), "{rendered}");
    }

    #[test]
    fn codebase_rows_round_trip_and_the_block_recomputes_tiers_from_stored_text() {
        let eval = scratch("codebase-rows");
        let mut writer = RunWriter::create(&eval, "r10-model", &head()).expect("create");
        for task in codebase_fixtures().map(codebase_task) {
            writer.append(task).expect("append");
        }
        let log = RunLog::load(writer.dir()).expect("load");
        assert_eq!(
            log.rows[0].codebase.as_ref().map(|c| c.prediction.as_str()),
            Some("let a = 1;")
        );
        let rendered = render_run(&log);
        let expected = [
            "codebase     3 tasks, 3 crossings, from 1 files (2 in_file, 1 function_body, \
             0 cross_file_first) — boundary-scanned (not AST); context: same-file, plus the \
             defining file for cross_file_first (engine window ≤ n_batch; extra from ctx)",
            "in_file                 exact 0.50   edit_sim",
            "symbols 1.00 (scored at run time)   (n=2)",
            "function_body           ident_f1",
            "tiers 6-7 skipped: --allow-exec not given",
        ];
        for line in expected {
            assert!(rendered.contains(line), "{rendered}");
        }
        assert!(
            !rendered.contains("tests elided"),
            "a run that cut nothing says nothing: {rendered}"
        );
    }

    /// The clause counts lines per distinct file, not per row, and names only
    /// the files that actually gave something up.
    #[test]
    fn a_codebase_block_says_how_many_test_lines_were_elided_and_from_how_many_files() {
        let eval = scratch("codebase-elided");
        let mut writer = RunWriter::create(&eval, "r14-model", &head()).expect("create");
        for (id, file, lines) in [
            ("in_file-aaa111-L7", "src/a.rs", 12),
            ("in_file-bbb222-L7", "src/b.rs", 9),
            ("in_file-ccc333-L7", "src/c.rs", 0),
        ] {
            writer
                .append(elided_codebase_task(id, file, lines))
                .expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains(
                "(engine window ≤ n_batch; extra from ctx); tiers 1-4 score the first \
                 gold_lines lines of each fill; tests elided: 21 lines in 2 files"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn a_partly_unavailable_codebase_run_scores_what_was_answered_and_says_how_many_were_not() {
        let eval = scratch("codebase-partial");
        let mut writer = RunWriter::create(&eval, "r12-model", &head()).expect("create");
        for task in codebase_fixtures().into_iter().take(2).map(codebase_task) {
            writer.append(task).expect("append");
        }
        writer
            .append(unavailable_codebase_task(
                "in_file-abc123-L11",
                "the server stopped answering",
                false,
            ))
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains(
                "codebase     2 tasks, 2 crossings, from 1 files (2 in_file, 0 function_body, \
                 0 cross_file_first)"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "(engine window ≤ n_batch; extra from ctx); tiers 1-4 score the first \
                 gold_lines lines of each fill (1 unavailable, excluded)"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("exact 0.50") && rendered.contains("(n=2)"),
            "the means run over the two that answered: {rendered}"
        );
    }

    /// The reason an outage at the infill endpoint actually records: the
    /// error's own words, URL and all — and that URL ends in `/infill`.
    #[test]
    fn an_outage_at_the_infill_endpoint_is_not_a_missing_capability() {
        let eval = scratch("codebase-na-generic");
        let mut writer = RunWriter::create(&eval, "r13-model", &head()).expect("create");
        let outage = ChekovError::UpstreamRefused {
            url: "http://fake/infill".into(),
            status: 500,
            reason: "internal error".into(),
        }
        .to_string();
        assert!(
            outage.to_lowercase().contains("infill"),
            "the URL is in the words, which is exactly the trap: {outage}"
        );
        writer
            .append(unavailable_codebase_task(
                "in_file-abc123-L7",
                &outage,
                false,
            ))
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("codebase     N/A — the server at http://fake/infill"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("infill unsupported"),
            "a 500 is not a model that cannot infill: {rendered}"
        );
        assert!(
            !rendered.contains("symbols 0.00"),
            "an unscored row is not a zero: {rendered}"
        );
    }

    #[test]
    fn an_infill_unsupported_run_reports_na_not_zero() {
        let eval = scratch("codebase-na");
        let mut writer = RunWriter::create(&eval, "r11-model", &head()).expect("create");
        writer
            .append(unavailable_codebase_task(
                "in_file-abc123-L7",
                "infill is not supported by this model",
                true,
            ))
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains(
                "codebase     N/A — infill unsupported by this model (infill is not supported"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("exact 0.00"), "{rendered}");
    }

    #[test]
    fn a_row_written_before_the_codebase_field_loads() {
        let line = r#"{"schema":1,"run_id":"r","seq":0,"suite":"tool_emit","task_id":"te-001","measure":{"prompt_n":4,"decode_samples":[1.0],"prefill_samples":[1.0],"warmup_dropped":0}}"#;
        let row: TaskRow = serde_json::from_str(line).expect("loads");
        assert!(row.codebase.is_none());
    }

    #[test]
    fn a_hot_cache_is_visible_in_the_rendering() {
        let eval = scratch("cache-n");
        let mut writer = RunWriter::create(&eval, "r6-model", &head()).expect("create");
        let mut warm = measure(&[19.0, 21.0, 22.0]);
        warm.cache_n = 512;
        writer
            .append(Task {
                suite: "throughput".into(),
                task_id: "depth-1024".into(),
                measure: warm,
                grade: None,
                transport: Transport::Buffered,
                codebase: None,
                judge: None,
            })
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(rendered.contains("cache_n 512"), "{rendered}");
    }

    #[test]
    fn a_run_round_trips_row_by_row() {
        let eval = scratch("roundtrip");
        let mut writer = RunWriter::create(&eval, "r1-model", &head()).expect("create");
        for (bump, id) in [(0.0, "depth-1024"), (1.0, "depth-4096")] {
            writer
                .append(Task {
                    suite: "throughput".into(),
                    task_id: id.into(),
                    measure: measure(&[19.0, 21.0, 22.0 + bump]),
                    grade: None,
                    transport: Transport::Buffered,
                    codebase: None,
                    judge: None,
                })
                .expect("append");
        }
        let log = RunLog::load(writer.dir()).expect("load");
        assert_eq!(log.rows.len(), 2);
        assert_eq!(log.rows[0].seq, 0);
        assert_eq!(log.rows[1].seq, 1);
        assert_eq!(log.head.stamp, stamp());
        assert_eq!(log.rows[0].measure.decode_samples, vec![19.0, 21.0, 22.0]);
    }

    #[test]
    fn resume_reopens_skips_completed_and_continues_the_sequence() {
        let eval = scratch("resume");
        let mut writer = RunWriter::create(&eval, "r2-model", &head()).expect("create");
        writer
            .append(Task {
                suite: "throughput".into(),
                task_id: "depth-1024".into(),
                measure: measure(&[19.0, 21.0]),
                grade: None,
                transport: Transport::Buffered,
                codebase: None,
                judge: None,
            })
            .expect("append");
        drop(writer);
        let (mut resumed, log) = RunWriter::resume(&eval, "r2-model", &head()).expect("resume");
        assert!(log.is_done(&super::TaskKey::buffered("throughput", "depth-1024")));
        assert!(!log.is_done(&super::TaskKey::buffered("throughput", "depth-4096")));
        resumed
            .append(Task {
                suite: "throughput".into(),
                task_id: "depth-4096".into(),
                measure: measure(&[15.0, 16.0]),
                grade: None,
                transport: Transport::Buffered,
                codebase: None,
                judge: None,
            })
            .expect("append after resume");
        let reloaded = RunLog::load(resumed.dir()).expect("reload");
        assert_eq!(reloaded.rows.len(), 2);
        assert_eq!(
            reloaded.rows[1].seq, 1,
            "the sequence continues, not restarts"
        );
    }

    #[test]
    fn resume_with_a_differing_stamp_is_refused_naming_the_field() {
        let eval = scratch("resume-mismatch");
        let writer = RunWriter::create(&eval, "r3-model", &head()).expect("create");
        drop(writer);
        let mut changed = head();
        changed.stamp.seed = 7;
        let err = RunWriter::resume(&eval, "r3-model", &changed).expect_err("mixed stamps");
        match err {
            ChekovError::BenchStampMismatch { field, .. } => assert_eq!(field, "seed"),
            other => panic!("expected a stamp mismatch, got {other}"),
        }
    }

    #[test]
    fn a_corrupt_line_is_loud_with_its_line_number() {
        let eval = scratch("corrupt");
        let mut writer = RunWriter::create(&eval, "r4-model", &head()).expect("create");
        writer
            .append(Task {
                suite: "throughput".into(),
                task_id: "depth-1024".into(),
                measure: measure(&[19.0, 21.0]),
                grade: None,
                transport: Transport::Buffered,
                codebase: None,
                judge: None,
            })
            .expect("append");
        let results = writer.dir().join("results.jsonl");
        let mut text = std::fs::read_to_string(&results).expect("read");
        text.push_str("{not json\n");
        std::fs::write(&results, text).expect("corrupt");
        let err = RunLog::load(writer.dir()).expect_err("loud");
        assert!(err.to_string().contains("line 2"), "{err}");
    }

    #[test]
    fn rendering_recomputes_summaries_and_shows_grades() {
        let eval = scratch("render");
        let mut writer = RunWriter::create(&eval, "r5-model", &head()).expect("create");
        writer
            .append(Task {
                suite: "throughput".into(),
                task_id: "depth-1024".into(),
                measure: measure(&[19.0, 21.0, 22.0, 22.4]),
                grade: None,
                transport: Transport::Buffered,
                codebase: None,
                judge: None,
            })
            .expect("append");
        writer
            .append(Task {
                suite: "fixture".into(),
                task_id: "greeting".into(),
                measure: measure(&[20.0, 20.0]),
                grade: Some(GradeRow::fail(
                    "missing expected substring \"hello\"".to_owned(),
                )),
                transport: Transport::Buffered,
                codebase: None,
                judge: None,
            })
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(rendered.contains("ornith-1.5-35b-a3b"));
        // Median of [21.0, 22.0, 22.4] after the warmup drop — from stats, not storage.
        assert!(rendered.contains("22.0"), "{rendered}");
        assert!(
            rendered.contains("insufficient depths to fit a curve"),
            "{rendered}"
        );
        assert!(rendered.contains("fixture FAIL greeting"), "{rendered}");
    }

    /// The header says whose clock measured the run exactly when it was not
    /// the server's own: a llama.cpp render never mentions it at all, and a
    /// chekov-timed run says so with its caveat attached (spec §6).
    #[test]
    fn the_header_names_chekovs_clock_only_when_it_measured() {
        let eval = scratch("timing-source");
        let served = rendered_with(&eval, "r-server", &head());
        assert!(!served.contains("timing source:"), "{served}");

        let mut foreign = head();
        foreign.stamp.runtime = "mtplx 0.4.1".into();
        foreign.stamp.timing_source = crate::core::bench::stamp::TIMING_CHEKOV_STREAMED.to_owned();
        let timed = rendered_with(&eval, "r-foreign", &foreign);
        assert!(
            timed.contains(
                "timing source: chekov-streamed (client wall-clock over SSE; includes wire \
                 overhead)\n"
            ),
            "{timed}"
        );
    }

    /// The header names speculative decoding exactly when the run used it: a
    /// plain run's render is what it always was (spec-stage design §6).
    #[test]
    fn the_header_names_the_draft_only_when_the_run_was_decoded_with_it() {
        let eval = scratch("speculative");
        let plain = rendered_with(&eval, "r-plain", &head());
        assert!(!plain.contains("speculative:"), "{plain}");

        let mut drafted = head();
        drafted.stamp.spec_type = "draft-mtp".into();
        drafted.stamp.spec_draft_n_max = "1".into();
        let with_head = rendered_with(&eval, "r-draft", &drafted);
        assert!(
            with_head.contains("speculative: draft-mtp, draft length 1\n"),
            "{with_head}"
        );

        let mut default_len = head();
        default_len.stamp.spec_type = "draft-mtp".into();
        let engine_len = rendered_with(&eval, "r-draft-default", &default_len);
        assert!(
            engine_len.contains("speculative: draft-mtp, draft length engine-default\n"),
            "{engine_len}"
        );
    }

    /// One depth recorded under the given head, rendered.
    fn rendered_with(eval: &std::path::Path, run_id: &str, head: &RunHead) -> String {
        let mut writer = RunWriter::create(eval, run_id, head).expect("create");
        writer
            .append(Task {
                suite: "throughput".into(),
                task_id: "depth-1024".into(),
                measure: measure(&[19.0, 21.0, 22.0, 22.4]),
                grade: None,
                transport: Transport::Buffered,
                codebase: None,
                judge: None,
            })
            .expect("append");
        render_run(&RunLog::load(writer.dir()).expect("load"))
    }

    /// A codebase task whose exec half is what the caller says.
    fn exec_task(id: &str, compile: super::ExecScore, tests: &[&str]) -> Task {
        let mut task = codebase_task(CodebaseFixture {
            id,
            tier: TaskTier::InFile,
            gold: "let a = 1;",
            prediction: "let a = 1;",
        });
        task.task_id = id.into();
        let passed = compile == super::ExecScore::Value(1.0);
        if let Some(row) = task.codebase.as_mut() {
            row.exec = Some(super::ExecRow {
                compile,
                compile_error: None,
                tests: tests.iter().map(|t| (*t).to_owned()).collect(),
                test: if !passed {
                    super::ExecScore::Skipped("did not compile".into())
                } else if tests.is_empty() {
                    super::ExecScore::Skipped("no covering test".into())
                } else {
                    super::ExecScore::Value(1.0)
                },
                test_failure: None,
                check_secs: 6.0,
                test_secs: 0.0,
            });
        }
        task
    }

    /// A head whose stamp says the run was allowed to build.
    fn exec_head() -> RunHead {
        let mut head = head();
        head.stamp.allow_exec = true;
        head.stamp.cargo_version = Some("cargo 1.95.0 (deadbeef 2026-01-01)".into());
        head.stamp.exec_target = "scratch".into();
        head
    }

    #[test]
    fn a_tier_line_carries_the_two_exec_cells_after_symbols() {
        let eval = scratch("codebase-exec-cells");
        let mut writer = RunWriter::create(&eval, "r30-model", &exec_head()).expect("create");
        writer
            .append(exec_task(
                "in_file-abc123-L7",
                super::ExecScore::Value(1.0),
                &["covers_alpha"],
            ))
            .expect("append");
        writer
            .append(exec_task(
                "in_file-abc123-L9",
                super::ExecScore::Value(0.0),
                &[],
            ))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains("compile 0.50 (n=2)   test 1.00 (n=1 of 2 had a covering test)"),
            "{block}"
        );
    }

    /// Every crossing skipped: the cell says `n/a`, never `0.00`.
    #[test]
    fn a_tier_line_with_no_verdict_says_n_a_and_never_zero() {
        let eval = scratch("codebase-exec-na");
        let mut writer = RunWriter::create(&eval, "r31-model", &exec_head()).expect("create");
        writer
            .append(exec_task(
                "in_file-abc123-L7",
                super::ExecScore::Skipped("needs network: no matching package".into()),
                &[],
            ))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains("compile n/a   test n/a (0 of 1 had a covering test)"),
            "{block}"
        );
        assert!(
            !block.contains("compile 0.00"),
            "a skip is not a zero: {block}"
        );
    }

    /// One crossing's recorded check time, reached through the two optional
    /// halves the fixture always fills.
    fn set_check_secs(task: &mut Task, secs: f64) {
        if let Some(exec) = task.codebase.as_mut().and_then(|row| row.exec.as_mut()) {
            exec.check_secs = secs;
        }
    }

    #[test]
    fn the_header_names_the_toolchain_and_the_trailer_the_timing_and_the_skips() {
        let eval = scratch("codebase-exec-trailer");
        let mut writer = RunWriter::create(&eval, "r32-model", &exec_head()).expect("create");
        let mut cold = exec_task("in_file-abc123-L7", super::ExecScore::Value(1.0), &["t"]);
        set_check_secs(&mut cold, 84.0);
        writer.append(cold).expect("append");
        for (i, secs) in [6.0_f64, 6.0, 7.0].into_iter().enumerate() {
            let mut task = exec_task(
                &format!("in_file-abc123-L{}", 10 + i),
                super::ExecScore::Value(1.0),
                &["t"],
            );
            set_check_secs(&mut task, secs);
            writer.append(task).expect("append");
        }
        writer
            .append(exec_task(
                "in_file-abc123-L20",
                super::ExecScore::Skipped("check timed out after 120 s".into()),
                &[],
            ))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains("; exec: cargo 1.95.0 (deadbeef 2026-01-01), offline, scratch target"),
            "{block}"
        );
        assert!(
            block.contains(
                "             tiers 6-7: cold check 84 s, then 6 s median per crossing; \
                 1 skipped (1 check timed out after 120 s)\n"
            ),
            "{block}"
        );
    }

    /// The two runs that never built anything each say so in their own words.
    #[test]
    fn a_run_without_the_flag_and_a_run_without_a_toolchain_say_different_things() {
        let eval = scratch("codebase-exec-off");
        let mut writer = RunWriter::create(&eval, "r33-model", &head()).expect("create");
        writer
            .append(codebase_task(CodebaseFixture {
                id: "in_file-abc123-L7",
                tier: TaskTier::InFile,
                gold: "let a = 1;",
                prediction: "let a = 1;",
            }))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains("             tiers 6-7 skipped: --allow-exec not given\n"),
            "{block}"
        );
        assert!(
            !block.contains("compile"),
            "no cells without the flag: {block}"
        );

        let eval = scratch("codebase-exec-no-toolchain");
        let mut writer = RunWriter::create(&eval, "r34-model", &exec_head()).expect("create");
        writer
            .append(exec_task(
                "in_file-abc123-L7",
                super::ExecScore::Skipped("no Rust toolchain: cargo is not runnable".into()),
                &[],
            ))
            .expect("append");
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            block.contains(
                "             tiers 6-7 skipped: no Rust toolchain: cargo is not runnable\n"
            ),
            "{block}"
        );
    }

    /// The lift's exec columns come from the pairs measured in BOTH arms.
    #[test]
    fn the_context_lift_reports_the_exec_tiers_too() {
        let eval = scratch("codebase-exec-lift");
        let mut writer = RunWriter::create(&eval, "r35-model", &exec_head()).expect("create");
        for (id, arm, compile) in [
            ("cross_file_first-abc123-L2", "no_extra", 0.0),
            ("cross_file_first-abc123-L2+extra", "extra", 1.0),
        ] {
            let mut task = cross_arm(id, arm, "let a = build(1);");
            if let Some(row) = task.codebase.as_mut() {
                row.exec = Some(super::ExecRow {
                    compile: super::ExecScore::Value(compile),
                    compile_error: None,
                    tests: Vec::new(),
                    test: super::ExecScore::Skipped("no covering test".into()),
                    test_failure: None,
                    check_secs: 6.0,
                    test_secs: 0.0,
                });
            }
            writer.append(task).expect("append");
        }
        let block = super::render_codebase(&RunLog::load(writer.dir()).expect("load"));
        assert!(block.contains("compile +1.00  test n/a"), "{block}");
    }

    /// Bundled so `judge_verdict` stays within the 3-argument limit.
    #[derive(Clone, Copy)]
    struct JudgeFixture<'a> {
        id: &'a str,
        equivalent: Option<bool>,
        gold_first: Option<bool>,
        prediction_first: Option<bool>,
        decided_by: DecidedBy,
        skipped: Option<&'a str>,
        /// What the pair cost. `0.0` is what production records for a crossing
        /// settled before any request went out.
        judge_secs: f64,
    }

    /// The pair time a crossing that reached the wire records.
    const ASKED_SECS: f64 = 2.1;

    fn judge_verdict(fixture: JudgeFixture) -> Task {
        judge_task(
            fixture.id,
            JudgeRow {
                equivalent: fixture.equivalent,
                gold_first: fixture.gold_first,
                prediction_first: fixture.prediction_first,
                decided_by: fixture.decided_by,
                skipped: fixture.skipped.map(str::to_owned),
                judge_secs: fixture.judge_secs,
            },
        )
    }

    /// `fb-1` identical, `fb-2..fb-4` swap agreements (two of them agreeing
    /// on `false`).
    fn identical_and_agreements() -> [JudgeFixture<'static>; 4] {
        [
            JudgeFixture {
                id: "fb-1",
                equivalent: Some(true),
                gold_first: None,
                prediction_first: None,
                decided_by: DecidedBy::Identical,
                skipped: None,
                judge_secs: 0.0,
            },
            JudgeFixture {
                id: "fb-2",
                equivalent: Some(true),
                gold_first: Some(true),
                prediction_first: Some(true),
                decided_by: DecidedBy::SwapAgreement,
                skipped: None,
                judge_secs: ASKED_SECS,
            },
            JudgeFixture {
                id: "fb-3",
                equivalent: Some(false),
                gold_first: Some(false),
                prediction_first: Some(false),
                decided_by: DecidedBy::SwapAgreement,
                skipped: None,
                judge_secs: ASKED_SECS,
            },
            JudgeFixture {
                id: "fb-4",
                equivalent: Some(false),
                gold_first: Some(false),
                prediction_first: Some(false),
                decided_by: DecidedBy::SwapAgreement,
                skipped: None,
                judge_secs: ASKED_SECS,
            },
        ]
    }

    /// `fb-5` a swap disagreement, `fb-6` skipped for an off-schema reply.
    fn disagreement_and_skip() -> [JudgeFixture<'static>; 2] {
        [
            JudgeFixture {
                id: "fb-5",
                equivalent: None,
                gold_first: Some(true),
                prediction_first: Some(false),
                decided_by: DecidedBy::SwapDisagreement,
                skipped: None,
                judge_secs: ASKED_SECS,
            },
            JudgeFixture {
                id: "fb-6",
                equivalent: None,
                gold_first: None,
                prediction_first: None,
                decided_by: DecidedBy::Skipped,
                skipped: Some("reply was not the schema: yes"),
                judge_secs: ASKED_SECS,
            },
        ]
    }

    fn judge_verdicts() -> impl Iterator<Item = JudgeFixture<'static>> {
        identical_and_agreements()
            .into_iter()
            .chain(disagreement_and_skip())
    }

    /// Three `function_body` crossings, all skipped for the same truncation —
    /// no order of any of them parsed, yet every one of them was asked, and
    /// paid for.
    fn all_skipped_verdicts() -> [JudgeFixture<'static>; 3] {
        ["fb-1", "fb-2", "fb-3"].map(|id| JudgeFixture {
            id,
            equivalent: None,
            gold_first: None,
            prediction_first: None,
            decided_by: DecidedBy::Skipped,
            skipped: Some("reply truncated at 512 tokens"),
            judge_secs: ASKED_SECS,
        })
    }

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

    /// A `function_body` run whose crossings are named by `ids`, judged by
    /// `verdicts`. Every call gets its own scratch directory — several tests
    /// build one of these, and `cargo test` runs them concurrently.
    fn judged_run_with(
        scratch_name: &str,
        ids: &[&'static str],
        verdicts: impl Iterator<Item = JudgeFixture<'static>>,
    ) -> RunLog {
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let eval = scratch(&format!("{scratch_name}-{n}"));
        let mut head = head();
        head.stamp.judge = Some(judge_stamp());
        let mut writer = RunWriter::create(&eval, "r-judged-report", &head).expect("create");
        for &id in ids {
            writer
                .append(codebase_task(CodebaseFixture {
                    id,
                    tier: TaskTier::FunctionBody,
                    gold: "let x = 1;\n    x",
                    prediction: "x",
                }))
                .expect("append");
        }
        for fixture in verdicts {
            writer.append(judge_verdict(fixture)).expect("append");
        }
        RunLog::load(writer.dir()).expect("load")
    }

    /// Six `function_body` crossings plus the verdicts `judge_verdicts`
    /// describes.
    fn judged_run() -> RunLog {
        judged_run_with(
            "judged-report",
            &["fb-1", "fb-2", "fb-3", "fb-4", "fb-5", "fb-6"],
            judge_verdicts(),
        )
    }

    /// Three `function_body` crossings, every one of them skipped.
    fn all_skipped_judged_run() -> RunLog {
        judged_run_with(
            "all-skipped-report",
            &["fb-1", "fb-2", "fb-3"],
            all_skipped_verdicts().into_iter(),
        )
    }

    /// A stored run with no judge, and the stamp a `--resume --judge` would
    /// bring: identical but for the judge.
    fn adoptable(scratch_name: &str) -> (PathBuf, Stamp) {
        let eval = scratch(scratch_name);
        RunWriter::create(&eval, "r-adopt", &head()).expect("create");
        let mut incoming = stamp();
        incoming.judge = Some(judge_stamp());
        (eval.join("r-adopt"), incoming)
    }

    #[test]
    fn a_run_without_a_judge_adopts_one_when_the_rest_of_the_stamp_matches() {
        let (dir, incoming) = adoptable("adopt-judge-match");
        super::adopt_judge(&dir, &incoming).expect("adopt");
        let reloaded = RunLog::load(&dir).expect("load");
        assert_eq!(reloaded.head.stamp.judge, Some(judge_stamp()));
    }

    /// The stamp is rewritten only for a resume that will be accepted — a
    /// refused one must leave the run exactly as it found it, since nothing
    /// can take an adopted judge back off.
    #[test]
    fn a_resume_that_will_be_refused_leaves_the_stored_stamp_alone() {
        let (dir, mut incoming) = adoptable("adopt-judge-mismatch");
        incoming.ctx = 65_536;
        let stamp_path = dir.join("stamp.json");
        let before = std::fs::read_to_string(&stamp_path).expect("read");
        super::adopt_judge(&dir, &incoming).expect("a no-op, not an error");
        let after = std::fs::read_to_string(&stamp_path).expect("read");
        assert_eq!(before, after, "a refused resume never rewrites the stamp");
        let eval = dir.parent().expect("eval dir");
        let refused = RunWriter::resume(
            eval,
            "r-adopt",
            &RunHead {
                stamp: incoming,
                ..head()
            },
        )
        .expect_err("the stamp still differs");
        assert!(
            matches!(refused, ChekovError::BenchStampMismatch { ref field, .. } if field == "ctx"),
            "{refused}"
        );
    }

    #[test]
    fn a_run_that_already_names_a_judge_keeps_it_and_resume_refuses_a_different_one() {
        let eval = scratch("adopt-judge-taken");
        let stored = RunHead {
            stamp: Stamp {
                judge: Some(judge_stamp()),
                ..stamp()
            },
            ..head()
        };
        RunWriter::create(&eval, "r-adopt", &stored).expect("create");
        let dir = eval.join("r-adopt");
        let incoming = Stamp {
            judge: Some(JudgeStamp {
                model: "gemma-3-12b-it".into(),
                ..judge_stamp()
            }),
            ..stamp()
        };
        super::adopt_judge(&dir, &incoming).expect("a no-op, not an error");
        let reloaded = RunLog::load(&dir).expect("load");
        assert_eq!(
            reloaded.head.stamp.judge,
            Some(judge_stamp()),
            "the judge a run was measured with is never replaced"
        );
        let refused = RunWriter::resume(
            &eval,
            "r-adopt",
            &RunHead {
                stamp: incoming,
                ..head()
            },
        )
        .expect_err("a different judge is a different environment");
        assert!(
            matches!(refused, ChekovError::BenchStampMismatch { ref field, .. } if field == "judge"),
            "{refused}"
        );
    }

    /// A judge-stamped run whose crossings are all `in_file`: there was never
    /// a `function_body` for the judge to be asked about.
    fn nothing_eligible_run() -> RunLog {
        let eval = scratch("nothing-eligible-report");
        let mut head = head();
        head.stamp.judge = Some(judge_stamp());
        let mut writer = RunWriter::create(&eval, "r-nothing-eligible", &head).expect("create");
        for fixture in codebase_fixtures()
            .into_iter()
            .filter(|f| f.tier == TaskTier::InFile)
        {
            writer.append(codebase_task(fixture)).expect("append");
        }
        RunLog::load(writer.dir()).expect("load")
    }

    /// A plain codebase run: no `--judge` given at all.
    fn codebase_only_run() -> RunLog {
        let eval = scratch("codebase-only-report");
        let mut writer = RunWriter::create(&eval, "r-codebase-only", &head()).expect("create");
        for task in codebase_fixtures().map(codebase_task) {
            writer.append(task).expect("append");
        }
        RunLog::load(writer.dir()).expect("load")
    }

    #[test]
    fn the_function_body_line_carries_the_equiv_cell_and_the_header_names_the_judge() {
        let out = render_codebase(&judged_run());
        assert!(out.contains("; judge: gpt-oss-20b (F16@1a2b3c4d5e6f, gpt-oss, effort low) rubric 9f8e7d6c5b4a, swap consistency 75% (3 of 4)"), "{out}");
        assert!(
            out.contains("equiv 0.50 (n=4 judged of 6; 1 undecided)"),
            "identical counts as true, 2 true of 4: {out}"
        );
        assert!(out.contains("             judge: 1 identical, 5 called, 1 undecided, 1 skipped; 2.1 s median per verdict\n"), "the off-schema crossing was asked like any other: {out}");
        assert!(out.contains("             warning: 1 reply was not the schema — the grammar was not enforced for this judge and the column is measuring the prompt alone\n"), "{out}");
        let in_file_line = out
            .lines()
            .find(|l| l.contains("in_file"))
            .unwrap_or_default();
        assert!(
            !in_file_line.contains("equiv"),
            "other tiers print no equiv cell: {in_file_line}"
        );
    }

    #[test]
    fn below_the_floor_the_column_is_voided_with_both_numbers() {
        let mut log = judged_run();
        if let Some(judge) = log.head.stamp.judge.as_mut() {
            judge.min_consistency_pct = 80;
        }
        let out = render_codebase(&log);
        assert!(
            out.contains("equiv voided (swap consistency 75% < 80%)"),
            "{out}"
        );
    }

    #[test]
    fn without_a_judge_the_trailer_says_so_and_an_unjudged_run_says_how_to_resume() {
        let out = render_codebase(&codebase_only_run());
        assert!(
            out.contains("             judge skipped: --judge not given\n"),
            "{out}"
        );
        let mut stamped = judged_run();
        stamped.rows.retain(|r| r.suite != "judge");
        let out = render_codebase(&stamped);
        assert!(
            out.contains(
                "             judge: 0 of 6 crossings judged — resume with --judge gpt-oss-20b\n"
            ),
            "{out}"
        );
    }

    /// A codebase run for a given stamp, under its own scratch directory —
    /// separate from `codebase_only_run`'s so the two runs in the transport
    /// test below cannot race the same fixture directory.
    fn codebase_run_stamped(dir_name: &str, run_id: &str, stamp: Stamp) -> RunLog {
        let eval = scratch(dir_name);
        let head = RunHead { stamp, ..head() };
        let mut writer = RunWriter::create(&eval, run_id, &head).expect("create");
        for task in codebase_fixtures().map(codebase_task) {
            writer.append(task).expect("append");
        }
        RunLog::load(writer.dir()).expect("load")
    }

    /// The report names the wire that filled the rows, read from the run's
    /// own stamp — never a separately stored choice that could drift from it.
    #[test]
    fn the_codebase_header_names_the_fim_transport() {
        let llama_cpp =
            codebase_run_stamped("codebase-infill-transport-report", "r-infill", stamp());
        let out = render_codebase(&llama_cpp);
        assert!(out.contains("fim transport: /infill\n"), "{out}");

        let foreign = codebase_run_stamped(
            "codebase-chat-transport-report",
            "r-chat",
            Stamp {
                runtime: "vllm 0.7.0".into(),
                ..stamp()
            },
        );
        let out = render_codebase(&foreign);
        assert!(out.contains("fim transport: chat\n"), "{out}");
    }

    /// A run the judge had nothing to ask about does not read as a run whose
    /// judge phase is still owed — there is nothing to resume.
    #[test]
    fn a_run_with_no_function_body_crossing_says_nothing_was_eligible() {
        let out = render_codebase(&nothing_eligible_run());
        assert!(
            out.contains("             judge: nothing eligible\n"),
            "{out}"
        );
        assert!(!out.contains("resume with --judge"), "{out}");
    }

    /// A judge that never got an answered order still names a rate — `n/a`,
    /// not a zero it never measured, and no `(k of n)` for a denominator
    /// of zero. The calls it made are counted all the same: three crossings
    /// were asked and three replies came back truncated.
    #[test]
    fn an_all_skipped_judge_run_renders_n_a_not_a_number() {
        let out = render_codebase(&all_skipped_judged_run());
        assert!(
            out.contains("equiv n/a (0 judged of 3; 0 undecided)"),
            "{out}"
        );
        assert!(out.contains("swap consistency n/a\n"), "{out}");
        assert!(
            out.contains(
                "             judge: 0 identical, 3 called, 0 undecided, 3 skipped; 2.1 s \
                 median per verdict\n"
            ),
            "a reply nobody could parse still cost a request: {out}"
        );
    }

    /// The call count is "was a request sent", not "did an answer come back".
    #[test]
    fn a_crossing_settled_before_the_wire_is_the_only_uncalled_one() {
        let row = |decided_by, judge_secs| JudgeRow {
            equivalent: None,
            gold_first: None,
            prediction_first: None,
            decided_by,
            skipped: None,
            judge_secs,
        };
        assert!(
            !super::was_called(&row(DecidedBy::Identical, 0.0)),
            "an identical pair is never asked"
        );
        assert!(
            !super::was_called(&row(DecidedBy::Skipped, 0.0)),
            "a `did not compile` skip is decided before the wire"
        );
        assert!(
            super::was_called(&row(DecidedBy::Skipped, 2.1)),
            "an off-schema reply is a request that was sent"
        );
        assert!(super::was_called(&row(DecidedBy::SwapAgreement, 2.1)));
        assert!(super::was_called(&row(DecidedBy::SwapDisagreement, 2.1)));
    }
}
