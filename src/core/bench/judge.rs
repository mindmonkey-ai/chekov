//! The position-swapped binary judge (spec C §4).
//!
//! What one verdict IS — rubric, eligibility, the request pair, the strict
//! reply parse and the swap rule. No HTTP here: requests go out through
//! `runner`, and what comes back is the Anthropic body `runner` already
//! hands every probe.

use serde::Deserialize;
use serde_json::Value;

use crate::core::bench::codebase::TaskTier;
use crate::core::bench::codebase::ladder::trimmed_to_gold;
use crate::core::bench::store::{CodebaseRow, DecidedBy, ExecScore, JudgeRow};
use crate::core::proxy::http::HttpRequest;

/// The prompt, and nothing else — a file so it diffs as text (§16.10).
pub const RUBRIC: &str = include_str!("judge_rubric.md");
pub const CONTEXT_BEFORE_LINES: usize = 40;
pub const CONTEXT_AFTER_LINES: usize = 20;
pub const SPAN_MAX_CHARS: usize = 4096;
const RUBRIC_VERSION: &str = "judge-v1";

/// The grammar every judge request asks for, and the shape `parse_reply` checks.
#[must_use]
pub fn schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {"same_behavior": {"type": "boolean"}},
        "required": ["same_behavior"],
        "additionalProperties": false,
    })
}

/// `sha256(file bytes ‖ schema ‖ the three constants ‖ "judge-v1")[..12]`.
#[must_use]
pub fn rubric_hash() -> String {
    let canonical = format!(
        "{RUBRIC}|{}|{CONTEXT_BEFORE_LINES}|{CONTEXT_AFTER_LINES}|{SPAN_MAX_CHARS}|{RUBRIC_VERSION}",
        schema()
    );
    crate::core::hash::sha256_hex(canonical.as_bytes())[..12].to_owned()
}

/// `general.architecture` with a trailing `moe` removed: `qwen35moe` and
/// `qwen35` are one family. A floor against sibling preference, not a proof
/// of independence (spec C §2.1).
#[must_use]
pub fn family_key(arch: &str) -> &str {
    arch.strip_suffix("moe").unwrap_or(arch)
}

/// The refusal when the judge shares a family with any candidate — or IS one.
#[must_use]
pub fn family_conflict(
    judge: (&str, &str),
    candidates: &[(String, String)],
) -> Option<crate::error::ChekovError> {
    let (judge_name, judge_arch) = judge;
    candidates
        .iter()
        .find(|(name, arch)| name == judge_name || family_key(arch) == family_key(judge_arch))
        .map(|(name, _)| crate::error::ChekovError::JudgeFamilyConflict {
            judge: judge_name.to_owned(),
            candidate: name.clone(),
            family: family_key(judge_arch).to_owned(),
        })
}

/// Everything the judge phase needs, resolved before any launch (spec C §3).
pub struct JudgePlan {
    pub judge: crate::core::registry::Effective,
    pub arch: String,
    pub rubric_hash: String,
    pub max_tokens: u32,
    pub min_consistency_pct: u32,
    pub reasoning_effort: crate::core::config::ReasoningEffort,
}

impl JudgePlan {
    #[must_use]
    pub fn stamp(&self) -> crate::core::bench::stamp::JudgeStamp {
        let entry = &self.judge.entry;
        crate::core::bench::stamp::JudgeStamp {
            model: self.judge.name.clone(),
            quant: entry.quant.clone(),
            revision: entry.revision.chars().take(12).collect(),
            arch: self.arch.clone(),
            rubric_hash: self.rubric_hash.clone(),
            max_tokens: self.max_tokens,
            reasoning_effort: self.reasoning_effort.as_str().to_owned(),
            min_consistency_pct: self.min_consistency_pct,
        }
    }

    /// The forced wire's inputs for a judge request.
    #[must_use]
    pub const fn forced<'a>(&self, schema: &'a Value) -> crate::core::bench::runner::Forced<'a> {
        crate::core::bench::runner::Forced {
            schema,
            reasoning_effort: Some(self.reasoning_effort.as_str()),
        }
    }
}

/// Whether a stored row gets a judge row, and which kind.
pub enum Eligibility<'a> {
    Identical,
    Skipped(&'static str),
    Judge(Pair<'a>),
}

/// The two spans and their bounded context, as the rubric shows them.
pub struct Pair<'a> {
    pub file: &'a str,
    pub before: String,
    pub after: String,
    pub gold: String,
    pub prediction: String,
}

/// `None` is "not a judge row at all": another tier, or a crossing nobody
/// answered. A compile failure is decided already and is skipped, never
/// re-opened.
#[must_use]
pub fn eligibility(row: &CodebaseRow) -> Option<Eligibility<'_>> {
    if row.tier != TaskTier::FunctionBody || row.prediction.is_empty() {
        return None;
    }
    if let Some(exec) = &row.exec
        && exec.compile == ExecScore::Value(0.0)
    {
        return Some(Eligibility::Skipped("did not compile"));
    }
    let prediction = trimmed_to_gold(&row.gold, &row.prediction);
    if row.gold.trim() == prediction.trim() {
        return Some(Eligibility::Identical);
    }
    Some(Eligibility::Judge(Pair {
        file: &row.file,
        before: last_lines(&row.prefix, CONTEXT_BEFORE_LINES),
        after: first_lines(&row.suffix, CONTEXT_AFTER_LINES),
        gold: cap(&row.gold),
        prediction: cap(&prediction),
    }))
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}

fn cap(span: &str) -> String {
    span.char_indices()
        .nth(SPAN_MAX_CHARS)
        .map_or_else(|| span.to_owned(), |(at, _)| span[..at].to_owned())
}

/// Gold-first, then prediction-first: the same bytes with A and B swapped.
#[must_use]
pub fn requests(pair: &Pair, max_tokens: u32) -> [HttpRequest; 2] {
    let ask = |a: &str, b: &str| {
        crate::core::bench::probes::anthropic_post(&serde_json::json!({
            "model": "claude-sonnet-4",
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": render(pair, a, b)}],
        }))
    };
    [
        ask(&pair.gold, &pair.prediction),
        ask(&pair.prediction, &pair.gold),
    ]
}

fn render(pair: &Pair, a: &str, b: &str) -> String {
    RUBRIC
        .replace("{{file}}", pair.file)
        .replace("{{before}}", &pair.before)
        .replace("{{after}}", &pair.after)
        .replace("{{a}}", a)
        .replace("{{b}}", b)
}

/// One order's outcome: the parsed answer, or why there is none.
pub enum Reply {
    Answer(bool),
    Skipped(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JudgeAnswer {
    same_behavior: bool,
}

/// The schema is asked for on the wire AND checked on the way back.
///
/// A grammar can be silently inactive (llama.cpp #20345), and a cut-off
/// reply is read from `stop_reason`, the one place the engine says so.
#[must_use]
pub fn parse_reply(anthropic_body: &str, max_tokens: u32) -> Reply {
    let Ok(body) = serde_json::from_str::<Value>(anthropic_body) else {
        return Reply::Skipped("reply was not the schema: <unreadable body>".to_owned());
    };
    if body["stop_reason"] == "max_tokens" {
        return Reply::Skipped(format!("reply truncated at {max_tokens} tokens"));
    }
    let text = body["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .unwrap_or_default();
    serde_json::from_str::<JudgeAnswer>(text).map_or_else(
        |_| {
            Reply::Skipped(format!(
                "reply was not the schema: {}",
                text.chars().take(80).collect::<String>()
            ))
        },
        |answer| Reply::Answer(answer.same_behavior),
    )
}

/// What the two orders settle to.
pub struct Verdict {
    pub equivalent: Option<bool>,
    pub decided_by: DecidedBy,
    pub skipped: Option<String>,
}

/// Agreement is the verdict; disagreement is an abstention; a skipped order
/// skips the crossing with its reason (the first one, in order).
#[must_use]
pub fn combine(gold_first: &Reply, prediction_first: &Reply) -> Verdict {
    match (gold_first, prediction_first) {
        (Reply::Answer(a), Reply::Answer(b)) if a == b => Verdict {
            equivalent: Some(*a),
            decided_by: DecidedBy::SwapAgreement,
            skipped: None,
        },
        (Reply::Answer(_), Reply::Answer(_)) => Verdict {
            equivalent: None,
            decided_by: DecidedBy::SwapDisagreement,
            skipped: None,
        },
        (Reply::Skipped(reason), _) | (_, Reply::Skipped(reason)) => Verdict {
            equivalent: None,
            decided_by: DecidedBy::Skipped,
            skipped: Some(reason.clone()),
        },
    }
}

/// `agreements / crossings both orders answered`, rounded; `None` when no
/// crossing was answered twice — a rate over nothing is not 0 %.
#[must_use]
pub fn consistency_pct(rows: &[&JudgeRow]) -> Option<u32> {
    let answered: Vec<&&JudgeRow> = rows
        .iter()
        .filter(|r| r.gold_first.is_some() && r.prediction_first.is_some())
        .collect();
    if answered.is_empty() {
        return None;
    }
    let agreed = answered
        .iter()
        .filter(|r| r.gold_first == r.prediction_first)
        .count();
    Some(u32::try_from((agreed * 100 + answered.len() / 2) / answered.len()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::{
        Eligibility, Reply, combine, consistency_pct, eligibility, family_key, parse_reply,
        requests, rubric_hash,
    };
    use crate::core::bench::codebase::TaskTier;
    use crate::core::bench::store::{CodebaseRow, DecidedBy, ExecRow, ExecScore, JudgeRow};
    use std::fmt::Write as _;

    fn numbered_lines(label: &str, n: usize) -> String {
        (1..=n).fold(String::new(), |mut lines, i| {
            let _ = writeln!(lines, "{label} {i}");
            lines
        })
    }

    fn row(tier: TaskTier, gold: &str, prediction: &str) -> CodebaseRow {
        CodebaseRow {
            tier,
            file: "src/lib.rs".into(),
            line: 10,
            label: "<mask>".into(),
            gold: gold.into(),
            prediction: prediction.into(),
            prefix: numbered_lines("before", 60),
            suffix: numbered_lines("after", 30),
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
        assert!(
            eligibility(&row(TaskTier::FunctionBody, "a", "")).is_none(),
            "nobody answered"
        );
        assert!(
            matches!(
                eligibility(&row(TaskTier::FunctionBody, "x = 1;", "x = 1;\nextra")),
                Some(Eligibility::Identical)
            ),
            "identical after the tiers-1-4 trim"
        );
        let mut failed = row(TaskTier::FunctionBody, "x = 1;", "y = 2;");
        failed.exec = Some(ExecRow {
            compile: ExecScore::Value(0.0),
            ..ExecRow::skipped("")
        });
        assert!(matches!(
            eligibility(&failed),
            Some(Eligibility::Skipped("did not compile"))
        ));
        let mut passed = row(TaskTier::FunctionBody, "x = 1;", "y = 2;");
        passed.exec = Some(ExecRow {
            compile: ExecScore::Value(1.0),
            ..ExecRow::skipped("")
        });
        assert!(matches!(eligibility(&passed), Some(Eligibility::Judge(_))));
        assert!(
            matches!(
                eligibility(&row(TaskTier::FunctionBody, "x = 1;", "y = 2;")),
                Some(Eligibility::Judge(_))
            ),
            "no exec means no compile verdict to defer to"
        );
    }

    #[test]
    fn the_two_requests_swap_a_and_b_and_bound_the_context() {
        let stored = row(TaskTier::FunctionBody, "GOLD;", "PRED;");
        let Some(Eligibility::Judge(pair)) = eligibility(&stored) else {
            panic!("eligible");
        };
        let [first, second] = requests(&pair, 512);
        let text = |r: &crate::core::proxy::http::HttpRequest| {
            let v: serde_json::Value = serde_json::from_slice(&r.body).expect("json");
            assert_eq!(v["max_tokens"], 512);
            assert_eq!(
                v["messages"].as_array().map(Vec::len),
                Some(1),
                "one user turn: Gemma has no system role"
            );
            v["messages"][0]["content"]
                .as_str()
                .expect("text")
                .to_owned()
        };
        let (t1, t2) = (text(&first), text(&second));
        assert!(
            t1.contains("A:\n```rust\nGOLD;") && t1.contains("B:\n```rust\nPRED;"),
            "{t1}"
        );
        assert!(
            t2.contains("A:\n```rust\nPRED;") && t2.contains("B:\n```rust\nGOLD;"),
            "{t2}"
        );
        assert!(
            t1.contains("before 21\n") && !t1.contains("before 20\n"),
            "last 40 prefix lines: {t1}"
        );
        assert!(
            t1.contains("after 20\n") && !t1.contains("after 21\n"),
            "first 20 suffix lines: {t1}"
        );
        assert!(t1.contains("File: src/lib.rs"));
    }

    #[test]
    fn both_spans_are_cut_at_the_same_cap() {
        let long = "x".repeat(super::SPAN_MAX_CHARS + 50);
        let stored = row(TaskTier::FunctionBody, &long, "y");
        let Some(Eligibility::Judge(pair)) = eligibility(&stored) else {
            panic!()
        };
        assert_eq!(pair.gold.len(), super::SPAN_MAX_CHARS);
    }

    #[test]
    fn a_reply_is_parsed_strictly_or_skipped_with_the_reason() {
        assert!(matches!(
            parse_reply(&body("{\"same_behavior\":true}", "end_turn"), 512),
            Reply::Answer(true)
        ));
        assert!(matches!(
            parse_reply(&body("{\"same_behavior\": false}", "end_turn"), 512),
            Reply::Answer(false)
        ));
        for bad in [
            "yes",
            "```json\n{\"same_behavior\":true}\n```",
            "{\"same_behavior\":true,\"why\":\"x\"}",
            "",
        ] {
            match parse_reply(&body(bad, "end_turn"), 512) {
                Reply::Skipped(reason) => {
                    assert!(reason.starts_with("reply was not the schema: "), "{reason}");
                }
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
        assert!(
            matches!(parse_reply(&with_thinking, 512), Reply::Answer(false)),
            "reasoning beside a valid answer is ignored"
        );
    }

    #[test]
    fn agreement_is_the_verdict_and_disagreement_an_abstention() {
        let v = combine(&Reply::Answer(true), &Reply::Answer(true));
        assert_eq!(
            (v.equivalent, v.decided_by, v.skipped),
            (Some(true), DecidedBy::SwapAgreement, None)
        );
        let v = combine(&Reply::Answer(true), &Reply::Answer(false));
        assert_eq!(
            (v.equivalent, v.decided_by),
            (None, DecidedBy::SwapDisagreement)
        );
        let v = combine(
            &Reply::Answer(true),
            &Reply::Skipped("reply truncated at 512 tokens".into()),
        );
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
        let rows = [
            judged(Some(true), Some(true)),
            judged(Some(false), Some(true)),
            judged(Some(false), None),
            judged(Some(false), Some(false)),
        ];
        let refs: Vec<&JudgeRow> = rows.iter().collect();
        assert_eq!(
            consistency_pct(&refs),
            Some(67),
            "2 agreements of 3 answered pairs"
        );
        assert_eq!(consistency_pct(&[]), None);
        let one = [judged(None, None)];
        assert_eq!(
            consistency_pct(&[&one[0]]),
            None,
            "nothing answered twice: no rate, not 0%"
        );
    }

    #[test]
    fn a_family_conflict_names_judge_candidate_and_family() {
        let candidates = vec![
            ("ornith-1.5-35b-a3b".to_owned(), "qwen35moe".to_owned()),
            ("minimax-m2.7".to_owned(), "minimax-m2".to_owned()),
        ];
        let err = super::family_conflict(("qwen3.8-27b", "qwen35"), &candidates).expect("conflict");
        assert!(
            matches!(err, crate::error::ChekovError::JudgeFamilyConflict { ref candidate, ref family, .. }
                if candidate == "ornith-1.5-35b-a3b" && family == "qwen35")
        );
        assert!(super::family_conflict(("gpt-oss-20b", "gpt-oss"), &candidates).is_none());
        let itself = vec![("gpt-oss-20b".to_owned(), "gpt-oss".to_owned())];
        assert!(
            super::family_conflict(("gpt-oss-20b", "gpt-oss"), &itself).is_some(),
            "a judge among the candidates conflicts with itself"
        );
    }

    fn plan_at(revision: &str) -> super::JudgePlan {
        super::JudgePlan {
            judge: crate::core::registry::Effective {
                name: "gpt-oss-20b".into(),
                ctx_size: 98_304,
                flags: vec![],
                entry: crate::core::registry::ModelEntry {
                    repo: "unsloth/gpt-oss-20b-GGUF".into(),
                    quant: "F16".into(),
                    revision: revision.to_owned(),
                    path: "models/gpt-oss-20b@d449b42d93e1".into(),
                    first_shard: "gpt-oss-20b-F16.gguf".into(),
                    hermes_ok: true,
                    ctx_size: None,
                    extra_flags: vec![],
                    role: Some(crate::core::registry::ModelRole::Judge),
                },
            },
            arch: "gpt-oss".into(),
            rubric_hash: super::rubric_hash(),
            max_tokens: 512,
            min_consistency_pct: 70,
            reasoning_effort: crate::core::config::ReasoningEffort::Low,
        }
    }

    #[test]
    fn the_plan_stamps_what_it_was_built_from() {
        let stamp = plan_at("d449b42d93e1c2c7bda5312f5c25c8fb91dfa9b4").stamp();
        assert_eq!(
            (
                stamp.model.as_str(),
                stamp.revision.as_str(),
                stamp.reasoning_effort.as_str(),
                stamp.max_tokens
            ),
            ("gpt-oss-20b", "d449b42d93e1", "low", 512)
        );
    }

    /// `revision` is hand-editable, so the twelve is twelve CHARACTERS —
    /// slicing bytes panics when one straddles the boundary.
    #[test]
    fn a_revision_is_shortened_on_a_char_boundary() {
        assert_eq!(
            plan_at("0123456789a€bcdef").stamp().revision,
            "0123456789a€"
        );
        assert_eq!(plan_at("abc").stamp().revision, "abc");
    }
}
