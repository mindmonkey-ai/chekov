//! The 25-field configuration stamp (spec §7.4).
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
    /// The runtime serving the model: `llama.cpp` for every run chekov
    /// launches; the declared `<name> <version>` for a foreign server.
    /// Stored runs from before this field existed were all llama.cpp,
    /// which is what the serde default says.
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Where the timing numbers came from: the server's own `timings`
    /// object, or chekov's wall clock over a streamed reply (foreign runs).
    #[serde(default = "default_timing_source")]
    pub timing_source: String,
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
    /// `--spec-type` as the argv said it, "engine-default" when absent (no
    /// speculative decoding), "unmanaged" on a foreign run. A run decoded with
    /// the MTP head and a run decoded without it are different environments.
    #[serde(default = "engine_default_flag")]
    pub spec_type: String,
    /// `--spec-draft-n-max` likewise — only meaningful beside a draft
    /// `spec_type`, recorded regardless so two runs never differ silently.
    #[serde(default = "engine_default_flag")]
    pub spec_draft_n_max: String,
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
    /// The judge a run's `equiv` column was measured with (spec C §5) —
    /// `None` when the run graded no judged column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeStamp>,
}

/// The judge a run's `equiv` column was measured with (spec C §5) — the
/// instrument, its budget and its floor, so a report is read against what
/// voided or kept the column, not against today's config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeStamp {
    pub model: String,
    pub quant: String,
    /// The pinned HF revision, first twelve characters.
    pub revision: String,
    pub arch: String,
    pub rubric_hash: String,
    pub max_tokens: u32,
    pub reasoning_effort: String,
    pub min_consistency_pct: u32,
}

/// A stamp written before the exec tiers existed ran nothing, and says so.
fn exec_target_off() -> String {
    EXEC_TARGET_OFF.to_owned()
}

/// `exec_target` when the run built into its own scratch directory.
pub const EXEC_TARGET_SCRATCH: &str = "scratch";
/// `exec_target` when the run built nothing at all.
pub const EXEC_TARGET_OFF: &str = "none";

/// A stamp written before the runtime field existed came from llama.cpp.
fn default_runtime() -> String {
    RUNTIME_LLAMA_CPP.to_owned()
}

/// `Stamp.runtime` for every run chekov launches itself.
pub const RUNTIME_LLAMA_CPP: &str = "llama.cpp";

/// A stamp written before the `timing_source` field existed was always
/// server-timed — chekov had no other way to measure a run.
fn default_timing_source() -> String {
    TIMING_SERVER.to_owned()
}

/// `Stamp.timing_source` for a run timed off the server's own `timings`
/// object — every run chekov launches itself.
pub const TIMING_SERVER: &str = "server-reported";
/// `Stamp.timing_source` for a run chekov timed itself, over a streamed
/// reply from a foreign runtime that reports no `timings` object.
pub const TIMING_CHEKOV_STREAMED: &str = "chekov-streamed";

/// A flag the argv does not set: the engine's own default applies, whatever
/// it is under this engine commit — comparable, never invented.
pub const FLAG_ENGINE_DEFAULT: &str = "engine-default";
/// A flag on a server chekov did not launch: not observed, not invented — a
/// third spelling distinct from "engine-default" (foreign-runtime spec §5).
pub const FLAG_UNMANAGED: &str = "unmanaged";

/// A stamp or record written before the speculative fields existed was
/// decoded without speculation.
fn engine_default_flag() -> String {
    FLAG_ENGINE_DEFAULT.to_owned()
}

/// The eight flag-sourced values a launch argv pins.
///
/// Read the same way for a bench stamp and a tune trial so the two describe a
/// configuration in the same words (tune spec-stage design §6). The two
/// speculative fields default so every tune record under `tune/` written
/// before them still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchFlags {
    pub kv_unified: String,
    pub n_batch: String,
    pub n_ubatch: String,
    pub type_k: String,
    pub type_v: String,
    pub flash_attn: String,
    #[serde(default = "engine_default_flag")]
    pub spec_type: String,
    #[serde(default = "engine_default_flag")]
    pub spec_draft_n_max: String,
}

/// Read the launch flags off an argv, each spelling covered.
#[must_use]
pub fn launch_flags(argv: &[String]) -> LaunchFlags {
    LaunchFlags {
        kv_unified: flag_value_either(argv, &["-kvu", "--kv-unified"]),
        n_batch: flag_value_either(argv, &["-b", "--batch-size"]),
        n_ubatch: flag_value_either(argv, &["-ub", "--ubatch-size"]),
        type_k: flag_value_either(argv, &["-ctk", "--cache-type-k"]),
        type_v: flag_value_either(argv, &["-ctv", "--cache-type-v"]),
        flash_attn: flag_value_either(argv, &["-fa", "--flash-attn"]),
        spec_type: flag_value_either(argv, &["--spec-type"]),
        spec_draft_n_max: flag_value_either(argv, &["--spec-draft-n-max"]),
    }
}

/// Every launch flag of a foreign server, all eight unobservable.
#[must_use]
pub fn unmanaged_flags() -> LaunchFlags {
    let sentinel = || FLAG_UNMANAGED.to_owned();
    LaunchFlags {
        kv_unified: sentinel(),
        n_batch: sentinel(),
        n_ubatch: sentinel(),
        type_k: sentinel(),
        type_v: sentinel(),
        flash_attn: sentinel(),
        spec_type: sentinel(),
        spec_draft_n_max: sentinel(),
    }
}

/// The FIRST differing field name, in declaration order — or `None` if equal.
#[must_use]
pub fn first_mismatch(a: &Stamp, b: &Stamp) -> Option<&'static str> {
    let pairs: [(&'static str, bool); 25] = [
        ("machine_id", a.machine_id != b.machine_id),
        ("runtime", a.runtime != b.runtime),
        ("timing_source", a.timing_source != b.timing_source),
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
        ("spec_type", a.spec_type != b.spec_type),
        ("spec_draft_n_max", a.spec_draft_n_max != b.spec_draft_n_max),
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
        ("judge", a.judge != b.judge),
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
        return FLAG_ENGINE_DEFAULT.to_owned();
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
        .find(|value| value != FLAG_ENGINE_DEFAULT)
        .unwrap_or_else(|| FLAG_ENGINE_DEFAULT.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        LaunchFlags, RUNTIME_LLAMA_CPP, Stamp, TIMING_CHEKOV_STREAMED, TIMING_SERVER,
        first_mismatch, flag_value, launch_flags, unmanaged_flags,
    };

    fn stamp() -> Stamp {
        Stamp {
            machine_id: "8d41f0c2a917".into(),
            runtime: RUNTIME_LLAMA_CPP.to_owned(),
            timing_source: TIMING_SERVER.to_owned(),
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
        if let Some(j) = other.judge.as_mut() {
            j.rubric_hash = "000000000000".into();
        }
        assert_eq!(first_mismatch(&with, &other), Some("judge"));
        assert_eq!(
            first_mismatch(&with, &stamp()),
            Some("judge"),
            "absent vs present differs"
        );
        let json = serde_json::to_string(&stamp()).expect("json");
        assert!(!json.contains("\"judge\""), "no judge, no key: {json}");
        let back: Stamp = serde_json::from_str(&json).expect("parse");
        assert_eq!(back.judge, None);
        let mut ctx_differs = with.clone();
        ctx_differs.ctx = 1;
        assert_eq!(
            first_mismatch(&with, &ctx_differs),
            Some("ctx"),
            "earlier fields still win"
        );
    }

    #[test]
    fn a_stamp_without_a_runtime_field_reads_as_llama_cpp() {
        // Every run stored before this field existed was a llama.cpp run.
        let json = serde_json::json!({
            "machine_id": "m", "engine_build_commit": "c",
            "weights_revision": "r/s", "quant": "q", "ctx": 1, "n_parallel": 1,
            "kv_unified": "engine-default", "n_batch": "engine-default",
            "n_ubatch": "engine-default", "type_k": "engine-default",
            "type_v": "engine-default", "flash_attn": "engine-default",
            "seed": 0, "temperature_milli": 0, "chekov_version": "0",
            "prompt_set_hash": "h", "corpus_id": "corp"
        });
        let stamp: super::Stamp = serde_json::from_value(json).unwrap();
        assert_eq!(stamp.runtime, super::RUNTIME_LLAMA_CPP);
    }

    #[test]
    fn runtime_differs_before_the_engine_commit() {
        let mut a: super::Stamp = serde_json::from_value(serde_json::json!({
            "machine_id": "m", "engine_build_commit": "aaa",
            "weights_revision": "r/s", "quant": "q", "ctx": 1, "n_parallel": 1,
            "kv_unified": "engine-default", "n_batch": "engine-default",
            "n_ubatch": "engine-default", "type_k": "engine-default",
            "type_v": "engine-default", "flash_attn": "engine-default",
            "seed": 0, "temperature_milli": 0, "chekov_version": "0",
            "prompt_set_hash": "h", "corpus_id": "corp"
        }))
        .unwrap();
        let mut b = a.clone();
        a.runtime = "mtplx 0.4.1".to_owned();
        a.engine_build_commit = "0.4.1".to_owned();
        b.engine_build_commit = "bbb".to_owned();
        assert_eq!(super::first_mismatch(&a, &b), Some("runtime"));
    }

    #[test]
    fn a_stamp_without_a_timing_source_reads_as_server_reported() {
        // Same JSON literal as the runtime test, minus both new fields — a
        // stamp written before either existed measured every run itself.
        let json = serde_json::json!({
            "machine_id": "m", "engine_build_commit": "c",
            "weights_revision": "r/s", "quant": "q", "ctx": 1, "n_parallel": 1,
            "kv_unified": "engine-default", "n_batch": "engine-default",
            "n_ubatch": "engine-default", "type_k": "engine-default",
            "type_v": "engine-default", "flash_attn": "engine-default",
            "seed": 0, "temperature_milli": 0, "chekov_version": "0",
            "prompt_set_hash": "h", "corpus_id": "corp"
        });
        let stamp: Stamp = serde_json::from_value(json).unwrap();
        assert_eq!(stamp.timing_source, TIMING_SERVER);
    }

    #[test]
    fn timing_source_differs_after_runtime_and_before_the_engine_commit() {
        let mut a = stamp();
        let b = stamp();
        a.timing_source = TIMING_CHEKOV_STREAMED.to_owned();
        a.engine_build_commit = "x".into();
        assert_eq!(first_mismatch(&a, &b), Some("timing_source"));
        a.runtime = "mtplx 1".into();
        assert_eq!(first_mismatch(&a, &b), Some("runtime"));
    }

    /// The two speculative fields sit after `flash_attn` and before
    /// `allow_exec` in comparison order (spec §6).
    #[test]
    fn spec_type_differs_after_flash_attn_and_before_allow_exec() {
        let mut b = stamp();
        b.spec_type = "draft-mtp".into();
        b.allow_exec = true;
        assert_eq!(first_mismatch(&stamp(), &b), Some("spec_type"));
        let mut b = stamp();
        b.spec_draft_n_max = "1".into();
        b.seed = 43;
        assert_eq!(first_mismatch(&stamp(), &b), Some("spec_draft_n_max"));
        let mut b = stamp();
        b.spec_type = "draft-mtp".into();
        b.flash_attn = "off".into();
        assert_eq!(first_mismatch(&stamp(), &b), Some("flash_attn"));
    }

    /// Every stored run predates the fields and was decoded without
    /// speculation, which is what the default says (spec §6).
    #[test]
    fn a_stamp_without_the_spec_fields_reads_as_engine_default() {
        let json = r#"{"machine_id":"m","engine_build_commit":"e","weights_revision":"w",
            "quant":"Q8_0","ctx":4096,"n_parallel":1,"kv_unified":"engine-default",
            "n_batch":"engine-default","n_ubatch":"engine-default","type_k":"q8_0",
            "type_v":"q8_0","flash_attn":"on","seed":42,"temperature_milli":0,
            "chekov_version":"0.1.0","prompt_set_hash":"e19a","corpus_id":"throughput-v1"}"#;
        let parsed: Stamp = serde_json::from_str(json).expect("a pre-spec stamp loads");
        assert_eq!(parsed.spec_type, "engine-default");
        assert_eq!(parsed.spec_draft_n_max, "engine-default");
    }

    /// One reader for the eight flag-sourced values, each spelling covered
    /// (spec §6).
    #[test]
    fn launch_flags_read_all_eight() {
        let argv: Vec<String> =
            "-fa on --cache-type-k q8_0 -ctv q8_0 -b 4096 --spec-type draft-mtp --spec-draft-n-max 1"
                .split(' ')
                .map(String::from)
                .collect();
        let flags = launch_flags(&argv);
        assert_eq!(
            (
                flags.flash_attn.as_str(),
                flags.type_k.as_str(),
                flags.type_v.as_str(),
                flags.n_batch.as_str(),
                flags.n_ubatch.as_str(),
                flags.kv_unified.as_str(),
                flags.spec_type.as_str(),
                flags.spec_draft_n_max.as_str(),
            ),
            (
                "on",
                "q8_0",
                "q8_0",
                "4096",
                "engine-default",
                "engine-default",
                "draft-mtp",
                "1"
            )
        );
        let plain = launch_flags(&[]);
        assert_eq!(plain.spec_type, "engine-default");
    }

    /// The foreign sentinel is eight of the same word, and a six-field record
    /// from before the speculative fields still loads (spec §6).
    #[test]
    fn unmanaged_is_eight_sentinels_and_a_six_field_record_loads() {
        let sentinel = unmanaged_flags();
        for value in [
            &sentinel.kv_unified,
            &sentinel.n_batch,
            &sentinel.n_ubatch,
            &sentinel.type_k,
            &sentinel.type_v,
            &sentinel.flash_attn,
            &sentinel.spec_type,
            &sentinel.spec_draft_n_max,
        ] {
            assert_eq!(value, "unmanaged");
        }
        let json = r#"{"kv_unified":"engine-default","n_batch":"4096","n_ubatch":"engine-default",
            "type_k":"q8_0","type_v":"q8_0","flash_attn":"on"}"#;
        let old: LaunchFlags = serde_json::from_str(json).expect("a six-field record loads");
        assert_eq!(old.spec_type, "engine-default");
    }
}
