//! `OpenAI` `chat.completion.chunk` SSE into Anthropic's event sequence.
//!
//! Anthropic's stream is a bracketed grammar, not a flat delta feed:
//!
//! ```text
//! message_start
//!   (content_block_start content_block_delta* content_block_stop)*
//! message_delta message_stop
//! ```
//!
//! The upstream sends unbracketed deltas, so the open block must be tracked and
//! closed whenever the delta kind changes. Claude Code parses this strictly —
//! a missing `content_block_stop` hangs the client.

use serde_json::{Value, json};

use super::{SseEvent, StreamTranslator, message_id, to_stop_reason, to_usage};

/// Reasoning models served by llama.cpp (e.g. Ornith-1.5) emit their thinking
/// block *inside* `content` rather than in `reasoning_content`, and it sits
/// ahead of the real answer. Anthropic's grammar has no room for thinking in a
/// text block, so strip the leading thinking span here before translating.
const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Which content block is currently open.
#[derive(PartialEq, Eq)]
enum Open {
    None,
    Text,
    Thinking,
    /// Tool call at the given upstream `tool_calls[].index`.
    Tool(u64),
}

pub struct ClaudeStream {
    model: String,
    started: bool,
    open: Open,
    /// Next Anthropic content-block index to allocate.
    next_index: usize,
    /// Index of the block currently open.
    index: usize,
    /// Thinking text buffered while a thinking span is still open, held until
    /// the closing tag lands so it never leaks into a text block.
    think_pending: String,
    stop_reason: Value,
    usage: Value,
    finished: bool,
}

impl ClaudeStream {
    #[must_use]
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            started: false,
            open: Open::None,
            next_index: 0,
            index: 0,
            think_pending: String::new(),
            stop_reason: Value::Null,
            usage: json!({ "input_tokens": 0, "output_tokens": 0 }),
            finished: false,
        }
    }

    /// `message_start`, emitted lazily so the upstream id can be used.
    fn start(&mut self, upstream_id: Option<&str>) -> Vec<SseEvent> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let message = json!({
            "id": message_id(upstream_id),
            "type": "message",
            "role": "assistant",
            "model": self.model,
            "content": [],
            "stop_reason": Value::Null,
            "stop_sequence": Value::Null,
            "usage": { "input_tokens": 0, "output_tokens": 0 },
        });
        vec![SseEvent::new(
            "message_start",
            json!({ "type": "message_start", "message": message }).to_string(),
        )]
    }

    /// Close the open block, if any.
    fn close_block(&mut self) -> Option<SseEvent> {
        if self.open == Open::None {
            return None;
        }
        self.open = Open::None;
        Some(SseEvent::new(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": self.index }).to_string(),
        ))
    }

    /// Ensure `want` is the open block, closing a different one first.
    fn ensure_open(&mut self, want: &Open, block: &Value) -> Vec<SseEvent> {
        if self.open == *want {
            return Vec::new();
        }
        let mut events: Vec<SseEvent> = self.close_block().into_iter().collect();
        self.index = self.next_index;
        self.next_index += 1;
        self.open = match want {
            Open::Tool(i) => Open::Tool(*i),
            Open::Text => Open::Text,
            Open::Thinking => Open::Thinking,
            Open::None => Open::None,
        };
        events.push(SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": self.index,
                "content_block": block,
            })
            .to_string(),
        ));
        events
    }

    /// One delta inside the currently open block. `kind` names the Anthropic
    /// delta type and `field` the payload key it pairs with — they always vary
    /// together, so they travel as one pair.
    fn delta(&self, kind: (&str, &str), text: &str) -> SseEvent {
        let (kind, field) = kind;
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": self.index,
                "delta": { "type": kind, field: text },
            })
            .to_string(),
        )
    }

    /// Text delta, opening a text block when one is not already open.
    ///
    /// Reasoning models served by llama.cpp (Ornith-1.5) emit their thinking
    /// block inside `content` ahead of the answer, so buffer here and drop the
    /// leading thinking span before translating. Streaming-safe: text that
    /// arrives before the closing tag (all thinking) is held until the tag
    /// lands.
    fn on_text(&mut self, text: &str) -> Vec<SseEvent> {
        let mut buf = std::mem::take(&mut self.think_pending);
        buf.push_str(text);

        if let Some(open) = buf.find(THINK_OPEN) {
            if let Some(close) = buf[open..].find(THINK_CLOSE) {
                let close_abs = open + close + THINK_CLOSE.len();
                self.emit_after_thinking(&buf[..open], &buf[close_abs..])
            } else {
                // Still thinking — hold the whole buffer until it closes.
                self.think_pending = buf;
                Vec::new()
            }
        } else {
            self.emit_text(&buf)
        }
    }

    /// Emit any real text before and after a closed thinking span.
    fn emit_after_thinking(&mut self, pre: &str, rest: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !pre.is_empty() {
            events.extend(self.emit_text(pre));
        }
        if !rest.is_empty() {
            events.extend(self.emit_text(rest));
        }
        events
    }

    /// Emit a text delta, opening a text block when one is not already open.
    fn emit_text(&mut self, text: &str) -> Vec<SseEvent> {
        let mut events = self.ensure_open(&Open::Text, &json!({ "type": "text", "text": "" }));
        events.push(self.delta(("text_delta", "text"), text));
        events
    }

    /// Reasoning delta — llama-server emits `reasoning_content` when the model
    /// has a thinking channel; Anthropic's equivalent is a `thinking` block.
    fn on_thinking(&mut self, text: &str) -> Vec<SseEvent> {
        let block = json!({ "type": "thinking", "thinking": "", "signature": "" });
        let mut events = self.ensure_open(&Open::Thinking, &block);
        events.push(self.delta(("thinking_delta", "thinking"), text));
        events
    }

    /// Tool-call deltas. Each upstream `index` is its own Anthropic block; the
    /// name and id arrive on the first fragment, arguments stream after.
    fn on_tool_calls(&mut self, calls: &[Value]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        for call in calls {
            let slot = call.get("index").and_then(Value::as_u64).unwrap_or(0);
            let func = call.get("function");
            let block = json!({
                "type": "tool_use",
                "id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                "name": func.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or_default(),
                "input": {},
            });
            events.extend(self.ensure_open(&Open::Tool(slot), &block));
            if let Some(args) = func
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .filter(|a| !a.is_empty())
            {
                events.push(self.delta(("input_json_delta", "partial_json"), args));
            }
        }
        events
    }

    /// Record terminal metadata carried on a chunk.
    fn absorb_terminal(&mut self, chunk: &Value) {
        if let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            && let Some(finish) = choice.get("finish_reason").and_then(Value::as_str)
        {
            self.stop_reason = to_stop_reason(Some(finish));
        }
        if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = to_usage(Some(usage));
        }
    }
}

impl StreamTranslator for ClaudeStream {
    fn on_chunk(&mut self, data: &str) -> Vec<SseEvent> {
        if data.trim() == "[DONE]" {
            return Vec::new();
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return Vec::new();
        };
        let mut events = self.start(chunk.get("id").and_then(Value::as_str));
        self.absorb_terminal(&chunk);
        let delta = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("delta"));
        let Some(delta) = delta else {
            return events;
        };
        if let Some(text) = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            events.extend(self.on_thinking(text));
        }
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            events.extend(self.on_text(text));
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            events.extend(self.on_tool_calls(calls));
        }
        events
    }

    fn finish(&mut self) -> Vec<SseEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        // A stream that produced nothing still owes the client a well-formed
        // message envelope, or the SDK reports a protocol error rather than an
        // empty reply.
        let mut events = self.start(None);
        events.extend(self.close_block());
        if self.stop_reason.is_null() {
            self.stop_reason = json!("end_turn");
        }
        events.push(SseEvent::new(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": self.stop_reason, "stop_sequence": Value::Null },
                "usage": self.usage,
            })
            .to_string(),
        ));
        events.push(SseEvent::new(
            "message_stop",
            json!({ "type": "message_stop" }).to_string(),
        ));
        events
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{ClaudeStream, StreamTranslator};
    use crate::core::proxy::SseEvent;

    /// Drive a full exchange and return `(event names, parsed payloads)`.
    fn run(chunks: &[Value]) -> (Vec<String>, Vec<Value>) {
        let mut stream = ClaudeStream::new("local");
        let mut events: Vec<_> = chunks
            .iter()
            .flat_map(|c| stream.on_chunk(&c.to_string()))
            .collect();
        events.extend(stream.finish());
        let names = events.iter().map(|e| e.event.clone()).collect();
        let payloads = events
            .iter()
            .map(|e| serde_json::from_str(&e.data).expect("event data is json"))
            .collect();
        (names, payloads)
    }

    fn text_chunk(text: &str) -> Value {
        json!({ "id": "c1", "choices": [{ "delta": { "content": text } }] })
    }

    #[test]
    fn text_stream_emits_the_full_anthropic_envelope_in_order() {
        let (names, _) = run(&[
            text_chunk("he"),
            text_chunk("llo"),
            json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] }),
        ]);
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
    }

    #[test]
    fn message_start_is_emitted_exactly_once() {
        let (names, _) = run(&[text_chunk("a"), text_chunk("b"), text_chunk("c")]);
        assert_eq!(names.iter().filter(|n| *n == "message_start").count(), 1);
    }

    #[test]
    fn every_opened_block_is_closed() {
        let (names, _) = run(&[
            json!({ "id": "c1", "choices": [{ "delta": { "reasoning_content": "hmm" } }] }),
            text_chunk("done"),
        ]);
        let opens = names.iter().filter(|n| *n == "content_block_start").count();
        let stops = names.iter().filter(|n| *n == "content_block_stop").count();
        assert_eq!(opens, 2, "thinking then text: {names:?}");
        assert_eq!(stops, opens, "unbalanced blocks: {names:?}");
    }

    #[test]
    fn switching_delta_kind_advances_the_block_index() {
        let (_, payloads) = run(&[
            json!({ "id": "c1", "choices": [{ "delta": { "reasoning_content": "hmm" } }] }),
            text_chunk("answer"),
        ]);
        let starts: Vec<u64> = payloads
            .iter()
            .filter(|p| p["type"] == "content_block_start")
            .filter_map(|p| p["index"].as_u64())
            .collect();
        assert_eq!(starts, vec![0, 1], "indices must not collide");
    }

    #[test]
    fn reasoning_content_becomes_a_thinking_block() {
        let (_, payloads) =
            run(&[json!({ "id": "c1", "choices": [{ "delta": { "reasoning_content": "why" } }] })]);
        let start = payloads
            .iter()
            .find(|p| p["type"] == "content_block_start")
            .expect("a block opened");
        assert_eq!(start["content_block"]["type"], "thinking");
        let delta = payloads
            .iter()
            .find(|p| p["type"] == "content_block_delta")
            .expect("a delta");
        assert_eq!(delta["delta"]["type"], "thinking_delta");
        assert_eq!(delta["delta"]["thinking"], "why");
    }

    #[test]
    fn tool_call_arguments_stream_as_input_json_delta() {
        let (_, payloads) = run(&[
            json!({ "id": "c1", "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "id": "call_1",
                "function": { "name": "read", "arguments": "" },
            }] } }] }),
            json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "function": { "arguments": "{\"p\":" },
            }] } }] }),
            json!({ "choices": [{ "delta": { "tool_calls": [{
                "index": 0, "function": { "arguments": "1}" },
            }] } }] }),
        ]);
        let start = payloads
            .iter()
            .find(|p| p["type"] == "content_block_start")
            .expect("tool block");
        assert_eq!(start["content_block"]["type"], "tool_use");
        assert_eq!(start["content_block"]["id"], "call_1");
        assert_eq!(start["content_block"]["name"], "read");
        let fragments: Vec<&str> = payloads
            .iter()
            .filter(|p| p["delta"]["type"] == "input_json_delta")
            .filter_map(|p| p["delta"]["partial_json"].as_str())
            .collect();
        assert_eq!(fragments.concat(), r#"{"p":1}"#);
    }

    #[test]
    fn two_parallel_tool_calls_get_distinct_blocks() {
        let (_, payloads) = run(
            &[json!({ "id": "c1", "choices": [{ "delta": { "tool_calls": [
                { "index": 0, "id": "a", "function": { "name": "one", "arguments": "" } },
                { "index": 1, "id": "b", "function": { "name": "two", "arguments": "" } },
            ] } }] })],
        );
        let ids: Vec<&str> = payloads
            .iter()
            .filter(|p| p["type"] == "content_block_start")
            .filter_map(|p| p["content_block"]["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn usage_and_stop_reason_land_on_message_delta() {
        let (_, payloads) = run(&[
            text_chunk("x"),
            json!({
                "choices": [{ "delta": {}, "finish_reason": "length" }],
                "usage": { "prompt_tokens": 7, "completion_tokens": 2 },
            }),
        ]);
        let delta = payloads
            .iter()
            .find(|p| p["type"] == "message_delta")
            .expect("message_delta");
        assert_eq!(delta["delta"]["stop_reason"], "max_tokens");
        assert_eq!(delta["usage"]["input_tokens"], 7);
        assert_eq!(delta["usage"]["output_tokens"], 2);
    }

    #[test]
    fn empty_stream_still_produces_a_valid_envelope() {
        let (names, _) = run(&[]);
        assert_eq!(
            names,
            vec!["message_start", "message_delta", "message_stop"]
        );
    }

    #[test]
    fn done_sentinel_and_garbage_lines_are_ignored() {
        let mut stream = ClaudeStream::new("local");
        assert!(stream.on_chunk("[DONE]").is_empty());
        assert!(stream.on_chunk("not json at all").is_empty());
    }

    /// Drive a full exchange and return the text blocks emitted.
    fn run_text(chunks: &[Value]) -> String {
        let mut stream = ClaudeStream::new("local");
        let mut text = String::new();
        for c in chunks {
            for e in stream.on_chunk(&c.to_string()) {
                text.push_str(&text_from(&e));
            }
        }
        for e in stream.finish() {
            text.push_str(&text_from(&e));
        }
        text
    }

    /// Extract a text delta's payload from a translated SSE event, or "".
    fn text_from(e: &SseEvent) -> String {
        if e.event != "content_block_delta" {
            return String::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(&e.data) else {
            return String::new();
        };
        v.get("delta")
            .and_then(|d| d.get("text"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default()
    }

    #[test]
    fn thinking_block_in_content_is_stripped_from_text() {
        let text = run_text(&[json!({
            "choices": [{ "delta": { "content": "<think>\nanswer\n</think>\n```python\nx=1\n```" } }]
        })]);
        assert_eq!(text, "\n```python\nx=1\n```");
    }

    #[test]
    fn thinking_block_split_across_chunks_is_held_then_stripped() {
        // Thinking arrives first, then the close tag + answer in a later chunk.
        let text = run_text(&[
            json!({ "choices": [{ "delta": { "content": "<think>\nlet me think" } }] }),
            json!({ "choices": [{ "delta": { "content": "</think>\n```python\nx=1\n```" } }] }),
        ]);
        assert_eq!(text, "\n```python\nx=1\n```");
    }

    #[test]
    fn content_before_thinking_is_kept() {
        let text = run_text(&[json!({
            "choices": [{ "delta": { "content": "hello <think>\nreasoning\n</think>\nworld" } }]
        })]);
        assert_eq!(text, "hello \nworld");
    }

    #[test]
    fn plain_text_without_thinking_is_untouched() {
        let text = run_text(&[text_chunk("he"), text_chunk("llo")]);
        assert_eq!(text, "hello");
    }
}
