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

use crate::core::bench::stamp::{Stamp, mismatch_error};
use crate::core::bench::sweep::curve_note;
use crate::core::stats;
use crate::error::ChekovError;

/// What this chekov writes and reads.
pub const SCHEMA_VERSION: u32 = 1;

/// Everything `stamp.json` records about a run, once.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunHead {
    pub model: String,
    /// Human-readable beside the hashed `machine_id`.
    pub machine_brand: Option<String>,
    /// The exact argv the measured server was launched with (flag hygiene).
    pub launch_args: Vec<String>,
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
    pub measure: Measure,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<GradeRow>,
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
}

/// One task to append: its identity plus what was measured.
pub struct Task {
    pub suite: String,
    pub task_id: String,
    pub measure: Measure,
    pub grade: Option<GradeRow>,
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
            measure: task.measure,
            grade: task.grade,
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

    /// Whether a task is already recorded (the `--resume` skip test).
    #[must_use]
    pub fn is_done(&self, suite: &str, task_id: &str) -> bool {
        self.rows
            .iter()
            .any(|r| r.suite == suite && r.task_id == task_id)
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
    out.push_str("depth  prompt_n  decode tok/s (median [p10..p90])  prefill tok/s  n\n");
    let throughput: Vec<&TaskRow> = log
        .rows
        .iter()
        .filter(|r| r.suite == "throughput")
        .collect();
    for row in &throughput {
        out.push_str(&depth_line(row));
    }
    let summarisable = throughput
        .iter()
        .filter(|r| stats::summarize(&r.measure.decode_samples).is_some())
        .count();
    if let Some(note) = curve_note(summarisable) {
        out.push_str(&note);
        out.push('\n');
    }
    let probes: String = log
        .rows
        .iter()
        .filter(|r| r.suite == "fixture")
        .map(probe_line)
        .collect();
    out.push_str(&probes);
    out.push_str(&suite_summaries(log));
    out
}

/// Per-suite summary lines for the agentic suites: counts always printed,
/// failures listed individually, passes counted — no silent caps.
fn suite_summaries(log: &RunLog) -> String {
    let mut out = String::new();
    let failures: String = log
        .rows
        .iter()
        .filter(|r| ["tool_emit", "grammar_gap", "instruction"].contains(&r.suite.as_str()))
        .filter(|r| r.grade.as_ref().is_some_and(|g| !g.pass))
        .map(agentic_fail_line)
        .collect();
    out.push_str(&failures);
    if let Some(line) = tool_emit_line(log) {
        out.push_str(&line);
    }
    if let Some(line) = grammar_gap_line(log) {
        out.push_str(&line);
    }
    if let Some(line) = instruction_line(log) {
        out.push_str(&line);
    }
    out
}

fn agentic_fail_line(row: &TaskRow) -> String {
    let reason = row
        .grade
        .as_ref()
        .and_then(|g| g.reason.as_deref())
        .unwrap_or("");
    format!("{} FAIL {}  {reason}\n", row.suite, row.task_id)
}

fn rows_of<'a>(log: &'a RunLog, suite: &'a str) -> impl Iterator<Item = &'a TaskRow> {
    log.rows.iter().filter(move |r| r.suite == suite)
}

fn passed(rows: &[&TaskRow]) -> usize {
    rows.iter()
        .filter(|r| r.grade.as_ref().is_some_and(|g| g.pass))
        .count()
}

fn tool_emit_line(log: &RunLog) -> Option<String> {
    let rows: Vec<&TaskRow> = rows_of(log, "tool_emit").collect();
    (!rows.is_empty()).then(|| format!("tool_emit    {}/{}\n", passed(&rows), rows.len()))
}

/// The §7.2 anti-self-deception line: forced vs unconstrained ON THE SAME
/// CASES — a large gap means "works only with a babysitter".
fn grammar_gap_line(log: &RunLog) -> Option<String> {
    let forced: Vec<&TaskRow> = rows_of(log, "grammar_gap").collect();
    if forced.is_empty() {
        return None;
    }
    let base_ids: Vec<&str> = forced
        .iter()
        .filter_map(|r| r.task_id.strip_prefix("gg-"))
        .collect();
    let unconstrained: Vec<&TaskRow> = rows_of(log, "tool_emit")
        .filter(|r| base_ids.contains(&r.task_id.as_str()))
        .collect();
    let pct = |pass: usize, total: usize| i64::try_from(pass * 100 / total.max(1)).unwrap_or(0);
    let gap = pct(passed(&forced), forced.len()) - pct(passed(&unconstrained), unconstrained.len());
    Some(format!(
        "grammar_gap  {}/{} forced — unconstrained on the same cases {}/{} (gap {gap:+}%)\n",
        passed(&forced),
        forced.len(),
        passed(&unconstrained),
        unconstrained.len(),
    ))
}

fn instruction_line(log: &RunLog) -> Option<String> {
    let rows: Vec<&TaskRow> = rows_of(log, "instruction").collect();
    if rows.is_empty() {
        return None;
    }
    let strict = passed(&rows);
    let loose = rows
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
        "instruction  strict {strict}/{}, loose {loose}/{} (chattiness gap {})\n",
        rows.len(),
        rows.len(),
        loose.saturating_sub(strict),
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

    use super::{GradeRow, Measure, RunHead, RunLog, RunWriter, Task, render_run};
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
                    }),
                })
                .expect("append");
        }
        writer
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
            })
            .expect("append");
        drop(writer);
        let (mut resumed, log) = RunWriter::resume(&eval, "r2-model", &head()).expect("resume");
        assert!(log.is_done("throughput", "depth-1024"));
        assert!(!log.is_done("throughput", "depth-4096"));
        resumed
            .append(Task {
                suite: "throughput".into(),
                task_id: "depth-4096".into(),
                measure: measure(&[15.0, 16.0]),
                grade: None,
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
            })
            .expect("append");
        writer
            .append(Task {
                suite: "fixture".into(),
                task_id: "greeting".into(),
                measure: measure(&[20.0, 20.0]),
                grade: Some(GradeRow {
                    pass: false,
                    reason: Some("missing expected substring \"hello\"".into()),
                }),
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
