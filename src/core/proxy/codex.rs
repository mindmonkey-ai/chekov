//! Stateless Responses API adapter for Codex and a local Chat Completions server.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::http::{HttpRequest, HttpResponse};
use super::{Action, AgentFacade, Forward, SseEvent, StreamTranslator};
use crate::error::ChekovError;

const CUSTOM_PREFIX: &str = "chekov_custom_";
const NAMESPACE_PREFIX: &str = "chekov_ns_";
static NEXT_RESPONSE: AtomicU64 = AtomicU64::new(1);

/// Codex reads `ModelInfo` entries from a catalog separately from `/v1/models`.
#[must_use]
pub fn model_catalog(model: &str, ctx_size: u32) -> String {
    json!({"models": [{
        "slug": model,
        "display_name": model,
        "description": "Local model served by Chekov",
        "supported_reasoning_levels": [],
        "shell_type": "unified_exec",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 0,
        "base_instructions": "You are a coding assistant running in Codex CLI against a local model. \
            Complete the user's coding task in the shared workspace. Inspect relevant code before \
            editing, respect repository instructions and configured permissions, preserve unrelated \
            work, and verify changes with appropriate checks. Communicate progress and report \
            results and any unresolved limitations clearly.",
        "include_apps_usage_instructions": false,
        "supports_reasoning_summary_parameter": false,
        "default_reasoning_summary": "none",
        "support_verbosity": false,
        "truncation_policy": {"mode": "bytes", "limit": 10_000},
        "context_window": ctx_size,
        "max_context_window": ctx_size,
        "experimental_supported_tools": [],
        "input_modalities": ["text"]
    }]}).to_string()
}

/// CLI overrides preserve Codex's existing home, MCP servers, skills and policies.
#[must_use]
pub fn launch_args(model: &str, ctx_size: u32, port: u16) -> Vec<String> {
    [
        format!("model={}", json!(model)),
        "model_provider=\"chekov\"".to_owned(),
        format!(
            "model_providers.chekov={{name=\"Chekov\", \
             base_url=\"http://127.0.0.1:{port}/v1\", wire_api=\"responses\", \
             requires_openai_auth=false, supports_websockets=false}}"
        ),
        format!("model_context_window={ctx_size}"),
        format!(
            "model_auto_compact_token_limit={}",
            u64::from(ctx_size) * 4 / 5
        ),
        "model_supports_reasoning_summaries=false".to_owned(),
        "web_search=\"disabled\"".to_owned(),
    ]
    .into_iter()
    .flat_map(|setting| ["-c".to_owned(), setting])
    .collect()
}

#[must_use]
pub fn shell_command(args: &[String]) -> String {
    std::iter::once("codex".to_owned())
        .chain(
            args.iter()
                .map(|arg| format!("'{}'", arg.replace('\'', "'\\''"))),
        )
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct CodexFacade {
    model: String,
}

impl CodexFacade {
    #[must_use]
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
        }
    }
}

impl AgentFacade for CodexFacade {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn route(&self, req: &HttpRequest) -> Result<Action, ChekovError> {
        let path = req.path.split('?').next().unwrap_or(&req.path);
        match (req.method.as_str(), path) {
            ("POST", "/v1/responses" | "/responses") => {
                let body = parse_json(req.body_str()?)?;
                let translated = to_chat_request(&body, &self.model)?;
                Ok(Action::Forward(Forward {
                    path: "/v1/chat/completions".to_owned(),
                    stream: translated["stream"] == true,
                    body: translated.to_string().into_bytes(),
                }))
            }
            ("GET", "/v1/models" | "/models") => Ok(Action::Reply(HttpResponse::json(
                200,
                json!({"object":"list","data":[
                    {"id":self.model,"object":"model","owned_by":"chekov"}
                ]})
                .to_string(),
            ))),
            _ => Ok(Action::Reply(HttpResponse::error(
                404,
                "not_found_error",
                &format!("{} {path} is not proxied by chekov", req.method),
            ))),
        }
    }

    fn translate_response(&self, upstream: &str) -> Result<String, ChekovError> {
        let body = parse_json(upstream)?;
        let choice = &body["choices"][0];
        let mut stream = CodexStream::new(&self.model);
        stream.on_chunk(
            &json!({
                "choices":[{"delta":choice["message"],"finish_reason":choice["finish_reason"]}],
                "usage":body["usage"]
            })
            .to_string(),
        );
        let (response, _) = stream.complete()?;
        Ok(response.to_string())
    }

    fn stream_translator(&self) -> Box<dyn StreamTranslator> {
        Box::new(CodexStream::new(&self.model))
    }
}

fn bad_request(reason: impl Into<String>) -> ChekovError {
    ChekovError::ProxyBadRequest {
        reason: reason.into(),
    }
}

fn parse_json(raw: &str) -> Result<Value, ChekovError> {
    serde_json::from_str(raw).map_err(|e| bad_request(format!("invalid Codex JSON: {e}")))
}

fn required_text<'a>(value: &'a Value, key: &str) -> Result<&'a str, ChekovError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request(format!("Codex requires a string '{key}'")))
}

fn to_chat_request(body: &Value, model: &str) -> Result<Value, ChekovError> {
    if body["store"] == true
        || body["background"] == true
        || !body["previous_response_id"].is_null()
        || !body["conversation"].is_null()
    {
        return Err(bad_request(
            "Chekov requires stateless Responses requests with the full input history",
        ));
    }
    let mut messages = Vec::new();
    if let Some(instructions) = body["instructions"].as_str() {
        messages.push(json!({"role":"system","content":instructions}));
    }
    match &body["input"] {
        Value::String(text) => messages.push(json!({"role":"user","content":text})),
        Value::Array(items) => {
            for item in items {
                push_input(&mut messages, item)?;
            }
        }
        _ => return Err(bad_request("Codex input must be a string or an array")),
    }
    let mut out = json!({"model":model,"messages":messages,"stream":body["stream"] == true});
    if out["stream"] == true {
        out["stream_options"] = json!({"include_usage":true});
    }
    for key in ["temperature", "top_p", "parallel_tool_calls"] {
        if let Some(value) = body.get(key) {
            out[key] = value.clone();
        }
    }
    if let Some(max) = body.get("max_output_tokens") {
        out["max_tokens"] = max.clone();
    }
    copy_tools(body, &mut out)?;
    Ok(out)
}

fn push_input(messages: &mut Vec<Value>, item: &Value) -> Result<(), ChekovError> {
    match item["type"].as_str().unwrap_or("message") {
        "message" => {
            let role = required_text(item, "role")?;
            let role = match role {
                "developer" => "system",
                "system" | "user" | "assistant" => role,
                _ => return Err(bad_request(format!("unsupported Codex role '{role}'"))),
            };
            messages.push(json!({"role":role,"content":chat_content(&item["content"])?}));
        }
        "function_call" | "custom_tool_call" => push_call(messages, item)?,
        "function_call_output" | "custom_tool_call_output" => messages.push(json!({
            "role":"tool","tool_call_id":required_text(item,"call_id")?,
            "content":chat_content(&item["output"])?
        })),
        // These are Responses metadata, not another user/assistant message.
        "reasoning" => {}
        kind => {
            return Err(bad_request(format!(
                "unsupported Codex input item '{kind}'"
            )));
        }
    }
    Ok(())
}

fn chat_content(content: &Value) -> Result<Value, ChekovError> {
    match content {
        Value::String(_) => Ok(content.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part["type"].as_str() {
                Some("input_text" | "output_text" | "text") => {
                    Ok(json!({"type":"text","text":required_text(part,"text")?}))
                }
                Some("input_image") => Ok(json!({
                    "type":"image_url","image_url":{"url":required_text(part,"image_url")?}
                })),
                _ => Err(bad_request("unsupported Codex content part")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Err(bad_request(
            "Codex content must be text or an array of content parts",
        )),
    }
}

fn push_call(messages: &mut Vec<Value>, item: &Value) -> Result<(), ChekovError> {
    let name = flat_name(item)?;
    let (name, arguments) = match item["type"].as_str() {
        Some("custom_tool_call") => (
            format!("{CUSTOM_PREFIX}{name}"),
            json!({"input":required_text(item,"input")?}).to_string(),
        ),
        _ => (name, required_text(item, "arguments")?.to_owned()),
    };
    let call = json!({"id":required_text(item,"call_id")?,"type":"function",
        "function":{"name":name,"arguments":arguments}});
    if let Some(last) = messages
        .last_mut()
        .filter(|last| last["role"] == "assistant")
    {
        if last["tool_calls"].is_null() {
            last["tool_calls"] = json!([]);
        }
        if let Some(calls) = last["tool_calls"].as_array_mut() {
            calls.push(call);
        }
    } else {
        messages.push(json!({"role":"assistant","content":null,"tool_calls":[call]}));
    }
    Ok(())
}

fn copy_tools(body: &Value, out: &mut Value) -> Result<(), ChekovError> {
    if let Some(tools) = body.get("tools") {
        let tools = tools
            .as_array()
            .ok_or_else(|| bad_request("Codex tools must be an array"))?;
        out["tools"] = Value::Array(chat_tools(tools)?);
    }
    if let Some(choice) = body.get("tool_choice") {
        out["tool_choice"] = match choice {
            Value::String(value) if ["auto", "none", "required"].contains(&value.as_str()) => {
                choice.clone()
            }
            Value::Object(_) => {
                let name = flat_name(choice)?;
                let name = match choice["type"].as_str() {
                    Some("function") => name,
                    Some("custom") => format!("{CUSTOM_PREFIX}{name}"),
                    _ => return Err(bad_request("unsupported Codex tool choice")),
                };
                json!({"type":"function","function":{"name":name}})
            }
            _ => return Err(bad_request("unsupported Codex tool choice")),
        };
    }
    Ok(())
}

fn flat_name(tool: &Value) -> Result<String, ChekovError> {
    let name = required_text(tool, "name")?;
    if name.starts_with(CUSTOM_PREFIX) || name.starts_with(NAMESPACE_PREFIX) {
        return Err(bad_request("tool name uses a Chekov-reserved prefix"));
    }
    Ok(tool["namespace"].as_str().map_or_else(
        || name.to_owned(),
        |namespace| format!("{NAMESPACE_PREFIX}{}_{namespace}_{name}", namespace.len()),
    ))
}

fn split_name(name: &str) -> Result<(Option<&str>, &str), ChekovError> {
    let Some(encoded) = name.strip_prefix(NAMESPACE_PREFIX) else {
        return Ok((None, name));
    };
    let invalid = || bad_request("upstream returned a malformed namespaced tool name");
    let (length, rest) = encoded.split_once('_').ok_or_else(invalid)?;
    let length = length.parse::<usize>().map_err(|_| invalid())?;
    let namespace = rest.get(..length).ok_or_else(invalid)?;
    let name = rest
        .get(length..)
        .and_then(|name| name.strip_prefix('_'))
        .ok_or_else(invalid)?;
    Ok((Some(namespace), name))
}

fn chat_tools(tools: &[Value]) -> Result<Vec<Value>, ChekovError> {
    let mut out = Vec::new();
    for tool in tools {
        if tool["type"] == "namespace" {
            let namespace = required_text(tool, "name")?;
            let members = tool["tools"]
                .as_array()
                .ok_or_else(|| bad_request("a Codex namespace requires a tools array"))?;
            for member in members {
                let mut member = member
                    .as_object()
                    .cloned()
                    .ok_or_else(|| bad_request("a Codex namespace member must be a tool object"))?;
                member.insert("namespace".to_owned(), json!(namespace));
                out.push(chat_tool(&Value::Object(member))?);
            }
        } else {
            out.push(chat_tool(tool)?);
        }
    }
    Ok(out)
}

fn chat_tool(tool: &Value) -> Result<Value, ChekovError> {
    let name = flat_name(tool)?;
    let function = match tool["type"].as_str() {
        Some("function") => {
            let mut function = tool.clone();
            if let Some(object) = function.as_object_mut() {
                object.remove("type");
                object.remove("defer_loading");
                object.remove("namespace");
            }
            function["name"] = json!(name);
            function
        }
        Some("custom") => json!({
            "name":format!("{CUSTOM_PREFIX}{name}"),
            "description":tool["description"],
            "parameters":{"type":"object","properties":{"input":{"type":"string"}},
                "required":["input"],"additionalProperties":false}
        }),
        _ => {
            return Err(bad_request(format!(
                "unsupported Codex tool type {}; Chekov supports function, custom and namespace tools",
                tool["type"]
            )));
        }
    };
    Ok(json!({"type":"function","function":function}))
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

enum End {
    Pending,
    Complete,
    Length,
    Failed(String),
    Emitted,
}

#[derive(Clone, Copy)]
enum TextKind {
    Message,
    Reasoning,
}

struct CodexStream {
    model: String,
    id: String,
    sequence: u64,
    output: Vec<Value>,
    message: Option<usize>,
    reasoning: Option<usize>,
    calls: BTreeMap<u64, PendingCall>,
    usage: Value,
    end: End,
}

impl CodexStream {
    fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            id: format!(
                "resp_chekov_{}",
                NEXT_RESPONSE.fetch_add(1, Ordering::Relaxed)
            ),
            sequence: 0,
            output: Vec::new(),
            message: None,
            reasoning: None,
            calls: BTreeMap::new(),
            usage: json!({"input_tokens":0,"output_tokens":0,"total_tokens":0}),
            end: End::Pending,
        }
    }

    fn event(&mut self, kind: &str, mut data: Value) -> SseEvent {
        data["type"] = json!(kind);
        data["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        SseEvent::new(kind, data.to_string())
    }

    fn envelope(&self, status: &str) -> Value {
        json!({"id":self.id,"object":"response","created_at":0,
            "model":self.model,"status":status,"output":self.output,
            "usage":self.usage,"error":null,"incomplete_details":null})
    }

    fn text_delta(&mut self, text: &str, kind: TextKind) -> Vec<SseEvent> {
        if text.is_empty() {
            return Vec::new();
        }
        let (index, mut events) = self.text_item(kind);
        let (field, delta_type) = match kind {
            TextKind::Message => ("content", "response.output_text.delta"),
            TextKind::Reasoning => ("summary", "response.reasoning_summary_text.delta"),
        };
        let previous = self.output[index][field][0]["text"].as_str().unwrap_or("");
        self.output[index][field][0]["text"] = json!(format!("{previous}{text}"));
        events.push(self.event(
            delta_type,
            json!({
                "item_id":self.output[index]["id"],"output_index":index,
                "content_index":0,"summary_index":0,"delta":text
            }),
        ));
        events
    }

    fn text_item(&mut self, kind: TextKind) -> (usize, Vec<SseEvent>) {
        let (slot, item, part_event, part) = match kind {
            TextKind::Message => (
                &mut self.message,
                json!({"type":"message","role":"assistant","status":"in_progress","content":[]}),
                "response.content_part.added",
                json!({"type":"output_text","text":"","annotations":[]}),
            ),
            TextKind::Reasoning => (
                &mut self.reasoning,
                json!({"type":"reasoning","summary":[]}),
                "response.reasoning_summary_part.added",
                json!({"type":"summary_text","text":""}),
            ),
        };
        if let Some(index) = *slot {
            return (index, Vec::new());
        }
        let index = self.output.len();
        *slot = Some(index);
        let mut item = item;
        item["id"] = json!(format!("item_{}_{index}", self.id));
        self.output.push(item.clone());
        let field = match kind {
            TextKind::Message => "content",
            TextKind::Reasoning => "summary",
        };
        self.output[index][field] = json!([part]);
        let events = vec![
            self.event(
                "response.output_item.added",
                json!({"output_index":index,"item":item}),
            ),
            self.event(
                part_event,
                json!({"item_id":self.output[index]["id"],
                "output_index":index,"content_index":0,"summary_index":0,"part":part}),
            ),
        ];
        (index, events)
    }

    fn collect_calls(&mut self, calls: &Value) -> Result<(), ChekovError> {
        if calls.is_null() {
            return Ok(());
        }
        let Some(calls) = calls.as_array() else {
            return Err(bad_request("upstream tool_calls must be an array"));
        };
        for (position, call) in calls.iter().enumerate() {
            let index = call["index"].as_u64().unwrap_or(position as u64);
            let pending = self.calls.entry(index).or_default();
            if let Some(id) = call["id"].as_str() {
                pending.id.push_str(id);
            }
            if let Some(name) = call["function"]["name"].as_str() {
                pending.name.push_str(name);
            }
            if let Some(args) = call["function"]["arguments"].as_str() {
                pending.arguments.push_str(args);
            }
        }
        Ok(())
    }

    fn update_usage(&mut self, usage: &Value) {
        if usage.is_object() {
            self.usage = json!({
                "input_tokens":usage["prompt_tokens"].as_u64().unwrap_or(0),
                "output_tokens":usage["completion_tokens"].as_u64().unwrap_or(0),
                "total_tokens":usage["total_tokens"].as_u64().unwrap_or(0),
                "input_tokens_details":{"cached_tokens":
                    usage["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0)},
                "output_tokens_details":{"reasoning_tokens":
                    usage["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0)}
            });
        }
    }

    fn chunk(&mut self, body: &Value) -> Result<Vec<SseEvent>, ChekovError> {
        if !body["error"].is_null() {
            return Err(bad_request(format!(
                "local server error: {}",
                body["error"]
            )));
        }
        let mut events = Vec::new();
        if self.sequence == 0 {
            events.push(self.event(
                "response.created",
                json!({"response":self.envelope("in_progress")}),
            ));
        }
        self.update_usage(&body["usage"]);
        let choice = &body["choices"][0];
        let delta = &choice["delta"];
        if let Some(text) = delta["reasoning_content"].as_str() {
            events.extend(self.text_delta(text, TextKind::Reasoning));
        }
        if let Some(text) = delta["content"].as_str() {
            events.extend(self.text_delta(text, TextKind::Message));
        }
        self.collect_calls(&delta["tool_calls"])?;
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.end = match reason {
                "stop" | "tool_calls" => End::Complete,
                "length" => End::Length,
                reason => End::Failed(format!("unsupported upstream finish reason '{reason}'")),
            };
        }
        Ok(events)
    }

    fn complete(&mut self) -> Result<(Value, Vec<SseEvent>), ChekovError> {
        if matches!(self.end, End::Length) && !self.calls.is_empty() {
            return Err(bad_request(
                "upstream hit its token limit during a tool call",
            ));
        }
        let status = match &self.end {
            End::Complete => "completed",
            End::Length => "incomplete",
            End::Pending => return Err(bad_request("upstream ended without a finish reason")),
            End::Failed(reason) => return Err(bad_request(reason.clone())),
            End::Emitted => return Err(bad_request("response already finished")),
        };
        let mut events = Vec::new();
        for (_, call) in std::mem::take(&mut self.calls) {
            let index = self.output.len();
            let item = call.item(&format!("item_{}_{index}", self.id))?;
            events.push(self.event(
                "response.output_item.added",
                json!({"output_index":index,"item":item}),
            ));
            self.output.push(item);
        }
        for index in 0..self.output.len() {
            if self.output[index]["type"] == "message" {
                self.output[index]["status"] = json!("completed");
            }
            events.push(self.event(
                "response.output_item.done",
                json!({
                    "output_index":index,"item":self.output[index]
                }),
            ));
        }
        let mut response = self.envelope(status);
        if status == "incomplete" {
            response["incomplete_details"] = json!({"reason":"max_output_tokens"});
        }
        self.end = End::Emitted;
        Ok((response, events))
    }

    fn fail(&mut self, reason: &str) -> Vec<SseEvent> {
        self.end = End::Emitted;
        let mut response = self.envelope("failed");
        response["error"] = json!({"code":"server_error","message":reason});
        vec![self.event("response.failed", json!({"response":response}))]
    }
}

impl PendingCall {
    fn item(&self, id: &str) -> Result<Value, ChekovError> {
        if self.id.is_empty() || self.name.is_empty() {
            return Err(bad_request(
                "upstream returned a tool call without its id or name",
            ));
        }
        let mut item = match self.name.strip_prefix(CUSTOM_PREFIX) {
            Some(name) => {
                let arguments = parse_json(&self.arguments)?;
                json!({"id":id,"type":"custom_tool_call","status":"completed",
                    "call_id":self.id,"name":name,"input":required_text(&arguments,"input")?})
            }
            None => json!({"id":id,"type":"function_call","status":"completed",
                "call_id":self.id,"name":self.name,"arguments":self.arguments}),
        };
        let (namespace, name) = split_name(required_text(&item, "name")?)?;
        let name = name.to_owned();
        if let Some(namespace) = namespace {
            item["namespace"] = json!(namespace);
        }
        item["name"] = json!(name);
        Ok(item)
    }
}

impl StreamTranslator for CodexStream {
    fn on_chunk(&mut self, data: &str) -> Vec<SseEvent> {
        if matches!(self.end, End::Emitted) || data == "[DONE]" {
            return Vec::new();
        }
        match parse_json(data).and_then(|body| self.chunk(&body)) {
            Ok(events) => events,
            Err(error) => self.fail(&error.to_string()),
        }
    }

    fn finish(&mut self) -> Vec<SseEvent> {
        if matches!(self.end, End::Emitted) {
            return Vec::new();
        }
        match self.complete() {
            Ok((response, mut events)) => {
                let event = match response["status"].as_str() {
                    Some("incomplete") => "response.incomplete",
                    _ => "response.completed",
                };
                events.push(self.event(event, json!({"response":response})));
                events
            }
            Err(error) => self.fail(&error.to_string()),
        }
    }

    fn on_upstream_error(&mut self, reason: &str) -> Vec<SseEvent> {
        if matches!(self.end, End::Emitted) {
            Vec::new()
        } else {
            self.fail(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(body: &Value) -> Value {
        to_chat_request(body, "local-model").expect("translate request")
    }

    fn payload(event: &SseEvent) -> Value {
        serde_json::from_str(&event.data).expect("SSE JSON")
    }

    fn delta(stream: &mut CodexStream, body: Value) -> Vec<SseEvent> {
        let choices = [body];
        stream.on_chunk(&json!({"choices":choices}).to_string())
    }

    #[test]
    fn overrides_parse_as_toml_and_declare_the_local_model_and_context() {
        let args = launch_args("model'\"\\\n", 131_072, 8787);
        let text = args
            .chunks_exact(2)
            .map(|pair| pair[1].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let config: toml::Table = toml::from_str(&text).expect("valid TOML overrides");
        assert_eq!(config["model"].as_str(), Some("model'\"\\\n"));
        assert_eq!(config["model_context_window"].as_integer(), Some(131_072));
        assert_eq!(
            config["model_auto_compact_token_limit"].as_integer(),
            Some(104_857)
        );
        assert_eq!(config["model_provider"].as_str(), Some("chekov"));
        let provider = &config["model_providers"]["chekov"];
        assert_eq!(
            provider["base_url"].as_str(),
            Some("http://127.0.0.1:8787/v1")
        );
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(false));
    }

    #[test]
    fn printed_command_quotes_shell_metacharacters() {
        let command = shell_command(&["a'b $(echo bad)".to_owned()]);
        assert_eq!(command, "codex 'a'\\''b $(echo bad)'");
    }

    #[test]
    fn responses_route_forces_the_local_model_and_requests_stream_usage() {
        let facade = CodexFacade::new("local-model");
        let req = HttpRequest {
            method: "POST".to_owned(),
            path: "/v1/responses?beta=1".to_owned(),
            body: json!({"input":"hello","model":"cloud-model","stream":true})
                .to_string()
                .into_bytes(),
        };
        let Action::Forward(forward) = facade.route(&req).expect("route") else {
            panic!("expected upstream request");
        };
        assert_eq!(forward.path, "/v1/chat/completions");
        assert!(forward.stream);
        let body: Value = serde_json::from_slice(&forward.body).expect("request JSON");
        assert_eq!(body["model"], "local-model");
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn instructions_and_developer_messages_reach_the_local_model() {
        let body = request(&json!({"instructions":"system rules","input":[
            {"role":"developer","content":[{"type":"input_text","text":"project rules"}]},
            {"role":"user","content":"hello"}
        ],"max_output_tokens":512}));
        assert_eq!(
            body["messages"][0],
            json!({"role":"system","content":"system rules"})
        );
        assert_eq!(body["messages"][1]["role"], "system");
        assert_eq!(body["messages"][1]["content"][0]["text"], "project rules");
        assert_eq!(body["max_tokens"], 512);
    }

    #[test]
    fn function_calls_and_results_preserve_ids_and_parallel_calls() {
        let body = request(&json!({"input":[
            {"type":"function_call","call_id":"a","name":"read","arguments":"{\"path\":\"a\"}"},
            {"type":"function_call","call_id":"b","name":"read","arguments":"{\"path\":\"b\"}"},
            {"type":"function_call_output","call_id":"a","output":"file a"},
            {"type":"function_call_output","call_id":"b","output":[{"type":"input_text","text":"file b"}]}
        ]}));
        assert_eq!(
            body["messages"][0]["tool_calls"]
                .as_array()
                .expect("calls")
                .len(),
            2
        );
        assert_eq!(body["messages"][0]["tool_calls"][1]["id"], "b");
        assert_eq!(body["messages"][1]["tool_call_id"], "a");
        assert_eq!(body["messages"][2]["content"][0]["text"], "file b");
    }

    #[test]
    fn custom_tools_use_a_string_input_schema_and_round_trip() {
        let body = request(&json!({"input":[
            {"type":"custom_tool_call","call_id":"patch1","name":"apply_patch","input":"patch text"},
            {"type":"custom_tool_call_output","call_id":"patch1","output":"applied"}
        ],"tools":[{"type":"custom","name":"apply_patch","description":"Apply edits"}],
        "tool_choice":{"type":"custom","name":"apply_patch"}}));
        assert_eq!(
            body["tools"][0]["function"]["name"],
            "chekov_custom_apply_patch"
        );
        assert_eq!(
            body["tool_choice"]["function"]["name"],
            "chekov_custom_apply_patch"
        );
        let call = &body["messages"][0]["tool_calls"][0];
        let args =
            parse_json(call["function"]["arguments"].as_str().expect("arguments")).expect("JSON");
        assert_eq!(args["input"], "patch text");
        assert_eq!(body["messages"][1]["tool_call_id"], "patch1");
    }

    #[test]
    fn namespaced_tools_keep_their_namespace_across_a_tool_turn() {
        let body = request(
            &json!({"input":[{"type":"function_call","namespace":"tools__v1",
            "name":"read","call_id":"read1","arguments":"{}"}],
            "tools":[{"type":"namespace","name":"tools__v1","tools":[
                {"type":"function","name":"read","parameters":{"type":"object"}}
            ]}]}),
        );
        let name = body["tools"][0]["function"]["name"]
            .as_str()
            .expect("flat name");
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["name"],
            name
        );
        let item = PendingCall {
            id: "read1".to_owned(),
            name: name.to_owned(),
            arguments: "{}".to_owned(),
        }
        .item("item1")
        .expect("restore namespace");
        assert_eq!(item["namespace"], "tools__v1");
        assert_eq!(item["name"], "read");
    }

    #[test]
    fn malformed_namespace_members_are_errors_not_panics() {
        for member in [Value::Null, json!("broken"), json!([]), json!(42)] {
            let body = json!({"input":"hello","tools":[
                {"type":"namespace","name":"tools","tools":[member]}
            ]});
            assert!(to_chat_request(&body, "local").is_err());
        }
    }

    #[test]
    fn malformed_upstream_tool_calls_fail_the_turn() {
        let mut stream = CodexStream::new("local");
        let events = delta(
            &mut stream,
            json!({
                "delta":{"tool_calls":"broken"},"finish_reason":"stop"
            }),
        );
        assert_eq!(events.last().expect("failure").event, "response.failed");
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn image_input_reaches_the_chat_api() {
        let body = request(&json!({"input":[{"role":"user","content":[
            {"type":"input_image","image_url":"data:image/png;base64,AAAA"}
        ]}]}));
        assert_eq!(
            body["messages"][0]["content"][0]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
    }

    #[test]
    fn unsupported_state_and_hosted_tools_are_refused() {
        for extra in [
            json!({"previous_response_id":"resp_old"}),
            json!({"store":true}),
            json!({"background":true}),
            json!({"conversation":"conv_old"}),
            json!({"tools":[{"type":"web_search","name":"web_search"}]}),
            json!({"input":[{"type":"item_reference","id":"old"}]}),
        ] {
            let mut body = json!({"input":"hello"});
            body.as_object_mut()
                .expect("object")
                .extend(extra.as_object().expect("extra").clone());
            assert!(to_chat_request(&body, "local").is_err(), "{body}");
        }
    }

    #[test]
    fn unknown_routes_and_malformed_json_are_refused() {
        let facade = CodexFacade::new("local");
        let mut req = HttpRequest {
            method: "POST".to_owned(),
            path: "/v1/responses/compact".to_owned(),
            body: b"{}".to_vec(),
        };
        let Action::Reply(reply) = facade.route(&req).expect("route") else {
            panic!("local reply");
        };
        assert_eq!(reply.status, 404);
        req.path = "/v1/responses".to_owned();
        req.body = b"{bad".to_vec();
        assert!(facade.route(&req).is_err());
    }

    #[test]
    fn streamed_text_and_reasoning_finish_with_usage_and_monotonic_events() {
        let mut stream = CodexStream::new("local");
        let mut events = delta(&mut stream, json!({"delta":{"reasoning_content":"think"}}));
        events.extend(delta(&mut stream, json!({"delta":{"content":"hello "}})));
        events.extend(delta(
            &mut stream,
            json!({"delta":{"content":"world"},"finish_reason":"stop"}),
        ));
        stream.on_chunk(
            &json!({"choices":[],"usage":{"prompt_tokens":10,
            "completion_tokens":4,"total_tokens":14}})
            .to_string(),
        );
        events.extend(stream.finish());
        let last = payload(events.last().expect("completed"));
        assert_eq!(last["type"], "response.completed");
        assert_eq!(last["response"]["output"][0]["summary"][0]["text"], "think");
        assert_eq!(
            last["response"]["output"][1]["content"][0]["text"],
            "hello world"
        );
        assert_eq!(
            last["response"]["output"][1]["content"][0]["annotations"],
            json!([])
        );
        assert_eq!(last["response"]["usage"]["total_tokens"], 14);
        for (i, event) in events.iter().enumerate() {
            assert_eq!(payload(event)["sequence_number"], json!(i));
        }
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn fragmented_tool_arguments_are_assembled_without_crossing_call_ids() {
        let mut stream = CodexStream::new("local");
        delta(
            &mut stream,
            json!({"delta":{"tool_calls":[
                {"index":0,"id":"a","function":{"name":"read","arguments":"{\"path\":"}},
                {"index":1,"id":"b","function":{"name":"read","arguments":"{\"path\":\"b\"}"}}
            ]}}),
        );
        delta(
            &mut stream,
            json!({"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"\"a\"}"}}
        ]},"finish_reason":"tool_calls"}),
        );
        let events = stream.finish();
        let response = &payload(events.last().expect("complete"))["response"];
        assert_eq!(response["output"][0]["arguments"], "{\"path\":\"a\"}");
        assert_eq!(response["output"][0]["call_id"], "a");
        assert_eq!(response["output"][1]["call_id"], "b");
    }

    #[test]
    fn whole_body_custom_tool_responses_restore_freeform_input() {
        let upstream = json!({"choices":[{"message":{"tool_calls":[{
            "id":"patch1","function":{"name":"chekov_custom_apply_patch",
                "arguments":"{\"input\":\"*** Begin Patch\\n*** End Patch\"}"}
        }]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":2}});
        let text = CodexFacade::new("local")
            .translate_response(&upstream.to_string())
            .expect("reply");
        let response = parse_json(&text).expect("JSON");
        assert_eq!(response["output"][0]["type"], "custom_tool_call");
        assert_eq!(response["output"][0]["name"], "apply_patch");
        assert_eq!(
            response["output"][0]["input"],
            "*** Begin Patch\n*** End Patch"
        );
    }

    #[test]
    fn truncated_streams_never_claim_success() {
        let mut stream = CodexStream::new("local");
        delta(&mut stream, json!({"delta":{"content":"partial"}}));
        let events = stream.finish();
        assert_eq!(events.last().expect("failure").event, "response.failed");
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn malformed_stream_frames_never_claim_success() {
        let mut stream = CodexStream::new("local");
        let events = stream.on_chunk("{malformed");
        assert_eq!(events.last().expect("failure").event, "response.failed");
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn upstream_disconnect_reports_a_terminal_failure() {
        let mut stream = CodexStream::new("local");
        let events = stream.on_upstream_error("connection lost");
        assert_eq!(
            payload(&events[0])["response"]["error"]["message"],
            "connection lost"
        );
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn token_limit_reports_an_incomplete_response() {
        let mut stream = CodexStream::new("local");
        delta(
            &mut stream,
            json!({"delta":{"content":"partial"},"finish_reason":"length"}),
        );
        let events = stream.finish();
        let last = payload(events.last().expect("incomplete"));
        assert_eq!(last["type"], "response.incomplete");
        assert_eq!(
            last["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    #[test]
    fn truncated_tool_calls_never_emit_an_executable_item() {
        let mut stream = CodexStream::new("local");
        delta(
            &mut stream,
            json!({"delta":{"tool_calls":[{
            "index":0,"id":"call1","function":{"name":"read","arguments":"{\"path\":"}
        }]},"finish_reason":"length"}),
        );
        let events = stream.finish();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "response.failed");
    }
}
