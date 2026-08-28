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
