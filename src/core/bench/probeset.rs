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
    pub tool_emit: Vec<ToolCase>,
    pub instruction: Vec<InstructionCase>,
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

/// The v0 set, validated. Loud on any defect — a malformed case must never
/// silently grade.
pub fn agentic_v0() -> Result<ProbeSet, ChekovError> {
    let set: ProbeSet = toml::from_str(AGENTIC_V0).map_err(|e| invalid(e.to_string()))?;
    if set.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "version {} — this chekov reads {SUPPORTED_VERSION}",
            set.version
        )));
    }
    validate_tool_cases(&set)?;
    validate_checks(&set)?;
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

fn validate_ids(set: &ProbeSet) -> Result<(), ChekovError> {
    let mut seen = std::collections::BTreeSet::new();
    let ids = set
        .tool_emit
        .iter()
        .map(|c| &c.id)
        .chain(set.instruction.iter().map(|c| &c.id));
    for id in ids {
        if !seen.insert(id.clone()) {
            return Err(invalid(format!("duplicate case id '{id}'")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Expect, agentic_v0, content_hash};

    #[test]
    fn the_shipped_set_parses_with_the_seed_counts() {
        let set = agentic_v0().expect("the compiled-in set is valid");
        assert_eq!(set.version, 0);
        assert_eq!(
            set.tool_emit.len(),
            10,
            "7 call + 2 abstention + 1 missing-function"
        );
        assert_eq!(
            set.tool_emit
                .iter()
                .filter(|c| c.expect == Expect::Call)
                .count(),
            7
        );
        assert_eq!(set.instruction.len(), 12);
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
}
