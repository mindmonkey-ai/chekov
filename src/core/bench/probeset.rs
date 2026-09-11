//! The compiled-in probe set (spec §7.2) — typed, validated, content-hashed.
//!
//! The TOML text is hashed into every agentic run's `prompt_set_hash`, so an
//! edited case makes old runs incomparable BY CONSTRUCTION. Validation is
//! loud at load: a malformed set must fail the build's tests, never grade.

use serde::Deserialize;

use crate::error::ChekovError;

const AGENTIC_V0: &str = include_str!("agentic_v0.toml");
const SUPPORTED_VERSION: u32 = 0;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeSet {
    pub version: u32,
    /// The system text every `tool_loop` turn carries — in the set, so the
    /// content hash covers it.
    pub loop_system: String,
    pub tool_emit: Vec<ToolCase>,
    pub instruction: Vec<InstructionCase>,
    pub tool_loop: Vec<LoopCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCase {
    pub id: String,
    pub prompt: String,
    pub expect: Expect,
    #[serde(default)]
    pub golden_name: Option<String>,
    /// The expected arguments as JSON text; compared as parsed values.
    #[serde(default)]
    pub golden_args: Option<String>,
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expect {
    Call,
    Abstain,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema as text; parsed where used.
    pub input_schema: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionCase {
    pub id: String,
    pub prompt: String,
    /// Check names from the grader's fixed vocabulary; an unknown name is a
    /// load-time refusal, never a silent pass.
    pub checks: Vec<String>,
}

/// A `tool_loop` case (tool-loop design §3): a canned repository, a task, a
/// palette, and the terminal state that counts as done.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopCase {
    pub id: String,
    pub prompt: String,
    pub files: Vec<CannedFile>,
    pub goal: Goal,
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CannedFile {
    pub path: String,
    pub text: String,
}

/// What "done" means for a loop case.
///
/// `Edited`: the named file carries one of `contains_any` (alternatives,
/// because two correct spellings of one edit must both pass) and every
/// `untouched` file is byte-identical to its canned copy. `Unchanged`: no
/// file differs and the final reply names `reply_mentions`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Goal {
    Edited {
        file: String,
        contains_any: Vec<String>,
        #[serde(default)]
        untouched: Vec<String>,
        /// What `run_tests` answers before the goal is met. Its presence is
        /// what puts `run_tests` in the palette.
        #[serde(default)]
        tests_fail: Option<String>,
    },
    Unchanged {
        reply_mentions: String,
    },
}

/// The tools the canned environment can answer (`toolloop::ToolEnv`). A
/// palette naming anything else is refused at load, never answered wrong.
pub(crate) const CANNED_TOOLS: [&str; 5] =
    ["read_file", "list_dir", "grep", "edit_file", "run_tests"];

/// The v0 set, validated. Loud on any defect — a malformed case must never
/// silently grade.
pub fn agentic_v0() -> Result<ProbeSet, ChekovError> {
    parse(AGENTIC_V0)
}

/// Any set's text, parsed and validated — the compiled-in one, or a test's.
pub(crate) fn parse(text: &str) -> Result<ProbeSet, ChekovError> {
    let set: ProbeSet = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    if set.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "version {} — this chekov reads {SUPPORTED_VERSION}",
            set.version
        )));
    }
    validate_tool_cases(&set)?;
    validate_checks(&set)?;
    validate_loop_cases(&set)?;
    validate_ids(&set)?;
    Ok(set)
}

/// Every instruction check must be in the grader's vocabulary — an unknown
/// name grading as a silent pass would be an invented result.
fn validate_checks(set: &ProbeSet) -> Result<(), ChekovError> {
    for case in &set.instruction {
        for check in &case.checks {
            if !crate::core::bench::grade::known_check(check) {
                return Err(invalid(format!("{}: unknown check '{check}'", case.id)));
            }
        }
    }
    Ok(())
}

/// sha256 of the TOML text — the agentic component of `prompt_set_hash`.
#[must_use]
pub fn content_hash() -> String {
    crate::core::hash::sha256_hex(AGENTIC_V0.as_bytes())[..12].to_owned()
}

/// The forced-pass grammar for one case: a `{"name","arguments"}` object
/// constrained to the case's OWN palette — one `oneOf` arm per tool, each
/// pinning the name and that tool's argument schema.
#[must_use]
pub fn forced_schema(case: &ToolCase) -> serde_json::Value {
    let arms: Vec<serde_json::Value> = case
        .tools
        .iter()
        .map(|tool| {
            let schema: serde_json::Value = serde_json::from_str(&tool.input_schema)
                .unwrap_or_else(|_| serde_json::json!({"type": "object"}));
            serde_json::json!({
                "type": "object",
                "properties": { "name": { "const": tool.name }, "arguments": schema },
                "required": ["name", "arguments"],
            })
        })
        .collect();
    serde_json::json!({ "oneOf": arms })
}

const fn invalid(reason: String) -> ChekovError {
    ChekovError::BenchProbeSetInvalid { reason }
}

/// A `call` case must name a golden tool from its own palette with parseable
/// golden arguments and a parseable schema per tool.
fn validate_tool_cases(set: &ProbeSet) -> Result<(), ChekovError> {
    for case in &set.tool_emit {
        for tool in &case.tools {
            serde_json::from_str::<serde_json::Value>(&tool.input_schema).map_err(|e| {
                invalid(format!(
                    "{}: tool {} schema is not JSON: {e}",
                    case.id, tool.name
                ))
            })?;
        }
        if case.expect == Expect::Call {
            let name = case
                .golden_name
                .as_deref()
                .ok_or_else(|| invalid(format!("{}: call case without golden_name", case.id)))?;
            if !case.tools.iter().any(|t| t.name == name) {
                return Err(invalid(format!(
                    "{}: golden tool '{name}' is not in the case's own palette",
                    case.id
                )));
            }
            let args = case
                .golden_args
                .as_deref()
                .ok_or_else(|| invalid(format!("{}: call case without golden_args", case.id)))?;
            serde_json::from_str::<serde_json::Value>(args)
                .map_err(|e| invalid(format!("{}: golden_args is not JSON: {e}", case.id)))?;
        }
    }
    Ok(())
}

/// Every loop case must be answerable from the palette it offers and unmet
/// at turn zero — a goal the canned files already satisfy would grade a
/// model that did nothing as done.
fn validate_loop_cases(set: &ProbeSet) -> Result<(), ChekovError> {
    for case in &set.tool_loop {
        for tool in &case.tools {
            serde_json::from_str::<serde_json::Value>(&tool.input_schema).map_err(|e| {
                invalid(format!(
                    "{}: tool {} schema is not JSON: {e}",
                    case.id, tool.name
                ))
            })?;
            if !CANNED_TOOLS.contains(&tool.name.as_str()) {
                return Err(invalid(format!(
                    "{}: '{}' has no canned behaviour in the loop environment",
                    case.id, tool.name
                )));
            }
        }
        validate_goal(case)?;
    }
    Ok(())
}

fn validate_goal(case: &LoopCase) -> Result<(), ChekovError> {
    let offers = |name: &str| case.tools.iter().any(|t| t.name == name);
    match &case.goal {
        Goal::Edited {
            file,
            contains_any,
            untouched,
            tests_fail,
        } => {
            validate_edit_target(case, file, contains_any)?;
            if let Some(missing) = untouched.iter().find(|p| canned_text(case, p).is_none()) {
                return Err(invalid(format!(
                    "{}: untouched file '{missing}' is not in the case's files",
                    case.id
                )));
            }
            if !offers("edit_file") {
                return Err(invalid(format!(
                    "{}: an edited goal needs edit_file in the palette",
                    case.id
                )));
            }
            if tests_fail.is_some() != offers("run_tests") {
                return Err(invalid(format!(
                    "{}: run_tests is in the palette exactly when tests_fail is set",
                    case.id
                )));
            }
        }
        Goal::Unchanged { .. } => {
            if offers("run_tests") {
                return Err(invalid(format!(
                    "{}: an unchanged goal has no tests to run",
                    case.id
                )));
            }
        }
    }
    Ok(())
}

/// The goal file exists in the case and does not already carry the answer.
fn validate_edit_target(case: &LoopCase, file: &str, wanted: &[String]) -> Result<(), ChekovError> {
    let text = canned_text(case, file).ok_or_else(|| {
        invalid(format!(
            "{}: goal file '{file}' is not in the case's files",
            case.id
        ))
    })?;
    if wanted.is_empty() {
        return Err(invalid(format!("{}: contains_any is empty", case.id)));
    }
    if let Some(present) = wanted.iter().find(|w| text.contains(w.as_str())) {
        return Err(invalid(format!(
            "{}: goal text {present:?} is already in '{file}'",
            case.id
        )));
    }
    Ok(())
}

pub(crate) fn canned_text<'a>(case: &'a LoopCase, path: &str) -> Option<&'a str> {
    case.files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.text.as_str())
}

fn validate_ids(set: &ProbeSet) -> Result<(), ChekovError> {
    let mut seen = std::collections::BTreeSet::new();
    let ids = set
        .tool_emit
        .iter()
        .map(|c| &c.id)
        .chain(set.instruction.iter().map(|c| &c.id))
        .chain(set.tool_loop.iter().map(|c| &c.id));
    for id in ids {
        if !seen.insert(id.clone()) {
            return Err(invalid(format!("duplicate case id '{id}'")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Expect, ProbeSet, agentic_v0, content_hash};

    #[test]
    fn the_expansion_preserves_every_legacy_question_and_loop() {
        let body = super::AGENTIC_V0
            .split_once("version = 0")
            .expect("schema")
            .1;
        let legacy = body
            .split_once("# Corpus expansion 2026-09-10")
            .map_or(body, |v| v.0);
        assert_eq!(
            crate::core::hash::sha256_hex(legacy.as_bytes()),
            "422554c22c2975c115da0167b67a51efcdb69db0eaca4bcbf5129ea960d1e79e"
        );
    }

    #[test]
    fn the_expansion_changes_the_old_agentic_workload_identity() {
        assert_ne!(content_hash(), "6e2669a1c242");
    }

    #[test]
    fn the_expanded_case_ids_are_contiguous_and_the_new_abstentions_have_no_goldens() {
        let set = agentic_v0().expect("valid set");
        let tool_ids: Vec<_> = set.tool_emit.iter().map(|c| c.id.clone()).collect();
        assert_eq!(
            tool_ids,
            (1..=39).map(|n| format!("te-{n:03}")).collect::<Vec<_>>()
        );
        let instruction_ids: Vec<_> = set.instruction.iter().map(|c| c.id.clone()).collect();
        assert_eq!(
            instruction_ids,
            (1..=40).map(|n| format!("if-{n:03}")).collect::<Vec<_>>()
        );
        for case in set.tool_emit.iter().skip(33) {
            assert_eq!(case.expect, Expect::Abstain);
            assert!(
                case.golden_name.is_none() && case.golden_args.is_none(),
                "{}",
                case.id
            );
        }
    }

    #[test]
    fn the_expanded_set_parses_with_the_approved_counts() {
        let set = agentic_v0().expect("the compiled-in set is valid");
        assert_eq!(set.version, 0);
        assert_eq!(set.tool_emit.len(), 39, "30 calls + 9 abstentions");
        assert_eq!(
            set.tool_emit
                .iter()
                .filter(|c| c.expect == Expect::Call)
                .count(),
            30
        );
        assert_eq!(set.instruction.len(), 40);
    }

    #[test]
    fn every_call_case_golden_tool_is_in_its_own_palette() {
        // agentic_v0() itself enforces this; the test pins that the shipped
        // content actually satisfies it (a content edit fails here, loudly).
        let set = agentic_v0().expect("valid");
        for case in set.tool_emit.iter().filter(|c| c.expect == Expect::Call) {
            let name = case.golden_name.as_deref().expect("call has golden");
            assert!(
                case.tools.iter().any(|t| t.name == name),
                "{}: {name} missing from palette",
                case.id
            );
        }
    }

    #[test]
    fn the_forced_schema_has_one_arm_per_palette_tool() {
        let set = agentic_v0().expect("valid");
        let case = set
            .tool_emit
            .iter()
            .find(|c| c.id == "te-002")
            .expect("te-002 exists");
        let schema = super::forced_schema(case);
        let arms = schema["oneOf"].as_array().expect("oneOf");
        assert_eq!(arms.len(), 2, "read_file and grep");
        assert_eq!(arms[1]["properties"]["name"]["const"], "grep");
        assert_eq!(
            arms[1]["properties"]["arguments"]["required"][0], "pattern",
            "the tool's own schema is embedded"
        );
    }

    #[test]
    fn the_content_hash_is_stable_and_twelve_hex() {
        let h = content_hash();
        assert_eq!(h.len(), 12);
        assert_eq!(h, content_hash());
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// A one-case set for validation tests: the goal and palette are the
    /// variable parts; the file is `src/a.rs` holding `const A: u32 = 3;`.
    fn loop_set(goal: &str, tools: &[&str]) -> Result<ProbeSet, crate::error::ChekovError> {
        let palette: String = tools
            .iter()
            .map(|name| {
                let schema = match *name {
                    "grep" => r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern","path"]}"#,
                    "edit_file" => r#"{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}"#,
                    "run_tests" => r#"{"type":"object","properties":{"filter":{"type":"string"}},"required":["filter"]}"#,
                    _ => r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
                };
                format!("[[tool_loop.tools]]\nname = \"{name}\"\ndescription = \"d\"\ninput_schema = '{schema}'\n")
            })
            .collect::<Vec<String>>()
            .concat();
        super::parse(&format!(
            "version = 0\nloop_system = \"s\"\ntool_emit = []\ninstruction = []\n\
             [[tool_loop]]\nid = \"tl-x\"\nprompt = \"p\"\n\
             [[tool_loop.files]]\npath = \"src/a.rs\"\ntext = \"const A: u32 = 3;\\n\"\n\
             [tool_loop.goal]\n{goal}\n{palette}"
        ))
    }

    const EDITED: &str =
        "kind = \"edited\"\nfile = \"src/a.rs\"\ncontains_any = [\"const A: u32 = 5;\"]";

    #[test]
    fn the_shipped_loop_cases_parse_with_the_seed_count() {
        let set = agentic_v0().expect("valid");
        assert_eq!(set.tool_loop.len(), 6);
        assert!(
            !set.loop_system.is_empty(),
            "the system text rides in the set"
        );
        assert!(set.tool_loop.iter().all(|c| c.id.starts_with("tl-")));
    }

    #[test]
    fn a_loop_goal_already_met_by_the_canned_files_is_refused() {
        let err = loop_set(
            "kind = \"edited\"\nfile = \"src/a.rs\"\ncontains_any = [\"const A: u32 = 3;\"]",
            &["read_file", "edit_file"],
        )
        .expect_err("a met goal grades doing nothing as done");
        assert!(err.to_string().contains("already in 'src/a.rs'"), "{err}");
    }

    #[test]
    fn a_loop_goal_naming_a_file_the_case_lacks_is_refused() {
        let err = loop_set(
            "kind = \"edited\"\nfile = \"src/b.rs\"\ncontains_any = [\"x\"]",
            &["read_file", "edit_file"],
        )
        .expect_err("no such canned file");
        assert!(
            err.to_string()
                .contains("'src/b.rs' is not in the case's files"),
            "{err}"
        );
    }

    #[test]
    fn an_edited_goal_needs_edit_file_and_run_tests_exactly_with_tests_fail() {
        let err = loop_set(EDITED, &["read_file"]).expect_err("no edit_file");
        assert!(err.to_string().contains("needs edit_file"), "{err}");
        let err = loop_set(EDITED, &["read_file", "edit_file", "run_tests"])
            .expect_err("run_tests without tests_fail");
        assert!(err.to_string().contains("exactly when tests_fail"), "{err}");
        let with_tests = format!("{EDITED}\ntests_fail = \"test a ... FAILED\"");
        loop_set(&with_tests, &["read_file", "edit_file", "run_tests"]).expect("consistent");
        let err = loop_set(&with_tests, &["read_file", "edit_file"])
            .expect_err("tests_fail without run_tests");
        assert!(err.to_string().contains("exactly when tests_fail"), "{err}");
    }

    #[test]
    fn an_unchanged_goal_offers_no_tests_and_a_palette_tool_must_be_canned() {
        let err = loop_set(
            "kind = \"unchanged\"\nreply_mentions = \"src/legacy.rs\"",
            &["read_file", "run_tests"],
        )
        .expect_err("nothing to test");
        assert!(err.to_string().contains("no tests to run"), "{err}");
        let err = loop_set(EDITED, &["read_file", "edit_file", "delete_file"])
            .expect_err("the environment cannot answer delete_file");
        assert!(
            err.to_string()
                .contains("'delete_file' has no canned behaviour"),
            "{err}"
        );
    }

    #[test]
    fn a_loop_case_id_may_not_repeat_an_id_from_another_array() {
        let text = format!(
            "version = 0\nloop_system = \"s\"\ninstruction = []\n\
             [[tool_emit]]\nid = \"tl-x\"\nprompt = \"p\"\nexpect = \"abstain\"\n\
             [[tool_emit.tools]]\nname = \"read_file\"\ndescription = \"d\"\n\
             input_schema = '{{\"type\":\"object\",\"properties\":{{\"path\":{{\"type\":\"string\"}}}},\"required\":[\"path\"]}}'\n\
             [[tool_loop]]\nid = \"tl-x\"\nprompt = \"p\"\n\
             [[tool_loop.files]]\npath = \"src/a.rs\"\ntext = \"const A: u32 = 3;\\n\"\n\
             [tool_loop.goal]\n{EDITED}\n\
             [[tool_loop.tools]]\nname = \"edit_file\"\ndescription = \"d\"\n\
             input_schema = '{{\"type\":\"object\",\"properties\":{{\"path\":{{\"type\":\"string\"}},\"old\":{{\"type\":\"string\"}},\"new\":{{\"type\":\"string\"}}}},\"required\":[\"path\",\"old\",\"new\"]}}'\n"
        );
        let err = super::parse(&text).expect_err("duplicate across arrays");
        assert!(
            err.to_string().contains("duplicate case id 'tl-x'"),
            "{err}"
        );
    }
}
