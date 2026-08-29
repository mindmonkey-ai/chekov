//! The streamed twin of `bench_probe_crosses_the_translator`: Claude Code
//! streams, so a probe that only crossed the buffered translator was grading
//! a path the agent never takes. A streamed probe must cross chekov's OWN
//! SSE translator and be graded on the message an SDK client would hold at
//! `message_stop` — and an upstream error frame must fail the crossing
//! rather than be papered over as a clean `end_turn`.
//!
//! No network and no llama.cpp: the upstream stream is canned.

use std::cell::RefCell;

use chekov::core::bench::runner::{ProbeWire, SamplingPins, Transport, cross_via};
use chekov::core::hub::{HttpClient, JsonRequest};
use chekov::core::proxy::claude::ClaudeFacade;
use chekov::core::proxy::http::HttpRequest;
use chekov::core::proxy::serve::Upstream;
use chekov::error::ChekovError;

/// Upstream answering every POST with one canned SSE body, as llama-server
/// writes it: `data:` frames, blank separators, the `[DONE]` sentinel.
struct CannedStream {
    frames: Vec<serde_json::Value>,
    sent: RefCell<Option<String>>,
}

impl HttpClient for CannedStream {
    fn get(&self, _url: &str) -> Result<String, ChekovError> {
        unreachable!("a probe crossing never GETs")
    }

    fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError> {
        *self.sent.borrow_mut() = Some(req.body.clone());
        let mut out = String::new();
        for frame in &self.frames {
            out.push_str("data: ");
            out.push_str(&frame.to_string());
            out.push_str("\n\n");
        }
        out.push_str("data: [DONE]\n\n");
        Ok(out)
    }
}

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

fn final_frame(finish: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "c1",
        "choices": [{ "delta": {}, "finish_reason": finish }],
        "usage": { "prompt_tokens": 9, "completion_tokens": 3 },
        "timings": {
            "prompt_n": 9, "prompt_per_second": 450.0,
            "predicted_n": 3, "predicted_per_second": 21.7
        }
    })
}

fn upstream() -> Upstream {
    Upstream {
        base_url: "http://fake".into(),
        api_key: "sekrit".into(),
    }
}

#[test]
fn a_streamed_probe_is_graded_on_the_message_the_agent_would_assemble() {
    let http = CannedStream {
        frames: vec![
            serde_json::json!({ "id": "c1", "choices": [{ "delta": { "content": "hello " } }] }),
            serde_json::json!({ "id": "c1", "choices": [{ "delta": { "content": "there" } }] }),
            final_frame("stop"),
        ],
        sent: RefCell::new(None),
    };
    let facade = ClaudeFacade::new("ornith-1.5-35b-a3b");
    let wire = ProbeWire {
        http: &http,
        facade: &facade,
        upstream: &upstream(),
        pins: SamplingPins { seed: 42 },
    };

    let artifact = cross_via(&wire, &anthropic_request("say hi"), Transport::Streamed)
        .expect("a well-formed stream crosses");

    // 1. What went on the wire is the OpenAI streaming request chekov's proxy
    //    would send for Claude Code: local model, stream, usage on the last frame.
    let sent: serde_json::Value =
        serde_json::from_str(&http.sent.borrow().clone().expect("posted")).expect("json");
    assert_eq!(sent["model"], "ornith-1.5-35b-a3b");
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["stream_options"]["include_usage"], true);

    // 2. What a grader sees is the ANTHROPIC message an SDK client would hold
    //    after `message_stop` — text reassembled from its deltas, the stop
    //    reason and usage from `message_delta` — never the raw SSE frames.
    let graded: serde_json::Value =
        serde_json::from_str(&artifact.anthropic_body).expect("the artifact is json");
    assert_eq!(graded["type"], "message");
    assert_eq!(graded["role"], "assistant");
    assert_eq!(graded["content"][0]["type"], "text");
    assert_eq!(graded["content"][0]["text"], "hello there");
    assert_eq!(graded["stop_reason"], "end_turn");
    assert_eq!(graded["usage"]["output_tokens"], 3);
    assert!((artifact.timings.predicted_per_second - 21.7).abs() < 1e-9);
}

#[test]
fn an_upstream_error_frame_fails_the_crossing_instead_of_forging_end_turn() {
    let http = CannedStream {
        frames: vec![
            serde_json::json!({ "id": "c1", "choices": [{ "delta": { "content": "par" } }] }),
            serde_json::json!({ "error": { "message": "context size exceeded", "code": 400 } }),
        ],
        sent: RefCell::new(None),
    };
    let facade = ClaudeFacade::new("ornith-1.5-35b-a3b");
    let wire = ProbeWire {
        http: &http,
        facade: &facade,
        upstream: &upstream(),
        pins: SamplingPins { seed: 42 },
    };
    let err = cross_via(&wire, &anthropic_request("go"), Transport::Streamed)
        .expect_err("a turn that died is not a graded reply");
    assert!(
        matches!(err, ChekovError::BenchStreamFailed { .. }),
        "typed as a stream failure, so the bench records it unavailable: {err}"
    );
    assert!(err.to_string().contains("context size exceeded"), "{err}");
}
