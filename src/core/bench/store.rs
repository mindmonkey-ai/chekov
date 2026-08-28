//! One bench run on disk, complete enough that a later `compare` — or a
//! skeptical human — can audit every summary back to its raw samples.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::bench::sweep::{DepthResult, curve_note};
use crate::core::stats;
use crate::error::ChekovError;

/// What this chekov writes and reads.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub schema_version: u32,
    pub created_utc: String,
    pub model: String,
    /// What the server LOADED (`/props`), not what the registry intended.
    pub ctx: u32,
    /// Flag hygiene: the exact argv the measured server was launched with.
    pub launch_args: Vec<String>,
    pub engine_build_commit: Option<String>,
    pub machine: MachineRecord,
    pub depths: Vec<DepthRecord>,
    #[serde(default)]
    pub fixture: Vec<ProbeRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineRecord {
    pub chip: Option<String>,
    pub memsize_bytes: Option<u64>,
    pub gpu_budget_mib: Option<u64>,
    pub budget_provenance: Option<String>,
}

/// Raw samples only — summaries are recomputed on read so a stored median can
/// never drift from what was measured.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthRecord {
    pub depth: u32,
    pub prompt_n: u64,
    pub decode_samples: Vec<f64>,
    pub prefill_samples: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRecord {
    pub id: String,
    pub pass: bool,
    pub reason: Option<String>,
}

impl From<&DepthResult> for DepthRecord {
    fn from(result: &DepthResult) -> Self {
        Self {
            depth: result.depth,
            prompt_n: result.prompt_n,
            decode_samples: result.decode_samples.clone(),
            prefill_samples: result.prefill_samples.clone(),
        }
    }
}

pub fn save(dir: &Path, record: &RunRecord) -> Result<PathBuf, ChekovError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| ChekovError::io(format!("creating {}", dir.display()), e))?;
    let path = dir.join(format!("{}-{}.json", record.created_utc, record.model));
    let json = serde_json::to_string_pretty(record).map_err(|e| ChekovError::BenchRunInvalid {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    std::fs::write(&path, json)
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))?;
    Ok(path)
}

pub fn load(path: &Path) -> Result<RunRecord, ChekovError> {
    let invalid = |reason: String| ChekovError::BenchRunInvalid {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|e| invalid(e.to_string()))?;
    let record: RunRecord = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    if record.schema_version != SCHEMA_VERSION {
        return Err(invalid(format!(
            "schema_version {} — this chekov reads {SCHEMA_VERSION}",
            record.schema_version
        )));
    }
    Ok(record)
}

/// The run as a table, summaries recomputed from the samples.
#[must_use]
pub fn render_run(record: &RunRecord) -> String {
    let mut out = header_line(record);
    out.push_str("depth  prompt_n  decode tok/s (median [p10..p90])  prefill tok/s  n\n");
    for depth in &record.depths {
        out.push_str(&depth_line(depth));
    }
    if let Some(note) = curve_note(summarisable_depths(record)) {
        out.push_str(&note);
        out.push('\n');
    }
    for probe in &record.fixture {
        let verdict = if probe.pass { "PASS" } else { "FAIL" };
        let reason = probe.reason.as_deref().unwrap_or("");
        out.push_str(&format!("fixture {verdict} {}  {reason}\n", probe.id));
    }
    out
}

fn header_line(record: &RunRecord) -> String {
    let engine = record.engine_build_commit.as_deref().unwrap_or("unknown");
    format!(
        "bench {}  ctx {}  engine {engine}  {}\n",
        record.model, record.ctx, record.created_utc
    )
}

fn depth_line(depth: &DepthRecord) -> String {
    let decode = stats::summarize(&depth.decode_samples);
    let prefill = stats::summarize(&depth.prefill_samples);
    match (decode, prefill) {
        (Some(d), Some(p)) => format!(
            "{:>5}  {:>8}  {:.1} [{:.1}..{:.1}]  {:.1}  {} ({} warmup dropped)\n",
            depth.depth, depth.prompt_n, d.median, d.p10, d.p90, p.median, d.n, d.warmup_dropped
        ),
        _ => format!(
            "{:>5}  {:>8}  too few samples to summarise\n",
            depth.depth, depth.prompt_n
        ),
    }
}

fn summarisable_depths(record: &RunRecord) -> usize {
    record
        .depths
        .iter()
        .filter(|d| stats::summarize(&d.decode_samples).is_some())
        .count()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{DepthRecord, MachineRecord, RunRecord, load, render_run, save};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-bench-store")
            .join(name);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn record() -> RunRecord {
        RunRecord {
            schema_version: 1,
            created_utc: "20260827T120000Z".into(),
            model: "ornith-1.5-35b-a3b".into(),
            ctx: 131_072,
            launch_args: vec!["-m".into(), "model.gguf".into()],
            engine_build_commit: Some("79aac7d9".into()),
            machine: MachineRecord {
                chip: Some("Apple M3 Ultra".into()),
                memsize_bytes: Some(274_877_906_944),
                gpu_budget_mib: Some(228_065),
                budget_provenance: Some("engine-reported".into()),
            },
            depths: vec![
                DepthRecord {
                    depth: 1024,
                    prompt_n: 1093,
                    decode_samples: vec![19.0, 21.0, 22.0, 22.4],
                    prefill_samples: vec![400.0, 450.0, 455.0, 452.0],
                },
                DepthRecord {
                    depth: 4096,
                    prompt_n: 4210,
                    decode_samples: vec![17.0, 18.5, 18.7, 18.6],
                    prefill_samples: vec![380.0, 420.0, 425.0, 422.0],
                },
            ],
            fixture: vec![],
        }
    }

    #[test]
    fn a_run_round_trips_through_disk() {
        let dir = scratch("roundtrip");
        let path = save(&dir, &record()).expect("save");
        let loaded = load(&path).expect("load");
        assert_eq!(loaded.model, "ornith-1.5-35b-a3b");
        assert_eq!(loaded.depths.len(), 2);
        assert_eq!(
            loaded.depths[0].decode_samples,
            vec![19.0, 21.0, 22.0, 22.4]
        );
    }

    #[test]
    fn an_unknown_field_in_a_stored_run_is_refused() {
        let dir = scratch("unknown-field");
        let path = dir.join("bad.json");
        std::fs::write(&path, r#"{"schema_version":1,"surprise":true}"#).expect("write");
        assert!(load(&path).is_err(), "deny_unknown_fields");
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_misread() {
        let dir = scratch("v2");
        let mut newer = record();
        newer.schema_version = 2;
        let path = save(&dir, &newer).expect("save");
        let err = load(&path).expect_err("too new");
        assert!(err.to_string().contains("schema_version"), "{err}");
    }

    #[test]
    fn the_rendering_recomputes_summaries_and_refuses_the_curve_below_three_depths() {
        let rendered = render_run(&record());
        assert!(rendered.contains("ornith-1.5-35b-a3b"));
        assert!(
            rendered.contains("insufficient depths to fit a curve"),
            "{rendered}"
        );
        // Median of [21.0, 22.0, 22.4] after the warmup drop — from stats, not storage.
        assert!(rendered.contains("22.0"), "{rendered}");
        assert!(
            rendered.contains("warmup"),
            "the drop is visible, never absorbed: {rendered}"
        );
    }
}
