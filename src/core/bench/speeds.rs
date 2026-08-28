//! Stored throughput measurements, matched to the frontier cells they were
//! taken in.
//!
//! A run applies to a cell only when model, quant, configured ctx AND machine
//! all match — a number from another machine's row, or from a different
//! context window, is not this cell's number (§5.4 rule 2). Nothing here
//! interpolates: a cell either has a run or it has none.

use std::path::{Path, PathBuf};

use crate::core::bench::store::{RunLog, TaskRow};
use crate::core::frontier::Speed;
use crate::core::stats;

/// What a cell asks for: an exact match on all four, or nothing.
#[derive(Debug, Clone, Copy)]
pub struct SpeedKey<'a> {
    pub model: &'a str,
    pub quant: &'a str,
    pub ctx: u32,
    pub machine_id: &'a str,
}

/// One run's headline decode measurement and the identity it was taken under.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasuredSpeed {
    pub model: String,
    pub quant: String,
    pub ctx: u32,
    pub machine_id: String,
    pub speed: Speed,
}

impl MeasuredSpeed {
    fn matches(&self, key: &SpeedKey) -> bool {
        self.model == key.model
            && self.quant == key.quant
            && self.ctx == key.ctx
            && self.machine_id == key.machine_id
    }
}

/// Every run that could be read, and a note for each that could not.
#[derive(Debug, Default)]
pub struct Loaded {
    pub speeds: Vec<MeasuredSpeed>,
    pub notes: Vec<String>,
}

fn depth_of(row: &TaskRow) -> Option<u32> {
    row.task_id.strip_prefix("depth-")?.parse().ok()
}

/// The deepest summarisable depth row of a run — the closest thing to an agent
/// loop with a full context, and the number a shallow probe would flatter.
#[must_use]
pub fn speed_of(log: &RunLog) -> Option<MeasuredSpeed> {
    let (depth, row, decode) = log
        .rows
        .iter()
        .filter(|r| r.suite == "throughput")
        .filter_map(|r| {
            Some((
                depth_of(r)?,
                r,
                stats::summarize(&r.measure.decode_samples)?,
            ))
        })
        .max_by_key(|(depth, _, _)| *depth)?;
    let stamp = &log.head.stamp;
    Some(MeasuredSpeed {
        model: log.head.model.clone(),
        quant: stamp.quant.clone(),
        ctx: stamp.ctx,
        machine_id: stamp.machine_id.clone(),
        speed: Speed {
            decode,
            depth,
            run_id: row.run_id.clone(),
            engine_commit: stamp.engine_build_commit.clone(),
        },
    })
}

/// The latest matching run — run ids begin with a UTC timestamp, so they sort
/// by time — and how many runs matched, so a choice among several is printed.
#[must_use]
pub fn pick<'a>(speeds: &'a [MeasuredSpeed], key: &SpeedKey) -> Option<(&'a MeasuredSpeed, usize)> {
    let matching: Vec<&MeasuredSpeed> = speeds.iter().filter(|s| s.matches(key)).collect();
    let latest = *matching.iter().max_by_key(|s| &s.speed.run_id)?;
    Some((latest, matching.len()))
}

/// Every run directory under `eval_dir`. A directory that will not load is a
/// note for the footer — not a crash, and not a silent skip.
#[must_use]
pub fn load_all(eval_dir: &Path) -> Loaded {
    let Ok(entries) = std::fs::read_dir(eval_dir) else {
        return Loaded::default();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    let mut loaded = Loaded::default();
    for dir in dirs {
        match RunLog::load(&dir) {
            Ok(log) => loaded.speeds.extend(speed_of(&log)),
            Err(err) => loaded.notes.push(format!(
                "{} could not be read: {err} — excluded",
                dir.display()
            )),
        }
    }
    loaded
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{MeasuredSpeed, SpeedKey, load_all, pick, speed_of};
    use crate::core::bench::stamp::Stamp;
    use crate::core::bench::store::{Measure, RunHead, RunLog, TaskRow};

    fn stamp(machine_id: &str, quant: &str, ctx: u32) -> Stamp {
        Stamp {
            machine_id: machine_id.into(),
            engine_build_commit: "dda1b0d67".into(),
            weights_revision: "fbbaed45c2f0/model-00001.gguf".into(),
            quant: quant.into(),
            ctx,
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

    fn depth_row(run_id: &str, depth: u32, decode: &[f64]) -> TaskRow {
        TaskRow {
            schema: crate::core::bench::store::SCHEMA_VERSION,
            run_id: run_id.into(),
            seq: 0,
            suite: "throughput".into(),
            task_id: format!("depth-{depth}"),
            measure: Measure {
                prompt_n: u64::from(depth),
                decode_samples: decode.to_vec(),
                prefill_samples: decode.to_vec(),
                warmup_dropped: 1,
                cache_n: 0,
            },
            grade: None,
        }
    }

    fn run(rows: Vec<TaskRow>) -> RunLog {
        RunLog {
            head: RunHead {
                model: "ornith-1.5-35b-a3b".into(),
                machine_brand: Some("Apple M3 Ultra".into()),
                launch_args: vec!["-m".into(), "model.gguf".into()],
                stamp: stamp("8d41f0c2a917", "Q8_0", 262_144),
            },
            rows,
        }
    }

    const SHALLOW: [f64; 5] = [70.0, 78.0, 79.0, 78.5, 78.2];
    // First sample is warmup and dropped; the remaining four have median 68.1.
    const DEEP: [f64; 5] = [60.0, 68.0, 68.1, 68.1, 68.2];

    #[test]
    fn the_deepest_summarisable_depth_is_the_headline() {
        let id = "20260828T034237Z-ornith-1.5-35b-a3b";
        let log = run(vec![
            depth_row(id, 1024, &SHALLOW),
            depth_row(id, 16_384, &DEEP),
        ]);
        let m = speed_of(&log).expect("a throughput run has a speed");
        assert_eq!(m.speed.depth, 16_384);
        assert!((m.speed.decode.median - 68.1).abs() < 1e-9, "{m:?}");
        assert_eq!(m.speed.run_id, id);
        assert_eq!(m.speed.engine_commit, "dda1b0d67");
        assert_eq!(
            (m.model.as_str(), m.quant.as_str(), m.ctx),
            ("ornith-1.5-35b-a3b", "Q8_0", 262_144)
        );
    }

    #[test]
    fn a_deep_row_with_too_few_samples_yields_to_the_next_depth() {
        // One sample is warmup only; it cannot be summarised, so it is not a
        // headline — the shallower row that CAN be is.
        let id = "20260828T034237Z-ornith-1.5-35b-a3b";
        let log = run(vec![
            depth_row(id, 4096, &SHALLOW),
            depth_row(id, 16_384, &[50.0]),
        ]);
        assert_eq!(speed_of(&log).expect("speed").speed.depth, 4096);
    }

    #[test]
    fn a_run_without_throughput_rows_has_no_speed() {
        let id = "20260828T044349Z-ornith-1.5-35b-a3b";
        let mut agentic = depth_row(id, 1024, &SHALLOW);
        agentic.suite = "tool_emit".into();
        agentic.task_id = "te-001".into();
        assert!(speed_of(&run(vec![agentic])).is_none());
    }

    fn measured(run_id: &str, taken_under: Stamp) -> MeasuredSpeed {
        let mut log = run(vec![depth_row(run_id, 16_384, &DEEP)]);
        log.head.stamp = taken_under;
        speed_of(&log).expect("speed")
    }

    #[test]
    fn a_run_from_another_machine_ctx_or_quant_never_matches() {
        let here = measured(
            "20260828T034237Z-ornith",
            stamp("8d41f0c2a917", "Q8_0", 262_144),
        );
        let speeds = [here];
        let key = |machine_id, quant, ctx| SpeedKey {
            model: "ornith-1.5-35b-a3b",
            quant,
            ctx,
            machine_id,
        };
        assert!(pick(&speeds, &key("8d41f0c2a917", "Q8_0", 262_144)).is_some());
        assert!(
            pick(&speeds, &key("ffffffffffff", "Q8_0", 262_144)).is_none(),
            "machine"
        );
        assert!(
            pick(&speeds, &key("8d41f0c2a917", "Q4_K_M", 262_144)).is_none(),
            "quant"
        );
        assert!(
            pick(&speeds, &key("8d41f0c2a917", "Q8_0", 131_072)).is_none(),
            "ctx"
        );
        let other_model = SpeedKey {
            model: "qwen3.8-27b",
            ..key("8d41f0c2a917", "Q8_0", 262_144)
        };
        assert!(pick(&speeds, &other_model).is_none(), "model");
    }

    #[test]
    fn the_latest_run_wins_and_the_count_is_reported() {
        let older = measured(
            "20260828T034237Z-ornith",
            stamp("8d41f0c2a917", "Q8_0", 262_144),
        );
        let newer = measured(
            "20260828T034340Z-ornith",
            stamp("8d41f0c2a917", "Q8_0", 262_144),
        );
        // Stored out of order on purpose: directory listing order is not time order.
        let speeds = [newer.clone(), older];
        let key = SpeedKey {
            model: "ornith-1.5-35b-a3b",
            quant: "Q8_0",
            ctx: 262_144,
            machine_id: "8d41f0c2a917",
        };
        let (chosen, count) = pick(&speeds, &key).expect("a match");
        assert_eq!(chosen.speed.run_id, newer.speed.run_id);
        assert_eq!(count, 2);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-bench-speeds")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write_run(eval_dir: &std::path::Path, log: &RunLog) {
        let dir = eval_dir.join(&log.rows[0].run_id);
        std::fs::create_dir_all(&dir).expect("run dir");
        std::fs::write(
            dir.join("stamp.json"),
            serde_json::to_string(&log.head).expect("head"),
        )
        .expect("stamp");
        let lines: Vec<String> = log
            .rows
            .iter()
            .map(|r| serde_json::to_string(r).expect("row"))
            .collect();
        std::fs::write(dir.join("results.jsonl"), lines.join("\n")).expect("rows");
    }

    #[test]
    fn an_unreadable_run_is_a_note_not_a_crash_and_not_a_silence() {
        let eval = scratch("unreadable");
        let id = "20260828T034237Z-ornith-1.5-35b-a3b";
        write_run(&eval, &run(vec![depth_row(id, 16_384, &DEEP)]));
        let broken = eval.join("20260828T999999Z-broken");
        std::fs::create_dir_all(&broken).expect("broken dir");
        std::fs::write(broken.join("stamp.json"), "{ not json").expect("garbage");
        std::fs::write(eval.join("stray-file.txt"), "ignored").expect("stray");

        let loaded = load_all(&eval);
        assert_eq!(loaded.speeds.len(), 1, "{loaded:?}");
        assert_eq!(loaded.speeds[0].speed.run_id, id);
        assert_eq!(loaded.notes.len(), 1, "{loaded:?}");
        assert!(
            loaded.notes[0].contains("20260828T999999Z-broken")
                && loaded.notes[0].contains("excluded"),
            "{}",
            loaded.notes[0]
        );
    }

    #[test]
    fn a_missing_eval_dir_is_simply_empty() {
        let loaded = load_all(&scratch("absent").join("never-created"));
        assert!(
            loaded.speeds.is_empty() && loaded.notes.is_empty(),
            "{loaded:?}"
        );
    }
}
