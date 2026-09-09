//! Thinking versus answer, counted in characters.
//!
//! Read where the thinking still exists — the `OpenAI` body before
//! translation — because a `--reasoning-format none` run leaves its thoughts
//! inline in `content` as the template family's own tags, and the proxy
//! strips them on the way to the agent (reasoning-stamp design §4, §11).

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
            chars(3, 0),
            "a tool call ends the span under none; the call itself is not content"
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
