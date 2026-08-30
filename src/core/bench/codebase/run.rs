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

/// The `arm` a cross-file row records.
pub const NO_EXTRA: &str = "no_extra";
/// The `arm` a cross-file row records when the defining file went up with it.
pub const WITH_EXTRA: &str = "extra";

/// One crossing of one task: the id it is recorded under, the arm it names,
/// and whether the defining file goes up with it.
struct Arm {
    id: String,
    label: Option<&'static str>,
    with_extra: bool,
}

/// The arms one task is crossed on: one for the same-file tiers, two for
/// `cross_file_first` — without the defining file, then with it, in that
/// fixed order (§5). Distinct ids mean `--resume` skips per arm.
fn arms(task: &CodebaseTask) -> Vec<Arm> {
    if task.tier != super::TaskTier::CrossFileFirst {
        return vec![Arm {
            id: task.id.clone(),
            label: None,
            with_extra: false,
        }];
    }
    vec![
        Arm {
            id: task.id.clone(),
            label: Some(NO_EXTRA),
            with_extra: false,
        },
        Arm {
            id: format!("{}{}", task.id, super::ARM_EXTRA_SUFFIX),
            label: Some(WITH_EXTRA),
            with_extra: true,
        },
    ]
}

/// One arm's crossing inputs (§4 — keeps `infill_or_latch` at 3 parameters).
struct Crossing<'a> {
    task: &'a CodebaseTask,
    with_extra: bool,
}

/// What this arm sends beside the file: the defining file on the "extra"
/// arm, nothing on the `no_extra` arm or on a same-file tier.
fn extra_chunk<'a>(crossing: &'a Crossing) -> Option<runner::ExtraChunk<'a>> {
    if !crossing.with_extra {
        return None;
    }
    let extra = crossing.task.extra.as_ref()?;
    Some(runner::ExtraChunk {
        filename: &extra.path,
        text: &crossing.task.extra_text,
    })
}

/// The gold's line count as the wire bounds `n_predict` by — floored at one,
/// so an empty gold still buys the crossing room to answer.
fn gold_lines(task: &CodebaseTask) -> usize {
    task.gold.lines().count().max(1)
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
    crossing: &Crossing,
    latch: &mut Option<String>,
) -> Result<ProbeArtifact, Unavailable> {
    use crate::core::bench::runner::{InfillOutcome, InfillTask, cross_infill};
    if let Some(reason) = latch {
        return Err(Unavailable::unsupported(reason.clone()));
    }
    let task = crossing.task;
    let infill_task = InfillTask {
        prefix: &task.prefix,
        suffix: &task.suffix,
        gold_lines: gold_lines(task),
        extra: extra_chunk(crossing),
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

/// What one arm's outcome needs to become a row (§4 — keeps
/// `record_codebase_task` at 3 params).
struct Recorded<'a> {
    outcome: Result<ProbeArtifact, Unavailable>,
    symbols: &'a ladder::Symbols,
    arm: &'a Arm,
    /// Tiers 6-7, when the run was allowed to build. `None` when it was not,
    /// and on a crossing nobody answered — there was no fill to splice, and a
    /// skip there would claim a question that was never asked.
    exec: Option<store::ExecRow>,
}

/// Tier 5 for one prediction, or `None` when the ladder skips it — never a
/// zero standing in for "not scored".
fn symbols_tier_score(scored: &ladder::Scored) -> Option<f64> {
    ladder::score_all(scored)
        .into_iter()
        .find_map(|(tier, score)| match (tier, score) {
            (ladder::Tier::Symbols, ladder::Score::Value(v)) => Some(v),
            _ => None,
        })
}

/// What tier 5 is scored against, once `Recorded` has given up its outcome.
struct Scoring<'a> {
    symbols: &'a ladder::Symbols,
    arm: &'a Arm,
}

/// Tier 5 for an answered arm, and `None` for one nobody answered — a task
/// with no prediction has no score, and a stored `0.0` would read as one.
fn tier_five(task: &CodebaseTask, parts: &RowParts, scoring: &Scoring) -> Option<f64> {
    if parts.grade.is_some() {
        return None;
    }
    symbols_tier_score(&scored_for(task, &parts.prediction, scoring))
}

/// Tier 5's inputs for this arm: the with-extra arm was shown G, so G's
/// names exist for it; the without arm was not, and is scored without them.
fn scored_for<'a>(
    task: &'a CodebaseTask,
    prediction: &'a str,
    scoring: &Scoring<'a>,
) -> ladder::Scored<'a> {
    ladder::Scored {
        task,
        prediction,
        symbols: scoring.symbols,
        extra: if scoring.arm.with_extra {
            &task.extra_text
        } else {
            ""
        },
    }
}

/// This arm's `excluded.cross_file`: the task's own sentence on the arm that
/// was shown the defining file, and the withheld form on the one that was
/// not. A same-file tier keeps what the task recorded.
fn cross_file_line(task: &CodebaseTask, arm: &Arm) -> String {
    match task.extra.as_ref() {
        Some(extra) if !arm.with_extra => {
            super::crossfile::cross_file_withheld_note(extra, task.excluded.cross_file_withheld)
        }
        _ => task.excluded.cross_file.clone(),
    }
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
    let Recorded {
        outcome,
        symbols,
        arm,
        exec,
    } = recorded;
    let parts = row_parts(outcome);
    let symbols_score = tier_five(task, &parts, &Scoring { symbols, arm });
    let excluded = super::Excluded {
        cross_file: cross_file_line(task, arm),
        ..task.excluded.clone()
    };
    sink.writer.append(store::Task {
        suite: "codebase".into(),
        task_id: arm.id.clone(),
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
            excluded,
            symbols_score,
            unsupported: parts.unsupported,
            arm: arm.label.map(str::to_owned),
            extra: arm.with_extra.then(|| task.extra.clone()).flatten(),
            also_first_uses: task.also_first_uses.clone(),
            name: task.name.clone(),
            n_predict: Some(runner::n_predict_for(gold_lines(task))),
            exec,
        }),
        judge: None,
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

/// What the exec tiers cost so far, so the run can say what the rest will.
struct ExecTiming {
    cold: Option<f64>,
    later: Vec<f64>,
    announced: bool,
}

impl ExecTiming {
    const fn new() -> Self {
        Self {
            cold: None,
            later: Vec::new(),
            announced: false,
        }
    }

    /// One check's wall clock, and — the second time round — the line the
    /// spec's live estimate asks for. Printed once: the first check pays for
    /// the whole target directory, and every check after it is incremental,
    /// so one number cannot describe both.
    fn record(&mut self, secs: f64) {
        let Some(cold) = self.cold else {
            self.cold = Some(secs);
            return;
        };
        self.later.push(secs);
        if !self.announced {
            self.announced = true;
            eprintln!(
                "chekov bench: exec cold check {cold:.0} s, ~{secs:.0} s per crossing thereafter"
            );
        }
    }
}

/// What `exec_row` needs beyond the prepared set and the task (§4).
struct ExecInput<'a> {
    /// The raw prediction, or `None` when the crossing was never answered.
    prediction: Option<&'a str>,
    timing: &'a std::cell::RefCell<ExecTiming>,
}

/// Tiers 6-7 for one answered crossing, or `None` when they did not apply.
fn exec_row(
    prepared: &super::Prepared,
    task: &CodebaseTask,
    parts: &ExecInput,
) -> Result<Option<store::ExecRow>, ChekovError> {
    let Some(prediction) = parts.prediction else {
        return Ok(None);
    };
    match &prepared.exec {
        super::exec::Exec::Off => Ok(None),
        super::exec::Exec::Unavailable(reason) => Ok(Some(store::ExecRow::skipped(reason))),
        super::exec::Exec::Ready(env) => {
            let fill = ladder::trimmed_to_gold(&task.gold, prediction);
            let row = super::exec::exec_crossing(env, task, &fill)?;
            parts.timing.borrow_mut().record(row.check_secs);
            Ok(Some(row))
        }
    }
}

/// Every sampled task through `/infill`, recorded with its raw prediction.
///
/// A cross-file task is crossed twice — without the defining file, then with
/// it — and each arm is its own row and its own `--resume` key. A model
/// without FIM records every arm unavailable with the reason and stops
/// firing: a capability, never a zero.
pub fn run_codebase(
    sink: &mut Sink,
    wire: &runner::ProbeWire,
    prepared: &super::Prepared,
) -> Result<(), ChekovError> {
    let mut unsupported: Option<String> = None;
    let timing = std::cell::RefCell::new(ExecTiming::new());
    for task in &prepared.tasks {
        for arm in arms(task) {
            if sink.is_done(&TaskKey::buffered("codebase", &arm.id)) {
                continue;
            }
            let crossing = Crossing {
                task,
                with_extra: arm.with_extra,
            };
            let outcome = infill_or_latch(wire, &crossing, &mut unsupported);
            let prediction = outcome.as_ref().ok().map(|a| a.anthropic_body.clone());
            let exec = exec_row(
                prepared,
                task,
                &ExecInput {
                    prediction: prediction.as_deref(),
                    timing: &timing,
                },
            )?;
            record_codebase_task(
                sink,
                task,
                Recorded {
                    outcome,
                    symbols: &prepared.symbols,
                    arm: &arm,
                    exec,
                },
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use crate::core::bench::codebase::ladder::Symbols;
    use crate::core::bench::codebase::{
        CodebaseTask, Counts, Excluded, ExtraFile, Prepared, TaskTier,
    };
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
        bodies: RefCell<Vec<String>>,
    }

    impl HttpClient for ScriptedInfill {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            unreachable!("the codebase run only POSTs")
        }

        fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError> {
            *self.posts.borrow_mut() += 1;
            self.bodies.borrow_mut().push(req.body.clone());
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
                allow_exec: false,
                cargo_version: None,
                exec_target: "none".into(),
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
            byte_range: 9..19,
            gold: "let a = 1;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "n/a: same-file".into(),
                cfg_test_lines: 11,
                cross_file_withheld: 0,
            },
            name: None,
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
            exec: crate::core::bench::codebase::exec::Exec::Off,
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

    /// One cross-file task with a defining file to send on the second arm.
    fn cross_task() -> CodebaseTask {
        CodebaseTask {
            id: "cross_file_first-abc123-L2".into(),
            tier: TaskTier::CrossFileFirst,
            file: "src/user.rs".into(),
            line: 2,
            byte_range: 15..32,
            gold: "let a = build(1);".into(),
            prefix: "pub fn run() {\n".into(),
            suffix: "\n    a\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "sent src/defs.rs (0.1 KiB); withheld 0 (contain the answer)".into(),
                cfg_test_lines: 0,
                cross_file_withheld: 0,
            },
            name: Some("build".into()),
            also_first_uses: vec!["Widget".into()],
            extra: Some(ExtraFile {
                path: "src/defs.rs".into(),
                bytes: 34,
                truncated: false,
            }),
            extra_text: "pub fn build(n: u32) -> u32 { n + 1 }\n".into(),
        }
    }

    fn prepared_cross() -> Prepared {
        Prepared {
            head: "4818813deeaa11112222333344445555666677".into(),
            set_hash: "abcdef123456".into(),
            tasks: vec![cross_task()],
            shortfall: vec![],
            symbols: Symbols::default(),
            cfg_test_lines: 0,
            cfg_test_files: 0,
            counts: Counts {
                in_file: 0,
                function_body: 0,
                cross_file_first: 1,
            },
            exec: crate::core::bench::codebase::exec::Exec::Off,
        }
    }

    /// The row ledger `--resume` reads.
    type Done = (String, String, crate::core::bench::store::Transport);

    /// Drive `run_codebase` over one prepared set with a scripted upstream and
    /// a `--resume` ledger: the rows, the ask count, and the bodies sent.
    fn drive(
        name: &str,
        prepared: &Prepared,
        script: (Vec<Result<String, ChekovError>>, Vec<Done>),
    ) -> (Vec<TaskRow>, usize, Vec<serde_json::Value>) {
        let (replies, done) = script;
        let http = ScriptedInfill {
            replies: RefCell::new(replies),
            posts: RefCell::new(0),
            bodies: RefCell::new(Vec::new()),
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
                done: &done,
            };
            super::run_codebase(&mut sink, &wire, prepared).expect("the run completes");
        }
        let log = RunLog::load(writer.dir()).expect("load");
        let bodies = http
            .bodies
            .into_inner()
            .iter()
            .map(|b| serde_json::from_str(b).expect("json"))
            .collect();
        (log.rows, http.posts.into_inner(), bodies)
    }

    fn refused(reason: &str) -> Result<String, ChekovError> {
        Err(ChekovError::UpstreamRefused {
            url: "http://fake/infill".into(),
            status: 400,
            reason: reason.to_owned(),
        })
    }

    /// The same shell-script cargo the exec tests use.
    fn fake_cargo(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).expect("dir");
        let path = dir.join("fake-cargo");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// A prepared set whose exec half is a real worktree over a one-crate
    /// repo, and a fake cargo that answers `script`.
    fn with_exec(name: &str, script: &str) -> Prepared {
        use crate::core::bench::codebase::exec;
        let dir = std::env::temp_dir().join("chekov-test-run-exec").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("src");
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n",
        )
        .expect("manifest");
        std::fs::write(repo.join("src/a.rs"), "fn f() {\nlet a = 1;\n}\n").expect("a.rs");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["add", "-A"],
            vec!["commit", "-qm", "fixture"],
        ] {
            crate::core::bench::codebase::tree::git(&repo, &args, "fixture").expect("git");
        }
        let worktree = crate::core::bench::codebase::tree::Worktree::add(&repo, &dir.join("tree"))
            .expect("worktree");
        let mut prepared = prepared_pair();
        prepared.tasks.truncate(1);
        prepared.tasks[0].file = "src/a.rs".into();
        prepared.tasks[0].byte_range = 9..19;
        prepared.counts.in_file = 1;
        prepared.exec = exec::Exec::Ready(exec::Env {
            worktree,
            cargo: fake_cargo(&dir, script),
            target_dir: dir.join("target"),
            cargo_version: "cargo 1.95.0 (fake)".to_owned(),
            timeouts: exec::Timeouts::DEFAULT,
        });
        prepared
    }

    #[test]
    fn an_answered_crossing_carries_its_exec_verdict_onto_the_row() {
        let prepared = with_exec("answered", "exit 0");
        let (rows, _, _) = drive("exec-answered", &prepared, (vec![Ok(infill_200())], vec![]));
        let exec = rows[0]
            .codebase
            .as_ref()
            .and_then(|c| c.exec.clone())
            .expect("the crossing recorded its exec half");
        assert_eq!(
            exec.compile,
            crate::core::bench::store::ExecScore::Value(1.0)
        );
        prepared.exec.finish().expect("cleanup");
    }

    /// A crossing nobody answered has no fill to splice, so it has no exec
    /// half at all — a `Skipped` there would claim a question was asked.
    #[test]
    fn an_unanswered_crossing_has_no_exec_half() {
        let prepared = with_exec("unanswered", "exit 0");
        let (rows, _, _) = drive(
            "exec-unanswered",
            &prepared,
            (vec![refused("the server is out of context")], vec![]),
        );
        assert!(rows[0].codebase.as_ref().is_some_and(|c| c.exec.is_none()));
        prepared.exec.finish().expect("cleanup");
    }

    /// No toolchain: every crossing records the one reason, and no cargo is
    /// ever spawned.
    #[test]
    fn an_unavailable_toolchain_skips_every_crossing_with_one_reason() {
        let mut prepared = prepared_pair();
        prepared.exec = crate::core::bench::codebase::exec::Exec::Unavailable(
            "no Rust toolchain: cargo is not runnable".to_owned(),
        );
        let (rows, _, _) = drive(
            "exec-unavailable",
            &prepared,
            (vec![Ok(infill_200()), Ok(infill_200())], vec![]),
        );
        for row in &rows {
            let exec = row
                .codebase
                .as_ref()
                .and_then(|c| c.exec.clone())
                .expect("the reason is recorded per crossing");
            assert_eq!(
                exec.compile,
                crate::core::bench::store::ExecScore::Skipped(
                    "no Rust toolchain: cargo is not runnable".to_owned()
                )
            );
            assert_eq!(exec.compile, exec.test);
        }
    }

    fn unavailable_reason(row: &TaskRow) -> String {
        let grade = row.grade.as_ref().expect("an unavailable row is graded");
        assert!(grade.unavailable, "{grade:?}");
        grade.reason.clone().unwrap_or_default()
    }

    #[test]
    fn a_model_without_infill_records_every_task_unavailable_and_asks_only_once() {
        let (rows, posts, _) = drive(
            "latch",
            &prepared_pair(),
            (
                vec![refused("infill is not supported by this model")],
                vec![],
            ),
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
        let (rows, posts, _) = drive(
            "one-bad-task",
            &prepared_pair(),
            (
                vec![refused("the server is out of context"), Ok(infill_200())],
                vec![],
            ),
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

    #[test]
    fn a_cross_file_task_crosses_twice_without_then_with_the_defining_file() {
        let (rows, posts, bodies) = drive(
            "two-arms",
            &prepared_cross(),
            (vec![Ok(infill_200()), Ok(infill_200())], vec![]),
        );
        assert_eq!(posts, 2, "two arms, two crossings");
        assert_eq!(bodies[0]["input_extra"], serde_json::json!([]));
        assert_eq!(bodies[1]["input_extra"][0]["filename"], "src/defs.rs");
        assert_eq!(
            bodies[1]["input_extra"][0]["text"],
            "pub fn build(n: u32) -> u32 { n + 1 }\n"
        );
        assert_eq!(bodies[0]["input_prefix"], bodies[1]["input_prefix"]);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].task_id, "cross_file_first-abc123-L2");
        assert_eq!(rows[1].task_id, "cross_file_first-abc123-L2+extra");
        let arm = |r: &TaskRow| r.codebase.as_ref().and_then(|c| c.arm.clone());
        assert_eq!(arm(&rows[0]).as_deref(), Some("no_extra"));
        assert_eq!(arm(&rows[1]).as_deref(), Some("extra"));
        let extra = |r: &TaskRow| r.codebase.as_ref().and_then(|c| c.extra.clone());
        assert!(extra(&rows[0]).is_none(), "the no_extra arm sent nothing");
        assert_eq!(
            extra(&rows[1]).map(|e| e.path),
            Some("src/defs.rs".to_owned())
        );
        assert_eq!(
            rows[1].codebase.as_ref().map(|c| c.also_first_uses.clone()),
            Some(vec!["Widget".to_owned()])
        );
    }

    /// Each arm's row has to be true read on its own: the one that was shown
    /// the file says "sent", the one that was not says "withheld".
    #[test]
    fn each_arm_records_what_that_arm_was_shown_and_never_the_other_arms_context() {
        let (rows, _, _) = drive(
            "arm-context",
            &prepared_cross(),
            (vec![Ok(infill_200()), Ok(infill_200())], vec![]),
        );
        let note = |r: &TaskRow| {
            r.codebase
                .as_ref()
                .map(|c| c.excluded.cross_file.clone())
                .expect("a codebase row")
        };
        assert_eq!(
            note(&rows[0]),
            "defining file src/defs.rs (0.0 KiB) withheld from this arm; \
             withheld 0 (contain the answer)"
        );
        assert_eq!(
            note(&rows[1]),
            "sent src/defs.rs (0.1 KiB); withheld 0 (contain the answer)"
        );
    }

    /// The other half of the arm split: a task whose defining file went
    /// missing carries no `extra`, so neither arm claims one was sent.
    #[test]
    fn a_same_file_tier_keeps_the_note_its_task_recorded() {
        let (rows, _, _) = drive(
            "same-file-note",
            &prepared_pair(),
            (vec![Ok(infill_200()), Ok(infill_200())], vec![]),
        );
        for row in &rows {
            let codebase = row.codebase.as_ref().expect("a codebase row");
            assert_eq!(codebase.excluded.cross_file, "n/a: same-file");
        }
    }

    /// The same task twice over, so one of them can lose an arm.
    fn prepared_two_cross() -> Prepared {
        let mut second = cross_task();
        second.id = "cross_file_first-abc123-L9".into();
        second.line = 9;
        let mut prepared = prepared_cross();
        prepared.tasks.push(second);
        prepared.counts.cross_file_first = 2;
        prepared
    }

    /// An outage is that crossing's alone — it does not latch, the other arm
    /// of the same task still goes up, and the lift says how many tasks it
    /// actually ran over.
    #[test]
    fn an_outage_on_the_without_arm_leaves_the_with_arm_crossing_and_narrows_the_lift() {
        let (rows, posts, bodies) = drive(
            "arm-outage",
            &prepared_two_cross(),
            (
                vec![
                    refused("internal error"),
                    Ok(infill_200()),
                    Ok(infill_200()),
                    Ok(infill_200()),
                ],
                vec![],
            ),
        );
        assert_eq!(
            posts, 4,
            "an outage is not a capability, so nothing latches"
        );
        assert!(
            unavailable_reason(&rows[0]).contains("internal error"),
            "{:?}",
            rows[0].grade
        );
        assert!(
            rows[0]
                .codebase
                .as_ref()
                .is_some_and(|c| !c.unsupported && c.arm.as_deref() == Some(super::NO_EXTRA)),
            "{:?}",
            rows[0].codebase
        );
        assert_eq!(bodies[1]["input_extra"][0]["filename"], "src/defs.rs");
        let block = crate::core::bench::store::render_codebase(&RunLog {
            head: run_head(),
            rows,
        });
        assert!(
            block.contains("(n=1 of 2; 1 files sent, 0.0 KiB, 0 truncated; 0 withheld)"),
            "{block}"
        );
    }

    #[test]
    fn resume_skips_one_arm_and_still_owes_the_other() {
        let done = vec![(
            "codebase".to_owned(),
            "cross_file_first-abc123-L2".to_owned(),
            crate::core::bench::store::Transport::Buffered,
        )];
        let (rows, posts, bodies) = drive(
            "resume-arm",
            &prepared_cross(),
            (vec![Ok(infill_200())], done),
        );
        assert_eq!(posts, 1, "only the extra arm was still owed");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task_id, "cross_file_first-abc123-L2+extra");
        assert_eq!(bodies[0]["input_extra"][0]["filename"], "src/defs.rs");
    }
}
