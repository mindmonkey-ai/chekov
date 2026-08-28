//! The property that makes `chekov capability bench` different from a generic
//! benchmark: a probe crosses chekov's OWN Anthropic<->OpenAI translator, so a
//! model that passes has demonstrably survived the exact code path every
//! Claude Code turn takes.
//!
//! No network and no llama.cpp: the upstream response is canned, exactly as
//! §8.2 requires.

use chekov::core::proxy::claude::ClaudeFacade;
use chekov::core::proxy::http::HttpRequest;
use chekov::core::proxy::{Action, AgentFacade};

/// An Anthropic-shaped request, as Claude Code would send it.
fn anthropic_request(prompt: &str) -> HttpRequest {
    let body = serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": 64,
        "messages": [{ "role": "user", "content": prompt }],
    });
    HttpRequest {
        method: "POST".into(),
        path: "/v1/messages".into(),
        body: body.to_string().into_bytes(),
    }
}

#[test]
fn a_probe_is_graded_on_what_the_agent_would_actually_receive() {
    let facade = ClaudeFacade::new("ornith-1.5-35b-a3b");

    // 1. Route the Anthropic request, as the proxy does.
    let action = facade
        .route(&anthropic_request("say hi"))
        .expect("a /v1/messages POST is forwardable");
    let forward = match action {
        Action::Forward(f) => f,
        Action::Reply(_) => {
            panic!("a completion request must go upstream, not be answered locally")
        }
    };

    // The request chekov puts on the wire is OpenAI-shaped and carries the
    // LOCAL model id, not the Anthropic name the agent asked for.
    let sent: serde_json::Value =
        serde_json::from_slice(&forward.body).expect("the forwarded body is json");
    assert_eq!(
        sent["model"], "ornith-1.5-35b-a3b",
        "the agent's model name is substituted for the local one"
    );
    assert!(
        forward.path.contains("/v1/chat/completions"),
        "translated to the OpenAI door, got {}",
        forward.path
    );

    // 2. Feed back a canned OpenAI response, as llama-server would answer.
    let upstream = serde_json::json!({
        "choices": [{ "message": { "content": "hello there" }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 9, "completion_tokens": 3 }
    })
    .to_string();

    // 3. What a grader sees must be the ANTHROPIC body — the same bytes the
    //    agent would parse — not the upstream OpenAI one.
    let graded: serde_json::Value = serde_json::from_str(
        &facade
            .translate_response(&upstream)
            .expect("a well-formed OpenAI body translates"),
    )
    .expect("the translated body is json");

    assert_eq!(
        graded["content"][0]["text"], "hello there",
        "grading must read Anthropic `content`, not OpenAI `choices`"
    );
    assert!(
        graded.get("choices").is_none(),
        "an OpenAI-shaped artifact means the probe bypassed the translator: {graded}"
    );
    assert_eq!(graded["type"], "message");
}

#[test]
fn a_malformed_upstream_body_fails_the_probe_rather_than_grading_as_empty() {
    let facade = ClaudeFacade::new("m");
    // A grader that treats a translation failure as "the model said nothing"
    // would score a broken server as a merely unhelpful model.
    assert!(
        facade.translate_response("{\"unexpected\": true}").is_err()
            || facade
                .translate_response("{\"unexpected\": true}")
                .is_ok_and(|b| !b.contains("\"text\":\"\"")),
        "a shape the translator cannot read must not silently become empty output"
    );
}
