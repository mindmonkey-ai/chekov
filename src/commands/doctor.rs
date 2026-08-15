//! `chekov doctor` — five independent checks, summary table, non-zero exit on
//! any failure. Skipped checks are reported as skipped, never as passed.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::core::config::Config;
use crate::core::hub::HttpClient;
use crate::core::registry::Effective;
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct DoctorCmd {}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail(String),
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: &'static str,
    pub status: CheckStatus,
}

/// Run all five checks against the server (via the HTTP seam — tests inject
/// canned responses).
pub fn run_checks(http: &dyn HttpClient, cfg: &Config, eff: &Effective) -> Vec<CheckResult> {
    let (openai, content) = check_openai(http, cfg, eff);
    let anthropic = check_anthropic(http, cfg, eff);
    let think = check_think(eff, content.as_deref());
    let canary = check_canary(http, cfg, eff);
    let ctx_floor = check_ctx(cfg, eff);
    [
        ("OpenAI door (/v1/chat/completions)", openai),
        ("Anthropic door (/v1/messages)", anthropic),
        ("think-tag retention", think),
        ("NaN canary (degenerate output)", canary),
        ("context floor (hermes)", ctx_floor),
    ]
    .map(|(name, status)| CheckResult { name, status })
    .to_vec()
}

fn chat_body(eff: &Effective, prompt: &str, max_tokens: u32) -> String {
    serde_json::json!({
        "model": eff.name,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
    })
    .to_string()
}

fn door_request(cfg: &Config, path: &str, body: String) -> crate::core::hub::JsonRequest {
    crate::core::hub::JsonRequest {
        url: format!("{}{path}", cfg.base_url()),
        body,
        bearer: Some(cfg.file.server.api_key.clone()),
    }
}

fn check_openai(
    http: &dyn HttpClient,
    cfg: &Config,
    eff: &Effective,
) -> (CheckStatus, Option<String>) {
    let req = door_request(
        cfg,
        "/v1/chat/completions",
        chat_body(eff, "Reply with a short greeting.", 64),
    );
    match http.post_json(&req) {
        Err(e) => (CheckStatus::Fail(e.to_string()), None),
        Ok(body) => crate::core::checks::chat_content(&body).map_or_else(
            || {
                (
                    CheckStatus::Fail("no choices[0].message.content in response".into()),
                    None,
                )
            },
            |content| (CheckStatus::Pass, Some(content)),
        ),
    }
}

fn check_anthropic(http: &dyn HttpClient, cfg: &Config, eff: &Effective) -> CheckStatus {
    let req = door_request(
        cfg,
        "/v1/messages",
        chat_body(eff, "Reply with a short greeting.", 64),
    );
    match http.post_json(&req) {
        Err(e) => CheckStatus::Fail(e.to_string()),
        Ok(body) => crate::core::checks::anthropic_content(&body).map_or_else(
            || CheckStatus::Fail("no content[0].text in response".into()),
            |_| CheckStatus::Pass,
        ),
    }
}

fn check_think(eff: &Effective, content: Option<&str>) -> CheckStatus {
    let interleaved = eff
        .flags
        .windows(2)
        .any(|w| w[0] == "--reasoning-format" && w[1] == "none");
    if !interleaved {
        return CheckStatus::Skipped("--reasoning-format none not set for this model".into());
    }
    match content {
        None => CheckStatus::Skipped("OpenAI door failed; nothing to inspect".into()),
        Some(c) if crate::core::checks::contains_think_tag(c) => CheckStatus::Pass,
        Some(_) => CheckStatus::Fail("response lost its <think> tags".into()),
    }
}

fn check_canary(http: &dyn HttpClient, cfg: &Config, eff: &Effective) -> CheckStatus {
    let prompt = "Write a complete Rust module implementing a doubly linked list with \
                  insert, remove, and iterator support, plus unit tests. Code only.";
    let req = door_request(
        cfg,
        "/v1/chat/completions",
        chat_body(eff, prompt, cfg.file.doctor.canary_max_tokens),
    );
    match http.post_json(&req) {
        Err(e) => CheckStatus::Fail(e.to_string()),
        Ok(body) => crate::core::checks::chat_content(&body).map_or_else(
            || CheckStatus::Fail("no choices[0].message.content in response".into()),
            |content| {
                crate::core::checks::degenerate_reason(&content, &cfg.file.doctor)
                    .map_or(CheckStatus::Pass, CheckStatus::Fail)
            },
        ),
    }
}

fn check_ctx(cfg: &Config, eff: &Effective) -> CheckStatus {
    let floor = cfg.file.limits.hermes_ctx_floor;
    if eff.entry.hermes_ok {
        if eff.ctx_size >= floor {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail(format!(
                "effective ctx {} is below the hermes floor {floor}",
                eff.ctx_size
            ))
        }
    } else {
        CheckStatus::Skipped(format!(
            "hermes_ok = false (ctx {} vs floor {floor} is advisory)",
            eff.ctx_size
        ))
    }
}

impl Command for DoctorCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let reg = ctx.registry()?;
        let name = crate::core::server::read_run_state(&ctx.config)
            .unwrap_or(reg.active_name()?.to_owned());
        let eff = reg.effective(&name)?;
        let results = run_checks(ctx.http.as_ref(), &ctx.config, &eff);
        let rows: Vec<Vec<String>> = results
            .iter()
            .map(|r| {
                let (status, detail) = match &r.status {
                    CheckStatus::Pass => ("PASS".to_owned(), String::new()),
                    CheckStatus::Fail(reason) => ("FAIL".to_owned(), reason.clone()),
                    CheckStatus::Skipped(note) => ("SKIP".to_owned(), note.clone()),
                };
                vec![r.name.to_owned(), status, detail]
            })
            .collect();
        println!(
            "{}",
            super::render_table(&["CHECK", "STATUS", "DETAIL"], &rows)
        );
        let failed = results
            .iter()
            .any(|r| matches!(r.status, CheckStatus::Fail(_)));
        Ok(if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::{CheckStatus, run_checks};
    use crate::core::config::Config;
    use crate::core::hub::{HttpClient, JsonRequest};
    use crate::core::registry::{ModelEntry, Registry};
    use crate::error::ChekovError;

    /// Pops one canned response per POST, in order (§8.2 boundary fake).
    struct SeqHttp {
        responses: RefCell<VecDeque<String>>,
    }

    impl SeqHttp {
        fn new(responses: &[&str]) -> Self {
            Self {
                responses: RefCell::new(responses.iter().map(|s| (*s).to_owned()).collect()),
            }
        }
    }

    impl HttpClient for SeqHttp {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            unreachable!("doctor never GETs")
        }

        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| ChekovError::EndpointDown {
                    url: "fake".into(),
                    reason: "no canned response left".into(),
                })
        }
    }

    fn openai(content: &str) -> String {
        serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string()
    }

    fn anthropic(text: &str) -> String {
        serde_json::json!({"content": [{"type": "text", "text": text}]}).to_string()
    }

    fn fixture(
        reasoning: bool,
        ctx_size: Option<u32>,
    ) -> (Config, crate::core::registry::Effective) {
        let root = std::env::temp_dir().join("chekov-test-doctor");
        let _ = std::fs::create_dir_all(&root);
        let cfg = Config::load(&root).expect("defaults");
        let mut reg = Registry::default();
        let extra_flags = if reasoning {
            vec!["--reasoning-format".into(), "none".into()]
        } else {
            vec![]
        };
        reg.models.insert(
            "m".into(),
            ModelEntry {
                repo: "org/repo".into(),
                quant: "Q8_0".into(),
                revision: "abc".into(),
                path: "models/m@abc".into(),
                first_shard: "m.gguf".into(),
                hermes_ok: true,
                ctx_size,
                extra_flags,
            },
        );
        (cfg, reg.effective("m").expect("registered"))
    }

    #[test]
    fn healthy_server_passes_all_five() {
        let (cfg, eff) = fixture(true, None);
        let http = SeqHttp::new(&[
            &openai("<think>plan</think>hello there"),
            &anthropic("hello"),
            &openai("fn main() { let list = LinkedList::new(); } // varied healthy code"),
        ]);
        let results = run_checks(&http, &cfg, &eff);
        assert_eq!(results.len(), 5);
        for r in &results {
            assert_eq!(r.status, CheckStatus::Pass, "{} not passing", r.name);
        }
    }

    #[test]
    fn degenerate_canary_fails_check_four() {
        let (cfg, eff) = fixture(true, None);
        let degenerate = format!("x {}", "loop ".repeat(40));
        let http = SeqHttp::new(&[
            &openai("<think>ok</think>fine"),
            &anthropic("fine"),
            &openai(&degenerate),
        ]);
        let results = run_checks(&http, &cfg, &eff);
        assert!(
            matches!(results[3].status, CheckStatus::Fail(ref r) if r.contains("identical")),
            "{:?}",
            results[3]
        );
    }

    #[test]
    fn think_check_skips_without_reasoning_flag() {
        let (cfg, eff) = fixture(false, None);
        let http = SeqHttp::new(&[&openai("no think tags"), &anthropic("hi"), &openai("code")]);
        let results = run_checks(&http, &cfg, &eff);
        assert!(
            matches!(results[2].status, CheckStatus::Skipped(_)),
            "{:?}",
            results[2]
        );
    }

    #[test]
    fn ctx_below_floor_fails_when_hermes_ok() {
        let (cfg, eff) = fixture(true, Some(4096));
        let http = SeqHttp::new(&[
            &openai("<think>x</think>y"),
            &anthropic("y"),
            &openai("healthy code output"),
        ]);
        let results = run_checks(&http, &cfg, &eff);
        assert!(
            matches!(results[4].status, CheckStatus::Fail(ref r) if r.contains("65536")),
            "{:?}",
            results[4]
        );
    }

    #[test]
    fn dead_endpoint_fails_loudly_not_silently() {
        let (cfg, eff) = fixture(true, None);
        let http = SeqHttp::new(&[]);
        let results = run_checks(&http, &cfg, &eff);
        assert!(
            matches!(results[0].status, CheckStatus::Fail(_)),
            "{:?}",
            results[0]
        );
    }
}
