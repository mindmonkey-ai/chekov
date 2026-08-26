//! Agent-protocol proxy facade (§C.6: traits carry behavior only).
//!
//! `llama-server` speaks `OpenAI` `/v1/chat/completions`. Coding agents do not:
//! Claude Code speaks Anthropic `/v1/messages`. Rather than teach the serve
//! loop every dialect, each agent gets an `AgentFacade` and the loop stays
//! protocol-blind — adding a second agent touches no shared code.

pub mod claude;
pub mod http;
pub mod serve;

use crate::error::ChekovError;
use http::{HttpRequest, HttpResponse};

/// What the facade decided to do with an inbound agent request.
pub enum Action {
    /// Answer locally — no upstream round trip (model lists, token counts).
    Reply(HttpResponse),
    /// Translate and forward to the upstream `OpenAI`-compatible server.
    Forward(Forward),
}

/// A translated upstream call.
pub struct Forward {
    /// Upstream path, e.g. `/v1/chat/completions`.
    pub path: String,
    /// Upstream request body (`OpenAI` dialect).
    pub body: Vec<u8>,
    /// Whether the agent asked for SSE. Decides which translator runs.
    pub stream: bool,
}

/// One SSE frame in the agent's own dialect.
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

impl SseEvent {
    /// Named event with a JSON payload.
    pub fn new(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            data: data.into(),
        }
    }
}

/// Per-agent protocol translation. One implementation per supported agent.
pub trait AgentFacade: Send + Sync {
    /// Facade name as it appears in logs and `chekov proxy <name>`.
    fn name(&self) -> &'static str;

    /// Classify an inbound request: answer it here, or forward it upstream.
    fn route(&self, req: &HttpRequest) -> Result<Action, ChekovError>;

    /// Whole-body upstream response (`OpenAI` JSON) into the agent's dialect.
    fn translate_response(&self, upstream: &str) -> Result<String, ChekovError>;

    /// Fresh translator for one streaming exchange — SSE translation is
    /// stateful (open content blocks, running token counts), so it cannot
    /// live on the shared facade value.
    fn stream_translator(&self) -> Box<dyn StreamTranslator>;
}

/// Stateful SSE translation for a single streamed response.
pub trait StreamTranslator {
    /// One upstream `data:` payload becomes zero or more agent-side frames.
    fn on_chunk(&mut self, data: &str) -> Vec<SseEvent>;

    /// The upstream stream ended; emit whatever terminal frames the agent's
    /// protocol requires (closing blocks, stop reason, usage).
    fn finish(&mut self) -> Vec<SseEvent>;

    /// The upstream connection failed mid-stream. Emit whatever the agent's
    /// protocol uses to report a failed turn; the default is silence, which
    /// keeps the serve loop protocol-blind for translators that have none.
    fn on_upstream_error(&mut self, reason: &str) -> Vec<SseEvent> {
        let _ = reason;
        Vec::new()
    }
}

/// Every agent chekov can proxy for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AgentKind {
    /// Anthropic `/v1/messages` — Claude Code, and any Anthropic SDK client.
    Claude,
}

impl AgentKind {
    /// Build the facade for this agent, bound to the upstream model alias.
    #[must_use]
    pub fn facade(self, model: &str) -> Box<dyn AgentFacade> {
        match self {
            Self::Claude => Box::new(claude::ClaudeFacade::new(model)),
        }
    }

    /// Directory name under `<root>/agents/` holding generated settings.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Claude => "claude",
        }
    }

    /// Executable `chekov launch` spawns.
    #[must_use]
    pub const fn binary(self) -> &'static str {
        match self {
            Self::Claude => "claude",
        }
    }

    /// Environment variable that redirects the agent at a config directory.
    /// This, not the `ANTHROPIC_*` block, is the mechanism a launcher needs:
    /// Claude Code writes its settings-file `env` entries over the inherited
    /// shell environment, so exported variables alone do not survive.
    #[must_use]
    pub const fn config_dir_var(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
        }
    }

    /// The user's real settings, for carry-forward. Unreadable or malformed
    /// settings yield `None`: a launch must not fail because another tool's
    /// config is broken. Claude Code keeps `claude mcp add` servers in
    /// `~/.claude.json`, not `settings.json`, and a `CLAUDE_CONFIG_DIR` session
    /// never reads the former — so its `mcpServers` are merged in here, else the
    /// launched session would show only the servers pinned in `settings.json`.
    #[must_use]
    pub fn read_user_settings(self) -> Option<serde_json::Value> {
        let home = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())?;
        match self {
            Self::Claude => {
                let settings = read_json(&home.join(".claude").join("settings.json"));
                let extra = read_json(&home.join(".claude.json"))
                    .and_then(|v| v.get("mcpServers").cloned());
                crate::core::launch::merge_mcp_servers(settings, extra)
            }
        }
    }
}

/// Best-effort JSON read: a missing or malformed file yields `None` so a launch
/// never fails because another tool's config is broken.
fn read_json(path: &std::path::Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
