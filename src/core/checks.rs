//! Doctor building blocks (§5 of the bootstrap prompt): pure functions over
//! injected data — thresholds come from config (§16.11), responses from the
//! `HttpClient` seam, so everything here is unit-testable offline.

use crate::core::config::DoctorSection;

/// Why a generation is considered degenerate, if it is.
/// Guards the known GGUF `blk.61` failure class.
#[must_use]
pub fn degenerate_reason(text: &str, cfg: &DoctorSection) -> Option<String> {
    let _ = (text, cfg);
    todo!("cycle 3 red")
}

/// `choices[0].message.content` from an OpenAI-door response body.
#[must_use]
pub fn chat_content(body: &str) -> Option<String> {
    let _ = body;
    todo!("cycle 3 red")
}

/// `content[0].text` from an Anthropic-door response body.
#[must_use]
pub fn anthropic_content(body: &str) -> Option<String> {
    let _ = body;
    todo!("cycle 3 red")
}

/// Think-tag retention: interleaved reasoning must survive the round trip.
#[must_use]
pub fn contains_think_tag(content: &str) -> bool {
    content.contains("<think>")
}

/// Parse `sysctl -n iogpu.wired_limit_mb` output.
#[must_use]
pub fn parse_sysctl_mb(output: &str) -> Option<u64> {
    let _ = output;
    todo!("cycle 3 red")
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
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"<think>x</think>hi"}}]}"#;
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
