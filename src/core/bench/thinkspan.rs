//! Thinking versus answer, counted in characters.
//!
//! Read where the thinking still exists — the `OpenAI` body before
//! translation — because a `--reasoning-format none` run leaves its thoughts
//! inline in `content` as the template family's own tags, and the proxy
//! strips them on the way to the agent (reasoning-stamp design §4, §11).

use serde_json::Value;

use crate::core::proxy::claude::{THINK_CLOSE, THINK_OPEN};

/// One template family's thinking span: how it opens and every way it ends.
pub struct ThinkTags {
    pub open: &'static str,
    pub closes: &'static [&'static str],
}

/// The families llama.cpp's chat parser knows.
///
/// Mirrored from `common/chat.cpp`'s `thinking_start_tag` /
/// `thinking_end_tags` at the pinned commit. Under `--reasoning-format none`
/// these are what a reply carries inline. A family missing here reads as
/// answer, so the CHANGELOG names the table.
pub const THINKING_TAGS: [ThinkTags; 5] = [
    ThinkTags {
        open: THINK_OPEN,
        closes: &[THINK_CLOSE, "<tool_call>"],
    },
    ThinkTags {
        open: "[THINK]",
        closes: &["[/THINK]"],
    },
    ThinkTags {
        open: "<|channel|>analysis<|message|>",
        closes: &["<|end|>"],
    },
    ThinkTags {
        open: "<|channel>thought",
        closes: &["<channel|>"],
    },
    ThinkTags {
        open: "<mm:think>",
        closes: &["</mm:think>"],
    },
];

/// Characters of a reply spent thinking and spent answering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplyChars {
    pub thinking: u64,
    pub answer: u64,
}

impl ReplyChars {
    const fn add(self, other: Self) -> Self {
        Self {
            thinking: self.thinking + other.thinking,
            answer: self.answer + other.answer,
        }
    }
}

fn count(text: &str) -> u64 {
    u64::try_from(text.chars().count()).unwrap_or(u64::MAX)
}

/// The earliest opening tag in `text`, with its family.
fn next_open(text: &str) -> Option<(usize, &'static ThinkTags)> {
    THINKING_TAGS
        .iter()
        .filter_map(|tags| text.find(tags.open).map(|at| (at, tags)))
        .min_by_key(|(at, _)| *at)
}

/// The earliest closing tag of `tags` in `text`: its start and its length.
fn next_close(text: &str, tags: &ThinkTags) -> Option<(usize, usize)> {
    tags.closes
        .iter()
        .filter_map(|close| text.find(close).map(|at| (at, close.len())))
        .min_by_key(|(at, _)| *at)
}

/// `content` split into thinking and answer characters.
///
/// Every span of every family counts, tags themselves never do, and a span
/// that never closes is thinking to the end — the reply the model was cut
/// off in, or the one that went straight to a tool call (design §11).
#[must_use]
pub fn split_thinking(content: &str) -> ReplyChars {
    let mut rest = content;
    let mut out = ReplyChars::default();
    while let Some((at, tags)) = next_open(rest) {
        out.answer += count(&rest[..at]);
        let inner = &rest[at + tags.open.len()..];
        let Some((end, close_len)) = next_close(inner, tags) else {
            out.thinking += count(inner);
            return out;
        };
        out.thinking += count(&inner[..end]);
        rest = &inner[end + close_len..];
    }
    out.answer += count(rest);
    out
}

fn text_of<'v>(value: &'v Value, key: &str) -> &'v str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The reasoning field under either spelling: llama.cpp's `reasoning_content`,
/// or the `reasoning` other OpenAI-compatible servers use.
fn reasoning_of(value: &Value) -> &str {
    let content = text_of(value, "reasoning_content");
    if content.is_empty() {
        text_of(value, "reasoning")
    } else {
        content
    }
}

/// Every `tool_calls[].function.arguments` fragment's characters — partial
/// JSON on a stream, whole on a buffered body; never parsed, only counted.
fn arguments_chars(value: &Value) -> u64 {
    value
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| call.get("function"))
        .map(|f| count(text_of(f, "arguments")))
        .sum()
}

/// A buffered `choices[0].message`: reasoning, the spans in `content`, and
/// the tool-call arguments as answer.
#[must_use]
pub fn reply_chars(message: &Value) -> ReplyChars {
    let spans = split_thinking(text_of(message, "content"));
    ReplyChars {
        thinking: count(reasoning_of(message)) + spans.thinking,
        answer: spans.answer + arguments_chars(message),
    }
}

/// An SSE body's frames, folded before they are scanned.
///
/// `content` fragments are concatenated BEFORE the span scan so a tag split
/// across chunks still counts; reasoning and tool arguments are summed per
/// fragment. Frames that are not JSON, carry no delta, or are the `[DONE]`
/// sentinel add nothing.
pub fn stream_reply_chars<'a>(frames: impl Iterator<Item = &'a str>) -> ReplyChars {
    let mut content = String::new();
    let mut rest = ReplyChars::default();
    for delta in frames.filter_map(delta_of) {
        content.push_str(text_of(&delta, "content"));
        rest = rest.add(ReplyChars {
            thinking: count(reasoning_of(&delta)),
            answer: arguments_chars(&delta),
        });
    }
    split_thinking(&content).add(rest)
}

fn delta_of(frame: &str) -> Option<Value> {
    serde_json::from_str::<Value>(frame)
        .ok()?
        .get("choices")?
        .get(0)?
        .get("delta")
        .cloned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ReplyChars, reply_chars, split_thinking, stream_reply_chars};

    fn chars(thinking: u64, answer: u64) -> ReplyChars {
        ReplyChars { thinking, answer }
    }

    #[test]
    fn a_closed_think_span_is_thinking_and_the_rest_is_answer() {
        assert_eq!(split_thinking("<think>abc</think>de"), chars(3, 2));
        assert_eq!(split_thinking("no tags at all"), chars(0, 14));
        assert_eq!(split_thinking(""), chars(0, 0));
    }

    #[test]
    fn an_unclosed_span_is_thinking_to_the_end_and_every_span_counts() {
        assert_eq!(
            split_thinking("<think>abc"),
            chars(3, 0),
            "cut by max_tokens"
        );
        assert_eq!(
            split_thinking("<think>abc<tool_call>{}"),
            chars(3, 2),
            "a tool call ends the span under none; a call left inline is answer, as its arguments are"
        );
        assert_eq!(
            split_thinking("x<think>ab</think>y<think>c</think>z"),
            chars(3, 3)
        );
    }

    #[test]
    fn every_family_in_the_table_is_scanned_and_tags_are_never_counted() {
        assert_eq!(split_thinking("[THINK]ab[/THINK]c"), chars(2, 1), "Mistral");
        assert_eq!(
            split_thinking("<|channel|>analysis<|message|>ab<|end|>c"),
            chars(2, 1),
            "gpt-oss"
        );
        assert_eq!(
            split_thinking("<|channel>thoughtab<channel|>c"),
            chars(2, 1),
            "Gemma"
        );
        assert_eq!(
            split_thinking("<mm:think>ab</mm:think>c"),
            chars(2, 1),
            "MiniMax"
        );
    }

    #[test]
    fn characters_are_scalars_not_bytes() {
        assert_eq!(split_thinking("<think>ééé</think>日本"), chars(3, 2));
    }

    #[test]
    fn a_buffered_message_counts_reasoning_content_spans_and_tool_arguments() {
        let message = json!({
            "reasoning_content": "abcd",
            "content": "<think>xy</think>ok",
            "tool_calls": [{"function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}}]
        });
        assert_eq!(reply_chars(&message), chars(6, 2 + 12));
        assert_eq!(reply_chars(&json!({})), chars(0, 0));
        assert_eq!(
            reply_chars(&json!({"reasoning": "ab"})),
            chars(2, 0),
            "the foreign spelling is an alias"
        );
    }

    #[test]
    fn a_stream_is_folded_before_it_is_scanned() {
        let frames = [
            r#"{"choices":[{"delta":{"content":"<thi"}}]}"#,
            r#"{"choices":[{"delta":{"content":"nk>ab</thi"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"zz","content":"nk>c"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"f","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":null}"#,
            "not json",
            "[DONE]",
        ];
        assert_eq!(stream_reply_chars(frames.into_iter()), chars(2 + 2, 1 + 7));
    }
}
