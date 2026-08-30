//! `chekov tune` stages, candidate lists, and argv rewriting (spec §4, §10).
//!
//! Pure, side-effect-free helpers: naming the four tune stages in their fixed
//! run order, listing the launch-flag values each stage tries, and rewriting
//! a launch argv to carry one candidate value under either flag spelling.

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
        assert_eq!(kv.len(), 1, "q8_0 is the incumbent; only f16 is a candidate");
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
        let values: Vec<&str> = candidates(Stage::Ubatch, &with_batch, &cfg)
            .iter()
            .map(|c| c.value.as_str())
            .collect();
        assert_eq!(values, vec!["256", "512", "1024"]);
        let engine_default: Vec<&str> = candidates(Stage::Ubatch, &[], &cfg)
            .iter()
            .map(|c| c.value.as_str())
            .collect();
        assert_eq!(
            engine_default,
            vec!["256", "512", "1024", "2048"],
            "no batch flag means the engine's 2048"
        );
        let batch: Vec<&str> = candidates(Stage::Batch, &with_batch, &cfg)
            .iter()
            .map(|c| c.value.as_str())
            .collect();
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
