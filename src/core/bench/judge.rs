#[cfg(test)]
mod tests {
    use super::{Eligibility, Reply, combine, consistency_pct, eligibility, family_key, parse_reply, requests, rubric_hash};
    use crate::core::bench::codebase::TaskTier;
    use crate::core::bench::store::{CodebaseRow, DecidedBy, ExecRow, ExecScore, JudgeRow};

    fn row(tier: TaskTier, gold: &str, prediction: &str) -> CodebaseRow {
        CodebaseRow {
            tier,
            file: "src/lib.rs".into(),
            line: 10,
            label: "<mask>".into(),
            gold: gold.into(),
            prediction: prediction.into(),
            prefix: (1..=60).map(|i| format!("before {i}\n")).collect(),
            suffix: (1..=30).map(|i| format!("after {i}\n")).collect(),
            excluded: crate::core::bench::codebase::Excluded::default(),
            symbols_score: Some(1.0),
            unsupported: false,
            arm: None,
            extra: None,
            also_first_uses: Vec::new(),
            name: None,
            n_predict: Some(16),
            exec: None,
        }
    }

    fn body(text: &str, stop: &str) -> String {
        serde_json::json!({
            "content": [{"type": "text", "text": text}],
            "stop_reason": stop,
        })
        .to_string()
    }

    #[test]
    fn the_rubric_hash_is_twelve_hex_chars_and_stable() {
        let h = rubric_hash();
        assert_eq!(h.len(), 12, "{h}");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, rubric_hash());
    }

    #[test]
    fn a_family_is_the_architecture_without_its_moe_suffix() {
        assert_eq!(family_key("qwen35moe"), "qwen35");
        assert_eq!(family_key("qwen35"), "qwen35");
        assert_ne!(family_key("qwen4exp"), family_key("qwen35"));
        assert_eq!(family_key("gpt-oss"), "gpt-oss");
    }

    #[test]
    fn only_answered_function_bodies_are_judged() {
        assert!(eligibility(&row(TaskTier::InFile, "a", "b")).is_none());
        assert!(eligibility(&row(TaskTier::FunctionBody, "a", "")).is_none(), "nobody answered");
        assert!(matches!(eligibility(&row(TaskTier::FunctionBody, "x = 1;", "x = 1;\nextra")), Some(Eligibility::Identical)), "identical after the tiers-1-4 trim");
        let mut failed = row(TaskTier::FunctionBody, "x = 1;", "y = 2;");
        failed.exec = Some(ExecRow { compile: ExecScore::Value(0.0), ..ExecRow::skipped("") });
        assert!(matches!(eligibility(&failed), Some(Eligibility::Skipped("did not compile"))));
        let mut passed = row(TaskTier::FunctionBody, "x = 1;", "y = 2;");
        passed.exec = Some(ExecRow { compile: ExecScore::Value(1.0), ..ExecRow::skipped("") });
        assert!(matches!(eligibility(&passed), Some(Eligibility::Judge(_))));
        assert!(matches!(eligibility(&row(TaskTier::FunctionBody, "x = 1;", "y = 2;")), Some(Eligibility::Judge(_))), "no exec means no compile verdict to defer to");
    }

    #[test]
    fn the_two_requests_swap_a_and_b_and_bound_the_context() {
        let Some(Eligibility::Judge(pair)) = eligibility(&row(TaskTier::FunctionBody, "GOLD;", "PRED;")) else {
            panic!("eligible");
        };
        let [first, second] = requests(&pair, 512);
        let text = |r: &crate::core::proxy::http::HttpRequest| {
            let v: serde_json::Value = serde_json::from_slice(&r.body).expect("json");
            assert_eq!(v["max_tokens"], 512);
            assert_eq!(v["messages"].as_array().map(Vec::len), Some(1), "one user turn: Gemma has no system role");
            v["messages"][0]["content"].as_str().expect("text").to_owned()
        };
        let (t1, t2) = (text(&first), text(&second));
        assert!(t1.find("A:\n```rust\nGOLD;").is_some() && t1.find("B:\n```rust\nPRED;").is_some(), "{t1}");
        assert!(t2.find("A:\n```rust\nPRED;").is_some() && t2.find("B:\n```rust\nGOLD;").is_some(), "{t2}");
        assert!(t1.contains("before 21\n") && !t1.contains("before 20\n"), "last 40 prefix lines: {t1}");
        assert!(t1.contains("after 20\n") && !t1.contains("after 21\n"), "first 20 suffix lines: {t1}");
        assert!(t1.contains("File: src/lib.rs"));
    }

    #[test]
    fn both_spans_are_cut_at_the_same_cap() {
        let long = "x".repeat(super::SPAN_MAX_CHARS + 50);
        let Some(Eligibility::Judge(pair)) = eligibility(&row(TaskTier::FunctionBody, &long, "y")) else { panic!() };
        assert_eq!(pair.gold.len(), super::SPAN_MAX_CHARS);
    }

    #[test]
    fn a_reply_is_parsed_strictly_or_skipped_with_the_reason() {
        assert!(matches!(parse_reply(&body("{\"same_behavior\":true}", "end_turn"), 512), Reply::Answer(true)));
        assert!(matches!(parse_reply(&body("{\"same_behavior\": false}", "end_turn"), 512), Reply::Answer(false)));
        for bad in ["yes", "```json\n{\"same_behavior\":true}\n```", "{\"same_behavior\":true,\"why\":\"x\"}", ""] {
            match parse_reply(&body(bad, "end_turn"), 512) {
                Reply::Skipped(reason) => assert!(reason.starts_with("reply was not the schema: "), "{reason}"),
                Reply::Answer(_) => panic!("{bad:?} must not parse"),
            }
        }
        match parse_reply(&body("{\"same_behavior\":true}", "max_tokens"), 512) {
            Reply::Skipped(reason) => assert_eq!(reason, "reply truncated at 512 tokens"),
            Reply::Answer(_) => panic!("a cut-off reply is not a verdict"),
        }
        let with_thinking = serde_json::json!({
            "content": [{"type": "thinking", "thinking": "hmm", "signature": ""}, {"type": "text", "text": "{\"same_behavior\":false}"}],
            "stop_reason": "end_turn",
        })
        .to_string();
        assert!(matches!(parse_reply(&with_thinking, 512), Reply::Answer(false)), "reasoning beside a valid answer is ignored");
    }

    #[test]
    fn agreement_is_the_verdict_and_disagreement_an_abstention() {
        let v = combine(&Reply::Answer(true), &Reply::Answer(true));
        assert_eq!((v.equivalent, v.decided_by, v.skipped), (Some(true), DecidedBy::SwapAgreement, None));
        let v = combine(&Reply::Answer(true), &Reply::Answer(false));
        assert_eq!((v.equivalent, v.decided_by), (None, DecidedBy::SwapDisagreement));
        let v = combine(&Reply::Answer(true), &Reply::Skipped("reply truncated at 512 tokens".into()));
        assert_eq!((v.equivalent, v.decided_by), (None, DecidedBy::Skipped));
        assert_eq!(v.skipped.as_deref(), Some("reply truncated at 512 tokens"));
    }

    fn judged(gold_first: Option<bool>, prediction_first: Option<bool>) -> JudgeRow {
        JudgeRow {
            equivalent: match (gold_first, prediction_first) {
                (Some(a), Some(b)) if a == b => Some(a),
                _ => None,
            },
            gold_first,
            prediction_first,
            decided_by: DecidedBy::SwapAgreement,
            skipped: None,
            judge_secs: 1.0,
        }
    }

    #[test]
    fn consistency_counts_only_crossings_both_orders_answered() {
        let rows = [judged(Some(true), Some(true)), judged(Some(false), Some(true)), judged(Some(false), None), judged(Some(false), Some(false))];
        let refs: Vec<&JudgeRow> = rows.iter().collect();
        assert_eq!(consistency_pct(&refs), Some(67), "2 agreements of 3 answered pairs");
        assert_eq!(consistency_pct(&[]), None);
        let one = [judged(None, None)];
        assert_eq!(consistency_pct(&[&one[0]]), None, "nothing answered twice: no rate, not 0%");
    }
}
