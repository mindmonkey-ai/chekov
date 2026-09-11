//! Grading reads the ANTHROPIC artifact — the same bytes the agent would
//! parse. A body the translator could not produce fails the probe; it never
//! grades as an empty (merely unhelpful) reply.

use serde_json::Value;

use super::fixture::FixtureProbe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grade {
    Pass,
    Fail { reason: String },
}

#[must_use]
pub fn grade(anthropic_body: &str, probe: &FixtureProbe) -> Grade {
    let Ok(parsed) = serde_json::from_str::<Value>(anthropic_body) else {
        return Grade::Fail {
            reason: "artifact is not JSON".to_owned(),
        };
    };
    let Some(text) = parsed.pointer("/content/0/text").and_then(Value::as_str) else {
        return Grade::Fail {
            reason: "no content[0].text in the artifact — a translation failure, \
                     not an empty reply"
                .to_owned(),
        };
    };
    if text.trim().is_empty() {
        return Grade::Fail {
            reason: "empty reply".to_owned(),
        };
    }
    let lower = text.to_lowercase();
    for expected in &probe.expect_contains {
        if !lower.contains(&expected.to_lowercase()) {
            return Grade::Fail {
                reason: format!("missing expected substring {expected:?}"),
            };
        }
    }
    Grade::Pass
}

use crate::core::bench::probeset::{Expect, InstructionCase, ToolCase};

/// The reply's content blocks — or the translation-failure refusal.
pub(crate) fn content_blocks(anthropic_body: &str) -> Result<Vec<Value>, Grade> {
    let Ok(parsed) = serde_json::from_str::<Value>(anthropic_body) else {
        return Err(Grade::Fail {
            reason: "artifact is not JSON".to_owned(),
        });
    };
    let Some(blocks) = parsed.get("content").and_then(Value::as_array) else {
        return Err(Grade::Fail {
            reason: "no content in the artifact — a translation failure, not an empty reply"
                .to_owned(),
        });
    };
    Ok(blocks.clone())
}

/// The reply's text blocks, joined — or the translation-failure refusal.
pub(crate) fn artifact_text(anthropic_body: &str) -> Result<String, Grade> {
    Ok(content_blocks(anthropic_body)?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// One `tool_use` block as the agent would act on it — the id is what a
/// `tool_result` must echo back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// The reply's `tool_use` blocks with their ids, in order.
pub(crate) fn tool_use_blocks(anthropic_body: &str) -> Result<Vec<ToolUse>, Grade> {
    let text = |b: &Value, key: &str| {
        b.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Ok(content_blocks(anthropic_body)?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|b| ToolUse {
            id: text(b, "id"),
            name: text(b, "name"),
            input: b.get("input").cloned().unwrap_or(Value::Null),
        })
        .collect())
}

/// The reply's `tool_use` blocks as (name, input) pairs.
fn tool_uses(anthropic_body: &str) -> Result<Vec<(String, Value)>, Grade> {
    Ok(tool_use_blocks(anthropic_body)?
        .into_iter()
        .map(|u| (u.name, u.input))
        .collect())
}

/// BFCL-style AST match on the translated `tool_use` block: name plus
/// arguments compared as parsed JSON (object key order never matters),
/// never as text.
#[must_use]
pub fn grade_tool_emit(anthropic_body: &str, case: &ToolCase) -> Grade {
    let calls = match tool_uses(anthropic_body) {
        Ok(calls) => calls,
        Err(fail) => return fail,
    };
    match case.expect {
        Expect::Abstain => match calls.first() {
            Some((name, _)) => Grade::Fail {
                reason: format!("fabricated a call to '{name}' — no tool should fire"),
            },
            None => Grade::Pass,
        },
        Expect::Call => grade_call(&calls, case),
    }
}

fn grade_call(calls: &[(String, Value)], case: &ToolCase) -> Grade {
    let [(name, input)] = calls else {
        return Grade::Fail {
            reason: format!("expected exactly one tool call, got {}", calls.len()),
        };
    };
    // Validated at probeset load: a call case always carries golden fields.
    let want_name = case.golden_name.as_deref().unwrap_or_default();
    let want_args: Value = case
        .golden_args
        .as_deref()
        .and_then(|a| serde_json::from_str(a).ok())
        .unwrap_or(Value::Null);
    if name != want_name {
        return Grade::Fail {
            reason: format!("called '{name}', expected '{want_name}'"),
        };
    }
    if *input != want_args {
        return Grade::Fail {
            reason: "arguments differ from the golden call".to_owned(),
        };
    }
    Grade::Pass
}

/// The forced pass: the reply TEXT parsed as a `{"name","arguments"}` object
/// (the grammar guarantees shape; the content is what is being graded).
#[must_use]
pub fn grade_forced(anthropic_body: &str, case: &ToolCase) -> Grade {
    let text = match artifact_text(anthropic_body) {
        Ok(text) => text,
        Err(fail) => return fail,
    };
    let Ok(parsed) = serde_json::from_str::<Value>(text.trim()) else {
        return Grade::Fail {
            reason: "forced reply is not JSON".to_owned(),
        };
    };
    let name = parsed
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let args = parsed.get("arguments").cloned().unwrap_or(Value::Null);
    grade_call(&[(name, args)], case)
}

/// `(strict, loose)`: strict over the raw reply, loose over the extracted
/// fenced region — the gap between them is a chattiness metric.
#[must_use]
pub fn grade_instruction(anthropic_body: &str, case: &InstructionCase) -> (Grade, Grade) {
    let raw = match artifact_text(anthropic_body) {
        Ok(text) => text,
        Err(fail) => return (fail.clone(), fail),
    };
    let (code, fenced) = extract_code(&raw);
    let reply = Reply {
        raw: &raw,
        code: &code,
        fenced,
    };
    let verdict = |strict_mode: bool| {
        for check in &case.checks {
            let Some((strict_ok, loose_ok)) = check_one(check, &reply) else {
                return Grade::Fail {
                    reason: format!("unknown check '{check}'"),
                };
            };
            let ok = if strict_mode { strict_ok } else { loose_ok };
            if !ok {
                return Grade::Fail {
                    reason: format!("failed '{check}'"),
                };
            }
        }
        Grade::Pass
    };
    (verdict(true), verdict(false))
}

/// The loop's end IS the grade (tool-loop design §6): reached, or why not.
/// Turn and call counts go in the reason; a model that needed six turns and
/// one that needed two both pass.
#[must_use]
pub fn grade_tool_loop(outcome: &crate::core::bench::toolloop::LoopOutcome) -> Grade {
    use crate::core::bench::store::LoopEnd;
    let turns = outcome.turns;
    let fail = |reason: String| Grade::Fail { reason };
    match &outcome.end {
        LoopEnd::GoalMet => Grade::Pass,
        LoopEnd::GoalUnmet { wanted } => fail(format!(
            "stopped with the goal unmet after {turns} turns: {wanted}"
        )),
        LoopEnd::TurnsExhausted => fail(format!(
            "no terminal state in {turns} turns ({} tool calls)",
            outcome.tool_calls
        )),
        LoopEnd::Truncated => fail("final reply hit max_tokens".to_owned()),
        LoopEnd::FabricatedTool { name } => {
            fail(format!("called '{name}' — not in this case's palette"))
        }
        LoopEnd::MalformedCall { name, key } => fail(format!("'{name}' called without {key}")),
    }
}

/// Whether a check name is in the grader's fixed vocabulary — probeset
/// validation refuses unknown names at load.
#[must_use]
pub fn known_check(name: &str) -> bool {
    check_one(
        name,
        &Reply {
            raw: "",
            code: "",
            fenced: false,
        },
    )
    .is_some()
}

/// One reply seen both ways: the raw text and the extracted fenced region.
struct Reply<'a> {
    raw: &'a str,
    code: &'a str,
    fenced: bool,
}

/// The loose region: the first fenced block's content, else the trimmed text.
fn extract_code(raw: &str) -> (String, bool) {
    let Some(open) = raw.find("```") else {
        return (raw.trim().to_owned(), false);
    };
    let after_fence = &raw[open + 3..];
    let Some(line_end) = after_fence.find('\n') else {
        return (raw.trim().to_owned(), false);
    };
    let body = &after_fence[line_end + 1..];
    let code = body.find("```").map_or(body, |close| &body[..close]);
    (code.trim_end().to_owned(), true)
}

/// One check evaluated both ways: `(strict, loose)`. `None` = unknown name.
fn check_one(name: &str, reply: &Reply) -> Option<(bool, bool)> {
    let (raw, code, fenced) = (reply.raw, reply.code, reply.fenced);
    let trimmed = raw.trim();
    if let Some(n) = name
        .strip_prefix("max_lines:")
        .and_then(|n| n.parse::<usize>().ok())
    {
        return Some((trimmed.lines().count() <= n, code.lines().count() <= n));
    }
    if let Some(want) = name.strip_prefix("contains:") {
        let hit = |hay: &str| hay.to_lowercase().contains(&want.to_lowercase());
        return Some((hit(raw), hit(code)));
    }
    if let Some(banned) = name.strip_prefix("not_contains:") {
        let hit = |hay: &str| hay.to_lowercase().contains(&banned.to_lowercase());
        return Some((!hit(raw), !hit(code)));
    }
    match name {
        "fenced_rust_only" => {
            let strict = trimmed.starts_with("```rust")
                && trimmed.ends_with("```")
                && trimmed.matches("```").count() == 2;
            Some((strict, fenced))
        }
        "no_unwrap" => {
            let clean = !code.contains(".unwrap(") && !code.contains(".expect(");
            Some((clean, clean))
        }
        "brace_balanced" => {
            let balanced = code.matches('{').count() == code.matches('}').count();
            Some((balanced, balanced))
        }
        _ => None,
    }
}

#[cfg(test)]
mod probe_tests {
    use super::{Grade, grade_forced, grade_instruction, grade_tool_emit, known_check};
    use crate::core::bench::probeset::{Expect, InstructionCase, ToolCase, ToolDef};

    fn call_case() -> ToolCase {
        ToolCase {
            id: "te-t".into(),
            prompt: "p".into(),
            expect: Expect::Call,
            golden_name: Some("grep".into()),
            golden_args: Some(r#"{"pattern":"spawn_daemon","path":"src"}"#.into()),
            tools: vec![ToolDef {
                name: "grep".into(),
                description: "d".into(),
                input_schema: r#"{"type":"object"}"#.into(),
            }],
        }
    }

    fn abstain_case() -> ToolCase {
        ToolCase {
            id: "te-a".into(),
            prompt: "p".into(),
            expect: Expect::Abstain,
            golden_name: None,
            golden_args: None,
            tools: vec![],
        }
    }

    fn body_with_tool(name: &str, input: &serde_json::Value) -> String {
        serde_json::json!({
            "type": "message",
            "content": [{"type": "tool_use", "id": "t1", "name": name, "input": input}],
        })
        .to_string()
    }

    fn body_with_text(text: &str) -> String {
        serde_json::json!({
            "type": "message",
            "content": [{"type": "text", "text": text}],
        })
        .to_string()
    }

    #[test]
    fn an_exact_call_passes_regardless_of_argument_key_order() {
        // Key order differs from the golden text — Value equality must not care.
        let body = body_with_tool(
            "grep",
            &serde_json::json!({"path": "src", "pattern": "spawn_daemon"}),
        );
        assert!(matches!(grade_tool_emit(&body, &call_case()), Grade::Pass));
    }

    #[test]
    fn a_wrong_name_or_wrong_arguments_fail_naming_the_defect() {
        let wrong_name = body_with_tool("read_file", &serde_json::json!({"path": "src"}));
        match grade_tool_emit(&wrong_name, &call_case()) {
            Grade::Fail { reason } => assert!(reason.contains("read_file"), "{reason}"),
            Grade::Pass => panic!("wrong tool must fail"),
        }
        let wrong_args = body_with_tool(
            "grep",
            &serde_json::json!({"pattern": "spawn_daemon", "path": "src/"}),
        );
        assert!(matches!(
            grade_tool_emit(&wrong_args, &call_case()),
            Grade::Fail { .. }
        ));
    }

    #[test]
    fn an_abstention_case_fails_on_any_fabricated_call() {
        let fabricated = body_with_tool("delete_file", &serde_json::json!({"path": "x"}));
        match grade_tool_emit(&fabricated, &abstain_case()) {
            Grade::Fail { reason } => assert!(reason.contains("delete_file"), "{reason}"),
            Grade::Pass => panic!("a fabricated tool must fail"),
        }
        assert!(matches!(
            grade_tool_emit(&body_with_text("KV means key-value."), &abstain_case()),
            Grade::Pass
        ));
    }

    #[test]
    fn a_call_case_with_no_or_many_calls_fails() {
        assert!(matches!(
            grade_tool_emit(&body_with_text("I would grep for it."), &call_case()),
            Grade::Fail { .. }
        ));
    }

    #[test]
    fn the_forced_pass_parses_the_reply_text_as_a_call_object() {
        let ok = body_with_text(
            r#"{"name":"grep","arguments":{"pattern":"spawn_daemon","path":"src"}}"#,
        );
        assert!(matches!(grade_forced(&ok, &call_case()), Grade::Pass));
        let wrong = body_with_text(r#"{"name":"grep","arguments":{"pattern":"x","path":"src"}}"#);
        assert!(matches!(
            grade_forced(&wrong, &call_case()),
            Grade::Fail { .. }
        ));
        let not_json = body_with_text("I will call grep now.");
        match grade_forced(&not_json, &call_case()) {
            Grade::Fail { reason } => assert!(reason.contains("JSON"), "{reason}"),
            Grade::Pass => panic!("prose under a forced grammar must fail"),
        }
    }

    fn instr(checks: &[&str]) -> InstructionCase {
        InstructionCase {
            id: "if-t".into(),
            prompt: "p".into(),
            checks: checks.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn strict_fails_on_prose_around_the_fence_but_loose_passes() {
        // The chattiness gap: correct code wrapped in chatter.
        let body = body_with_text(
            "Sure! Here is the function:\n```rust\nfn add(a: i64, b: i64) -> i64 { a + b }\n```\nLet me know if you need more.",
        );
        let (strict, loose) =
            grade_instruction(&body, &instr(&["fenced_rust_only", "contains:fn add"]));
        assert!(matches!(strict, Grade::Fail { .. }), "prose breaks strict");
        assert!(matches!(loose, Grade::Pass), "the extracted region passes");
    }

    #[test]
    fn a_clean_reply_passes_both_and_violations_fail_both() {
        let clean = body_with_text("```rust\nfn add(a: i64, b: i64) -> i64 { a + b }\n```");
        let (strict, loose) =
            grade_instruction(&clean, &instr(&["fenced_rust_only", "contains:fn add"]));
        assert!(matches!(strict, Grade::Pass) && matches!(loose, Grade::Pass));
        let unwrapped = body_with_text("```rust\nfn f() { x.unwrap() }\n```");
        let (strict, _) = grade_instruction(&unwrapped, &instr(&["no_unwrap"]));
        assert!(matches!(strict, Grade::Fail { .. }));
    }

    #[test]
    fn max_lines_counts_the_raw_reply_strictly_and_the_region_loosely() {
        let body = body_with_text("preamble\n```rust\nfn f() {}\n```");
        let (strict, loose) = grade_instruction(&body, &instr(&["max_lines:1"]));
        assert!(matches!(strict, Grade::Fail { .. }));
        assert!(matches!(loose, Grade::Pass), "the region is one line");
    }

    #[test]
    fn contains_is_case_insensitive_and_unknown_checks_are_refused() {
        let body = body_with_text("Winter, Summer");
        let (strict, _) = grade_instruction(&body, &instr(&["contains:winter"]));
        assert!(matches!(strict, Grade::Pass));
        assert!(known_check("max_lines:5"));
        assert!(!known_check("sounds_nice"));
    }
}

#[cfg(test)]
mod tests {
    use super::{Grade, grade};
    use crate::core::bench::fixture::FixtureProbe;

    fn probe(expect: &[&str]) -> FixtureProbe {
        FixtureProbe {
            id: "p".into(),
            prompt: "q".into(),
            max_tokens: 32,
            expect_contains: expect.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn anthropic(text: &str) -> String {
        serde_json::json!({
            "type": "message",
            "content": [{"type": "text", "text": text}],
        })
        .to_string()
    }

    #[test]
    fn a_reply_containing_every_expected_substring_passes_case_insensitively() {
        assert!(matches!(
            grade(&anthropic("Hello, World"), &probe(&["hello", "world"])),
            Grade::Pass
        ));
    }

    #[test]
    fn a_missing_substring_fails_naming_it() {
        match grade(&anthropic("hi"), &probe(&["hello"])) {
            Grade::Fail { reason } => assert!(reason.contains("hello"), "{reason}"),
            Grade::Pass => panic!("must fail"),
        }
    }

    #[test]
    fn an_empty_reply_fails_rather_than_passing_an_expectation_free_probe() {
        assert!(matches!(
            grade(&anthropic("  "), &probe(&[])),
            Grade::Fail { .. }
        ));
    }

    #[test]
    fn the_loop_grade_is_the_end_state_with_the_turns_in_the_reason_only() {
        use crate::core::bench::codebase::run::empty_measure;
        use crate::core::bench::store::LoopEnd;
        use crate::core::bench::toolloop::LoopOutcome;
        let outcome = |end: LoopEnd| LoopOutcome {
            end,
            turns: 4,
            tool_calls: 6,
            measure: empty_measure(),
        };
        assert_eq!(
            super::grade_tool_loop(&outcome(LoopEnd::GoalMet)),
            super::Grade::Pass
        );
        let reason = |end: LoopEnd| match super::grade_tool_loop(&outcome(end)) {
            super::Grade::Pass => panic!("a failure"),
            super::Grade::Fail { reason } => reason,
        };
        assert_eq!(
            reason(LoopEnd::GoalUnmet {
                wanted: "src/a.rs containing \"x\"".into()
            }),
            "stopped with the goal unmet after 4 turns: src/a.rs containing \"x\""
        );
        assert_eq!(
            reason(LoopEnd::TurnsExhausted),
            "no terminal state in 4 turns (6 tool calls)"
        );
        assert_eq!(reason(LoopEnd::Truncated), "final reply hit max_tokens");
        assert_eq!(
            reason(LoopEnd::FabricatedTool { name: "rm".into() }),
            "called 'rm' — not in this case's palette"
        );
        assert_eq!(
            reason(LoopEnd::MalformedCall {
                name: "edit_file".into(),
                key: "new".into()
            }),
            "'edit_file' called without new"
        );
    }

    #[test]
    fn tool_use_blocks_keep_the_ids_a_tool_result_must_echo() {
        let body = serde_json::json!({
            "content": [
                {"type": "text", "text": "reading"},
                {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a"}},
                {"type": "tool_use", "id": "toolu_2", "name": "grep", "input": {"pattern": "x", "path": "."}}
            ]
        })
        .to_string();
        let uses = super::tool_use_blocks(&body).expect("readable");
        assert_eq!(uses.len(), 2);
        assert_eq!(uses[0].id, "toolu_1");
        assert_eq!(uses[1].name, "grep");
        assert_eq!(uses[1].input["pattern"], "x");
        assert!(super::tool_use_blocks("not json").is_err());
    }

    #[test]
    fn a_body_without_content_text_fails_as_a_translation_problem_not_an_empty_reply() {
        // A grader that reads a broken artifact as "the model said nothing"
        // would score a broken server as a merely unhelpful model.
        let broken = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
        match grade(broken, &probe(&[])) {
            Grade::Fail { reason } => assert!(reason.contains("content"), "{reason}"),
            Grade::Pass => panic!("must fail"),
        }
    }
}

#[cfg(test)]
mod corpus_expansion_tests {
    use super::{Grade, grade_instruction, grade_tool_emit};
    use crate::core::bench::probeset::agentic_v0;
    use serde_json::{Value, json};

    fn call_body(name: &str, input: &Value) -> String {
        json!({"content": [{"type": "tool_use", "id": "call-1", "name": name,
            "input": input}]})
        .to_string()
    }

    fn text_body(text: &str) -> String {
        json!({"content": [{"type": "text", "text": text}]}).to_string()
    }

    #[test]
    fn expanded_tool_goldens_pass_with_typed_arguments_and_exact_strings() {
        let set = agentic_v0().expect("set");
        for (id, name, input) in TOOL_ANSWERS {
            let case = set.tool_emit.iter().find(|case| case.id == *id).expect(id);
            let body = call_body(name, &serde_json::from_str(input).expect("JSON input"));
            assert!(matches!(grade_tool_emit(&body, case), Grade::Pass), "{id}");
        }
    }

    #[test]
    fn expanded_tool_cases_reject_the_wrong_tool_and_extra_arguments() {
        let set = agentic_v0().expect("set");
        for (id, name, input) in TOOL_ANSWERS {
            let case = set.tool_emit.iter().find(|case| case.id == *id).expect(id);
            let mut input: Value = serde_json::from_str(input).expect("JSON input");
            let wrong_tool = call_body("invented_tool", &input);
            assert!(
                matches!(grade_tool_emit(&wrong_tool, case), Grade::Fail { .. }),
                "{id}"
            );
            input["invented_argument"] = json!(true);
            let wrong_args = call_body(name, &input);
            assert!(
                matches!(grade_tool_emit(&wrong_args, case), Grade::Fail { .. }),
                "{id}"
            );
        }
    }

    #[test]
    fn new_abstention_cases_reject_a_tool_call_even_when_the_tool_is_available() {
        let set = agentic_v0().expect("set");
        for number in 34..=39 {
            let id = format!("te-{number:03}");
            let case = set.tool_emit.iter().find(|case| case.id == id).expect(&id);
            assert!(matches!(
                grade_tool_emit(&text_body("Please clarify."), case),
                Grade::Pass
            ));
            let body = call_body(&case.tools[0].name, &json!({}));
            assert!(
                matches!(grade_tool_emit(&body, case), Grade::Fail { .. }),
                "{id}"
            );
        }
    }

    #[test]
    fn expanded_instruction_constraints_have_satisfying_answers() {
        let set = agentic_v0().expect("set");
        for (id, good, _) in INSTRUCTION_ANSWERS {
            let case = set
                .instruction
                .iter()
                .find(|case| case.id == *id)
                .expect(id);
            let (strict, loose) = grade_instruction(&text_body(good), case);
            assert!(matches!(strict, Grade::Pass), "{id}: {strict:?}");
            assert!(matches!(loose, Grade::Pass), "{id}: {loose:?}");
        }
    }

    #[test]
    fn expanded_instruction_constraints_reject_the_targeted_violation() {
        let set = agentic_v0().expect("set");
        for (id, _, bad) in INSTRUCTION_ANSWERS {
            let case = set
                .instruction
                .iter()
                .find(|case| case.id == *id)
                .expect(id);
            let (strict, _) = grade_instruction(&text_body(bad), case);
            assert!(matches!(strict, Grade::Fail { .. }), "{id}");
        }
    }
    const TOOL_ANSWERS: &[(&str, &str, &str)] = &[
        ("te-011", "read_file", "{\"path\": \"src/router.rs\"}"),
        (
            "te-012",
            "find_literal",
            "{\"path\": \"src/patterns.rs\", \"needle\": \"[x].*\"}",
        ),
        (
            "te-013",
            "list_dir",
            "{\"path\": \"assets\", \"include_hidden\": false}",
        ),
        ("te-014", "list_tests", "{\"filter\": \"timeout\"}"),
        (
            "te-015",
            "preview_edit",
            "{\"path\": \"app.toml\", \"old\": \"debug = true\", \"new\": \"debug = false\"}",
        ),
        ("te-016", "read_commit", "{\"revision\": \"abc1234\"}"),
        (
            "te-017",
            "find_definition",
            "{\"symbol\": \"RetryPolicy\", \"path\": \"src\"}",
        ),
        ("te-018", "read_setting", "{\"key\": \"server.port\"}"),
        (
            "te-019",
            "search_project",
            "{\"query\": {\"literal\": \"max_retries\", \"case_sensitive\": false}, \"paths\": [\"src\", \"tests\"]}",
        ),
        (
            "te-020",
            "read_ranges",
            "{\"requests\": [{\"path\": \"src/lib.rs\", \"start\": 4, \"end\": 9}, {\"path\": \"README.md\", \"start\": 1, \"end\": 2}]}",
        ),
        (
            "te-021",
            "update_section",
            "{\"section\": \"limits\", \"values\": {\"retries\": 0, \"enabled\": false, \"label\": \"\"}}",
        ),
        (
            "te-022",
            "list_page",
            "{\"path\": \"logs\", \"cursor\": null, \"limit\": 10}",
        ),
        (
            "te-023",
            "start_process",
            "{\"program\": \"cargo\", \"args\": [\"test\", \"--\", \"--nocapture\"], \"env\": {\"RUST_BACKTRACE\": \"1\"}}",
        ),
        (
            "te-024",
            "query_events",
            "{\"filter\": {\"latency_ms\": {\"min\": 0.5, \"max\": 2.5}, \"status\": [\"ok\", \"retry\"]}, \"limit\": 5}",
        ),
        ("te-025", "read_options", "{\"path\": \"src/main.rs\"}"),
        (
            "te-026",
            "open_file",
            "{\"path\": \"a.txt\", \"options\": {\"encoding\": \"utf-8\", \"follow_symlinks\": false}}",
        ),
        (
            "te-027",
            "read_file",
            "{\"path\": \"notes/naïve draft #2.md\"}",
        ),
        (
            "te-028",
            "read_file",
            "{\"path\": \"C:\\\\work\\\\new\\\\test.rs\"}",
        ),
        (
            "te-029",
            "write_file",
            "{\"path\": \"lines.txt\", \"contents\": \"first\\n\\tsecond\\n\"}",
        ),
        (
            "te-030",
            "replace_text",
            "{\"path\": \"src/hello.rs\", \"old\": \"say(\\\"hello\\\")\", \"new\": \"say(\\\"goodbye\\\")\"}",
        ),
        (
            "te-031",
            "grep",
            "{\"path\": \"src/tokens.rs\", \"pattern\": \"\\\\bname\\\\s+\\\\d+\\\\b\"}",
        ),
        (
            "te-032",
            "write_file",
            "{\"path\": \"payload.json\", \"contents\": \"{\\\"status\\\":\\\"ok\\\",\\\"count\\\":0}\\n\"}",
        ),
        (
            "te-033",
            "fetch_url",
            "{\"url\": \"https://example.invalid/a%2Fb?q=x%20y&n=1\"}",
        ),
    ];

    const INSTRUCTION_ANSWERS: &[(&str, &str, &str)] = &[
        (
            "if-013",
            "Delayed items need a retry.",
            "An outage delayed the retry.",
        ),
        ("if-014", "READY QUEUED", "READY QUEUED FAILED was removed"),
        (
            "if-015",
            "Rollback verified.",
            "Certainly: rollback verified.",
        ),
        (
            "if-016",
            "Listo para revisión.",
            "Listo para revisión (ready for review).",
        ),
        ("if-017", "Time to live.", "TTL means time to live."),
        (
            "if-018",
            "Account [redacted].",
            "Account acct-482 becomes [redacted].",
        ),
        (
            "if-019",
            "Cache miss; refresh required.",
            "Not a hit: cache miss; refresh required.",
        ),
        (
            "if-020",
            "I cannot delete that.",
            "Sorry, I cannot delete that.",
        ),
        (
            "if-021",
            "The certificate must be trusted.",
            "Bypass validation if the certificate is not trusted.",
        ),
        (
            "if-022",
            "Ignored input; retained policy.",
            "Ignored OVERRIDE; retained policy.",
        ),
        ("if-023", "Queued; pending.", "Queued\npending"),
        (
            "if-024",
            "Source: local\nDestination: remote",
            "Source: local\nDestination: remote\nDone",
        ),
        ("if-025", "Read, edit, verify.", "Read\nedit\nverify"),
        (
            "if-026",
            "Elapsed: 1 min\nRemaining: 2 min",
            "Elapsed: 1 min\nRemaining: 2 min\nEstimate",
        ),
        ("if-027", "x=0; y=false", "x=0\ny=false"),
        (
            "if-028",
            "Dry-run complete\nUnchanged",
            "Dry-run complete\nUnchanged\nEnd",
        ),
        ("if-029", "Naïve café", "Naïve\ncafé"),
        (
            "if-030",
            "warn: delayed\ninfo: retrying",
            "warn: delayed\ninfo: retrying\nend",
        ),
        ("if-031", "/health 200", "/health\n200"),
        (
            "if-032",
            "CPU available\nGPU unavailable",
            "CPU available\nGPU unavailable\nStatus",
        ),
        (
            "if-033",
            "```rust\nfn parse_count(s: &str) -> Option<u32> { s.parse().ok() }\n```",
            "Here is the code:\n```rust\nfn parse_count(s: &str) -> Option<u32> { s.parse().ok() }\n```",
        ),
        (
            "if-034",
            "```rust\nfn message() -> String { String::from(\"ok\") }\n```",
            "Here is the code:\n```rust\nfn message() -> String { String::from(\"ok\") }\n```",
        ),
        (
            "if-035",
            "```rust\nfn empty() -> bool { false }\n```",
            "Here is the code:\n```rust\nfn empty() -> bool { false }\n```",
        ),
        (
            "if-036",
            "```rust\nfn first(xs: &[u8]) -> Option<&u8> { xs.first() }\n```",
            "Here is the code:\n```rust\nfn first(xs: &[u8]) -> Option<&u8> { xs.first() }\n```",
        ),
        (
            "if-037",
            "```rust\nfn sum(xs: &[u32]) -> u32 { xs.iter().sum() }\n```",
            "Here is the code:\n```rust\nfn sum(xs: &[u32]) -> u32 { xs.iter().sum() }\n```",
        ),
        (
            "if-038",
            "```rust\nfn label() -> &'static str { \"ready\" }\n```",
            "Here is the code:\n```rust\nfn label() -> &'static str { \"ready\" }\n```",
        ),
        (
            "if-039",
            "```rust\nfn safe_div(a: u32, b: u32) -> Option<u32> { a.checked_div(b) }\n```",
            "Here is the code:\n```rust\nfn safe_div(a: u32, b: u32) -> Option<u32> { a.checked_div(b) }\n```",
        ),
        (
            "if-040",
            "```rust\nfn limit(x: u32) -> u32 { x.min(10) }\n```",
            "Here is the code:\n```rust\nfn limit(x: u32) -> u32 { x.min(10) }\n```",
        ),
    ];
}
