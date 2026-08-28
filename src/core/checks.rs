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

/// A raw sysctl value of 0 means "unset — macOS system default", which is
/// ~75% of physical RAM, NOT zero. Returns (effective MB, `is_system_default`).
#[must_use]
pub const fn effective_wired_mb(raw: u64, memsize_bytes: u64) -> (u64, bool) {
    if raw == 0 {
        (memsize_bytes / 4 * 3 / (1024 * 1024), true)
    } else {
        (raw, false)
    }
}

/// What a wired-limit gate should do for this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiredVerdict {
    /// The live limit already satisfies the requirement.
    Satisfied,
    /// Raising `iogpu.wired_limit_mb` can satisfy the requirement.
    Low,
    /// The requirement exceeds physical RAM — no sysctl can ever satisfy it.
    Unreachable,
}

/// Pure verdict over injected numbers, so the branch is testable offline.
#[must_use]
pub const fn wired_verdict(required_mb: u64, actual_mb: u64, ram_mb: u64) -> WiredVerdict {
    if actual_mb >= required_mb {
        WiredVerdict::Satisfied
    } else if required_mb > ram_mb {
        WiredVerdict::Unreachable
    } else {
        WiredVerdict::Low
    }
}

/// Read the live GPU wired limit (macOS), resolving the 0-means-default
/// sentinel. `None` when unreadable — callers decide warning vs hard stop.
#[must_use]
pub fn wired_limit_mb() -> Option<(u64, bool)> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "iogpu.wired_limit_mb"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = parse_sysctl_mb(&String::from_utf8_lossy(&out.stdout))?;
    let memsize = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_sysctl_mb(&String::from_utf8_lossy(&o.stdout)))?;
    Some(effective_wired_mb(raw, memsize))
}

/// Physical RAM in MB (`hw.memsize` is bytes). `None` when unreadable.
#[must_use]
pub fn physical_ram_mb() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let bytes = parse_sysctl_mb(&String::from_utf8_lossy(&out.stdout))?;
    Some(bytes / (1024 * 1024))
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
    use super::{WiredVerdict, wired_verdict};

    #[test]
    fn a_requirement_above_physical_ram_is_unreachable_not_merely_low() {
        // 32 GB Mac, chekov's built-in 200000 MB requirement: no sysctl can help.
        assert_eq!(
            wired_verdict(200_000, 24_576, 32_768),
            WiredVerdict::Unreachable
        );
    }

    #[test]
    fn a_requirement_the_machine_could_hold_is_merely_low() {
        // 256 GB Mac at the 75% system default: raising the sysctl would work.
        assert_eq!(wired_verdict(200_000, 196_608, 262_144), WiredVerdict::Low);
    }

    #[test]
    fn a_satisfied_limit_is_satisfied() {
        assert_eq!(
            wired_verdict(187_000, 196_608, 262_144),
            WiredVerdict::Satisfied
        );
    }
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

    #[test]
    fn wired_zero_resolves_to_three_quarters_of_ram() {
        // 256 GiB machine, unset limit → 196608 MB system default.
        let (mb, is_default) = super::effective_wired_mb(0, 274_877_906_944);
        assert_eq!(mb, 196_608);
        assert!(is_default);
        let (mb, is_default) = super::effective_wired_mb(230_000, 274_877_906_944);
        assert_eq!(mb, 230_000);
        assert!(!is_default);
    }
}
