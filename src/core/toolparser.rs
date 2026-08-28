//! Which llama.cpp tool-call parser a chat template will resolve to.
//!
//! Replays the substring cascade in `common/chat.cpp`'s
//! `common_chat_try_specialized_template` (vendored at `llama.cpp/common/chat.cpp`,
//! ~3430-3552). Falling through every arm means llama.cpp uses the generic PEG
//! autoparser, which is not a tool-call contract — it is a guess.
//!
//! Fallthrough means "no dedicated parser", NOT "cannot call tools" — the
//! generic autoparser handles many shapes. `unsloth/MiniMax-M2.7-GGUF` falls
//! through and works. So this is a *signal to report*, not a gate to refuse
//! on; see the note in IDEAS.md.
//!
//! This matters more than any popularity signal.
//! `OBLITERATUS/Qwen3.8-27B-OBLITERATED` has over half a million downloads and
//! a 506-character template with **zero** tool markup; `unsloth/Qwen3.8-27B-GGUF`
//! ships 9,993 characters that resolve to Qwen3-Coder. Ranking by downloads
//! recommends the one that cannot call a tool.

/// The parser a template resolves to, or the fallthrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolParser {
    Ministral,
    GptOss,
    MuseGlimmer,
    FunctionaryV32,
    KimiK2Thinking,
    KimiK3,
    Cohere2Moe,
    Lfm25,
    GigaChatV3,
    MiniMaxM3,
    DeepSeekV32,
    MiniCpm5,
    Qwen3Coder,
    /// No specialized arm matched: llama.cpp falls back to the generic PEG
    /// autoparser. Not a contract.
    AutoparserFallthrough,
}

impl ToolParser {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ministral => "ministral",
            Self::GptOss => "gpt-oss",
            Self::MuseGlimmer => "muse-glimmer",
            Self::FunctionaryV32 => "functionary-v3.2",
            Self::KimiK2Thinking => "kimi-k2-thinking",
            Self::KimiK3 => "kimi-k3",
            Self::Cohere2Moe => "cohere2-moe",
            Self::Lfm25 => "lfm2.5",
            Self::GigaChatV3 => "gigachat-v3",
            Self::MiniMaxM3 => "minimax-m3",
            Self::DeepSeekV32 => "deepseek-v3.2",
            Self::MiniCpm5 => "minicpm5",
            Self::Qwen3Coder => "qwen3-coder-xml",
            Self::AutoparserFallthrough => "autoparser-fallthrough",
        }
    }

    /// Whether a model with this parser can be trusted to emit tool calls in a
    /// shape llama.cpp will parse.
    #[must_use]
    pub const fn is_specialized(self) -> bool {
        !matches!(self, Self::AutoparserFallthrough)
    }
}

/// Classify a chat template, in the cascade's own order — the first arm that
/// matches wins, exactly as llama.cpp evaluates it.
#[must_use]
pub fn classify(src: &str) -> ToolParser {
    let has = |n: &str| src.contains(n);
    if has("[SYSTEM_PROMPT]") && has("[TOOL_CALLS]") && has("[ARGS]") && !has("[CALL_ID]") {
        return ToolParser::Ministral;
    }
    if has("<|channel|>") {
        return ToolParser::GptOss;
    }
    if has("<atem:function_calls>") && has("<|eom|>") {
        return ToolParser::MuseGlimmer;
    }
    if has(">>>all") && has(">>>${recipient}") {
        return ToolParser::FunctionaryV32;
    }
    if has("<|tool_calls_section_begin|>") && has("<|tool_call_begin|>") {
        return ToolParser::KimiK2Thinking;
    }
    if has("<|open|>") && has("<|close|>") && has("<|end_of_msg|>") {
        return ToolParser::KimiK3;
    }
    if has("<|START_TEXT|>") && has("<|START_ACTION|>") {
        return ToolParser::Cohere2Moe;
    }
    if has("List of tools: [") && !has("<|tool_list_start|>") {
        return ToolParser::Lfm25;
    }
    if has("<|role_sep|>") && has("<|message_sep|>") && !has("<|function_call|>") {
        return ToolParser::GigaChatV3;
    }
    classify_tail(src)
}

/// The tail of the same cascade, split only to stay under the 40-LOC gate.
/// Order is still the engine's order.
fn classify_tail(src: &str) -> ToolParser {
    let has = |n: &str| src.contains(n);
    if has("]<]minimax[>[") && has("<tool_call>") && has("<invoke name=") {
        return ToolParser::MiniMaxM3;
    }
    if has("dsml_token") && has("DSML") && (has("function_calls") || has("tool_calls")) {
        return ToolParser::DeepSeekV32;
    }
    if has("Tool usage guidelines:") && has("<function name=\"") && has("<param name=\"") {
        return ToolParser::MiniCpm5;
    }
    if has("<tool_call>") && has("<function=") && has("<parameter=") {
        return ToolParser::Qwen3Coder;
    }
    ToolParser::AutoparserFallthrough
}

#[cfg(test)]
mod tests {
    use super::{ToolParser, classify};

    #[test]
    fn a_template_with_no_tool_markup_falls_through() {
        // Shape of OBLITERATUS/Qwen3.8-27B-OBLITERATED's real 506-char template:
        // a plain role loop with no tool vocabulary anywhere.
        let src = "{% for message in messages %}{{'<|im_start|>' + message['role'] + '\\n' \
                   + message['content'] + '<|im_end|>\\n'}}{% endfor %}";
        assert_eq!(classify(src), ToolParser::AutoparserFallthrough);
        assert!(
            !classify(src).is_specialized(),
            "half a million downloads does not make a model tool-capable"
        );
    }

    #[test]
    fn a_working_model_can_still_fall_through_the_cascade() {
        // Shape of unsloth/MiniMax-M2.7-GGUF's real 6594-char template, which
        // the author runs daily for agentic work with hermes_ok = true. It
        // carries `<minimax:tool_call>` and `<invoke name=`, but NOT the M3
        // namespace token `]<]minimax[>[` that llama.cpp's only MiniMax arm
        // requires — so it falls through to the generic PEG autoparser.
        //
        // Fallthrough therefore means "no specialized parser", NOT "cannot
        // call tools". Treating it as a hard refusal would reject a model that
        // demonstrably works.
        let src = "<tools>…</tools> <minimax:tool_call><invoke name=\"x\">\
                   <parameter>y</parameter></invoke></minimax:tool_call>";
        assert_eq!(classify(src), ToolParser::AutoparserFallthrough);
        assert!(
            src.contains("<invoke name="),
            "it has tool vocabulary — just not a shape with a dedicated arm"
        );
    }

    #[test]
    fn the_qwen3_coder_xml_shape_is_recognised() {
        let src = "…<tool_call>… <function=name> … <parameter=arg> …";
        assert_eq!(classify(src), ToolParser::Qwen3Coder);
        assert!(classify(src).is_specialized());
    }

    #[test]
    fn gpt_oss_wins_on_the_channel_marker_alone() {
        assert_eq!(classify("blah <|channel|> blah"), ToolParser::GptOss);
    }

    #[test]
    fn the_cascade_order_is_the_engines_order() {
        // A template carrying BOTH Ministral and gpt-oss markers must resolve
        // to Ministral, because that arm is evaluated first.
        let both = "[SYSTEM_PROMPT] [TOOL_CALLS] [ARGS] <|channel|>";
        assert_eq!(classify(both), ToolParser::Ministral);
    }

    #[test]
    fn a_negative_marker_can_veto_an_arm() {
        // Ministral requires [CALL_ID] to be ABSENT.
        let with_call_id = "[SYSTEM_PROMPT] [TOOL_CALLS] [ARGS] [CALL_ID]";
        assert_ne!(classify(with_call_id), ToolParser::Ministral);
        // LFM2.5 requires <|tool_list_start|> to be absent.
        assert_eq!(classify("List of tools: [x]"), ToolParser::Lfm25);
        assert_ne!(
            classify("List of tools: [x] <|tool_list_start|>"),
            ToolParser::Lfm25
        );
    }
}
