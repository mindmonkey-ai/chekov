//! Server readiness and the `/props` assertion.
//!
//! Readiness is `/health` AND the pid: a server that dies while loading must
//! fail as "died" (go read the log), never as a timeout (keep waiting).

use std::time::Duration;

use serde_json::Value;

use crate::core::config::BenchSection;
use crate::core::hub::{HttpClient, JsonRequest};
use crate::core::proxy::http::HttpRequest;
use crate::core::proxy::serve::Upstream;
use crate::core::proxy::{Action, AgentFacade};
use crate::error::ChekovError;

/// What readiness watches: the health endpoint and the process behind it.
pub struct ReadyTarget {
    pub base_url: String,
    pub pid: i32,
}

/// Poll budget, from `[bench]` config.
#[derive(Debug, Clone, Copy)]
pub struct ReadyPolicy {
    pub max_polls: u32,
    pub interval: Duration,
}

impl From<&BenchSection> for ReadyPolicy {
    fn from(bench: &BenchSection) -> Self {
        Self {
            max_polls: bench.ready_max_polls,
            interval: Duration::from_millis(bench.ready_interval_ms),
        }
    }
}

/// Wait until `/health` answers, watching the pid between polls.
///
/// `/health` is public (no api key), so the plain seam `get` suffices;
/// 503-while-loading surfaces as `Err` and the poll continues.
pub fn wait_ready(
    http: &dyn HttpClient,
    target: &ReadyTarget,
    policy: ReadyPolicy,
) -> Result<(), ChekovError> {
    let url = format!("{}/health", target.base_url);
    for _ in 0..policy.max_polls {
        if !crate::core::server::process_alive(target.pid) {
            return Err(ChekovError::ServerDiedWhileLoading { pid: target.pid });
        }
        if http.get(&url).is_ok() {
            return Ok(());
        }
        std::thread::sleep(policy.interval);
    }
    Err(ChekovError::EndpointDown {
        url,
        reason: format!("not ready after {} polls", policy.max_polls),
    })
}

/// How the `/props` body is fetched — `serve::get_bearer` in production
/// (the endpoint sits behind `--api-key`), a canned closure in tests.
pub type PropsFetch<'a> = dyn Fn() -> Result<String, ChekovError> + 'a;

/// What `/props` says the server actually loaded. `n_ctx` is the PER-SLOT
/// window (`meta.slot_n_ctx`), not the `-c` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropsInfo {
    pub n_ctx: u32,
    pub total_slots: u32,
}

/// Read `/props`. Every field is required — a missing one is loud, never
/// assumed.
pub fn read_props(fetch: &PropsFetch) -> Result<PropsInfo, ChekovError> {
    let raw = fetch()?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| ChekovError::EndpointDown {
        url: "/props".to_owned(),
        reason: format!("/props is not JSON: {e}"),
    })?;
    let field = |pointer: &str| {
        parsed
            .pointer(pointer)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| ChekovError::EndpointDown {
                url: "/props".to_owned(),
                reason: format!("no {pointer} in /props"),
            })
    };
    Ok(PropsInfo {
        n_ctx: field("/default_generation_settings/n_ctx")?,
        total_slots: field("/total_slots")?,
    })
}

/// The context the server ACTUALLY loaded, asserted against the config's
/// intent — a bench under the wrong context would be recorded under a config
/// the server is not running.
pub fn assert_props_ctx(fetch: &PropsFetch, expected: u32) -> Result<PropsInfo, ChekovError> {
    let props = read_props(fetch)?;
    if props.n_ctx == expected {
        Ok(props)
    } else {
        Err(ChekovError::PropsCtxMismatch {
            server: props.n_ctx,
            config: expected,
        })
    }
}

/// llama-server's own measurement of one exchange, from its `timings` object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    pub prompt_n: u64,
    pub prompt_per_second: f64,
    pub predicted_n: u64,
    pub predicted_per_second: f64,
}

/// One measured probe: what the agent would receive, and what it cost.
#[derive(Debug)]
pub struct ProbeArtifact {
    /// The Anthropic-shaped body — the same bytes an agent would parse.
    pub anthropic_body: String,
    pub timings: Timings,
}

/// Sampling the bench pins ON THE WIRE (spec §7.3.6): greedy, seeded. The
/// pins are injected into the forwarded body after translation, so they hold
/// regardless of what the probe's Anthropic body carried.
#[derive(Debug, Clone, Copy)]
pub struct SamplingPins {
    pub seed: u32,
}

/// Everything one crossing needs, bundled (§4).
pub struct ProbeWire<'a> {
    pub http: &'a dyn HttpClient,
    pub facade: &'a dyn AgentFacade,
    pub upstream: &'a Upstream,
    pub pins: SamplingPins,
}

/// Route → POST upstream → capture `timings` → translate.
///
/// Timings are read from the upstream `OpenAI` body BEFORE translation (the
/// translator rightly drops them); the artifact handed to grading is the
/// Anthropic body, per `tests/bench_probe_crosses_the_translator.rs`.
pub fn cross(wire: &ProbeWire, req: &HttpRequest) -> Result<ProbeArtifact, ChekovError> {
    let forward = match wire.facade.route(req)? {
        Action::Forward(f) => f,
        Action::Reply(_) => {
            return Err(ChekovError::ProxyBadRequest {
                reason: "a bench probe must forward upstream; this request was \
                         answered locally"
                    .to_owned(),
            });
        }
    };
    let body = String::from_utf8(forward.body).map_err(|e| ChekovError::ProxyBadRequest {
        reason: format!("forwarded body is not UTF-8: {e}"),
    })?;
    let body = pin_sampling(&body, wire.pins)?;
    let upstream_body = wire.http.post_json(&JsonRequest {
        url: format!("{}{}", wire.upstream.base_url, forward.path),
        body,
        bearer: Some(wire.upstream.api_key.clone()),
    })?;
    let timings = read_timings(&upstream_body)?;
    let anthropic_body = wire.facade.translate_response(&upstream_body)?;
    Ok(ProbeArtifact {
        anthropic_body,
        timings,
    })
}

/// Overwrite the forwarded body's sampling with the pinned values —
/// whatever the probe asked for, the measurement is greedy and seeded.
fn pin_sampling(body: &str, pins: SamplingPins) -> Result<String, ChekovError> {
    let mut parsed: Value =
        serde_json::from_str(body).map_err(|e| ChekovError::ProxyBadRequest {
            reason: format!("forwarded body is not JSON: {e}"),
        })?;
    let object = parsed
        .as_object_mut()
        .ok_or_else(|| ChekovError::ProxyBadRequest {
            reason: "forwarded body is not a JSON object".to_owned(),
        })?;
    object.insert("temperature".to_owned(), Value::from(0));
    object.insert("top_k".to_owned(), Value::from(1));
    object.insert("seed".to_owned(), Value::from(pins.seed));
    Ok(parsed.to_string())
}

/// The four numbers the sweep records. All-or-nothing: a partial timings
/// object must not become a partial measurement.
fn read_timings(upstream: &str) -> Result<Timings, ChekovError> {
    let parsed: Value = serde_json::from_str(upstream).map_err(|_| ChekovError::BenchNoTimings)?;
    let timings = parsed.get("timings").ok_or(ChekovError::BenchNoTimings)?;
    let float = |key: &str| timings.get(key).and_then(Value::as_f64);
    let count = |key: &str| timings.get(key).and_then(Value::as_u64);
    match (
        count("prompt_n"),
        float("prompt_per_second"),
        count("predicted_n"),
        float("predicted_per_second"),
    ) {
        (Some(prompt_n), Some(pps), Some(predicted_n), Some(gps)) => Ok(Timings {
            prompt_n,
            prompt_per_second: pps,
            predicted_n,
            predicted_per_second: gps,
        }),
        _ => Err(ChekovError::BenchNoTimings),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::{ReadyPolicy, ReadyTarget, assert_props_ctx, wait_ready};
    use crate::core::hub::{HttpClient, JsonRequest};
    use crate::error::ChekovError;

    /// /health that answers 503-as-error `failures_left` times, then 200.
    struct FlakyHealth {
        failures_left: RefCell<u32>,
    }

    impl HttpClient for FlakyHealth {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            let mut left = self.failures_left.borrow_mut();
            if *left == 0 {
                return Ok(r#"{"status":"ok"}"#.into());
            }
            *left -= 1;
            Err(ChekovError::EndpointDown {
                url: "fake".into(),
                reason: "503 while loading".into(),
            })
        }

        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("readiness never POSTs")
        }
    }

    fn own_pid() -> i32 {
        i32::try_from(std::process::id()).expect("pid fits")
    }

    fn instant_policy(max_polls: u32) -> ReadyPolicy {
        ReadyPolicy {
            max_polls,
            interval: Duration::ZERO,
        }
    }

    #[test]
    fn readiness_waits_through_loading_then_succeeds() {
        let http = FlakyHealth {
            failures_left: RefCell::new(2),
        };
        let target = ReadyTarget {
            base_url: "http://fake".into(),
            pid: own_pid(),
        };
        wait_ready(&http, &target, instant_policy(5)).expect("ready on the third poll");
    }

    #[test]
    fn a_dead_pid_fails_as_died_not_as_a_timeout() {
        // The server exiting during load must be reported as its own failure —
        // a timeout message would send the user waiting instead of to the log.
        let http = FlakyHealth {
            failures_left: RefCell::new(u32::MAX),
        };
        let target = ReadyTarget {
            base_url: "http://fake".into(),
            pid: 99_999_999,
        };
        let err = wait_ready(&http, &target, instant_policy(5)).expect_err("died");
        assert!(matches!(
            err,
            ChekovError::ServerDiedWhileLoading { pid: 99_999_999 }
        ));
    }

    #[test]
    fn readiness_gives_up_after_the_poll_budget() {
        let http = FlakyHealth {
            failures_left: RefCell::new(u32::MAX),
        };
        let target = ReadyTarget {
            base_url: "http://fake".into(),
            pid: own_pid(),
        };
        let err = wait_ready(&http, &target, instant_policy(3)).expect_err("budget spent");
        assert!(matches!(err, ChekovError::EndpointDown { .. }));
    }

    use crate::core::proxy::claude::ClaudeFacade;
    use crate::core::proxy::http::HttpRequest;
    use crate::core::proxy::serve::Upstream;

    /// Upstream answering every POST with one canned `OpenAI` body.
    struct CannedUpstream {
        body: String,
        bearer_seen: RefCell<Option<String>>,
        sent_body: RefCell<Option<String>>,
    }

    impl CannedUpstream {
        fn new(body: String) -> Self {
            Self {
                body,
                bearer_seen: RefCell::new(None),
                sent_body: RefCell::new(None),
            }
        }
    }

    impl HttpClient for CannedUpstream {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            unreachable!("a probe crossing never GETs")
        }

        fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError> {
            *self.bearer_seen.borrow_mut() = req.bearer.clone();
            *self.sent_body.borrow_mut() = Some(req.body.clone());
            Ok(self.body.clone())
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

    fn openai_with_timings() -> String {
        serde_json::json!({
            "choices": [{ "message": { "content": "hello there" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 900, "completion_tokens": 100 },
            "timings": {
                "prompt_n": 900, "prompt_ms": 2000.0, "prompt_per_second": 450.0,
                "predicted_n": 100, "predicted_ms": 4608.3, "predicted_per_second": 21.7
            }
        })
        .to_string()
    }

    fn fake_upstream() -> Upstream {
        Upstream {
            base_url: "http://fake".into(),
            api_key: "sekrit".into(),
        }
    }

    fn wire<'a>(
        http: &'a CannedUpstream,
        facade: &'a ClaudeFacade,
        up: &'a Upstream,
    ) -> super::ProbeWire<'a> {
        super::ProbeWire {
            http,
            facade,
            upstream: up,
            pins: super::SamplingPins { seed: 42 },
        }
    }

    #[test]
    fn cross_returns_the_anthropic_body_and_the_upstream_timings() {
        let http = CannedUpstream::new(openai_with_timings());
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let art = super::cross(&wire(&http, &facade, &up), &anthropic_request("say hi"))
            .expect("crossing succeeds");
        // Timings are the server's own measurement, read before translation.
        assert!((art.timings.predicted_per_second - 21.7).abs() < 1e-9);
        assert_eq!(art.timings.prompt_n, 900);
        // The artifact is the ANTHROPIC body — the bytes an agent would parse.
        let graded: serde_json::Value =
            serde_json::from_str(&art.anthropic_body).expect("artifact is json");
        assert_eq!(graded["content"][0]["text"], "hello there");
        assert!(
            graded.get("choices").is_none(),
            "translator was bypassed: {graded}"
        );
        assert_eq!(http.bearer_seen.borrow().as_deref(), Some("sekrit"));
    }

    #[test]
    fn the_wire_carries_pinned_greedy_seeded_sampling() {
        let http = CannedUpstream::new(openai_with_timings());
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        super::cross(&wire(&http, &facade, &up), &anthropic_request("say hi")).expect("crossing");
        let sent = http.sent_body.borrow().clone().expect("a body was sent");
        let sent: serde_json::Value = serde_json::from_str(&sent).expect("sent body is json");
        assert_eq!(sent["temperature"], 0, "greedy: {sent}");
        assert_eq!(sent["top_k"], 1, "greedy: {sent}");
        assert_eq!(sent["seed"], 42, "seeded: {sent}");
    }

    #[test]
    fn a_response_without_timings_fails_rather_than_inventing_a_number() {
        let no_timings = serde_json::json!({
            "choices": [{ "message": { "content": "hi" }, "finish_reason": "stop" }]
        })
        .to_string();
        let http = CannedUpstream::new(no_timings);
        let facade = ClaudeFacade::new("m");
        let up = fake_upstream();
        let err = super::cross(&wire(&http, &facade, &up), &anthropic_request("hi"))
            .expect_err("no measurement");
        assert!(matches!(err, ChekovError::BenchNoTimings));
    }

    #[test]
    fn a_locally_answered_request_cannot_be_a_probe() {
        // GET /v1/models is answered by the facade without touching the server —
        // "measuring" it would time chekov, not the model.
        let http = CannedUpstream::new(String::new());
        let facade = ClaudeFacade::new("m");
        let up = fake_upstream();
        let req = HttpRequest {
            method: "GET".into(),
            path: "/v1/models".into(),
            body: vec![],
        };
        assert!(super::cross(&wire(&http, &facade, &up), &req).is_err());
    }

    fn props(n_ctx: u64) -> String {
        serde_json::json!({
            "default_generation_settings": {"n_ctx": n_ctx},
            "total_slots": 1,
        })
        .to_string()
    }

    #[test]
    fn a_matching_props_ctx_passes_and_reports_the_slots() {
        let body = props(131_072);
        let got = assert_props_ctx(&|| Ok(body.clone()), 131_072).expect("matches");
        assert_eq!(got.n_ctx, 131_072);
        assert_eq!(got.total_slots, 1);
    }

    #[test]
    fn a_mismatched_props_ctx_is_refused_naming_both_numbers() {
        // The server loaded something other than what the registry intended —
        // benching it would attribute the numbers to a config that is not running.
        let body = props(65_536);
        let err = assert_props_ctx(&|| Ok(body.clone()), 131_072).expect_err("mismatch");
        assert!(matches!(
            err,
            ChekovError::PropsCtxMismatch {
                server: 65_536,
                config: 131_072
            }
        ));
    }

    #[test]
    fn props_without_n_ctx_is_loud_rather_than_assumed() {
        let err = assert_props_ctx(&|| Ok(r#"{"total_slots": 4}"#.into()), 131_072)
            .expect_err("no n_ctx");
        assert!(err.to_string().contains("n_ctx"), "{err}");
    }
}
