//! Accept loop and upstream plumbing.
//!
//! Thread-per-connection: an agent holds one long streaming request at a time,
//! so a thread pool would add machinery with nothing to schedule. Every
//! connection is `Connection: close`, so a thread lives exactly one exchange.

use std::io::{BufRead, BufReader, Read};
use std::net::{TcpListener, TcpStream};

use super::http::{self, HttpResponse};
use super::{Action, AgentFacade, StreamTranslator};
use crate::error::ChekovError;

/// Where translated requests go, and how to authenticate to it.
pub struct Upstream {
    /// Base URL of the llama-server, e.g. `http://127.0.0.1:8080`.
    pub base_url: String,
    /// `--api-key` the server was launched with.
    pub api_key: String,
}

/// The translating pair every handler needs: which protocol, and to where.
struct Bridge<'a> {
    facade: &'a dyn AgentFacade,
    upstream: &'a Upstream,
}

/// Serve until the process is killed. Blocks.
///
/// Concurrency is required, not an optimization: Claude Code issues background
/// calls (title generation, the haiku model) while a main response streams. A
/// sequential loop would stall the agent behind its own long request.
pub fn serve(
    listener: &TcpListener,
    facade: &dyn AgentFacade,
    upstream: &Upstream,
) -> Result<(), ChekovError> {
    let bridge = Bridge { facade, upstream };
    std::thread::scope(|scope| {
        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    scope.spawn(|| handle_logged(stream, &bridge));
                }
                // A failed accept is per-connection, never fatal to the listener.
                Err(e) => eprintln!("chekov proxy: accept failed: {e}"),
            }
        }
    });
    Ok(())
}

/// Handle one connection, reporting failures without killing the loop.
fn handle_logged(stream: TcpStream, bridge: &Bridge) {
    if let Err(e) = handle(stream, bridge) {
        eprintln!("chekov proxy: {e}");
    }
}

fn handle(mut stream: TcpStream, bridge: &Bridge) -> Result<(), ChekovError> {
    let read_side = stream
        .try_clone()
        .map_err(|e| ChekovError::io("cloning proxy socket", e))?;
    let req = match http::read_request(read_side) {
        Ok(req) => req,
        Err(e) => {
            let res = HttpResponse::error(400, "invalid_request_error", &e.to_string());
            return http::write_response(&mut stream, &res);
        }
    };
    match bridge.facade.route(&req) {
        Ok(Action::Reply(res)) => http::write_response(&mut stream, &res),
        Ok(Action::Forward(fwd)) if fwd.stream => stream_upstream(&mut stream, bridge, &fwd),
        Ok(Action::Forward(fwd)) => forward_once(&mut stream, bridge, &fwd),
        Err(e) => {
            let res = HttpResponse::error(400, "invalid_request_error", &e.to_string());
            http::write_response(&mut stream, &res)
        }
    }
}

/// Non-streaming: one upstream call, one translated response.
fn forward_once(
    stream: &mut TcpStream,
    bridge: &Bridge,
    fwd: &super::Forward,
) -> Result<(), ChekovError> {
    let reader = match bridge.post(fwd) {
        Ok(reader) => reader,
        Err(e) => {
            let res = HttpResponse::error(502, "api_error", &e.to_string());
            return http::write_response(stream, &res);
        }
    };
    let translated = bridge.facade.translate_response(&read_all(reader)?)?;
    http::write_response(stream, &HttpResponse::json(200, translated))
}

/// Streaming: relay upstream SSE, translating each frame as it arrives.
fn stream_upstream(
    stream: &mut TcpStream,
    bridge: &Bridge,
    fwd: &super::Forward,
) -> Result<(), ChekovError> {
    let reader = match bridge.post(fwd) {
        Ok(reader) => reader,
        Err(e) => {
            let res = HttpResponse::error(502, "api_error", &e.to_string());
            return http::write_response(stream, &res);
        }
    };
    http::write_sse_head(stream)?;
    let mut translator = bridge.facade.stream_translator();
    // A failed relay still owes the client a terminating envelope. Dropping
    // the socket instead leaves the SDK reporting a protocol error rather than
    // the real failure, and `finish()` alone would forge a clean `end_turn`
    // over a turn that died (§C.2 — nothing degrades silently).
    if let Err(e) = relay(stream, &mut translator, reader) {
        for ev in translator.on_upstream_error(&e.to_string()) {
            http::write_sse_event(stream, &ev)?;
        }
        return Err(e);
    }
    for ev in translator.finish() {
        http::write_sse_event(stream, &ev)?;
    }
    Ok(())
}

/// Pump upstream `data:` lines through the translator until EOF.
fn relay<R: Read>(
    stream: &mut TcpStream,
    translator: &mut Box<dyn StreamTranslator>,
    reader: R,
) -> Result<(), ChekovError> {
    for line in BufReader::new(reader).lines() {
        let line = line.map_err(|e| ChekovError::io("reading upstream SSE", e))?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        for ev in translator.on_chunk(data.trim()) {
            http::write_sse_event(stream, &ev)?;
        }
    }
    Ok(())
}

impl Bridge<'_> {
    /// POST the translated body upstream, returning the response reader.
    fn post(&self, fwd: &super::Forward) -> Result<impl Read, ChekovError> {
        let url = format!("{}{}", self.upstream.base_url, fwd.path);
        // http_status_as_error(false): ureq's default renders a non-2xx as
        // "http status: 400" and drops the body, but llama-server puts the real
        // cause there. Take the status ourselves so the explanation survives.
        let res = ureq::post(&url)
            .config()
            .http_status_as_error(false)
            .build()
            .header("content-type", "application/json")
            .header(
                "authorization",
                &format!("Bearer {}", self.upstream.api_key),
            )
            .send(&fwd.body)
            .map_err(|e| ChekovError::ProxyUpstreamFailed {
                url: url.clone(),
                reason: e.to_string(),
            })?;
        let status = res.status().as_u16();
        let mut body = res.into_body().into_reader();
        if !(200..300).contains(&status) {
            let mut text = String::new();
            // A body we cannot read is not a reason to lose the status.
            let _ = body.read_to_string(&mut text);
            return Err(ChekovError::ProxyUpstreamFailed {
                url,
                reason: upstream_reason(status, &text),
            });
        }
        Ok(body)
    }
}

/// Authenticated GET against the upstream llama-server.
///
/// `/props` and friends sit behind `--api-key`. Mirrors `Bridge::post`:
/// non-2xx keeps the body's own explanation instead of ureq's bare status
/// line. Network-only by nature; exercised live, not in tests (like the
/// shard download in `hub`).
pub fn get_bearer(upstream: &Upstream, path: &str) -> Result<String, ChekovError> {
    let url = format!("{}{}", upstream.base_url, path);
    let res = ureq::get(&url)
        .config()
        .http_status_as_error(false)
        .build()
        .header("authorization", &format!("Bearer {}", upstream.api_key))
        .call()
        .map_err(|e| ChekovError::EndpointDown {
            url: url.clone(),
            reason: e.to_string(),
        })?;
    let status = res.status().as_u16();
    let mut text = String::new();
    // A body we cannot read is not a reason to lose the status.
    let _ = res.into_body().into_reader().read_to_string(&mut text);
    answered(&url, status, text)
}

/// A response the server DID send: the body on 2xx, `UpstreamRefused` with
/// the status and the server's own words otherwise. Never `EndpointDown` —
/// a 400 is an answer, and telling the user to restart a server that just
/// answered sends the diagnosis the wrong way. Shared by `hub::post_json`
/// (the bench path) and `get_bearer`.
pub(crate) fn answered(url: &str, status: u16, body: String) -> Result<String, ChekovError> {
    if (200..300).contains(&status) {
        return Ok(body);
    }
    Err(ChekovError::UpstreamRefused {
        url: url.to_owned(),
        status,
        reason: upstream_detail(&body),
    })
}

/// Why an upstream call failed, in words the user can act on.
///
/// ureq renders a non-2xx as `http status: 400` and drops the body, but the
/// body is the only thing that says WHY — llama-server puts the real cause
/// (context overflow, a bad sampler value) in `error.message` there.
pub(crate) fn upstream_reason(status: u16, body: &str) -> String {
    format!("http status: {status}: {}", upstream_detail(body))
}

/// The server's own explanation out of a non-2xx body: `error.message` when
/// the body is llama-server's JSON, the trimmed body otherwise, clipped —
/// error strings are logged and shown, and a runaway body must not become a
/// 20 KB log line.
fn upstream_detail(body: &str) -> String {
    const MAX: usize = 400;
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| body.trim().to_owned());
    if detail.is_empty() {
        return "upstream sent no explanation".to_owned();
    }
    let clipped: String = detail.chars().take(MAX).collect();
    let ellipsis = if detail.chars().count() > MAX {
        "…"
    } else {
        ""
    };
    format!("{clipped}{ellipsis}")
}

fn read_all<R: Read>(mut reader: R) -> Result<String, ChekovError> {
    let mut out = String::new();
    reader
        .read_to_string(&mut out)
        .map_err(|e| ChekovError::io("reading upstream response", e))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::upstream_reason;

    #[test]
    fn an_answered_request_is_classified_by_its_status_not_called_down() {
        use crate::error::ChekovError;
        let url = "http://127.0.0.1:8080/v1/chat/completions";
        assert_eq!(
            super::answered(url, 200, "{\"ok\":true}".to_owned()).expect("2xx is the body"),
            "{\"ok\":true}"
        );
        let refused = super::answered(
            url,
            400,
            r#"{"error":{"code":400,"message":"Failed to initialize samplers: std::exception"}}"#
                .to_owned(),
        )
        .expect_err("a 400 is a refusal");
        match refused {
            ChekovError::UpstreamRefused { status, reason, .. } => {
                assert_eq!(status, 400);
                assert_eq!(reason, "Failed to initialize samplers: std::exception");
            }
            other => panic!("expected UpstreamRefused, got {other}"),
        }
        // A plain-text body still reaches the user; an empty one says so.
        let loading = super::answered(url, 503, "Loading model".to_owned()).expect_err("503");
        assert!(
            matches!(&loading, ChekovError::UpstreamRefused { status: 503, reason, .. } if reason == "Loading model"),
            "{loading}"
        );
        let silent = super::answered(url, 500, String::new()).expect_err("500");
        assert!(
            matches!(&silent, ChekovError::UpstreamRefused { reason, .. } if reason.contains("no explanation")),
            "{silent}"
        );
    }

    #[test]
    fn an_upstream_failure_keeps_the_servers_own_explanation() {
        let body = r#"{"error":{"code":400,"message":"the request exceeds the available context size","type":"invalid_request_error"}}"#;
        let reason = upstream_reason(400, body);
        assert!(
            reason.contains("the request exceeds the available context size"),
            "the body is the only thing that says why: {reason}"
        );
        assert!(
            reason.contains("400"),
            "the status should survive too: {reason}"
        );
    }

    #[test]
    fn a_non_json_upstream_body_still_reaches_the_user() {
        let reason = upstream_reason(503, "upstream connect error");
        assert!(
            reason.contains("upstream connect error"),
            "a plain-text body must not be dropped either: {reason}"
        );
    }

    #[test]
    fn a_runaway_upstream_body_is_bounded() {
        let huge = "x".repeat(20_000);
        let reason = upstream_reason(500, &huge);
        assert!(
            reason.len() < 1_000,
            "an error string is logged and shown; it must not carry 20 KB: {} bytes",
            reason.len()
        );
    }
}
