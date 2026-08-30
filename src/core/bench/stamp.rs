//! The 20-field configuration stamp (spec §7.4).
//!
//! llama.cpp does not guarantee bit-identical results across configurations:
//! GPU reduction kernels pick different accumulation orders and float
//! addition is not associative. Determinism therefore holds only inside one
//! pinned configuration, and the stamp is what pins it.

use serde::{Deserialize, Serialize};

/// One pinned configuration. Field order is comparison order — `first_mismatch`
/// reports the earliest differing field, so the most identity-like fields
/// come first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub machine_id: String,
    pub engine_build_commit: String,
    /// `<revision>/<first_shard>` — the pinned HF revision is content-addressed
    /// upstream; hashing 35-180 GiB of weights per run would cost minutes for
    /// the same attestation.
    pub weights_revision: String,
    pub quant: String,
    /// The per-slot window the server LOADED (`/props`), not the `-c` value.
    pub ctx: u32,
    pub n_parallel: u32,
    /// Flag-sourced values record what the argv SAID; a flag the argv does not
    /// set is the literal "engine-default" — comparable under one engine
    /// commit, never invented.
    pub kv_unified: String,
    pub n_batch: String,
    pub n_ubatch: String,
    pub type_k: String,
    pub type_v: String,
    pub flash_attn: String,
    /// Whether `--allow-exec` was given. Runs that executed the repository and
    /// runs that only read it are not the same environment: tiers 6-7 exist in
    /// one and are absent from the other, so `compare` refuses across it.
    #[serde(default)]
    pub allow_exec: bool,
    /// The `cargo --version` line, when exec ran. `None` both when the flag was
    /// absent and when the machine had no toolchain — the report tells those
    /// two apart from the rows, not from here.
    #[serde(default)]
    pub cargo_version: Option<String>,
    /// Where the build artefacts went: `"scratch"` for the run's own
    /// `CARGO_TARGET_DIR`, `"none"` when nothing was built. A later slice that
    /// reuses the repository's `target/` is a different environment and this
    /// field is how `compare` will say so.
    #[serde(default = "exec_target_off")]
    pub exec_target: String,
    pub seed: u32,
    /// Millitemperature (0 = greedy) — integer so every field compares exactly.
    pub temperature_milli: u32,
    pub chekov_version: String,
    pub prompt_set_hash: String,
    pub corpus_id: String,
}

/// A stamp written before the exec tiers existed ran nothing, and says so.
fn exec_target_off() -> String {
    EXEC_TARGET_OFF.to_owned()
}

/// `exec_target` when the run built into its own scratch directory.
pub const EXEC_TARGET_SCRATCH: &str = "scratch";
/// `exec_target` when the run built nothing at all.
pub const EXEC_TARGET_OFF: &str = "none";

/// The FIRST differing field name, in declaration order — or `None` if equal.
#[must_use]
pub fn first_mismatch(a: &Stamp, b: &Stamp) -> Option<&'static str> {
    let pairs: [(&'static str, bool); 20] = [
        ("machine_id", a.machine_id != b.machine_id),
        (
            "engine_build_commit",
            a.engine_build_commit != b.engine_build_commit,
        ),
        ("weights_revision", a.weights_revision != b.weights_revision),
        ("quant", a.quant != b.quant),
        ("ctx", a.ctx != b.ctx),
        ("n_parallel", a.n_parallel != b.n_parallel),
        ("kv_unified", a.kv_unified != b.kv_unified),
        ("n_batch", a.n_batch != b.n_batch),
        ("n_ubatch", a.n_ubatch != b.n_ubatch),
        ("type_k", a.type_k != b.type_k),
        ("type_v", a.type_v != b.type_v),
        ("flash_attn", a.flash_attn != b.flash_attn),
        ("allow_exec", a.allow_exec != b.allow_exec),
        ("cargo_version", a.cargo_version != b.cargo_version),
        ("exec_target", a.exec_target != b.exec_target),
        ("seed", a.seed != b.seed),
        (
            "temperature_milli",
            a.temperature_milli != b.temperature_milli,
        ),
        ("chekov_version", a.chekov_version != b.chekov_version),
        ("prompt_set_hash", a.prompt_set_hash != b.prompt_set_hash),
        ("corpus_id", a.corpus_id != b.corpus_id),
    ];
    pairs
        .iter()
        .find(|(_, differs)| *differs)
        .map(|(name, _)| *name)
}

/// The refusal for the first differing field, values included — or `None`
/// when the stamps match.
#[must_use]
pub fn mismatch_error(a: &Stamp, b: &Stamp) -> Option<crate::error::ChekovError> {
    let field = first_mismatch(a, b)?;
    let show = |s: &Stamp| {
        serde_json::to_value(s)
            .ok()
            .and_then(|v| v.get(field).map(std::string::ToString::to_string))
            .unwrap_or_else(|| "?".to_owned())
    };
    Some(crate::error::ChekovError::BenchStampMismatch {
        field: field.to_owned(),
        a: show(a),
        b: show(b),
    })
}

/// One flag's value out of a launch argv. `--flag value` yields the value; a
/// bare switch yields "on"; an absent flag yields "engine-default".
#[must_use]
pub fn flag_value(args: &[String], flag: &str) -> String {
    let Some(position) = args.iter().position(|a| a == flag) else {
        return "engine-default".to_owned();
    };
    match args.get(position + 1) {
        Some(next) if !next.starts_with('-') => next.clone(),
        _ => "on".to_owned(),
    }
}

/// The first of several spellings (short and long form) that the argv sets.
#[must_use]
pub fn flag_value_either(args: &[String], names: &[&str]) -> String {
    names
        .iter()
        .map(|name| flag_value(args, name))
        .find(|value| value != "engine-default")
        .unwrap_or_else(|| "engine-default".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{Stamp, first_mismatch, flag_value};

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
            allow_exec: false,
            cargo_version: None,
            exec_target: "none".into(),
            seed: 42,
            temperature_milli: 0,
            chekov_version: "0.1.0".into(),
            prompt_set_hash: "e19a".into(),
            corpus_id: "throughput-v1".into(),
        }
    }

    #[test]
    fn identical_stamps_have_no_mismatch() {
        assert_eq!(first_mismatch(&stamp(), &stamp()), None);
    }

    #[test]
    fn the_first_differing_field_is_named_in_declaration_order() {
        let mut b = stamp();
        b.type_k = "f16".into();
        b.engine_build_commit = "00c0ffee".into();
        // engine_build_commit precedes type_k in declaration order.
        assert_eq!(first_mismatch(&stamp(), &b), Some("engine_build_commit"));
    }

    #[test]
    fn flag_values_parse_and_absence_is_engine_default() {
        let args: Vec<String> = ["--cache-type-k", "q8_0", "--flash-attn", "on", "-kvu"]
            .map(String::from)
            .to_vec();
        assert_eq!(flag_value(&args, "--cache-type-k"), "q8_0");
        assert_eq!(flag_value(&args, "--flash-attn"), "on");
        assert_eq!(flag_value(&args, "-kvu"), "on", "a bare switch reports on");
        assert_eq!(flag_value(&args, "--batch-size"), "engine-default");
    }

    #[test]
    fn the_exec_fields_refuse_like_any_other_environment_field() {
        let mut b = stamp();
        b.allow_exec = true;
        assert_eq!(first_mismatch(&stamp(), &b), Some("allow_exec"));
        let mut b = stamp();
        b.cargo_version = Some("cargo 1.95.0 (0000000 2026-01-01)".into());
        assert_eq!(first_mismatch(&stamp(), &b), Some("cargo_version"));
        let mut b = stamp();
        b.exec_target = "scratch".into();
        assert_eq!(first_mismatch(&stamp(), &b), Some("exec_target"));
        // Declaration order: an environment field still loses to the identity
        // fields above it, and still beats the seed below it.
        let mut b = stamp();
        b.allow_exec = true;
        b.machine_id = "0000".into();
        assert_eq!(first_mismatch(&stamp(), &b), Some("machine_id"));
        let mut b = stamp();
        b.allow_exec = true;
        b.seed = 43;
        assert_eq!(first_mismatch(&stamp(), &b), Some("allow_exec"));
    }

    /// A stamp written before B2 has none of the three. It must still load —
    /// and load as what it was: a run that never ran anything.
    #[test]
    fn a_pre_b2_stamp_loads_with_exec_off() {
        let json = r#"{"machine_id":"m","engine_build_commit":"e","weights_revision":"w",
            "quant":"Q8_0","ctx":4096,"n_parallel":1,"kv_unified":"engine-default",
            "n_batch":"engine-default","n_ubatch":"engine-default","type_k":"q8_0",
            "type_v":"q8_0","flash_attn":"on","seed":42,"temperature_milli":0,
            "chekov_version":"0.1.0","prompt_set_hash":"e19a","corpus_id":"throughput-v1"}"#;
        let parsed: Stamp = serde_json::from_str(json).expect("a pre-B2 stamp loads");
        assert!(!parsed.allow_exec);
        assert_eq!(parsed.cargo_version, None);
        assert_eq!(parsed.exec_target, "none");
    }

    fn judged() -> super::JudgeStamp {
        super::JudgeStamp {
            model: "gpt-oss-20b".into(),
            quant: "F16".into(),
            revision: "d449b42d93e1".into(),
            arch: "gpt-oss".into(),
            rubric_hash: "9f8e7d6c5b4a".into(),
            max_tokens: 512,
            reasoning_effort: "low".into(),
            min_consistency_pct: 70,
        }
    }

    #[test]
    fn a_differing_judge_is_the_last_field_named_and_an_absent_one_round_trips() {
        let mut with = stamp();
        with.judge = Some(judged());
        let mut other = with.clone();
        other.judge.as_mut().map(|j| j.rubric_hash = "000000000000".into());
        assert_eq!(first_mismatch(&with, &other), Some("judge"));
        assert_eq!(first_mismatch(&with, &stamp()), Some("judge"), "absent vs present differs");
        let json = serde_json::to_string(&stamp()).expect("json");
        assert!(!json.contains("\"judge\""), "no judge, no key: {json}");
        let back: Stamp = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.judge, None);
        let mut ctx_differs = with.clone();
        ctx_differs.ctx = 1;
        assert_eq!(first_mismatch(&with, &ctx_differs), Some("ctx"), "earlier fields still win");
    }
}
