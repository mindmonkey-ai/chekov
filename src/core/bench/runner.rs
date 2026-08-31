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
    /// Prompt tokens served from the KV cache instead of being processed —
    /// the reason a warm rerun's `prompt_n` can shrink. Absent means zero
    /// cached, not a missing measurement.
    pub cache_n: u64,
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

/// Which wire fills a codebase mask (spec §6).
///
/// llama.cpp's native `/infill`, or a deterministic chat-completions
/// instruction for a runtime with no FIM endpoint. The transport is a
/// function of the runtime, so the report derives it from the stamp
/// instead of storing it twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FimTransport {
    Infill,
    Chat,
}

/// Route → POST upstream → capture `timings` → translate.
///
/// Timings are read from the upstream `OpenAI` body BEFORE translation (the
/// translator rightly drops them); the artifact handed to grading is the
/// Anthropic body, per `tests/bench_probe_crosses_the_translator.rs`.
pub fn cross(wire: &ProbeWire, req: &HttpRequest) -> Result<ProbeArtifact, ChekovError> {
    cross_inner(wire, req, None)
}

/// What a forced crossing constrains: the grammar, and — on the judge wire
/// only — the engine's reasoning effort. Candidate probes never set the latter.
pub struct Forced<'a> {
    pub schema: &'a Value,
    pub reasoning_effort: Option<&'a str>,
}

/// `cross` with a JSON schema forced on the wire (`response_format`) — the
/// `grammar_gap` probe's forced half. The sampling pins still apply.
pub fn cross_forced(
    wire: &ProbeWire,
    req: &HttpRequest,
    schema: &Value,
) -> Result<ProbeArtifact, ChekovError> {
    cross_inner(
        wire,
        req,
        Some(&Forced {
            schema,
            reasoning_effort: None,
        }),
    )
}

/// `cross_forced` with the judge's extra field (spec C §3.0: one uniform judge wire).
pub fn cross_forced_with(
    wire: &ProbeWire,
    req: &HttpRequest,
    forced: &Forced,
) -> Result<ProbeArtifact, ChekovError> {
    cross_inner(wire, req, Some(forced))
}

/// `cross` or `cross_streaming`, by door.
pub fn cross_via(
    wire: &ProbeWire,
    req: &HttpRequest,
    transport: Transport,
) -> Result<ProbeArtifact, ChekovError> {
    match transport {
        Transport::Buffered => cross(wire, req),
        Transport::Streamed => cross_streaming(wire, req),
    }
}

fn cross_inner(
    wire: &ProbeWire,
    req: &HttpRequest,
    forced: Option<&Forced>,
) -> Result<ProbeArtifact, ChekovError> {
    let (path, body) = forward_of(wire, req)?;
    let body = adjust_body(&body, wire.pins, forced)?;
    let upstream_body = wire.http.post_json(&JsonRequest {
        url: format!("{}{}", wire.upstream.base_url, path),
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

/// Route through the facade; the upstream path and the translated body.
fn forward_of(wire: &ProbeWire, req: &HttpRequest) -> Result<(String, String), ChekovError> {
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
    Ok((forward.path, body))
}

/// Which door a probe took. Claude Code streams; the buffered door is the one
/// `doctor` and the first bench runs used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Buffered,
    Streamed,
}

/// The streaming crossing — the door Claude Code actually takes.
///
/// Same route and pins as `cross`, with `stream: true` on the Anthropic
/// request (so the translator asks upstream for a stream with usage, as it
/// does for Claude Code); the SSE body is pumped through a fresh
/// `stream_translator()` exactly as `serve::relay` does, and the agent-side
/// events are reassembled into the message the agent would hold at
/// `message_stop`. An error frame is the failure, before any missing timings
/// can be blamed. Exercises translation, not the socket — §7.1's honest
/// scope limit.
pub fn cross_streaming(wire: &ProbeWire, req: &HttpRequest) -> Result<ProbeArtifact, ChekovError> {
    let (path, body) = forward_of(wire, &with_stream_flag(req)?)?;
    let body = adjust_body(&body, wire.pins, None)?;
    let sse = wire.http.post_json(&JsonRequest {
        url: format!("{}{}", wire.upstream.base_url, path),
        body,
        bearer: Some(wire.upstream.api_key.clone()),
    })?;
    let mut translator = wire.facade.stream_translator();
    let mut events = Vec::new();
    for data in data_lines(&sse) {
        events.extend(translator.on_chunk(data));
    }
    events.extend(translator.finish());
    let anthropic_body = assemble(&events)?;
    Ok(ProbeArtifact {
        anthropic_body,
        timings: stream_timings(&sse)?,
    })
}

/// The probe's Anthropic body with `stream: true`, as Claude Code sends it.
fn with_stream_flag(req: &HttpRequest) -> Result<HttpRequest, ChekovError> {
    let mut parsed: Value =
        serde_json::from_slice(&req.body).map_err(|e| ChekovError::ProxyBadRequest {
            reason: format!("probe body is not JSON: {e}"),
        })?;
    let object = parsed
        .as_object_mut()
        .ok_or_else(|| ChekovError::ProxyBadRequest {
            reason: "probe body is not a JSON object".to_owned(),
        })?;
    object.insert("stream".to_owned(), Value::Bool(true));
    Ok(HttpRequest {
        method: req.method.clone(),
        path: req.path.clone(),
        body: parsed.to_string().into_bytes(),
    })
}

/// The `data:` payloads of an SSE body — the same split `serve::relay` makes,
/// so what the bench feeds the translator is what the proxy would.
fn data_lines(sse: &str) -> impl Iterator<Item = &str> {
    sse.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
}

/// llama-server attaches `timings` to the last frame it streams; the latest
/// frame carrying one is the measurement. None at all is loud, as buffered.
fn stream_timings(sse: &str) -> Result<Timings, ChekovError> {
    let last = data_lines(sse)
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .filter(|frame| frame.get("timings").is_some())
        .last()
        .ok_or(ChekovError::BenchNoTimings)?;
    read_timings(&last.to_string())
}

/// The agent-side SSE events folded back into one Anthropic message: what an
/// SDK client holds after `message_stop`. An `error` event is a turn that was
/// never answered — it fails the crossing, and can never read as `end_turn`.
fn assemble(events: &[crate::core::proxy::SseEvent]) -> Result<String, ChekovError> {
    let mut assembly = Assembly::default();
    for event in events {
        let payload: Value =
            serde_json::from_str(&event.data).map_err(|e| ChekovError::ProxyBadRequest {
                reason: format!("translator emitted a non-JSON `{}` event: {e}", event.event),
            })?;
        match event.event.as_str() {
            "message_start" => assembly.message = payload["message"].clone(),
            "content_block_start" => assembly.open(&payload),
            "content_block_delta" => assembly.delta(&payload),
            "content_block_stop" => assembly.close(&payload),
            "message_delta" => assembly.terminal(&payload),
            "error" => {
                return Err(ChekovError::BenchStreamFailed {
                    reason: payload["error"]["message"]
                        .as_str()
                        .unwrap_or("unspecified")
                        .to_owned(),
                });
            }
            _ => {}
        }
    }
    assembly.message["content"] = Value::Array(assembly.blocks);
    Ok(assembly.message.to_string())
}

/// The message under construction: its envelope, its content blocks, and the
/// still-unparsed tool arguments per block.
#[derive(Default)]
struct Assembly {
    message: Value,
    blocks: Vec<Value>,
    partial_json: Vec<String>,
}

impl Assembly {
    fn index(payload: &Value) -> usize {
        usize::try_from(payload["index"].as_u64().unwrap_or(0)).unwrap_or(0)
    }

    fn open(&mut self, payload: &Value) {
        let index = Self::index(payload);
        self.blocks.resize(index + 1, Value::Null);
        self.partial_json.resize(index + 1, String::new());
        self.blocks[index] = payload["content_block"].clone();
    }

    fn delta(&mut self, payload: &Value) {
        let index = Self::index(payload);
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        let delta = &payload["delta"];
        let appended = |field: &str| delta[field].as_str().unwrap_or_default().to_owned();
        match delta["type"].as_str() {
            Some("text_delta") => push_str(&mut block["text"], &appended("text")),
            Some("thinking_delta") => push_str(&mut block["thinking"], &appended("thinking")),
            Some("input_json_delta") => {
                self.partial_json[index].push_str(&appended("partial_json"));
            }
            _ => {}
        }
    }

    /// A closed `tool_use` block parses its accumulated arguments; the
    /// buffered translator makes the same `{}` of an unparseable string.
    fn close(&mut self, payload: &Value) {
        let index = Self::index(payload);
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        if block["type"] == "tool_use" {
            let raw = self.partial_json.get(index).map_or("", String::as_str);
            block["input"] = serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({}));
        }
    }

    fn terminal(&mut self, payload: &Value) {
        if let Some(stop) = payload["delta"].get("stop_reason") {
            self.message["stop_reason"] = stop.clone();
        }
        if let Some(usage) = payload.get("usage") {
            self.message["usage"] = usage.clone();
        }
    }
}

/// Append to a JSON string field, creating it when absent.
fn push_str(field: &mut Value, text: &str) {
    let mut current = field.as_str().unwrap_or_default().to_owned();
    current.push_str(text);
    *field = Value::String(current);
}

/// Overwrite the forwarded body's sampling with the pinned values — and,
/// for a forced probe, the response-format grammar. Whatever the probe asked
/// for, the measurement is greedy, seeded, and (when forced) shaped.
fn adjust_body(
    body: &str,
    pins: SamplingPins,
    forced: Option<&Forced>,
) -> Result<String, ChekovError> {
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
    if let Some(f) = forced {
        object.insert(
            "response_format".to_owned(),
            serde_json::json!({
                "type": "json_schema",
                "json_schema": { "name": "tool_call", "schema": f.schema },
            }),
        );
        object.insert(
            "reasoning_format".to_owned(),
            Value::from(FORCED_REASONING_FORMAT),
        );
        if let Some(effort) = f.reasoning_effort {
            object.insert("reasoning_effort".to_owned(), Value::from(effort));
        }
    }
    Ok(parsed.to_string())
}

/// Per-request reasoning extraction on the forced wire ONLY.
///
/// A thinking-prefill template (ornith, the Qwen/Hermes family) is refused a
/// forced grammar by llama.cpp's specialized chat handler: it builds the
/// grammar root without a `<think>` alternative while the prefill carries the
/// template's own `<think>\n`, and sampler init throws (HTTP 400). Asking the
/// engine to extract reasoning admits the `<think>` span ahead of the
/// schema-constrained JSON — validated live 2026-08-29 (IDEAS, mechanism b).
/// It is one more difference from the unconstrained arm, so the report names
/// it; the unconstrained and streamed wires never carry it.
pub const FORCED_REASONING_FORMAT: &str = "deepseek";

/// The four numbers the sweep records. All-or-nothing: a partial timings
/// object must not become a partial measurement.
fn read_timings(upstream: &str) -> Result<Timings, ChekovError> {
    let parsed: Value = serde_json::from_str(upstream).map_err(|_| ChekovError::BenchNoTimings)?;
    timings_from(&parsed)
}

/// `read_timings` for a body that is already parsed — the infill crossing
/// needs the same object for its `content`, and one parse cannot disagree
/// with itself.
fn timings_from(parsed: &Value) -> Result<Timings, ChekovError> {
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
            cache_n: count("cache_n").unwrap_or(0),
        }),
        _ => Err(ChekovError::BenchNoTimings),
    }
}

/// Recast a missing-`timings` failure on the foreign path (C1).
///
/// `BenchNoTimings` prescribes rebuilding llama.cpp — meaningless advice
/// against a declared foreign runtime. On the foreign path only, name what
/// was declared instead; every other error, and the llama.cpp path
/// (`runtime: None`), passes through unchanged.
#[must_use]
pub fn foreign_timings_error(err: ChekovError, runtime: Option<&str>) -> ChekovError {
    match (err, runtime) {
        (ChekovError::BenchNoTimings, Some(runtime)) => ChekovError::ForeignTimingsUnsupported {
            runtime: runtime.to_owned(),
            reason: "the reply carried no llama.cpp timings object".to_owned(),
        },
        (err, _) => err,
    }
}

/// One extra file the model is shown beside the masked one, in llama.cpp's
/// `input_extra` shape.
///
/// The engine keeps the TAIL of the extra tokens when they exceed
/// `n_ctx − n_batch − 2·n_predict`; one file under 32 KiB at ctx ≥ 32K is
/// never trimmed.
pub struct ExtraChunk<'a> {
    pub filename: &'a str,
    pub text: &'a str,
}

/// One infill task on the wire: the file before and after the mask, the
/// gold's line count (to bound `n_predict`), and the other file when this
/// arm sends one.
pub struct InfillTask<'a> {
    pub prefix: &'a str,
    pub suffix: &'a str,
    pub gold_lines: usize,
    pub extra: Option<ExtraChunk<'a>>,
}

/// What `/infill` said: a fill, or that this model cannot infill at all —
/// a capability, recorded N/A, never a zero (spec §8).
pub enum InfillOutcome {
    Answered(ProbeArtifact),
    Unsupported(String),
}

/// `POST /infill` with the same pins as every probe.
///
/// llama.cpp resolves the FIM sentinels from GGUF metadata; chekov never
/// writes them. The artifact's `anthropic_body` carries the raw `content` —
/// there is no Anthropic door for infill, and the graders read it as text.
pub fn cross_infill(wire: &ProbeWire, task: &InfillTask) -> Result<InfillOutcome, ChekovError> {
    let posted = wire.http.post_json(&JsonRequest {
        url: format!("{}/infill", wire.upstream.base_url),
        body: infill_body(task, wire.pins.seed).to_string(),
        bearer: Some(wire.upstream.api_key.clone()),
    });
    let upstream_body = match posted {
        Ok(text) => text,
        Err(ChekovError::UpstreamRefused { reason, .. })
            if reason.to_lowercase().contains("infill") =>
        {
            return Ok(InfillOutcome::Unsupported(reason));
        }
        Err(e) => return Err(e),
    };
    let parsed: Value =
        serde_json::from_str(&upstream_body).map_err(|_| ChekovError::BenchNoTimings)?;
    let timings = timings_from(&parsed)?;
    // A 200 with no `content` is a broken reply, not an empty answer: graded
    // as "" it would score a silent zero on every tier.
    let content = parsed
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ChekovError::ProxyBadRequest {
            reason: "/infill reply has no string `content`".to_owned(),
        })?;
    Ok(InfillOutcome::Answered(ProbeArtifact {
        anthropic_body: content.to_owned(),
        timings,
    }))
}

/// The chat arm's fixed instruction (spec §6, verbatim). Hashing it into the
/// prompt-set hash makes a template edit a NAMED stamp change.
pub const FIM_CHAT_INSTRUCTION: &str = "You are completing code. Output ONLY \
the missing code between PREFIX and SUFFIX. No explanation, no code fences, \
no repetition of the prefix or suffix.\n";

/// The one user message the chat arm sends: instruction, the extra file when
/// this arm carries one, then PREFIX/SUFFIX/MIDDLE.
fn chat_fim_prompt(task: &InfillTask) -> String {
    let extra = task.extra.as_ref().map_or_else(String::new, |e| {
        format!("FILE {}:\n{}\n\n", e.filename, e.text)
    });
    format!(
        "{FIM_CHAT_INSTRUCTION}\n{extra}PREFIX:\n{}\n\nSUFFIX:\n{}\n\nMIDDLE:\n",
        task.prefix, task.suffix
    )
}

/// Spec §6's two normalization rules, in order, and nothing else.
fn normalize_chat_fill(reply: &str) -> String {
    let unfenced = strip_whole_fence(reply);
    unfenced.strip_suffix('\n').unwrap_or(&unfenced).to_owned()
}

/// Rule 1: when the ENTIRE reply is one fenced block, strip the fences and
/// any language tag. A fence anywhere else is content.
fn strip_whole_fence(reply: &str) -> String {
    let trimmed = reply.trim_end_matches('\n');
    let Some(rest) = trimmed.strip_prefix("```") else {
        return reply.to_owned();
    };
    let Some(body) = rest.strip_suffix("```") else {
        return reply.to_owned();
    };
    // Drop the language tag line (possibly empty) after the opening fence.
    match body.split_once('\n') {
        Some((_tag, inner)) => inner.to_owned(),
        None => String::new(),
    }
}

/// The stamp's prompt-set hash when the chat arm filled the codebase suite:
/// hash(existing value ‖ the template), first twelve hex chars (spec §6).
#[must_use]
pub fn chat_fim_hash(base: &str) -> String {
    let canonical = format!("{base}|chat-fim|{FIM_CHAT_INSTRUCTION}");
    crate::core::hash::sha256_hex(canonical.as_bytes())[..12].to_owned()
}

/// The reply's first TEXT block — a leading `thinking` block (a reasoning
/// model's extracted `reasoning_content`, translated ahead of the answer) is
/// skipped rather than mistaken for the fill. Mirrors `judge::parse_reply`,
/// the house pattern for reading an Anthropic-shaped body's content array.
fn chat_text_of(artifact: &ProbeArtifact) -> Result<String, ChekovError> {
    serde_json::from_str::<Value>(&artifact.anthropic_body)
        .ok()
        .and_then(|v| {
            v["content"]
                .as_array()
                .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
                .and_then(|b| b["text"].as_str())
                .map(str::to_owned)
        })
        .ok_or_else(|| ChekovError::ProxyBadRequest {
            reason: "chat fill has no text content".to_owned(),
        })
}

/// The chat-completions fill: same pins as `/infill` (`temperature 0`,
/// `top_k 1`, the gold-bounded budget), one deterministic user message,
/// crossing the translator exactly as the agentic probes do.
fn cross_fim_chat(wire: &ProbeWire, task: &InfillTask) -> Result<InfillOutcome, ChekovError> {
    let body = serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": n_predict_for(task.gold_lines),
        "temperature": 0,
        "top_k": 1,
        "messages": [{"role": "user", "content": chat_fim_prompt(task)}],
    });
    let artifact = cross(wire, &crate::core::bench::probes::anthropic_post(&body))?;
    let text = chat_text_of(&artifact)?;
    Ok(InfillOutcome::Answered(ProbeArtifact {
        anthropic_body: normalize_chat_fill(&text),
        timings: artifact.timings,
    }))
}

/// One fill, whichever transport this run rides (spec §6).
///
/// `fim` is a parameter rather than a `ProbeWire` field: `ProbeWire` is
/// constructed by struct literal from a read-only integration test
/// (`tests/bench_streamed_probe_crosses_the_translator.rs`, pinned by
/// `pushkin.toml`'s `read_only_paths`), so widening it would break a file
/// this task cannot touch. The caller (ultimately Task 4's runtime-based
/// selection) passes the choice down explicitly instead.
pub fn cross_fim(
    wire: &ProbeWire,
    fim: FimTransport,
    task: &InfillTask,
) -> Result<InfillOutcome, ChekovError> {
    match fim {
        FimTransport::Infill => cross_infill(wire, task),
        FimTransport::Chat => cross_fim_chat(wire, task),
    }
}

/// The token budget a gold of this many lines earns. The run loop records
/// the same number on the row, from here, so what the row says was sent and
/// what the wire sent cannot drift.
#[must_use]
pub fn n_predict_for(gold_lines: usize) -> u32 {
    u32::try_from((gold_lines * 36).max(64)).unwrap_or(u32::MAX)
}

/// The `/infill` request body: prefix/suffix, no chat prompt, the pins, the
/// extra files (one or none), and an `n_predict` bounded by the gold's size
/// (three tokens per twelve characters of line, floored at 64 so a one-liner
/// still gets room).
fn infill_body(task: &InfillTask, seed: u32) -> Value {
    let n_predict = n_predict_for(task.gold_lines);
    let input_extra = task.extra.as_ref().map_or_else(
        || serde_json::json!([]),
        |e| serde_json::json!([{ "filename": e.filename, "text": e.text }]),
    );
    serde_json::json!({
        "input_prefix": task.prefix,
        "input_suffix": task.suffix,
        "prompt": "",
        "input_extra": input_extra,
        "n_predict": n_predict,
        "temperature": 0,
        "top_k": 1,
        "seed": seed,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::{ReadyPolicy, ReadyTarget, assert_props_ctx, wait_ready};
    use crate::core::hub::{HttpClient, JsonRequest, StreamMarks};
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

    /// Upstream answering every POST with one canned `OpenAI` body, or — via
    /// `new_streamed` — one canned SSE body plus canned `StreamMarks` for the
    /// timed door.
    struct CannedUpstream {
        body: String,
        marks: Option<StreamMarks>,
        bearer_seen: RefCell<Option<String>>,
        sent_body: RefCell<Option<String>>,
        url_seen: RefCell<Option<String>>,
    }

    impl CannedUpstream {
        fn new(body: String) -> Self {
            Self {
                body,
                marks: None,
                bearer_seen: RefCell::new(None),
                sent_body: RefCell::new(None),
                url_seen: RefCell::new(None),
            }
        }

        fn new_streamed(body: String, marks: StreamMarks) -> Self {
            Self {
                marks: Some(marks),
                ..Self::new(body)
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
            *self.url_seen.borrow_mut() = Some(req.url.clone());
            Ok(self.body.clone())
        }

        fn post_json_stream_timed(
            &self,
            req: &JsonRequest,
        ) -> Result<(String, StreamMarks), ChekovError> {
            *self.bearer_seen.borrow_mut() = req.bearer.clone();
            *self.sent_body.borrow_mut() = Some(req.body.clone());
            *self.url_seen.borrow_mut() = Some(req.url.clone());
            Ok((
                self.body.clone(),
                self.marks.expect("test wires marks before streaming"),
            ))
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
                "cache_n": 512,
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

    /// Canned `StreamMarks` for tests that don't care about the exact
    /// numbers, only that a derivation is possible.
    fn some_marks() -> StreamMarks {
        StreamMarks {
            to_first_data: Duration::from_millis(100),
            first_to_done: Duration::from_secs(1),
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

    /// An SSE body as llama-server writes it: `data:` frames, a comment line,
    /// blank separators, and the `[DONE]` sentinel — everything `serve::relay`
    /// has to skip or stop on.
    fn sse(frames: &[serde_json::Value]) -> String {
        let mut out = String::from(": llama-server\n\n");
        for frame in frames {
            out.push_str("data: ");
            out.push_str(&frame.to_string());
            out.push_str("\n\n");
        }
        out.push_str("data: [DONE]\n\n");
        out
    }

    fn text_frame(text: &str) -> serde_json::Value {
        serde_json::json!({ "id": "c1", "choices": [{ "delta": { "content": text } }] })
    }

    /// The last frame llama-server sends: empty delta, finish reason, usage,
    /// and — with `include_usage` — the timings object.
    fn final_frame() -> serde_json::Value {
        serde_json::json!({
            "id": "c1",
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 900, "completion_tokens": 100 },
            "timings": {
                "cache_n": 512,
                "prompt_n": 900, "prompt_ms": 2000.0, "prompt_per_second": 450.0,
                "predicted_n": 100, "predicted_ms": 4608.3, "predicted_per_second": 21.7
            }
        })
    }

    fn parsed(artifact: &super::ProbeArtifact) -> serde_json::Value {
        serde_json::from_str(&artifact.anthropic_body).expect("the artifact is json")
    }

    #[test]
    fn a_streamed_crossing_asks_upstream_to_stream_and_grades_the_assembled_message() {
        let http = CannedUpstream::new(sse(&[
            text_frame("hel"),
            text_frame("lo there"),
            final_frame(),
        ]));
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let artifact = super::cross_streaming(&wire(&http, &facade, &up), &anthropic_request("hi"))
            .expect("a well-formed stream crosses");

        // The wire asks for a stream with usage, exactly as Claude Code's
        // request does after translation — and the pins still hold.
        let sent: serde_json::Value =
            serde_json::from_str(&http.sent_body.borrow().clone().expect("posted")).expect("json");
        assert_eq!(sent["stream"], true, "{sent}");
        assert_eq!(sent["stream_options"]["include_usage"], true, "{sent}");
        assert_eq!(sent["temperature"], 0, "{sent}");
        assert_eq!(sent["seed"], 42, "{sent}");

        // The artifact is the message the agent would have assembled.
        let body = parsed(&artifact);
        assert_eq!(body["type"], "message", "{body}");
        assert_eq!(body["content"][0]["type"], "text", "{body}");
        assert_eq!(body["content"][0]["text"], "hello there", "{body}");
        assert_eq!(body["stop_reason"], "end_turn", "{body}");
        assert_eq!(body["usage"]["output_tokens"], 100, "{body}");
        assert!((artifact.timings.predicted_per_second - 21.7).abs() < 1e-9);
        assert_eq!(artifact.timings.cache_n, 512);
    }

    #[test]
    fn a_streamed_tool_call_is_reassembled_from_its_json_deltas() {
        let http = CannedUpstream::new(sse(&[
            serde_json::json!({ "id": "c1", "choices": [{ "delta": { "tool_calls": [
                { "index": 0, "id": "call_1", "function": { "name": "get_weather", "arguments": "" } }
            ] } }] }),
            serde_json::json!({ "id": "c1", "choices": [{ "delta": { "tool_calls": [
                { "index": 0, "function": { "arguments": "{\"city\":" } }
            ] } }] }),
            serde_json::json!({ "id": "c1", "choices": [{ "delta": { "tool_calls": [
                { "index": 0, "function": { "arguments": " \"Paris\"}" } }
            ] } }] }),
            serde_json::json!({
                "id": "c1",
                "choices": [{ "delta": {}, "finish_reason": "tool_calls" }],
                "usage": { "prompt_tokens": 9, "completion_tokens": 12 },
                "timings": final_frame()["timings"]
            }),
        ]));
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let artifact = super::cross_streaming(
            &wire(&http, &facade, &up),
            &anthropic_request("weather in paris"),
        )
        .expect("crosses");
        let body = parsed(&artifact);
        assert_eq!(body["content"][0]["type"], "tool_use", "{body}");
        assert_eq!(body["content"][0]["name"], "get_weather", "{body}");
        assert_eq!(body["content"][0]["id"], "call_1", "{body}");
        assert_eq!(
            body["content"][0]["input"],
            serde_json::json!({ "city": "Paris" }),
            "{body}"
        );
        assert_eq!(body["stop_reason"], "tool_use", "{body}");
    }

    #[test]
    fn an_upstream_error_frame_is_unavailable_never_a_fake_end_turn() {
        // A context overflow mid-turn is the daily one: llama-server keeps the
        // 200 and sends an `error` frame. That turn was never answered.
        let http = CannedUpstream::new(sse(&[
            text_frame("partial"),
            serde_json::json!({ "error": { "message": "context overflow", "code": 400 } }),
        ]));
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let err = super::cross_streaming(&wire(&http, &facade, &up), &anthropic_request("hi"))
            .expect_err("an error frame is not a graded reply");
        assert!(
            matches!(err, ChekovError::BenchStreamFailed { .. }),
            "typed as a mid-stream failure: {err}"
        );
        assert!(err.to_string().contains("context overflow"), "{err}");
    }

    #[test]
    fn a_stream_without_timings_is_loud_rather_than_a_number() {
        let http = CannedUpstream::new(sse(&[
            text_frame("hello"),
            serde_json::json!({ "id": "c1", "choices": [{ "delta": {}, "finish_reason": "stop" }] }),
        ]));
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let err = super::cross_streaming(&wire(&http, &facade, &up), &anthropic_request("hi"))
            .expect_err("no timings, no measurement");
        assert!(matches!(err, ChekovError::BenchNoTimings), "{err}");
    }

    #[test]
    fn stream_timings_derive_from_usage_counts_and_the_two_windows() {
        let usage = super::StreamUsage {
            prompt_tokens: 100,
            completion_tokens: 51,
        };
        let marks = StreamMarks {
            to_first_data: Duration::from_millis(500),
            first_to_done: Duration::from_secs(2),
        };
        let t = super::timings_from_stream(&usage, &marks).unwrap();
        assert_eq!(t.prompt_n, 100);
        assert!((t.prompt_per_second - 200.0).abs() < 1e-9);
        assert_eq!(t.predicted_n, 51);
        assert!((t.predicted_per_second - 25.0).abs() < 1e-9);
        assert_eq!(t.cache_n, 0);
    }

    /// Each undervivable stream is refused with its own reason (spec §3):
    /// zero prompt tokens, too few completion tokens to time a decode
    /// window, and a zero-length measurement window.
    #[test]
    fn each_underivable_stream_is_refused_with_its_reason() {
        let marks = some_marks();

        let zero_prompt = super::StreamUsage {
            prompt_tokens: 0,
            completion_tokens: 10,
        };
        let err = super::timings_from_stream(&zero_prompt, &marks).unwrap_err();
        assert!(err.to_string().contains("usage.prompt_tokens is 0"), "{err}");

        let one_completion = super::StreamUsage {
            prompt_tokens: 10,
            completion_tokens: 1,
        };
        let err = super::timings_from_stream(&one_completion, &marks).unwrap_err();
        assert!(
            err.to_string()
                .contains("fewer than 2 completion tokens — no decode window to time"),
            "{err}"
        );

        let zero_window = super::StreamUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
        };
        let stalled = StreamMarks {
            to_first_data: Duration::ZERO,
            first_to_done: Duration::from_secs(1),
        };
        let err = super::timings_from_stream(&zero_window, &stalled).unwrap_err();
        assert!(err.to_string().contains("zero-length timing window"), "{err}");
    }

    #[test]
    fn the_usage_frame_is_the_last_one_and_absence_is_none() {
        let sse = "data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\
                   data: {\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":8}}\n\
                   data: [DONE]\n";
        let u = super::stream_usage(sse).unwrap();
        assert_eq!((u.prompt_tokens, u.completion_tokens), (9, 8));
        assert!(super::stream_usage("data: {\"x\":1}\n").is_none());
    }

    /// `cross_stream_timed` runs the same SSE machinery as `cross_streaming`
    /// but times it with chekov's own clock instead of reading a llama.cpp
    /// `timings` object.
    #[test]
    fn cross_stream_timed_assembles_the_body_and_times_it_with_chekovs_clock() {
        let marks = some_marks();
        let http = CannedUpstream::new_streamed(
            sse(&[
                text_frame("hi there"),
                serde_json::json!({
                    "id": "c1",
                    "choices": [{ "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 10, "completion_tokens": 3 }
                }),
            ]),
            marks,
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let artifact =
            super::cross_stream_timed(&wire(&http, &facade, &up), &anthropic_request("hi"))
                .expect("a well-formed timed stream crosses");

        let sent: serde_json::Value =
            serde_json::from_str(&http.sent_body.borrow().clone().expect("posted")).expect("json");
        assert_eq!(sent["stream"], true, "{sent}");

        let body = parsed(&artifact);
        assert_eq!(body["content"][0]["text"], "hi there", "{body}");

        let usage = super::StreamUsage {
            prompt_tokens: 10,
            completion_tokens: 3,
        };
        let expected = super::timings_from_stream(&usage, &marks).expect("derivable");
        assert_eq!(artifact.timings.prompt_n, expected.prompt_n);
        assert!((artifact.timings.predicted_per_second - 2.0).abs() < 1e-9);
        assert!(
            (artifact.timings.predicted_per_second - expected.predicted_per_second).abs() < 1e-9
        );

        let no_usage = CannedUpstream::new_streamed(
            sse(&[
                text_frame("hi there"),
                serde_json::json!({ "id": "c1", "choices": [{ "delta": {}, "finish_reason": "stop" }] }),
            ]),
            marks,
        );
        let err =
            super::cross_stream_timed(&wire(&no_usage, &facade, &up), &anthropic_request("hi"))
                .expect_err("no usage frame, no measurement");
        assert!(
            matches!(&err, ChekovError::ForeignTimingsUnsupported { reason, .. } if reason == "no usage object in the stream"),
            "{err}"
        );
    }

    #[test]
    fn a_transport_dispatches_to_its_door() {
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();

        let buffered = CannedUpstream::new(openai_with_timings());
        let artifact = super::cross_via(
            &wire(&buffered, &facade, &up),
            &anthropic_request("hi"),
            super::Transport::Buffered,
        )
        .expect("buffered crossing");
        let sent: serde_json::Value =
            serde_json::from_str(&buffered.sent_body.borrow().clone().expect("posted"))
                .expect("json");
        assert!(
            sent.get("stream").is_none(),
            "buffered never streams: {sent}"
        );
        assert_eq!(parsed(&artifact)["content"][0]["text"], "hello there");

        let streamed = CannedUpstream::new(sse(&[text_frame("hello there"), final_frame()]));
        let artifact = super::cross_via(
            &wire(&streamed, &facade, &up),
            &anthropic_request("hi"),
            super::Transport::Streamed,
        )
        .expect("streamed crossing");
        let sent: serde_json::Value =
            serde_json::from_str(&streamed.sent_body.borrow().clone().expect("posted"))
                .expect("json");
        assert_eq!(sent["stream"], true, "{sent}");
        assert_eq!(parsed(&artifact)["content"][0]["text"], "hello there");
    }

    /// What went up the wire, parsed.
    fn sent(http: &CannedUpstream) -> serde_json::Value {
        serde_json::from_str(&http.sent_body.borrow().clone().expect("posted")).expect("json")
    }

    #[test]
    fn an_infill_crossing_posts_prefix_suffix_and_pins_and_returns_the_raw_fill() {
        let http = CannedUpstream::new(
            serde_json::json!({
                "content": "    a + b\n",
                "tokens_predicted": 6,
                "timings": final_frame()["timings"]
            })
            .to_string(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask {
            prefix: "fn add(a: i32, b: i32) -> i32 {\n",
            suffix: "\n}\n",
            gold_lines: 1,
            extra: None,
        };
        let outcome = super::cross_infill(&wire(&http, &facade, &up), &task).expect("crosses");
        let super::InfillOutcome::Answered(artifact) = outcome else {
            panic!("a 200 with content is an answer");
        };
        assert_eq!(artifact.anthropic_body, "    a + b\n");
        assert_eq!(artifact.timings.cache_n, 512);
        let sent = sent(&http);
        assert_eq!(sent["input_prefix"], "fn add(a: i32, b: i32) -> i32 {\n");
        assert_eq!(sent["input_suffix"], "\n}\n");
        assert_eq!(sent["prompt"], "");
        assert_eq!(sent["input_extra"], serde_json::json!([]));
        assert_eq!(sent["temperature"], 0);
        assert_eq!(sent["top_k"], 1);
        assert_eq!(sent["seed"], 42);
        assert_eq!(sent["n_predict"], 64, "max(64, 3*lines*12)");
        assert!(
            http.url_seen
                .borrow()
                .as_deref()
                .unwrap_or("")
                .ends_with("/infill")
        );
    }

    #[test]
    fn an_extra_chunk_goes_up_as_input_extra_in_llama_cpps_shape() {
        let http = CannedUpstream::new(
            serde_json::json!({
                "content": "    Widget { id: 1 }\n",
                "tokens_predicted": 6,
                "timings": final_frame()["timings"]
            })
            .to_string(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask {
            prefix: "fn f() {\n",
            suffix: "\n}\n",
            gold_lines: 1,
            extra: Some(super::ExtraChunk {
                filename: "src/defs.rs",
                text: "pub struct Widget { pub id: u32 }\n",
            }),
        };
        super::cross_infill(&wire(&http, &facade, &up), &task).expect("crosses");
        let sent = sent(&http);
        assert_eq!(
            sent["input_extra"].as_array().map(Vec::len),
            Some(1),
            "{sent}"
        );
        assert_eq!(sent["input_extra"][0]["filename"], "src/defs.rs");
        assert_eq!(
            sent["input_extra"][0]["text"],
            "pub struct Widget { pub id: u32 }\n"
        );
        assert_eq!(sent["input_prefix"], "fn f() {\n", "nothing else moved");
        assert_eq!(sent["n_predict"], 64);
        assert_eq!(sent["temperature"], 0);
        assert_eq!(sent["top_k"], 1);
        assert_eq!(sent["seed"], 42);
    }

    #[test]
    fn a_200_without_content_is_an_error_not_an_empty_fill() {
        let http = CannedUpstream::new(
            serde_json::json!({ "timings": final_frame()["timings"] }).to_string(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask {
            prefix: "fn f() {\n",
            suffix: "\n}\n",
            gold_lines: 1,
            extra: None,
        };
        let Err(err) = super::cross_infill(&wire(&http, &facade, &up), &task) else {
            panic!("a reply with no fill is not an answer");
        };
        assert!(
            matches!(&err, ChekovError::ProxyBadRequest { reason } if reason.contains("no string `content`")),
            "{err}"
        );
    }

    #[test]
    fn a_model_without_fim_tokens_is_a_capability_not_a_failure() {
        struct Refusing;
        impl HttpClient for Refusing {
            fn get(&self, _url: &str) -> Result<String, ChekovError> {
                unreachable!()
            }
            fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
                Err(ChekovError::UpstreamRefused {
                    url: "http://fake/infill".into(),
                    status: 400,
                    reason: "infill is not supported by this model: missing FIM tokens".into(),
                })
            }
        }
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let w = super::ProbeWire {
            http: &Refusing,
            facade: &facade,
            upstream: &up,
            pins: super::SamplingPins { seed: 42 },
        };
        let task = super::InfillTask {
            prefix: "x",
            suffix: "y",
            gold_lines: 1,
            extra: None,
        };
        match super::cross_infill(&w, &task).expect("a refusal naming infill is an outcome") {
            super::InfillOutcome::Unsupported(reason) => {
                assert!(reason.contains("FIM tokens"), "{reason}");
            }
            super::InfillOutcome::Answered(_) => panic!("must not be graded"),
        }
    }

    #[test]
    fn the_chat_fim_prompt_carries_the_instruction_the_extra_and_the_three_sections() {
        let task = super::InfillTask {
            prefix: "fn a() {",
            suffix: "}",
            gold_lines: 1,
            extra: Some(super::ExtraChunk {
                filename: "lib.rs",
                text: "pub fn b() {}",
            }),
        };
        let prompt = super::chat_fim_prompt(&task);
        assert!(prompt.starts_with(super::FIM_CHAT_INSTRUCTION));
        for needle in [
            "FILE lib.rs:",
            "pub fn b() {}",
            "PREFIX:\nfn a() {",
            "SUFFIX:\n}",
            "MIDDLE:\n",
        ] {
            assert!(prompt.contains(needle), "missing {needle}");
        }
        let suffix_at = prompt.find("SUFFIX:").unwrap();
        assert!(prompt.find("PREFIX:").unwrap() < suffix_at);
        assert!(suffix_at < prompt.find("MIDDLE:").unwrap());
    }

    #[test]
    fn a_fenced_reply_is_unwrapped_and_one_trailing_newline_trimmed() {
        assert_eq!(
            super::normalize_chat_fill("```rust\nlet x = 1;\n```\n"),
            "let x = 1;"
        );
        assert_eq!(super::normalize_chat_fill("let x = 1;\n"), "let x = 1;");
        // A fence in the middle is content, not wrapping:
        assert_eq!(super::normalize_chat_fill("a\n```\nb"), "a\n```\nb");
    }

    #[test]
    fn the_chat_fim_hash_diverges_from_its_base_and_is_stable() {
        let a = super::chat_fim_hash("codebase-only");
        assert_ne!(a, "codebase-only");
        assert_eq!(a, super::chat_fim_hash("codebase-only"));
        assert_ne!(a, super::chat_fim_hash("other"));
        assert_eq!(a.len(), 12);
    }

    /// A one-user-message chat crossing sends the pins and the exact
    /// template prompt, and returns the normalized fill (spec §6, I3a).
    #[test]
    fn cross_fim_chat_sends_the_pins_and_the_prompt_and_returns_the_normalized_fill() {
        let http = CannedUpstream::new_streamed(
            sse(&[
                text_frame("```rust\nlet a = 1;\n```\n"),
                serde_json::json!({
                    "id": "c1",
                    "choices": [{ "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 900, "completion_tokens": 100 }
                }),
            ]),
            some_marks(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask {
            prefix: "fn a() {\n",
            suffix: "\n}\n",
            gold_lines: 1,
            extra: None,
        };
        let outcome =
            super::cross_fim(&wire(&http, &facade, &up), super::FimTransport::Chat, &task)
                .expect("crosses");
        let super::InfillOutcome::Answered(artifact) = outcome else {
            panic!("a 200 with content is an answer");
        };
        assert_eq!(artifact.anthropic_body, "let a = 1;");
        let sent = sent(&http);
        assert_eq!(sent["messages"].as_array().map(Vec::len), Some(1));
        assert_eq!(sent["messages"][0]["role"], "user");
        assert_eq!(
            sent["messages"][0]["content"],
            super::chat_fim_prompt(&task)
        );
        assert_eq!(sent["temperature"], 0);
        assert_eq!(sent["top_k"], 1);
        assert_eq!(sent["seed"], 42);
        assert_eq!(sent["max_tokens"], super::n_predict_for(task.gold_lines));
        assert_eq!(sent["stream"], true, "{sent}");
    }

    /// `cross_fim(.., Infill, ..)` still rides `/infill` — the chat wire never
    /// hijacks llama.cpp's own door (I3d).
    #[test]
    fn cross_fim_still_dispatches_infill_to_the_infill_door() {
        let http = CannedUpstream::new(
            serde_json::json!({
                "content": "let a = 1;",
                "timings": final_frame()["timings"]
            })
            .to_string(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask {
            prefix: "fn a() {\n",
            suffix: "\n}\n",
            gold_lines: 1,
            extra: None,
        };
        super::cross_fim(
            &wire(&http, &facade, &up),
            super::FimTransport::Infill,
            &task,
        )
        .expect("crosses");
        assert!(
            http.url_seen
                .borrow()
                .as_deref()
                .unwrap_or("")
                .ends_with("/infill")
        );
    }

    /// A leading `thinking` block (a reasoning model's `reasoning_content`,
    /// translated ahead of the answer) must not hide the text block that
    /// follows it — I1, locked here against a wire-level canned reply.
    #[test]
    fn a_leading_thinking_block_does_not_hide_the_chat_fill_text() {
        let http = CannedUpstream::new_streamed(
            sse(&[
                serde_json::json!({ "id": "c1", "choices": [{ "delta": {
                    "reasoning_content": "considering the prefix and suffix",
                    "content": "let a = 1;"
                } }] }),
                serde_json::json!({
                    "id": "c1",
                    "choices": [{ "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 900, "completion_tokens": 100 }
                }),
            ]),
            some_marks(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask {
            prefix: "fn a() {\n",
            suffix: "\n}\n",
            gold_lines: 1,
            extra: None,
        };
        let outcome =
            super::cross_fim(&wire(&http, &facade, &up), super::FimTransport::Chat, &task)
                .expect("a leading thinking block still yields the text");
        let super::InfillOutcome::Answered(artifact) = outcome else {
            panic!("must be answered");
        };
        assert_eq!(artifact.anthropic_body, "let a = 1;");
    }

    /// A reply with no text block anywhere (a thinking-only or empty
    /// content) fails loudly rather than grading an empty string (I3c).
    #[test]
    fn a_chat_reply_with_no_text_block_fails_as_a_bad_request() {
        let http = CannedUpstream::new_streamed(
            sse(&[serde_json::json!({
                "id": "c1",
                "choices": [{ "delta": {}, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 900, "completion_tokens": 100 }
            })]),
            some_marks(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask {
            prefix: "fn a() {\n",
            suffix: "\n}\n",
            gold_lines: 1,
            extra: None,
        };
        let Err(err) =
            super::cross_fim(&wire(&http, &facade, &up), super::FimTransport::Chat, &task)
        else {
            panic!("no text content should not be an answer");
        };
        assert!(
            matches!(&err, ChekovError::ProxyBadRequest { reason } if reason == "chat fill has no text content"),
            "{err}"
        );
    }

    #[test]
    fn only_the_forced_wire_asks_the_engine_to_extract_reasoning() {
        // A thinking-prefill template (ornith) 400s on a forced grammar unless
        // the engine extracts reasoning: the specialized chat handler builds
        // the grammar root without a <think> alternative while the prefill
        // carries the template's own `<think>\n`. `reasoning_format: deepseek`
        // on the forced wire ONLY is the validated way through (IDEAS,
        // mechanism b); the unconstrained arms must stay byte-identical.
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let schema = serde_json::json!({ "type": "object" });

        let forced = CannedUpstream::new(openai_with_timings());
        super::cross_forced(
            &wire(&forced, &facade, &up),
            &anthropic_request("go"),
            &schema,
        )
        .expect("forced crossing");
        assert_eq!(
            sent(&forced)["reasoning_format"],
            "deepseek",
            "{}",
            sent(&forced)
        );
        assert_eq!(sent(&forced)["response_format"]["type"], "json_schema");

        let plain = CannedUpstream::new(openai_with_timings());
        super::cross(&wire(&plain, &facade, &up), &anthropic_request("go")).expect("buffered");
        assert!(
            sent(&plain).get("reasoning_format").is_none(),
            "{}",
            sent(&plain)
        );

        let streamed = CannedUpstream::new(sse(&[text_frame("hi"), final_frame()]));
        super::cross_streaming(&wire(&streamed, &facade, &up), &anthropic_request("go"))
            .expect("streamed");
        assert!(
            sent(&streamed).get("reasoning_format").is_none(),
            "{}",
            sent(&streamed)
        );
        assert_eq!(super::FORCED_REASONING_FORMAT, "deepseek");
    }

    #[test]
    fn transport_names_are_the_stored_spelling() {
        assert_eq!(
            serde_json::to_string(&super::Transport::Streamed).expect("json"),
            "\"streamed\""
        );
        assert_eq!(super::Transport::default(), super::Transport::Buffered);
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
        assert_eq!(art.timings.cache_n, 512, "prefix-cache reuse is recorded");
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
    fn the_forced_wire_carries_response_format_beside_the_pins() {
        let http = CannedUpstream::new(openai_with_timings());
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let schema = serde_json::json!({"oneOf": [{"type": "object"}]});
        super::cross_forced(
            &wire(&http, &facade, &up),
            &anthropic_request("call it"),
            &schema,
        )
        .expect("crossing");
        let sent = http.sent_body.borrow().clone().expect("a body was sent");
        let sent: serde_json::Value = serde_json::from_str(&sent).expect("sent body is json");
        assert_eq!(sent["response_format"]["type"], "json_schema", "{sent}");
        assert_eq!(
            sent["response_format"]["json_schema"]["schema"]["oneOf"][0]["type"], "object",
            "{sent}"
        );
        assert_eq!(sent["temperature"], 0, "the pins still hold: {sent}");
    }

    #[test]
    fn only_a_judge_crossing_carries_reasoning_effort() {
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let schema = serde_json::json!({"type": "object"});
        let judge = CannedUpstream::new(openai_with_timings());
        super::cross_forced_with(
            &wire(&judge, &facade, &up),
            &anthropic_request("judge it"),
            &super::Forced {
                schema: &schema,
                reasoning_effort: Some("low"),
            },
        )
        .expect("judge crossing");
        assert_eq!(sent(&judge)["reasoning_effort"], "low", "{}", sent(&judge));
        assert_eq!(sent(&judge)["response_format"]["type"], "json_schema");
        assert_eq!(sent(&judge)["reasoning_format"], "deepseek");

        let probe = CannedUpstream::new(openai_with_timings());
        super::cross_forced(
            &wire(&probe, &facade, &up),
            &anthropic_request("go"),
            &schema,
        )
        .expect("forced probe");
        assert!(
            sent(&probe).get("reasoning_effort").is_none(),
            "{}",
            sent(&probe)
        );
    }

    #[test]
    fn a_missing_cache_n_is_zero_cached_not_a_missing_measurement() {
        let body = serde_json::json!({
            "choices": [{ "message": { "content": "hi" }, "finish_reason": "stop" }],
            "timings": {
                "prompt_n": 10, "prompt_per_second": 100.0,
                "predicted_n": 5, "predicted_per_second": 20.0
            }
        })
        .to_string();
        let http = CannedUpstream::new(body);
        let facade = ClaudeFacade::new("m");
        let up = fake_upstream();
        let art =
            super::cross(&wire(&http, &facade, &up), &anthropic_request("hi")).expect("crosses");
        assert_eq!(art.timings.cache_n, 0);
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

    /// A missing-timings failure against a declared foreign runtime is
    /// recast naming that runtime — `chekov update --engine` is meaningless
    /// advice for a server chekov did not build (C1). Untouched (no
    /// declared runtime) it passes through unchanged, and it never rewrites
    /// an unrelated error.
    #[test]
    fn foreign_timings_error_names_the_runtime_and_leaves_everything_else_alone() {
        let recast = super::foreign_timings_error(ChekovError::BenchNoTimings, Some("mtplx 0.4.1"));
        assert!(
            matches!(&recast, ChekovError::ForeignTimingsUnsupported { runtime, .. } if runtime == "mtplx 0.4.1"),
            "{recast}"
        );
        assert!(recast.to_string().contains("mtplx 0.4.1"), "{recast}");
        assert!(
            !recast.to_string().contains("chekov update --engine"),
            "a foreign server is not fixed by rebuilding chekov's own engine: {recast}"
        );

        let unchanged = super::foreign_timings_error(ChekovError::BenchNoTimings, None);
        assert!(matches!(unchanged, ChekovError::BenchNoTimings));

        let other = ChekovError::ProxyBadRequest {
            reason: "unrelated".to_owned(),
        };
        let passthrough = super::foreign_timings_error(other, Some("mtplx 0.4.1"));
        assert!(matches!(passthrough, ChekovError::ProxyBadRequest { .. }));

        // A guard inside `timings_from_stream`/`cross_stream_timed` has no
        // declared runtime to hand, so it raises with the placeholder
        // "unknown" — the recast must replace it with the caller's runtime,
        // not merely pass an already-`ForeignTimingsUnsupported` error through.
        let placeholder = ChekovError::ForeignTimingsUnsupported {
            runtime: "unknown".to_owned(),
            reason: "no usage object in the stream".to_owned(),
        };
        let recast_again = super::foreign_timings_error(placeholder, Some("mtplx 0.4.1"));
        assert!(
            matches!(&recast_again, ChekovError::ForeignTimingsUnsupported { runtime, reason }
                if runtime == "mtplx 0.4.1" && reason == "no usage object in the stream"),
            "the declared runtime overwrites the guard's placeholder: {recast_again}"
        );
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
