//! `chekov tune` stages, candidate lists, and argv rewriting (spec §4, §10).
//!
//! Pure, side-effect-free helpers: naming the four tune stages in their fixed
//! run order, listing the launch-flag values each stage tries, and rewriting
//! a launch argv to carry one candidate value under either flag spelling.

use crate::core::config::TuneSection;
use crate::error::ChekovError;

/// llama-server's default `--batch-size` when the flag is absent, per
/// `llama-server --help` on this machine.
const ENGINE_DEFAULT_BATCH: u32 = 2048;

/// One dimension `chekov tune` sweeps, in the fixed order they run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Fa,
    Kv,
    Batch,
    Ubatch,
}

impl Stage {
    pub const ORDER: [Self; 4] = [Self::Fa, Self::Kv, Self::Batch, Self::Ubatch];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fa => "fa",
            Self::Kv => "kv",
            Self::Batch => "batch",
            Self::Ubatch => "ubatch",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ORDER.into_iter().find(|stage| stage.label() == s)
    }

    #[must_use]
    pub const fn metric(self) -> Metric {
        match self {
            Self::Fa | Self::Kv => Metric::Decode,
            Self::Batch | Self::Ubatch => Metric::Prefill,
        }
    }
}

/// The throughput measurement a stage's candidates are judged on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metric {
    Decode,
    Prefill,
}

impl Metric {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Prefill => "prefill",
        }
    }

    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Decode => Self::Prefill,
            Self::Prefill => Self::Decode,
        }
    }
}

/// A launch-flag variant tried by one tune stage, with the rewritten argv it
/// produces.
pub struct Candidate {
    pub stage: Stage,
    pub value: String,
    pub argv: Vec<String>,
}

/// A launch flag `chekov tune` rewrites, with its short and long spellings.
#[derive(Clone, Copy)]
pub enum Flag {
    FlashAttn,
    CacheTypeK,
    CacheTypeV,
    BatchSize,
    UbatchSize,
}

impl Flag {
    #[must_use]
    pub const fn names(self) -> [&'static str; 2] {
        match self {
            Self::FlashAttn => ["-fa", "--flash-attn"],
            Self::CacheTypeK => ["-ctk", "--cache-type-k"],
            Self::CacheTypeV => ["-ctv", "--cache-type-v"],
            Self::BatchSize => ["-b", "--batch-size"],
            Self::UbatchSize => ["-ub", "--ubatch-size"],
        }
    }
}

/// Rewrites `argv` so `flag` carries `value`: the first occurrence of either
/// spelling is replaced in place, later duplicates are dropped, and an
/// absent flag is appended under its long spelling.
#[must_use]
pub fn rewrite(argv: &[String], flag: Flag, value: &str) -> Vec<String> {
    let names = flag.names();
    let mut out = Vec::with_capacity(argv.len() + 2);
    let mut replaced = false;
    let mut index = 0;
    while index < argv.len() {
        let token = &argv[index];
        if !names.contains(&token.as_str()) {
            out.push(token.clone());
            index += 1;
            continue;
        }
        if !replaced {
            out.push(token.clone());
            out.push(value.to_owned());
            replaced = true;
        }
        let skip_value = argv.get(index + 1).is_some();
        index += if skip_value { 2 } else { 1 };
    }
    if !replaced {
        out.push(names[1].to_owned());
        out.push(value.to_owned());
    }
    out
}

/// The value `flag` currently carries in `argv`, under either spelling.
#[must_use]
pub fn value_of(argv: &[String], flag: Flag) -> Option<String> {
    let names = flag.names();
    let position = argv.iter().position(|a| names.contains(&a.as_str()))?;
    argv.get(position + 1).cloned()
}

fn incumbent_batch(incumbent: &[String]) -> u32 {
    value_of(incumbent, Flag::BatchSize)
        .and_then(|value| value.parse().ok())
        .unwrap_or(ENGINE_DEFAULT_BATCH)
}

/// The candidate values a stage tries, per the `[tune]` config (spec §4).
fn values_for(stage: Stage, incumbent: &[String], cfg: &TuneSection) -> Vec<String> {
    match stage {
        Stage::Fa => cfg.flash_attn.clone(),
        Stage::Kv => cfg.cache_types.clone(),
        Stage::Batch => cfg.batch_sizes.iter().map(u32::to_string).collect(),
        Stage::Ubatch => {
            let batch = incumbent_batch(incumbent);
            cfg.ubatch_sizes
                .iter()
                .filter(|&&value| value <= batch)
                .map(u32::to_string)
                .collect()
        }
    }
}

/// `incumbent` rewritten to carry `value` for `stage`'s flag(s); `Kv`
/// rewrites K and V together.
fn apply(stage: Stage, incumbent: &[String], value: &str) -> Vec<String> {
    match stage {
        Stage::Fa => rewrite(incumbent, Flag::FlashAttn, value),
        Stage::Kv => {
            let with_k = rewrite(incumbent, Flag::CacheTypeK, value);
            rewrite(&with_k, Flag::CacheTypeV, value)
        }
        Stage::Batch => rewrite(incumbent, Flag::BatchSize, value),
        Stage::Ubatch => rewrite(incumbent, Flag::UbatchSize, value),
    }
}

/// Every candidate `stage` tries against `incumbent`, excluding the
/// incumbent's own argv (it is not a candidate against itself).
#[must_use]
pub fn candidates(stage: Stage, incumbent: &[String], cfg: &TuneSection) -> Vec<Candidate> {
    values_for(stage, incumbent, cfg)
        .into_iter()
        .filter_map(|value| {
            let argv = apply(stage, incumbent, &value);
            (argv != incumbent).then_some(Candidate { stage, value, argv })
        })
        .collect()
}

/// The stages to run, in the fixed order, filtered to `requested` labels
/// when given (duplicates collapse); an unrecognized label is an error.
pub fn stages(requested: Option<&[String]>) -> Result<Vec<Stage>, ChekovError> {
    let Some(labels) = requested else {
        return Ok(Stage::ORDER.to_vec());
    };
    let mut parsed = Vec::with_capacity(labels.len());
    for label in labels {
        let stage = Stage::parse(label).ok_or_else(|| ChekovError::TuneUnknownStage {
            stage: label.clone(),
        })?;
        parsed.push(stage);
    }
    Ok(Stage::ORDER
        .into_iter()
        .filter(|stage| parsed.contains(stage))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{Candidate, Flag, Metric, Stage, candidates, rewrite, stages, value_of};
    use crate::core::config::TuneSection;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn a_flag_is_rewritten_under_either_spelling_or_appended_when_absent() {
        let long = argv(&["--flash-attn", "on", "--cache-type-k", "q8_0"]);
        assert_eq!(
            rewrite(&long, Flag::FlashAttn, "off"),
            argv(&["--flash-attn", "off", "--cache-type-k", "q8_0"])
        );
        let short = argv(&["-fa", "on", "-b", "2048"]);
        assert_eq!(
            rewrite(&short, Flag::BatchSize, "4096"),
            argv(&["-fa", "on", "-b", "4096"])
        );
        assert_eq!(
            rewrite(&short, Flag::UbatchSize, "1024"),
            argv(&["-fa", "on", "-b", "2048", "--ubatch-size", "1024"])
        );
        let twice = argv(&["-b", "512", "--batch-size", "1024"]);
        assert_eq!(
            rewrite(&twice, Flag::BatchSize, "2048"),
            argv(&["-b", "2048"]),
            "later duplicates are removed"
        );
        assert_eq!(value_of(&short, Flag::BatchSize).as_deref(), Some("2048"));
        assert_eq!(value_of(&short, Flag::UbatchSize), None);
    }

    #[test]
    fn stages_run_in_the_fixed_order_and_name_their_metric() {
        assert_eq!(stages(None).expect("all"), Stage::ORDER.to_vec());
        let picked = stages(Some(&argv(&["ubatch", "fa"]))).expect("subset");
        assert_eq!(
            picked,
            vec![Stage::Fa, Stage::Ubatch],
            "the argument's order does not matter"
        );
        assert!(stages(Some(&argv(&["threads"]))).is_err());
        assert_eq!(
            (Stage::Fa.metric(), Stage::Kv.metric()),
            (Metric::Decode, Metric::Decode)
        );
        assert_eq!(
            (Stage::Batch.metric(), Stage::Ubatch.metric()),
            (Metric::Prefill, Metric::Prefill)
        );
        assert_eq!(Metric::Decode.other(), Metric::Prefill);
    }

    #[test]
    fn kv_candidates_rewrite_k_and_v_together_and_the_incumbent_is_not_a_candidate() {
        let cfg = TuneSection::default();
        let incumbent = argv(&[
            "--flash-attn",
            "on",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q8_0",
        ]);
        let kv = candidates(Stage::Kv, &incumbent, &cfg);
        assert_eq!(
            kv.len(),
            1,
            "q8_0 is the incumbent; only f16 is a candidate"
        );
        assert_eq!(kv[0].value, "f16");
        assert_eq!(
            kv[0].argv,
            argv(&[
                "--flash-attn",
                "on",
                "--cache-type-k",
                "f16",
                "--cache-type-v",
                "f16"
            ])
        );
        let fa = candidates(Stage::Fa, &incumbent, &cfg);
        assert_eq!(
            fa.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
            vec!["off"]
        );
    }

    #[test]
    fn ubatch_candidates_never_exceed_the_incumbent_batch() {
        let cfg = TuneSection::default();
        let with_batch = argv(&["--batch-size", "1024"]);
        let ubatch_with_batch = candidates(Stage::Ubatch, &with_batch, &cfg);
        let values: Vec<&str> = ubatch_with_batch.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["256", "512", "1024"]);
        let ubatch_no_batch = candidates(Stage::Ubatch, &[], &cfg);
        let engine_default: Vec<&str> = ubatch_no_batch.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(
            engine_default,
            vec!["256", "512", "1024", "2048"],
            "no batch flag means the engine's 2048"
        );
        let batch_candidates = candidates(Stage::Batch, &with_batch, &cfg);
        let batch: Vec<&str> = batch_candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(batch, vec!["512", "2048", "4096"]);
    }

    #[test]
    fn a_candidate_carries_its_stage_and_value() {
        let c = Candidate {
            stage: Stage::Batch,
            value: "4096".into(),
            argv: argv(&["-b", "4096"]),
        };
        assert_eq!((c.stage.label(), c.value.as_str()), ("batch", "4096"));
    }
}
