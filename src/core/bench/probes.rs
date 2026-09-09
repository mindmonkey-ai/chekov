//! Depth-targeted probe requests, Anthropic-shaped as Claude Code would send
//! them — a probe that skipped the translator would measure a server chekov
//! does not actually serve.

use super::fixture::FixtureProbe;
use crate::core::proxy::http::HttpRequest;

/// The decode-exercising instruction every throughput probe carries. A const
/// so `prompt_set_hash` covers the exact text a run measured with.
const THROUGHPUT_PROMPT: &str = "Count upward from one, one number per line, and do not stop.";

/// A probe whose prompt approximates `depth_tokens` and whose reply exercises
/// decode for up to `max_tokens`.
///
/// Common short filler words tokenize near 1:1; the HONEST depth is
/// `timings.prompt_n`, which the sweep records alongside every sample.
#[must_use]
pub fn throughput_probe(depth_tokens: u32, max_tokens: u32) -> HttpRequest {
    let filler = "lorem ".repeat(depth_tokens as usize);
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": max_tokens,
        "system": filler,
        "messages": [{ "role": "user", "content": THROUGHPUT_PROMPT }],
    }))
}

/// Hash of everything that defines the task set: a run measured under a
/// different set must never compare as the same one (spec §7.4 stamp field).
#[must_use]
pub fn prompt_set_hash(plan: &crate::core::bench::sweep::SweepPlan, seed: u32) -> String {
    let canonical = format!(
        "throughput-v1|depths={:?}|max_tokens={}|repetitions={}|seed={seed}|prompt={THROUGHPUT_PROMPT}",
        plan.depths, plan.max_tokens, plan.repetitions
    );
    crate::core::hash::sha256_hex(canonical.as_bytes())[..12].to_owned()
}

/// What pins the agentic task set beyond its content: the sampling seed and
/// the loop's turn budget (tool-loop design §8). Two runs judged under
/// different budgets measured different tasks.
#[derive(Debug, Clone, Copy)]
pub struct HashPins {
    pub seed: u32,
    pub max_turns: u32,
}

/// The suite-aware prompt-set hash. A throughput-only run keeps the original
/// value, so runs recorded before `--suite` existed stay comparable.
#[must_use]
pub fn suite_prompt_hash(
    suite: crate::core::bench::lifecycle::Suite,
    plan: &crate::core::bench::sweep::SweepPlan,
    pins: HashPins,
) -> String {
    use crate::core::bench::lifecycle::Suite;
    let throughput = prompt_set_hash(plan, pins.seed);
    let agentic = crate::core::bench::probeset::content_hash();
    let (seed, turns) = (pins.seed, pins.max_turns);
    match suite {
        Suite::Throughput => throughput,
        Suite::Agentic => hash12(&format!("agentic|{agentic}|turns={turns}|seed={seed}")),
        Suite::All => hash12(&format!(
            "all|{throughput}|{agentic}|turns={turns}|seed={seed}"
        )),
    }
}

fn hash12(canonical: &str) -> String {
    crate::core::hash::sha256_hex(canonical.as_bytes())[..12].to_owned()
}

/// The palette as Anthropic `tools`, shared by every probe that offers one.
fn palette(tools: &[crate::core::bench::probeset::ToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": parse_schema(&tool.input_schema),
            })
        })
        .collect()
}

/// A `tool_emit` case: the palette rides as real Anthropic `tools`, so the
/// call crosses the translator's tool mapping exactly as an agent's would.
#[must_use]
pub fn tool_probe(case: &crate::core::bench::probeset::ToolCase) -> HttpRequest {
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": 256,
        "tools": palette(&case.tools),
        "messages": [{"role": "user", "content": case.prompt}],
    }))
}

/// One turn of a `tool_loop` case: the set's system text, the palette, and
/// the transcript so far — the shape Claude Code sends on every turn.
#[must_use]
pub fn loop_probe(
    case: &crate::core::bench::probeset::LoopCase,
    system: &str,
    messages: &[serde_json::Value],
) -> HttpRequest {
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": 512,
        "system": system,
        "tools": palette(&case.tools),
        "messages": messages,
    }))
}

/// The forced half: no `tools` param (the grammar replaces the tool-call
/// machinery); the palette is shown in a system prompt instead, and the
/// reply shape is constrained by `response_format` on the wire.
#[must_use]
pub fn forced_probe(case: &crate::core::bench::probeset::ToolCase) -> HttpRequest {
    let palette: Vec<serde_json::Value> = case
        .tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": parse_schema(&tool.input_schema),
            })
        })
        .collect();
    let system = format!(
        "Invoke exactly one of these tools by replying with a JSON object \
         {{\"name\": ..., \"arguments\": {{...}}}} and nothing else. Tools: {}",
        serde_json::Value::Array(palette)
    );
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": 256,
        "system": system,
        "messages": [{"role": "user", "content": case.prompt}],
    }))
}

/// An instruction case: the prompt alone — the constraints live in its text.
#[must_use]
pub fn instruction_probe(case: &crate::core::bench::probeset::InstructionCase) -> HttpRequest {
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": 512,
        "messages": [{"role": "user", "content": case.prompt}],
    }))
}

fn parse_schema(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|_| serde_json::json!({"type": "object"}))
}

/// A graded probe from a fixture, in the same dialect as every other probe.
#[must_use]
pub fn fixture_probe(probe: &FixtureProbe) -> HttpRequest {
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": probe.max_tokens,
        "messages": [{"role": "user", "content": probe.prompt}],
    }))
}

/// POST `/v1/messages` with `body`, exactly as an Anthropic SDK client would.
pub(crate) fn anthropic_post(body: &serde_json::Value) -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        path: "/v1/messages".into(),
        body: body.to_string().into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use crate::core::proxy::claude::ClaudeFacade;
    use crate::core::proxy::{Action, AgentFacade};

    #[test]
    fn a_deeper_probe_carries_a_proportionally_longer_prompt() {
        let shallow = super::throughput_probe(1_024, 64);
        let deep = super::throughput_probe(16_384, 64);
        assert!(
            deep.body.len() > shallow.body.len() * 8,
            "16x the depth must be roughly 16x the filler: {} vs {}",
            deep.body.len(),
            shallow.body.len()
        );
    }

    #[test]
    fn a_loop_turn_carries_the_system_text_the_palette_and_the_transcript_in_order() {
        let set = crate::core::bench::probeset::agentic_v0().expect("valid");
        let case = &set.tool_loop[0];
        let messages = vec![
            serde_json::json!({"role": "user", "content": "do it"}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]}),
        ];
        let req = super::loop_probe(case, &set.loop_system, &messages);
        let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json");
        assert_eq!(body["system"], set.loop_system);
        assert_eq!(
            body["tools"].as_array().map(Vec::len),
            Some(case.tools.len())
        );
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["max_tokens"], 512);
    }

    #[test]
    fn a_different_turn_budget_changes_the_agentic_hash_and_not_the_throughput_one() {
        use crate::core::bench::lifecycle::Suite;
        use crate::core::bench::sweep::SweepPlan;
        let plan = SweepPlan {
            depths: vec![1024],
            repetitions: 5,
            max_tokens: 128,
        };
        let eight = super::HashPins {
            seed: 42,
            max_turns: 8,
        };
        let three = super::HashPins {
            seed: 42,
            max_turns: 3,
        };
        assert_ne!(
            super::suite_prompt_hash(Suite::Agentic, &plan, eight),
            super::suite_prompt_hash(Suite::Agentic, &plan, three)
        );
        assert_eq!(
            super::suite_prompt_hash(Suite::Throughput, &plan, eight),
            super::suite_prompt_hash(Suite::Throughput, &plan, three),
            "a throughput-only run's hash never saw the budget"
        );
    }

    #[test]
    fn the_prompt_set_hash_pins_the_task_set() {
        use crate::core::bench::sweep::SweepPlan;
        let plan = SweepPlan {
            depths: vec![1024, 4096],
            repetitions: 5,
            max_tokens: 128,
        };
        let base = super::prompt_set_hash(&plan, 42);
        assert_eq!(base, super::prompt_set_hash(&plan, 42), "stable");
        let mut deeper = SweepPlan {
            depths: vec![1024, 8192],
            repetitions: 5,
            max_tokens: 128,
        };
        assert_ne!(
            base,
            super::prompt_set_hash(&deeper, 42),
            "depths change it"
        );
        deeper.depths = vec![1024, 4096];
        assert_ne!(base, super::prompt_set_hash(&deeper, 7), "seed changes it");
    }

    #[test]
    fn a_probe_is_anthropic_shaped_and_crosses_the_translator() {
        let req = super::throughput_probe(64, 16);
        assert_eq!(req.path, "/v1/messages", "probes speak the agent's dialect");
        let facade = ClaudeFacade::new("local-model");
        match facade.route(&req).expect("routable") {
            Action::Forward(forward) => {
                let sent: serde_json::Value =
                    serde_json::from_slice(&forward.body).expect("forwarded body is json");
                assert_eq!(sent["model"], "local-model");
                assert_eq!(sent["max_tokens"], 16);
                assert_eq!(forward.path, "/v1/chat/completions");
            }
            Action::Reply(_) => panic!("a probe must go upstream"),
        }
    }
}
