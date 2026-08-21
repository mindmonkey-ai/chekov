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
    relay(stream, &mut translator, reader)?;
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
        ureq::post(&url)
            .header("content-type", "application/json")
            .header(
                "authorization",
                &format!("Bearer {}", self.upstream.api_key),
            )
            .send(&fwd.body)
            .map(|res| res.into_body().into_reader())
            .map_err(|e| ChekovError::ProxyUpstreamFailed {
                url,
                reason: e.to_string(),
            })
    }
}

fn read_all<R: Read>(mut reader: R) -> Result<String, ChekovError> {
    let mut out = String::new();
    reader
        .read_to_string(&mut out)
        .map_err(|e| ChekovError::io("reading upstream response", e))?;
    Ok(out)
}
