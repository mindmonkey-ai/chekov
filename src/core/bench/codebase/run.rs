//! The `--codebase` run loop: every sampled task through `/infill`, recorded
//! with its raw prediction.
//!
//! It lives beside the task generation rather than in the command layer: the
//! command owns the run directory and the lifecycle, this module owns what a
//! codebase task IS on the wire (spec §10 — the slice-A "run cluster lives in
//! the command layer" item, retired).

use crate::core::bench::codebase::{CodebaseTask, MASK_LABEL, ladder};
use crate::core::bench::runner::{self, ProbeArtifact};
use crate::core::bench::store::{self, TaskKey};
use crate::error::ChekovError;

/// Where rows land and what a resumed run already holds. The command layer
/// owns the writer; this module owns the loop.
pub struct Sink<'a> {
    pub writer: &'a mut store::RunWriter,
    pub done: &'a [(String, String, store::Transport)],
}

impl Sink<'_> {
    /// The `--resume` skip test: the same task through the same door.
    fn is_done(&self, key: &TaskKey) -> bool {
        self.done.iter().any(|(suite, task_id, transport)| {
            suite == key.suite && task_id == key.task_id && *transport == key.transport
        })
    }
}

/// The zeroed `Measure` a task that never ran records — nothing was timed,
/// so nothing is invented.
pub(crate) const fn empty_measure() -> store::Measure {
    store::Measure {
        prompt_n: 0,
        decode_samples: vec![],
        prefill_samples: vec![],
        warmup_dropped: 0,
        cache_n: 0,
    }
}

pub(crate) fn probe_measure(timings: &runner::Timings) -> store::Measure {
    store::Measure {
        prompt_n: timings.prompt_n,
        decode_samples: vec![timings.predicted_per_second],
        prefill_samples: vec![timings.prompt_per_second],
        warmup_dropped: 0,
        cache_n: timings.cache_n,
    }
}

/// One codebase task through `/infill`, or the reason it could not be
/// measured.
///
/// Only a missing FIM capability latches — it is a property of the model, so
/// asking the next task would waste the whole run. Every other failure (a
/// timeout, a 5xx, a reply without timings) is THAT task's alone: it records
/// unavailable and the run goes on, exactly as `failed_probe` treats the
/// agentic crossings. Aborting the run on one bad crossing would throw away
/// the tasks that did answer.
fn infill_or_latch(
    wire: &runner::ProbeWire,
    task: &CodebaseTask,
    latch: &mut Option<String>,
) -> Result<ProbeArtifact, Unavailable> {
    use crate::core::bench::runner::{InfillOutcome, InfillTask, cross_infill};
    if let Some(reason) = latch {
        return Err(Unavailable::unsupported(reason.clone()));
    }
    let infill_task = InfillTask {
        prefix: &task.prefix,
        suffix: &task.suffix,
        gold_lines: task.gold.lines().count().max(1),
        extra: None,
    };
    match cross_infill(wire, &infill_task) {
        Ok(InfillOutcome::Answered(artifact)) => Ok(artifact),
        Ok(InfillOutcome::Unsupported(reason)) => {
            eprintln!(
                "chekov bench: infill unsupported by this model — codebase is N/A ({reason})"
            );
            *latch = Some(reason.clone());
            Err(Unavailable::unsupported(reason))
        }
        Err(e) => {
            let reason = e.to_string();
            eprintln!(
                "chekov bench: codebase task {} unavailable: {reason}",
                task.id
            );
            Err(Unavailable::outage(reason))
        }
    }
}

/// Why a codebase task has no answer.
///
/// The two cases are told apart HERE, where the engine's own outcome says
/// which it is — never later by reading the reason. A refusal's words carry
/// the URL it was refused at, and that URL ends in `/infill`, so any
/// after-the-fact substring test reports a dead server as a model that
/// cannot infill.
struct Unavailable {
    reason: String,
    unsupported: bool,
}

impl Unavailable {
    /// The engine says this model has no FIM capability at all.
    const fn unsupported(reason: String) -> Self {
        Self {
            reason,
            unsupported: true,
        }
    }

    /// Everything else: a timeout, a 5xx, a reply chekov could not read.
    const fn outage(reason: String) -> Self {
        Self {
            reason,
            unsupported: false,
        }
    }
}

/// What one codebase task's outcome needs to become a row (§4 — keeps
/// `record_codebase_task` at 3 params).
struct Recorded<'a> {
    outcome: Result<ProbeArtifact, Unavailable>,
    symbols: &'a ladder::Symbols,
}

/// Tier 5 for one prediction against the worktree's symbol set, or `None`
/// when the ladder skips it — never a zero standing in for "not scored".
fn symbols_tier_score(
    task: &CodebaseTask,
    prediction: &str,
    symbols: &ladder::Symbols,
) -> Option<f64> {
    use crate::core::bench::codebase::ladder::{Score, Scored, Tier, score_all};
    score_all(&Scored {
        task,
        prediction,
        symbols,
    })
    .into_iter()
    .find_map(|(tier, score)| match (tier, score) {
        (Tier::Symbols, Score::Value(v)) => Some(v),
        _ => None,
    })
}

/// Assemble and append one codebase row: an answered task's raw prediction
/// with tier 5 scored against the worktree's symbol set, or an unavailable
/// one's reason with no tier-5 score at all — a task nobody answered has no
/// score, and a stored `0.0` would read as one.
fn record_codebase_task(
    sink: &mut Sink,
    task: &CodebaseTask,
    recorded: Recorded,
) -> Result<(), ChekovError> {
    let parts = row_parts(recorded.outcome);
    let symbols_score = parts
        .grade
        .is_none()
        .then(|| symbols_tier_score(task, &parts.prediction, recorded.symbols))
        .flatten();
    sink.writer.append(store::Task {
        suite: "codebase".into(),
        task_id: task.id.clone(),
        measure: parts.measure,
        grade: parts.grade,
        transport: store::Transport::Buffered,
        codebase: Some(store::CodebaseRow {
            tier: task.tier,
            file: task.file.clone(),
            line: task.line,
            label: MASK_LABEL.to_owned(),
            gold: task.gold.clone(),
            prediction: parts.prediction,
            prefix: task.prefix.clone(),
            suffix: task.suffix.clone(),
            excluded: task.excluded.clone(),
            symbols_score,
            unsupported: parts.unsupported,
        }),
    })
}

/// One outcome flattened into the fields a row is built from.
struct RowParts {
    measure: store::Measure,
    grade: Option<store::GradeRow>,
    prediction: String,
    unsupported: bool,
}

fn row_parts(outcome: Result<ProbeArtifact, Unavailable>) -> RowParts {
    match outcome {
        Ok(artifact) => RowParts {
            measure: probe_measure(&artifact.timings),
            grade: None,
            prediction: artifact.anthropic_body,
            unsupported: false,
        },
        Err(u) => RowParts {
            measure: empty_measure(),
            grade: Some(store::GradeRow::unavailable(u.reason)),
            prediction: String::new(),
            unsupported: u.unsupported,
        },
    }
}

/// Every sampled task through `/infill`, recorded with its raw prediction.
///
/// A model without FIM records every task unavailable with the reason and
/// stops firing — a capability, never a zero. A task that failed for any
/// other reason is unavailable on its own, and the rest still run.
pub fn run_codebase(
    sink: &mut Sink,
    wire: &runner::ProbeWire,
    prepared: &super::Prepared,
) -> Result<(), ChekovError> {
    let mut unsupported: Option<String> = None;
    for task in &prepared.tasks {
        if sink.is_done(&TaskKey::buffered("codebase", &task.id)) {
            continue;
        }
        let outcome = infill_or_latch(wire, task, &mut unsupported);
        record_codebase_task(
            sink,
            task,
            Recorded {
                outcome,
                symbols: &prepared.symbols,
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use crate::core::bench::codebase::ladder::Symbols;
    use crate::core::bench::codebase::{CodebaseTask, Counts, Excluded, Prepared, TaskTier};
    use crate::core::bench::runner;
    use crate::core::bench::store::{RunHead, RunLog, RunWriter, TaskRow};
    use crate::core::hub::{HttpClient, JsonRequest};
    use crate::core::proxy::claude::ClaudeFacade;
    use crate::core::proxy::serve::Upstream;
    use crate::error::ChekovError;

    /// An upstream that answers each POST from a script and counts the asks —
    /// the latch is only observable as a POST that never happened.
    struct ScriptedInfill {
        replies: RefCell<Vec<Result<String, ChekovError>>>,
        posts: RefCell<usize>,
    }

    impl HttpClient for ScriptedInfill {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            unreachable!("the codebase run only POSTs")
        }

        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            *self.posts.borrow_mut() += 1;
            let mut replies = self.replies.borrow_mut();
            assert!(!replies.is_empty(), "one POST more than the script allows");
            replies.remove(0)
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-run-codebase")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn run_head() -> RunHead {
        RunHead {
            model: "local-model".into(),
            machine_brand: None,
            launch_args: vec![],
            forced_reasoning_format: None,
            stamp: crate::core::bench::stamp::Stamp {
                machine_id: "8d41f0c2a917".into(),
                engine_build_commit: "dda1b0d67".into(),
                weights_revision: "fbbaed45c2f0/model.gguf".into(),
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
                prompt_set_hash: "codebase-only".into(),
                corpus_id: "codebase:4818813deeaa:abcdef123456".into(),
            },
        }
    }

    fn codebase_task_fixture(id: &str, line: usize) -> CodebaseTask {
        CodebaseTask {
            id: id.into(),
            tier: TaskTier::InFile,
            file: "src/a.rs".into(),
            line,
            gold: "let a = 1;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "n/a: same-file".into(),
                cfg_test_lines: 11,
                cross_file_withheld: 0,
            },
            also_first_uses: Vec::new(),
            extra: None,
            extra_text: String::new(),
        }
    }

    fn prepared_pair() -> Prepared {
        Prepared {
            head: "4818813deeaa11112222333344445555666677".into(),
            set_hash: "abcdef123456".into(),
            tasks: vec![
                codebase_task_fixture("in_file-abc123-L7", 7),
                codebase_task_fixture("in_file-abc123-L9", 9),
            ],
            shortfall: vec![],
            symbols: Symbols::default(),
            cfg_test_lines: 11,
            cfg_test_files: 1,
            counts: Counts {
                in_file: 2,
                function_body: 0,
                cross_file_first: 0,
            },
        }
    }

    fn infill_200() -> String {
        serde_json::json!({
            "content": "let a = 1;",
            "timings": {
                "prompt_n": 12, "prompt_per_second": 400.0,
                "predicted_n": 5, "predicted_per_second": 20.0
            }
        })
        .to_string()
    }

    /// Drive `run_codebase` over the two fixtures with a scripted upstream:
    /// the rows it wrote, and how many times the wire was actually asked.
    fn drive_codebase(
        name: &str,
        replies: Vec<Result<String, ChekovError>>,
    ) -> (Vec<TaskRow>, usize) {
        let http = ScriptedInfill {
            replies: RefCell::new(replies),
            posts: RefCell::new(0),
        };
        let facade = ClaudeFacade::new("local-model");
        let up = Upstream {
            base_url: "http://fake".into(),
            api_key: "sekrit".into(),
        };
        let wire = runner::ProbeWire {
            http: &http,
            facade: &facade,
            upstream: &up,
            pins: runner::SamplingPins { seed: 42 },
        };
        let mut writer =
            RunWriter::create(&scratch(name), "r-codebase", &run_head()).expect("create");
        {
            let mut sink = super::Sink {
                writer: &mut writer,
                done: &[],
            };
            super::run_codebase(&mut sink, &wire, &prepared_pair()).expect("the run completes");
        }
        let log = RunLog::load(writer.dir()).expect("load");
        (log.rows, http.posts.into_inner())
    }

    fn refused(reason: &str) -> Result<String, ChekovError> {
        Err(ChekovError::UpstreamRefused {
            url: "http://fake/infill".into(),
            status: 400,
            reason: reason.to_owned(),
        })
    }

    fn unavailable_reason(row: &TaskRow) -> String {
        let grade = row.grade.as_ref().expect("an unavailable row is graded");
        assert!(grade.unavailable, "{grade:?}");
        grade.reason.clone().unwrap_or_default()
    }

    #[test]
    fn a_model_without_infill_records_every_task_unavailable_and_asks_only_once() {
        let (rows, posts) = drive_codebase(
            "latch",
            vec![refused("infill is not supported by this model")],
        );
        assert_eq!(posts, 1, "the latch spares the second crossing");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            unavailable_reason(&rows[0]),
            unavailable_reason(&rows[1]),
            "both rows carry the one capability reason"
        );
        assert!(
            unavailable_reason(&rows[0]).contains("infill is not supported"),
            "{:?}",
            rows[0].grade
        );
        assert!(
            rows.iter().all(|r| r
                .codebase
                .as_ref()
                .is_some_and(|c| c.symbols_score.is_none())),
            "an unanswered task has no tier-5 score"
        );
        assert!(
            rows.iter()
                .all(|r| r.codebase.as_ref().is_some_and(|c| c.unsupported)),
            "the capability verdict is recorded at the crossing — the latched row too"
        );
    }

    #[test]
    fn a_task_that_failed_for_another_reason_is_unavailable_alone() {
        let (rows, posts) = drive_codebase(
            "one-bad-task",
            vec![refused("the server is out of context"), Ok(infill_200())],
        );
        assert_eq!(posts, 2, "a non-capability failure never latches");
        assert_eq!(rows.len(), 2);
        assert!(
            unavailable_reason(&rows[0]).contains("out of context"),
            "{:?}",
            rows[0].grade
        );
        let failed = rows[0].codebase.as_ref().expect("a codebase row");
        assert!(
            !failed.unsupported,
            "a refusal at the /infill URL is an outage, not a missing capability"
        );
        assert!(rows[1].grade.is_none(), "task 2 answered: {:?}", rows[1]);
        let answered = rows[1].codebase.as_ref().expect("a codebase row");
        assert_eq!(answered.prediction, "let a = 1;");
        assert!(answered.symbols_score.is_some(), "scored at run time");
    }
}
