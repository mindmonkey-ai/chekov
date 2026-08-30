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
use crate::core::bench::stamp::{Stamp, mismatch_error};
use crate::core::bench::sweep::curve_note;
use crate::core::stats;
use crate::error::ChekovError;

/// What this chekov writes and reads.
pub const SCHEMA_VERSION: u32 = 1;

pub use crate::core::bench::runner::Transport;

/// The suites whose rows are graded per case.
const AGENTIC: [&str; 3] = ["tool_emit", "grammar_gap", "instruction"];

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
const fn door_tag(transport: Transport) -> &'static str {
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

fn is_unavailable(row: &TaskRow) -> bool {
    row.grade.as_ref().is_some_and(|g| g.unavailable)
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

/// The codebase block: counts and labels, then one line per tier group
/// with the mean of every tier that has a value.
///
/// The header says `engine window ≤ n_batch` because llama.cpp's `/infill`
/// caps the prefix at ~¾·`n_batch` tokens and the suffix at ~¼·`n_batch`:
/// chekov sends the whole file and grades over the whole file, but a long
/// file reaches the model only in part. Truncating here instead would make
/// the tiers score a different question than the one the spec asks.
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
    let count = |tier: TaskTier| {
        kept.iter()
            .filter(|r| r.codebase.as_ref().is_some_and(|c| c.tier == tier))
            .count()
    };
    let mut out = format!(
        "codebase     {} tasks from {} files ({} in_file, {} function_body) — {}; \
         context: same-file (engine window ≤ n_batch){}{}\n",
        kept.len(),
        distinct_files(&kept),
        count(TaskTier::InFile),
        count(TaskTier::FunctionBody),
        crate::core::bench::codebase::MASK_LABEL,
        elided_note(&kept),
        excluded_note(excluded),
    );
    for tier in [TaskTier::InFile, TaskTier::FunctionBody] {
        out.push_str(&tier_line(&kept, tier));
    }
    out.push_str("             tiers 6-7 skipped: slice B (--allow-exec)\n");
    out
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

fn tier_line(rows: &[&TaskRow], tier: TaskTier) -> String {
    let group: Vec<&CodebaseRow> = rows
        .iter()
        .filter_map(|r| r.codebase.as_ref())
        .filter(|c| c.tier == tier)
        .collect();
    if group.is_empty() {
        return String::new();
    }
    let mut cells = Vec::new();
    for t in [Tier::Exact, Tier::EditSim, Tier::IdentF1, Tier::Parse] {
        if let Some(mean) = tier_mean(&group, t) {
            cells.push(format!("{} {mean:.2}", t.label()));
        }
    }
    cells.push(symbols_cell(&group));
    format!(
        "             {:<14} {}   (n={})\n",
        tier.label(),
        cells.join("   "),
        group.len()
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
fn recompute(c: &CodebaseRow, tier: Tier) -> Score {
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
    let (kept, excluded) = measured(&rows);
    if kept.is_empty() {
        return Some(format!(
            "tool_emit    {label}N/A — nothing was measured ({})\n",
            unavailable_reason(&rows)
        ));
    }
    Some(format!(
        "tool_emit    {label}{}/{}{}\n",
        passed(&kept),
        kept.len(),
        excluded_note(excluded)
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
    let (kept, excluded) = measured(&forced);
    if kept.is_empty() {
        return Some(format!(
            "grammar_gap  N/A — the forced pass could not run ({}); \
             no gap is reported because none was measured\n",
            unavailable_reason(&forced)
        ));
    }
    // "The same cases" means the ones actually forced — an unavailable case
    // has no forced result to compare against, so including its
    // unconstrained result on one side of the gap invents the difference.
    let base_ids: Vec<&str> = kept
        .iter()
        .filter_map(|r| r.task_id.strip_prefix("gg-"))
        .collect();
    // The forced pass is buffered, so it pairs against buffered rows only —
    // adding the streamed crossings to one side would invent a gap.
    let unconstrained: Vec<&TaskRow> = rows_via(log, "tool_emit", Transport::Buffered)
        .filter(|r| base_ids.contains(&r.task_id.as_str()))
        .collect();
    let (paired, _) = measured(&unconstrained);
    if paired.is_empty() {
        return Some(format!(
            "grammar_gap  {}/{} forced — no unconstrained result to compare against{}\n",
            passed(&kept),
            kept.len(),
            excluded_note(excluded)
        ));
    }
    let pct = |pass: usize, total: usize| i64::try_from(pass * 100 / total.max(1)).unwrap_or(0);
    let gap = pct(passed(&kept), kept.len()) - pct(passed(&paired), paired.len());
    Some(format!(
        "grammar_gap  {}/{} forced — unconstrained on the same cases {}/{} (gap {gap:+}%){}{}\n",
        passed(&kept),
        kept.len(),
        passed(&paired),
        paired.len(),
        excluded_note(excluded),
        forced_mode_note(log),
    ))
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
    let (kept, excluded) = measured(&rows);
    if kept.is_empty() {
        return Some(format!(
            "instruction  {label}N/A — nothing was measured ({})\n",
            unavailable_reason(&rows)
        ));
    }
    let strict = passed(&kept);
    let loose = kept
        .iter()
        .filter(|r| {
            r.grade.as_ref().is_some_and(|g| {
                g.pass
                    || g.reason
                        .as_deref()
                        .is_some_and(|s| s.contains("loose:pass"))
            })
        })
        .count();
    Some(format!(
        "instruction  {label}strict {strict}/{}, loose {loose}/{} (chattiness gap {}){}\n",
        kept.len(),
        kept.len(),
        loose.saturating_sub(strict),
        excluded_note(excluded)
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
        CodebaseRow, GradeRow, Measure, RunHead, RunLog, RunWriter, Task, TaskRow, Transport,
        render_run,
    };
    use crate::core::bench::codebase::{Excluded, TaskTier};
    use crate::core::bench::stamp::Stamp;
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
            seed: 42,
            temperature_milli: 0,
            chekov_version: "0.1.0".into(),
            prompt_set_hash: "e19a".into(),
            corpus_id: "throughput-v1".into(),
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
            }),
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
            "codebase     3 tasks from 1 files (2 in_file, 1 function_body) — \
             boundary-scanned (not AST); context: same-file (engine window ≤ n_batch)",
            "in_file        exact 0.50   edit_sim",
            "symbols 1.00 (scored at run time)   (n=2)",
            "function_body  ident_f1",
            "tiers 6-7 skipped: slice B (--allow-exec)",
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
                "context: same-file (engine window ≤ n_batch); tests elided: 21 lines in 2 files"
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
            rendered.contains("codebase     2 tasks from 1 files (2 in_file, 0 function_body)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("(engine window ≤ n_batch) (1 unavailable, excluded)"),
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
}
