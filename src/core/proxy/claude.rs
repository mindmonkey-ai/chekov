//! Anthropic `/v1/messages` ⇄ `OpenAI` `/v1/chat/completions`.
//!
//! Claude Code speaks Anthropic exclusively. Translation is pure JSON-to-JSON
//! so every rule below is unit-testable without a socket or a model.

use serde_json::{Map, Value, json};

use super::http::{HttpRequest, HttpResponse};
use super::{Action, AgentFacade, Forward, SseEvent, StreamTranslator};
use crate::error::ChekovError;

mod stream;

pub use stream::ClaudeStream;

/// Anthropic requires `max_tokens`; llama-server does not. Absent means
/// unbounded upstream, which lets a runaway generation fill the context.
const DEFAULT_MAX_TOKENS: u64 = 4096;

pub struct ClaudeFacade {
    /// Upstream model id substituted for whatever Claude Code asks for — the
    /// agent's picker is a fixed list of Anthropic names, none of which the
    /// local server knows.
    model: String,
}

impl ClaudeFacade {
    #[must_use]
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
        }
    }

    /// `/v1/models` in Anthropic's shape, listing only the local model.
    fn model_list(&self) -> HttpResponse {
        let body = json!({
            "data": [{
                "type": "model",
                "id": self.model,
                "display_name": self.model,
                "created_at": "2020-01-01T00:00:00Z",
            }],
            "has_more": false,
        });
        HttpResponse::json(200, body.to_string())
    }
}

impl AgentFacade for ClaudeFacade {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn route(&self, req: &HttpRequest) -> Result<Action, ChekovError> {
        let path = req.path.split('?').next().unwrap_or(&req.path);
        match (req.method.as_str(), path) {
            ("POST", "/v1/messages") => {
                let body: Value = parse_json(req.body_str()?)?;
                let stream = body.get("stream").and_then(Value::as_bool) == Some(true);
                let translated = to_openai_request(&body, &self.model)?;
                Ok(Action::Forward(Forward {
                    path: "/v1/chat/completions".to_owned(),
                    body: translated.to_string().into_bytes(),
                    stream,
                }))
            }
            ("POST", "/v1/messages/count_tokens") => {
                let body: Value = parse_json(req.body_str()?)?;
                Ok(Action::Reply(count_tokens_reply(&body)))
            }
            ("GET", "/v1/models") => Ok(Action::Reply(self.model_list())),
            _ => Ok(Action::Reply(HttpResponse::error(
                404,
                "not_found_error",
                &format!("{} {path} is not proxied by chekov", req.method),
            ))),
        }
    }

    fn translate_response(&self, upstream: &str) -> Result<String, ChekovError> {
        let parsed: Value = parse_json(upstream)?;
        Ok(to_anthropic_response(&parsed, &self.model).to_string())
    }

    fn stream_translator(&self) -> Box<dyn StreamTranslator> {
        Box::new(ClaudeStream::new(&self.model))
    }
}

fn parse_json(raw: &str) -> Result<Value, ChekovError> {
    serde_json::from_str(raw).map_err(|e| ChekovError::ProxyBadRequest {
        reason: format!("body is not valid JSON: {e}"),
    })
}

/// Approximate token count. Anthropic clients use this only to size their
/// context budget, so a 4-chars-per-token estimate is honest enough — the
/// alternative is shipping a tokenizer chekov has no other use for.
fn count_tokens_reply(body: &Value) -> HttpResponse {
    let mut chars = 0_usize;
    collect_text(body, &mut chars);
    let tokens = chars.div_ceil(4);
    HttpResponse::json(200, json!({ "input_tokens": tokens }).to_string())
}

/// Sum the length of every prompt-bearing string. Only the keys that carry
/// prompt text are descended into — counting the whole document would inflate
/// the estimate with ids, roles, and tool schemas.
fn collect_text(value: &Value, chars: &mut usize) {
    match value {
        Value::String(s) => *chars += s.chars().count(),
        Value::Array(items) => items.iter().for_each(|v| collect_text(v, chars)),
        Value::Object(map) => {
            for (key, val) in map {
                if matches!(key.as_str(), "messages" | "content" | "system" | "text") {
                    collect_text(val, chars);
                }
            }
        }
        _ => {}
    }
}

/// Anthropic request body into an `OpenAI` chat-completions body.
pub fn to_openai_request(req: &Value, model: &str) -> Result<Value, ChekovError> {
    let mut out = Map::new();
    out.insert("model".to_owned(), json!(model));
    out.insert("messages".to_owned(), Value::Array(to_openai_messages(req)));
    let max_tokens = req
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    out.insert("max_tokens".to_owned(), json!(max_tokens));
    copy_sampling(req, &mut out);
    copy_tools(req, &mut out);
    if req.get("stream").and_then(Value::as_bool) == Some(true) {
        out.insert("stream".to_owned(), json!(true));
        out.insert(
            "stream_options".to_owned(),
            json!({ "include_usage": true }),
        );
    }
    Ok(Value::Object(out))
}

/// The system prompt, then every message, in `OpenAI` shape.
fn to_openai_messages(req: &Value) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(system) = req.get("system") {
        let text = flatten_text(system);
        if !text.is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }
    for msg in req
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        push_message(msg, &mut messages);
    }
    messages
}

/// Tool definitions and the choice policy, when present.
fn copy_tools(req: &Value, out: &mut Map<String, Value>) {
    if let Some(tools) = req.get("tools").and_then(Value::as_array) {
        let mapped: Vec<Value> = tools.iter().map(to_openai_tool).collect();
        if !mapped.is_empty() {
            out.insert("tools".to_owned(), Value::Array(mapped));
        }
    }
    if let Some(choice) = req.get("tool_choice").map(to_openai_tool_choice) {
        out.insert("tool_choice".to_owned(), choice);
    }
}

/// Sampling knobs that carry over unchanged, plus the one that is renamed.
fn copy_sampling(req: &Value, out: &mut Map<String, Value>) {
    for key in ["temperature", "top_p", "top_k"] {
        if let Some(v) = req.get(key) {
            out.insert(key.to_owned(), v.clone());
        }
    }
    // Anthropic's stop_sequences is OpenAI's stop.
    if let Some(stop) = req.get("stop_sequences") {
        out.insert("stop".to_owned(), stop.clone());
    }
}

/// One Anthropic message becomes one or more `OpenAI` messages: `tool_result`
/// blocks cannot ride inside a user message, they must become separate
/// `role: "tool"` messages.
fn push_message(msg: &Value, out: &mut Vec<Value>) {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
    let content = msg.get("content").unwrap_or(&Value::Null);
    let Some(blocks) = content.as_array() else {
        out.push(json!({ "role": role, "content": flatten_text(content) }));
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            out.push(tool_result_message(block));
        }
    }
    let (parts, calls) = split_blocks(blocks);
    if parts.is_empty() && calls.is_empty() {
        return;
    }
    let mut mapped = Map::new();
    mapped.insert("role".to_owned(), json!(role));
    mapped.insert("content".to_owned(), content_value(parts));
    if !calls.is_empty() {
        mapped.insert("tool_calls".to_owned(), Value::Array(calls));
    }
    out.push(Value::Object(mapped));
}

/// Partition content blocks into `OpenAI` content parts and tool calls.
fn split_blocks(blocks: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let mut parts = Vec::new();
    let mut calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(json!({ "type": "text", "text": text }));
                }
            }
            Some("image") => {
                if let Some(part) = to_image_part(block) {
                    parts.push(part);
                }
            }
            Some("tool_use") => calls.push(to_openai_tool_call(block)),
            _ => {}
        }
    }
    (parts, calls)
}

/// Collapse a single text part back to a plain string — llama.cpp chat
/// templates handle strings uniformly, arrays only when multimodal.
fn content_value(parts: Vec<Value>) -> Value {
    let single_text =
        parts.len() == 1 && parts[0].get("type").and_then(Value::as_str) == Some("text");
    if single_text {
        return parts[0].get("text").cloned().unwrap_or_else(|| json!(""));
    }
    if parts.is_empty() {
        return json!("");
    }
    Value::Array(parts)
}

/// Anthropic base64 image block into an `OpenAI` data-URL image part.
fn to_image_part(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media = source.get("media_type").and_then(Value::as_str)?;
            let data = source.get("data").and_then(Value::as_str)?;
            format!("data:{media};base64,{data}")
        }
        Some("url") => source.get("url").and_then(Value::as_str)?.to_owned(),
        _ => return None,
    };
    Some(json!({ "type": "image_url", "image_url": { "url": url } }))
}

/// Anthropic `tool_use` block into an `OpenAI` assistant tool call.
fn to_openai_tool_call(block: &Value) -> Value {
    let args = block.get("input").map_or_else(
        || "{}".to_owned(),
        |v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_owned()),
    );
    json!({
        "id": block.get("id").and_then(Value::as_str).unwrap_or_default(),
        "type": "function",
        "function": {
            "name": block.get("name").and_then(Value::as_str).unwrap_or_default(),
            "arguments": args,
        },
    })
}

/// Anthropic `tool_result` block into a standalone `OpenAI` tool message.
fn tool_result_message(block: &Value) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or_default(),
        "content": flatten_text(block.get("content").unwrap_or(&Value::Null)),
    })
}

/// Anthropic tool definition into an `OpenAI` function definition.
fn to_openai_tool(tool: &Value) -> Value {
    let mut func = Map::new();
    func.insert(
        "name".to_owned(),
        tool.get("name").cloned().unwrap_or_else(|| json!("")),
    );
    if let Some(desc) = tool.get("description") {
        func.insert("description".to_owned(), desc.clone());
    }
    func.insert(
        "parameters".to_owned(),
        tool.get("input_schema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" })),
    );
    json!({ "type": "function", "function": Value::Object(func) })
}

/// Anthropic `tool_choice` into its `OpenAI` equivalent.
fn to_openai_tool_choice(choice: &Value) -> Value {
    match choice.get("type").and_then(Value::as_str) {
        Some("any") => json!("required"),
        Some("none") => json!("none"),
        Some("tool") => json!({
            "type": "function",
            "function": { "name": choice.get("name").cloned().unwrap_or_else(|| json!("")) },
        }),
        _ => json!("auto"),
    }
}

/// Any Anthropic content shape (string, block, block array) as plain text.
fn flatten_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(flatten_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

/// `OpenAI` `finish_reason` into an Anthropic `stop_reason`.
pub(crate) fn to_stop_reason(finish: Option<&str>) -> Value {
    match finish {
        Some("length") => json!("max_tokens"),
        Some("tool_calls" | "function_call") => json!("tool_use"),
        Some("stop") => json!("end_turn"),
        Some(other) => json!(other),
        None => Value::Null,
    }
}

/// Anthropic message id derived from the upstream completion id.
pub(crate) fn message_id(upstream: Option<&str>) -> String {
    upstream.map_or_else(|| "msg_chekov".to_owned(), |id| format!("msg_{id}"))
}

/// Usage block, defaulting to zeros when llama-server omits it.
pub(crate) fn to_usage(usage: Option<&Value>) -> Value {
    let field = |key: &str| {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    json!({
        "input_tokens": field("prompt_tokens"),
        "output_tokens": field("completion_tokens"),
    })
}

/// A whole `OpenAI` chat completion into an Anthropic message.
#[must_use]
pub fn to_anthropic_response(res: &Value, model: &str) -> Value {
    let choice = res
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first());
    let message = choice.and_then(|c| c.get("message"));
    let mut content = Vec::new();
    if let Some(text) = message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        content.push(json!({ "type": "text", "text": text }));
    }
    for call in message
        .and_then(|m| m.get("tool_calls"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        content.push(to_anthropic_tool_use(call));
    }
    let finish = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str);
    json!({
        "id": message_id(res.get("id").and_then(Value::as_str)),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": to_stop_reason(finish),
        "stop_sequence": Value::Null,
        "usage": to_usage(res.get("usage")),
    })
}

/// `OpenAI` tool call into an Anthropic `tool_use` block. Arguments arrive as a
/// JSON *string*; Anthropic expects a parsed object.
fn to_anthropic_tool_use(call: &Value) -> Value {
    let func = call.get("function");
    let raw = func
        .and_then(|f| f.get("arguments"))
        .and_then(Value::as_str)
        .unwrap_or("{}");
    json!({
        "type": "tool_use",
        "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
        "name": func.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or_default(),
        "input": serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({})),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::http::HttpRequest;
    use super::super::{Action, AgentFacade};
    use super::{ClaudeFacade, to_anthropic_response, to_openai_request};

    fn translate(req: &Value) -> Value {
        to_openai_request(req, "local").expect("translate")
    }

    fn post(path: &str, body: &str) -> HttpRequest {
        HttpRequest {
            method: "POST".to_owned(),
            path: path.to_owned(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn system_string_becomes_a_leading_system_message() {
        let out = translate(&json!({
            "system": "be terse",
            "messages": [{ "role": "user", "content": "hi" }],
        }));
        let msgs = out["messages"].as_array().expect("messages");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "be terse");
        assert_eq!(msgs[1]["content"], "hi");
    }

    #[test]
    fn system_block_array_is_flattened_not_dropped() {
        let out = translate(&json!({
            "system": [
                { "type": "text", "text": "rule one" },
                { "type": "text", "text": "rule two" },
            ],
            "messages": [],
        }));
        assert_eq!(out["messages"][0]["content"], "rule one\nrule two");
    }

    #[test]
    fn model_is_overridden_with_the_local_alias() {
        let out = translate(&json!({ "model": "claude-opus-4-6", "messages": [] }));
        assert_eq!(out["model"], "local");
    }

    #[test]
    fn absent_max_tokens_gets_a_stated_default() {
        let out = translate(&json!({ "messages": [] }));
        assert_eq!(out["max_tokens"], 4096);
    }

    #[test]
    fn stop_sequences_is_renamed_to_stop() {
        let out = translate(&json!({ "messages": [], "stop_sequences": ["END"] }));
        assert_eq!(out["stop"][0], "END");
        assert!(out.get("stop_sequences").is_none(), "{out}");
    }

    #[test]
    fn single_text_block_collapses_to_a_plain_string() {
        let out = translate(&json!({
            "messages": [{
                "role": "user",
                "content": [{ "type": "text", "text": "hello" }],
            }],
        }));
        assert_eq!(out["messages"][0]["content"], "hello");
    }

    #[test]
    fn tool_result_becomes_a_separate_tool_role_message() {
        let out = translate(&json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": [{ "type": "text", "text": "42" }],
                }],
            }],
        }));
        let msgs = out["messages"].as_array().expect("messages");
        assert_eq!(msgs.len(), 1, "tool_result must not stay inline: {out}");
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "toolu_1");
        assert_eq!(msgs[0]["content"], "42");
    }

    #[test]
    fn tool_use_becomes_a_tool_call_with_stringified_arguments() {
        let out = translate(&json!({
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_9",
                    "name": "read",
                    "input": { "path": "/x" },
                }],
            }],
        }));
        let call = &out["messages"][0]["tool_calls"][0];
        assert_eq!(call["id"], "toolu_9");
        assert_eq!(call["function"]["name"], "read");
        assert_eq!(call["function"]["arguments"], r#"{"path":"/x"}"#);
    }

    #[test]
    fn image_block_becomes_a_data_url_part() {
        let out = translate(&json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image",
                    "source": { "type": "base64", "media_type": "image/png", "data": "QUJD" },
                }],
            }],
        }));
        let part = &out["messages"][0]["content"][0];
        assert_eq!(part["type"], "image_url");
        assert_eq!(part["image_url"]["url"], "data:image/png;base64,QUJD");
    }

    #[test]
    fn input_schema_is_renamed_to_parameters() {
        let out = translate(&json!({
            "messages": [],
            "tools": [{
                "name": "grep",
                "description": "search",
                "input_schema": { "type": "object", "properties": {} },
            }],
        }));
        let tool = &out["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "grep");
        assert_eq!(tool["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn tool_choice_any_maps_to_required() {
        let out = translate(&json!({ "messages": [], "tool_choice": { "type": "any" } }));
        assert_eq!(out["tool_choice"], "required");
        let out = translate(&json!({
            "messages": [],
            "tool_choice": { "type": "tool", "name": "grep" },
        }));
        assert_eq!(out["tool_choice"]["function"]["name"], "grep");
    }

    #[test]
    fn streaming_request_asks_upstream_for_usage() {
        let out = translate(&json!({ "messages": [], "stream": true }));
        assert_eq!(out["stream"], true);
        assert_eq!(out["stream_options"]["include_usage"], true);
    }

    #[test]
    fn response_carries_text_stop_reason_and_usage() {
        let upstream = json!({
            "id": "chatcmpl-7",
            "choices": [{
                "message": { "role": "assistant", "content": "hello" },
                "finish_reason": "stop",
            }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 3 },
        });
        let out = to_anthropic_response(&upstream, "local");
        assert_eq!(out["id"], "msg_chatcmpl-7");
        assert_eq!(out["type"], "message");
        assert_eq!(out["content"][0]["text"], "hello");
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["usage"]["input_tokens"], 11);
        assert_eq!(out["usage"]["output_tokens"], 3);
    }

    #[test]
    fn tool_call_response_parses_arguments_back_into_an_object() {
        let upstream = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_1",
                        "function": { "name": "read", "arguments": r#"{"path":"/x"}"# },
                    }],
                },
                "finish_reason": "tool_calls",
            }],
        });
        let out = to_anthropic_response(&upstream, "local");
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["content"][0]["type"], "tool_use");
        assert_eq!(out["content"][0]["input"]["path"], "/x");
    }

    #[test]
    fn length_finish_maps_to_max_tokens() {
        let upstream = json!({ "choices": [{ "message": {}, "finish_reason": "length" }] });
        assert_eq!(
            to_anthropic_response(&upstream, "local")["stop_reason"],
            "max_tokens"
        );
    }

    #[test]
    fn messages_route_forwards_to_chat_completions() {
        let facade = ClaudeFacade::new("local");
        let req = post("/v1/messages", r#"{"messages":[],"stream":true}"#);
        match facade.route(&req).expect("route") {
            Action::Forward(f) => {
                assert_eq!(f.path, "/v1/chat/completions");
                assert!(f.stream);
            }
            Action::Reply(_) => panic!("must forward"),
        }
    }

    #[test]
    fn unknown_route_replies_404_rather_than_forwarding() {
        let facade = ClaudeFacade::new("local");
        let req = post("/v1/complete", "{}");
        match facade.route(&req).expect("route") {
            Action::Reply(res) => assert_eq!(res.status, 404),
            Action::Forward(_) => panic!("must not forward"),
        }
    }

    #[test]
    fn model_list_advertises_only_the_local_model() {
        let facade = ClaudeFacade::new("minimax-m2.7");
        let req = HttpRequest {
            method: "GET".to_owned(),
            path: "/v1/models".to_owned(),
            body: Vec::new(),
        };
        match facade.route(&req).expect("route") {
            Action::Reply(res) => {
                assert_eq!(res.status, 200);
                assert!(res.body.contains("minimax-m2.7"), "{}", res.body);
            }
            Action::Forward(_) => panic!("must reply locally"),
        }
    }

    #[test]
    fn count_tokens_is_answered_without_an_upstream_call() {
        let facade = ClaudeFacade::new("local");
        let req = post(
            "/v1/messages/count_tokens",
            r#"{"messages":[{"role":"user","content":"12345678"}]}"#,
        );
        match facade.route(&req).expect("route") {
            Action::Reply(res) => {
                let body: Value = serde_json::from_str(&res.body).expect("json");
                assert_eq!(body["input_tokens"], 2);
            }
            Action::Forward(_) => panic!("must reply locally"),
        }
    }

    #[test]
    fn malformed_json_is_a_typed_error_not_a_panic() {
        let facade = ClaudeFacade::new("local");
        assert!(facade.route(&post("/v1/messages", "{not json")).is_err());
    }
}
