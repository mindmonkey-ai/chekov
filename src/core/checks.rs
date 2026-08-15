//! Doctor building blocks (§5 of the bootstrap prompt).
//!
//! Pure functions over injected data — thresholds come from config (§16.11),
//! responses from the `HttpClient` seam, so everything is testable offline.

use crate::core::config::DoctorSection;

/// Why a generation is considered degenerate, if it is.
/// Guards the known GGUF `blk.61` failure class.
#[must_use]
pub fn degenerate_reason(text: &str, cfg: &DoctorSection) -> Option<String> {
    if let Some(run) = longest_identical_run(text).filter(|&r| r >= cfg.degenerate_run_len) {
        return Some(format!(
            "{run} identical consecutive tokens (threshold {})",
            cfg.degenerate_run_len
        ));
    }
    let total = text.chars().count();
    let bad = text.chars().filter(|&c| c == '\u{FFFD}').count();
    if total > 0 && bad * 100 > total * usize::from(cfg.replacement_char_max_pct) {
        return Some(format!(
            "replacement-char density {}% exceeds {}%",
            bad * 100 / total,
            cfg.replacement_char_max_pct
        ));
    }
    None
}

fn longest_identical_run(text: &str) -> Option<usize> {
    let mut prev: Option<&str> = None;
    let mut run = 0usize;
    let mut max_run = 0usize;
    for token in text.split_whitespace() {
        run = if prev == Some(token) { run + 1 } else { 1 };
        max_run = max_run.max(run);
        prev = Some(token);
    }
    (max_run > 0).then_some(max_run)
}

/// `choices[0].message.content` from an OpenAI-door response body.
#[must_use]
pub fn chat_content(body: &str) -> Option<String> {
    json_pointer_str(body, "/choices/0/message/content")
}

/// `content[0].text` from an Anthropic-door response body.
#[must_use]
pub fn anthropic_content(body: &str) -> Option<String> {
    json_pointer_str(body, "/content/0/text")
}

fn json_pointer_str(body: &str, pointer: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .pointer(pointer)?
        .as_str()
        .map(ToOwned::to_owned)
}

/// Think-tag retention: interleaved reasoning must survive the round trip.
#[must_use]
pub fn contains_think_tag(content: &str) -> bool {
    content.contains("<think>")
}

/// Parse `sysctl -n iogpu.wired_limit_mb` output.
#[must_use]
pub fn parse_sysctl_mb(output: &str) -> Option<u64> {
    output.trim().parse().ok()
}

/// True when something is already listening on `host:port`.
#[must_use]
pub fn port_in_use(host: &str, port: u16) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::{anthropic_content, chat_content, degenerate_reason, parse_sysctl_mb};
    use crate::core::config::DoctorSection;

    fn cfg() -> DoctorSection {
        DoctorSection::default()
    }

    #[test]
    fn healthy_text_is_not_degenerate() {
        let text = "fn main() { println!(\"hello\"); } // ordinary rust output".repeat(40);
        assert_eq!(degenerate_reason(&text, &cfg()), None);
    }

    #[test]
    fn long_identical_run_is_degenerate() {
        let text = format!("prefix {} suffix", "same ".repeat(30));
        let reason = degenerate_reason(&text, &cfg()).expect("30-run must trip");
        assert!(reason.contains("30"), "run length missing: {reason}");
    }

    #[test]
    fn run_below_threshold_passes() {
        let text = format!("prefix {} suffix", "same ".repeat(29));
        assert_eq!(degenerate_reason(&text, &cfg()), None);
    }

    #[test]
    fn replacement_char_density_is_degenerate() {
        let text = format!("ok {}", "\u{FFFD}".repeat(50));
        let reason = degenerate_reason(&text, &cfg()).expect("density must trip");
        assert!(reason.contains("replacement"), "wrong reason: {reason}");
    }

    #[test]
    fn chat_content_reads_openai_shape() {
        let body =
            r#"{"choices":[{"message":{"role":"assistant","content":"<think>x</think>hi"}}]}"#;
        assert_eq!(chat_content(body).as_deref(), Some("<think>x</think>hi"));
        assert_eq!(chat_content("{}"), None);
    }

    #[test]
    fn anthropic_content_reads_messages_shape() {
        let body = r#"{"content":[{"type":"text","text":"hello"}],"role":"assistant"}"#;
        assert_eq!(anthropic_content(body).as_deref(), Some("hello"));
        assert_eq!(anthropic_content(r#"{"content":[]}"#), None);
    }

    #[test]
    fn sysctl_output_parses_with_whitespace() {
        assert_eq!(parse_sysctl_mb("163840\n"), Some(163_840));
        assert_eq!(parse_sysctl_mb("garbage"), None);
    }
}
