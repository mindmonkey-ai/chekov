//! Agent-launch settings generation.
//!
//! Claude Code writes every `env` entry from its settings file into the
//! process environment at startup, replacing whatever the shell exported. A
//! launcher that only sets environment variables is therefore a no-op against
//! any user who pins `ANTHROPIC_MODEL` in `~/.claude/settings.json`. The
//! working mechanism is `CLAUDE_CONFIG_DIR`: point the agent at a
//! chekov-owned config directory whose `settings.json` chekov generates.

use serde_json::{Map, Value, json};

/// Settings keys carried forward from the user's real config so a local-model
/// session keeps the tools they expect. `env` is deliberately absent: it is
/// the block chekov owns.  `extraKnownMarketplaces` is needed so local plugins
/// (e.g. `pushkin-review@pushkin-review`) resolve to a known marketplace rather
/// than producing a "marketplace not found" warning on launch.
const CARRIED_KEYS: [&str; 5] = [
    "mcpServers",
    "hooks",
    "enabledPlugins",
    "permissions",
    "extraKnownMarketplaces",
];

/// What a generated `settings.json` needs to describe. Bundled to stay within
/// the 3-argument limit (§3.4).
pub struct LocalSession<'a> {
    /// Model alias to advertise, e.g. `minimax-m2.7`.
    pub model: &'a str,
    /// Declared context window — the agent otherwise assumes 200k.
    pub ctx_size: u32,
    /// Loopback port the chekov proxy is listening on.
    pub proxy_port: u16,
    /// `--api-key` the local server was launched with.
    pub api_key: &'a str,
}

/// The `env` block pointing Claude Code at the local proxy.
///
/// `ANTHROPIC_CUSTOM_MODEL_OPTION` is what makes a non-Anthropic id
/// selectable: it is the one model id accepted without a validation probe.
/// Gateway discovery is not used — it filters ids to those containing
/// `claude`, which would force renaming the model to satisfy a substring
/// check and forfeit the honest context declaration.
#[must_use]
pub fn render_env_block(session: &LocalSession) -> Map<String, Value> {
    let LocalSession {
        model,
        ctx_size,
        proxy_port,
        api_key,
    } = *session;
    let mut env = Map::new();
    let mut set = |key: &str, val: String| env.insert(key.to_owned(), Value::String(val));
    set(
        "ANTHROPIC_BASE_URL",
        format!("http://127.0.0.1:{proxy_port}"),
    );
    set("ANTHROPIC_AUTH_TOKEN", api_key.to_owned());
    set("ANTHROPIC_CUSTOM_MODEL_OPTION", model.to_owned());
    set(
        "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME",
        format!("{model} (chekov)"),
    );
    set(
        "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION",
        format!("local llama.cpp · {ctx_size} ctx"),
    );
    // MiniMax emits reasoning_content and the stream translator maps it to
    // thinking blocks, so the capability is real, not aspirational.
    set(
        "ANTHROPIC_CUSTOM_MODEL_OPTION_SUPPORTED_CAPABILITIES",
        "thinking".to_owned(),
    );
    set("ANTHROPIC_MODEL", model.to_owned());
    // Background traffic (title generation, the haiku slot) would otherwise
    // try to reach Anthropic and fail with no local route.
    set("ANTHROPIC_DEFAULT_HAIKU_MODEL", model.to_owned());
    set("CLAUDE_CODE_MAX_CONTEXT_TOKENS", ctx_size.to_string());
    env
}

/// Generated settings: chekov's `env` block plus carried-forward keys.
///
/// `source` is the user's real settings. Absent or malformed source settings
/// yield the env block alone — a missing MCP list must never stop a launch.
#[must_use]
pub fn render_local_settings(session: &LocalSession, source: Option<&Value>) -> Value {
    let mut out = Map::new();
    if let Some(Value::Object(real)) = source {
        for key in CARRIED_KEYS {
            if let Some(value) = real.get(key) {
                out.insert(key.to_owned(), value.clone());
            }
        }
    }
    out.insert("env".to_owned(), Value::Object(render_env_block(session)));
    Value::Object(out)
}

/// Pretty JSON with a trailing newline, ready to write.
#[must_use]
pub fn render_settings_json(session: &LocalSession, source: Option<&Value>) -> String {
    let mut text = serde_json::to_string_pretty(&render_local_settings(session, source))
        .unwrap_or_else(|_| json!({}).to_string());
    text.push('\n');
    text
}

/// Fold user-scope MCP servers into the `settings.json` `source`.
///
/// Claude Code stores servers added with `claude mcp add` in `~/.claude.json`
/// (top-level `mcpServers`), not in `~/.claude/settings.json`. A `chekov
/// launch` session runs under its own `CLAUDE_CONFIG_DIR`, so that file is
/// never read — those servers must be carried forward explicitly or the
/// session sees a truncated MCP list. `extra` is the `mcpServers` object read
/// from `~/.claude.json`; on a name conflict the `settings.json` entry wins, as
/// it is the more specific, explicitly carried file.
#[must_use]
pub fn merge_mcp_servers(settings: Option<Value>, extra: Option<Value>) -> Option<Value> {
    let Some(Value::Object(extra)) = extra else {
        return settings;
    };
    let mut merged = extra;
    if let Some(Value::Object(root)) = &settings
        && let Some(Value::Object(own)) = root.get("mcpServers")
    {
        for (name, cfg) in own {
            merged.insert(name.clone(), cfg.clone());
        }
    }
    let mut out = match settings {
        Some(Value::Object(root)) => Value::Object(root),
        _ => json!({}),
    };
    if let Value::Object(root) = &mut out {
        root.insert("mcpServers".to_owned(), Value::Object(merged));
    }
    Some(out)
}

/// Inject `servers` into an existing `.claude.json` value under top-level
/// `mcpServers`, preserving every other key Claude Code owns.
///
/// This is the file Claude Code actually reads user-scope MCP servers from
/// under `CLAUDE_CONFIG_DIR`; `settings.json` is ignored for MCP definitions.
/// Claude rewrites `.claude.json` on launch with its own state (project
/// history, auth caches), so this is a read-modify-write: `existing` is
/// whatever is already on disk (or `{}` when absent), and only `mcpServers` is
/// touched. A per-server conflict lets our `servers` entry win, since it is the
/// merged carry-forward the session was launched to provide.
#[must_use]
pub fn inject_mcp_servers(existing: Option<Value>, servers: &Map<String, Value>) -> Value {
    let mut out = match existing {
        Some(Value::Object(root)) => root,
        _ => Map::new(),
    };
    let mut merged = match out.get("mcpServers") {
        Some(Value::Object(own)) => own.clone(),
        _ => Map::new(),
    };
    for (name, cfg) in servers {
        merged.insert(name.clone(), cfg.clone());
    }
    out.insert("mcpServers".to_owned(), Value::Object(merged));
    Value::Object(out)
}

/// The `mcpServers` map from a carried-forward settings value, for injection
/// into `.claude.json`. An absent or shapeless block yields an empty map.
#[must_use]
pub fn mcp_servers_of(source: Option<&Value>) -> Map<String, Value> {
    match source.and_then(|v| v.get("mcpServers")) {
        Some(Value::Object(servers)) => servers.clone(),
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::{
        LocalSession, inject_mcp_servers, mcp_servers_of, merge_mcp_servers, render_env_block,
        render_local_settings, render_settings_json,
    };

    fn session() -> LocalSession<'static> {
        LocalSession {
            model: "minimax-m2.7",
            ctx_size: 163_840,
            proxy_port: 8787,
            api_key: "chekov-local",
        }
    }

    #[test]
    fn env_block_points_at_the_proxy_not_the_server() {
        let env = render_env_block(&session());
        assert_eq!(env["ANTHROPIC_BASE_URL"], json!("http://127.0.0.1:8787"));
    }

    #[test]
    fn env_block_registers_the_custom_model_option() {
        let env = render_env_block(&session());
        assert_eq!(env["ANTHROPIC_CUSTOM_MODEL_OPTION"], json!("minimax-m2.7"));
        assert_eq!(env["ANTHROPIC_MODEL"], json!("minimax-m2.7"));
    }

    #[test]
    fn env_block_routes_background_traffic_locally() {
        let env = render_env_block(&session());
        assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], json!("minimax-m2.7"));
    }

    #[test]
    fn env_block_declares_the_real_context_window() {
        let env = render_env_block(&session());
        assert_eq!(env["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], json!("163840"));
    }

    #[test]
    fn env_block_advertises_thinking_capability() {
        let env = render_env_block(&session());
        assert_eq!(
            env["ANTHROPIC_CUSTOM_MODEL_OPTION_SUPPORTED_CAPABILITIES"],
            json!("thinking")
        );
    }

    fn real_settings() -> Value {
        json!({
            "env": { "ANTHROPIC_MODEL": "fable-5", "ANTHROPIC_BASE_URL": "http://localhost:7888" },
            "mcpServers": { "context7": { "command": "npx" } },
            "hooks": { "PreToolUse": [{ "matcher": "Bash" }] },
            "enabledPlugins": { "some-plugin@marketplace": true },
            "permissions": { "allow": ["Bash(git status)"] },
            "theme": "dark"
        })
    }

    #[test]
    fn carries_forward_mcp_hooks_plugins_and_permissions() {
        let out = render_local_settings(&session(), Some(&real_settings()));
        for key in ["mcpServers", "hooks", "enabledPlugins", "permissions"] {
            assert_eq!(out[key], real_settings()[key], "{key} not carried forward");
        }
    }

    #[test]
    fn overrides_the_env_block_rather_than_merging_it() {
        let out = render_local_settings(&session(), Some(&real_settings()));
        // The whole point: a pinned fable-5 must not survive into the local
        // session, because Claude Code writes settings env over the shell.
        assert_eq!(out["env"]["ANTHROPIC_MODEL"], json!("minimax-m2.7"));
        assert_eq!(
            out["env"]["ANTHROPIC_BASE_URL"],
            json!("http://127.0.0.1:8787")
        );
    }

    #[test]
    fn leaves_uncarried_keys_behind() {
        let out = render_local_settings(&session(), Some(&real_settings()));
        assert!(out.get("theme").is_none(), "carried an unlisted key: {out}");
    }

    #[test]
    fn absent_source_settings_still_produce_a_usable_env_block() {
        let out = render_local_settings(&session(), None);
        assert_eq!(out["env"]["ANTHROPIC_MODEL"], json!("minimax-m2.7"));
        assert!(out.get("mcpServers").is_none());
    }

    #[test]
    fn malformed_source_settings_do_not_abort_the_launch() {
        let out = render_local_settings(&session(), Some(&json!("not an object")));
        assert_eq!(out["env"]["ANTHROPIC_MODEL"], json!("minimax-m2.7"));
    }

    #[test]
    fn settings_json_is_newline_terminated() {
        let text = render_settings_json(&session(), None);
        assert!(text.ends_with("}\n"), "{text}");
    }

    fn claude_json_mcp() -> Value {
        json!({ "scout": { "command": "scout" }, "tavily-mcp": { "command": "npx" } })
    }

    #[test]
    fn merges_user_scope_mcp_servers_from_claude_json() {
        let out = merge_mcp_servers(Some(real_settings()), Some(claude_json_mcp()))
            .expect("merge yields settings");
        for name in ["context7", "scout", "tavily-mcp"] {
            assert!(
                out["mcpServers"].get(name).is_some(),
                "{name} missing: {out}"
            );
        }
    }

    #[test]
    fn settings_json_wins_on_mcp_name_conflict() {
        let extra = json!({ "context7": { "command": "SHADOWED" } });
        let out =
            merge_mcp_servers(Some(real_settings()), Some(extra)).expect("merge yields settings");
        assert_eq!(out["mcpServers"]["context7"]["command"], json!("npx"));
    }

    #[test]
    fn merge_carries_extra_servers_when_settings_absent() {
        let out = merge_mcp_servers(None, Some(claude_json_mcp()))
            .expect("extra servers alone still produce settings");
        assert_eq!(out["mcpServers"]["scout"]["command"], json!("scout"));
    }

    #[test]
    fn merge_without_extra_leaves_settings_untouched() {
        assert_eq!(
            merge_mcp_servers(Some(real_settings()), None),
            Some(real_settings())
        );
    }

    fn servers(names: &[(&str, &str)]) -> Map<String, Value> {
        names
            .iter()
            .map(|(name, cmd)| ((*name).to_owned(), json!({ "command": cmd })))
            .collect()
    }

    #[test]
    fn injects_mcp_servers_into_empty_claude_json() {
        let out = inject_mcp_servers(None, &servers(&[("scout", "scout")]));
        assert_eq!(out["mcpServers"]["scout"]["command"], json!("scout"));
    }

    #[test]
    fn injection_preserves_claudes_own_keys() {
        let existing = json!({ "projects": { "p": 1 }, "oauthAccount": "x" });
        let out = inject_mcp_servers(Some(existing), &servers(&[("scout", "scout")]));
        assert_eq!(out["projects"], json!({ "p": 1 }));
        assert_eq!(out["oauthAccount"], json!("x"));
        assert_eq!(out["mcpServers"]["scout"]["command"], json!("scout"));
    }

    #[test]
    fn injection_merges_with_existing_mcp_servers() {
        let existing = json!({ "mcpServers": { "keep": { "command": "keep" } } });
        let out = inject_mcp_servers(Some(existing), &servers(&[("scout", "scout")]));
        assert_eq!(out["mcpServers"]["keep"]["command"], json!("keep"));
        assert_eq!(out["mcpServers"]["scout"]["command"], json!("scout"));
    }

    #[test]
    fn injection_lets_carried_server_win_on_conflict() {
        let existing = json!({ "mcpServers": { "scout": { "command": "STALE" } } });
        let out = inject_mcp_servers(Some(existing), &servers(&[("scout", "scout")]));
        assert_eq!(out["mcpServers"]["scout"]["command"], json!("scout"));
    }

    #[test]
    fn mcp_servers_of_extracts_the_block() {
        let out = mcp_servers_of(Some(&real_settings()));
        assert!(out.contains_key("context7"), "context7 missing: {out:?}");
    }

    #[test]
    fn mcp_servers_of_absent_yields_empty() {
        assert!(mcp_servers_of(None).is_empty());
        assert!(mcp_servers_of(Some(&json!("not an object"))).is_empty());
    }
}
