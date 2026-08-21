//! `chekov launch <agent>` — start an agent against the local model.
//!
//! The proxy runs in a thread of this process and the agent runs as a child,
//! so the translator dies with the session: nothing is left listening after
//! the agent exits.
//!
//! Settings reach the agent through a chekov-owned config directory, not the
//! environment. Claude Code writes its settings-file `env` block over the
//! inherited shell environment at startup, so an env-only launcher is a no-op
//! for anyone who pins `ANTHROPIC_MODEL` in their own settings.
//!
//! `--proxy-only` runs just the protocol translator in the foreground on a
//! fixed `--port` (no child, no generated settings) — for wiring a different
//! client by hand or debugging the translation. A daemonized translator whose
//! upstream has been swapped underneath it is worse than one the user can see,
//! so proxy-only mode is deliberately foreground.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;

use super::{Command, Ctx};
use crate::core::launch::{LocalSession, inject_mcp_servers, mcp_servers_of, render_settings_json};
use crate::core::plugins::sync_local_plugins;
use crate::core::proxy::serve::{Upstream, serve};
use crate::core::proxy::{AgentFacade, AgentKind};
use crate::core::registry::Effective;
use crate::core::server;
use crate::error::ChekovError;

/// Loopback only: the proxy forwards an api-key-bearing request to a local
/// server, so a wider bind would put that credential on the network.
const BIND_HOST: &str = "127.0.0.1";

/// Default listen port for `--proxy-only`. Full launch binds an ephemeral
/// port instead: the child learns it from the generated config dir, so it
/// need not be fixed or known in advance.
const DEFAULT_PROXY_PORT: u16 = 8787;

#[derive(Debug, clap::Args)]
pub struct LaunchCmd {
    /// Agent to launch (currently: claude).
    #[arg(value_enum)]
    pub agent: AgentKind,
    /// Model to serve; defaults to the active model.
    #[arg(long)]
    pub model: Option<String>,
    /// Run only the protocol translator in the foreground — no agent child,
    /// no generated settings. Prints the export lines for a hand-wired client.
    #[arg(long)]
    pub proxy_only: bool,
    /// Listen port for `--proxy-only` (ignored in a full launch).
    #[arg(long, default_value_t = DEFAULT_PROXY_PORT)]
    pub port: u16,
    /// Print the generated config dir and command instead of starting.
    #[arg(long)]
    pub print: bool,
    /// Arguments forwarded verbatim to the agent binary.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/// One resolved launch: which model, on which socket, into which config dir.
/// Bundled so the helpers stay within the 3-argument limit (§3.4).
struct Session {
    eff: Effective,
    listener: TcpListener,
    port: u16,
    dir: PathBuf,
}

/// What a running proxy-only translator is bridging — banner inputs bundled
/// to stay within the 3-argument limit (§3.4).
pub struct Banner<'a> {
    pub agent: &'a str,
    pub port: u16,
    pub model: &'a str,
    pub upstream: &'a str,
}

/// The proxy-only startup banner, on stderr so it never pollutes piped output.
#[must_use]
pub fn render_banner(b: &Banner) -> String {
    let (port, model) = (b.port, b.model);
    format!(
        "chekov launch --proxy-only: {agent} on http://{BIND_HOST}:{port} -> {upstream} as '{model}'\n\
         export ANTHROPIC_BASE_URL='http://{BIND_HOST}:{port}'\n\
         export ANTHROPIC_MODEL='{model}'\n",
        agent = b.agent,
        upstream = b.upstream,
    )
}

/// Run the accept loop, reporting a stop rather than propagating: the caller
/// is a scoped thread whose failure must not mask the agent's exit code.
fn translate_until_exit(listener: &TcpListener, facade: &dyn AgentFacade, upstream: &Upstream) {
    if let Err(e) = serve(listener, facade, upstream) {
        eprintln!("chekov launch: proxy stopped: {e}");
    }
}

/// What the user is told, and what `--print` emits.
#[must_use]
pub fn render_summary(agent: &str, model: &str, dir: &Path) -> String {
    let path = dir.display();
    format!(
        "chekov launch: {agent} against '{model}'\n\
         CLAUDE_CONFIG_DIR='{path}' {agent}\n"
    )
}

impl Command for LaunchCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        if self.proxy_only {
            return self.run_proxy_only(ctx);
        }
        let session = self.resolve(ctx)?;
        self.write_settings(ctx, &session)?;
        eprint!(
            "{}",
            render_summary(self.agent.binary(), &session.eff.name, &session.dir)
        );
        if self.print {
            return Ok(ExitCode::SUCCESS);
        }
        self.bridge(ctx, &session)
    }
}

impl LaunchCmd {
    /// `--proxy-only`: translate on a fixed port in the foreground. No config
    /// dir, no child — the user points their own client at the printed URL.
    fn run_proxy_only(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let reg = ctx.registry()?;
        let model = match &self.model {
            Some(model) => model.clone(),
            None => reg.active_name()?.to_owned(),
        };
        let facade = self.agent.facade(&model);
        let upstream = Upstream {
            base_url: ctx.config.base_url(),
            api_key: ctx.config.file.server.api_key.clone(),
        };
        let listener = TcpListener::bind((BIND_HOST, self.port))
            .map_err(|_| ChekovError::PortOccupied { port: self.port })?;
        eprint!(
            "{}",
            render_banner(&Banner {
                agent: facade.name(),
                port: self.port,
                model: &model,
                upstream: &upstream.base_url,
            })
        );
        if self.print {
            return Ok(ExitCode::SUCCESS);
        }
        serve(&listener, facade.as_ref(), &upstream)?;
        Ok(ExitCode::SUCCESS)
    }

    /// Resolve the model, guarantee a live server, and claim a proxy port.
    fn resolve(&self, ctx: &Ctx) -> Result<Session, ChekovError> {
        let reg = ctx.registry()?;
        let name = match &self.model {
            Some(model) => model.clone(),
            None => reg.active_name()?.to_owned(),
        };
        let eff = reg.effective(&name)?;
        ensure_server_up(ctx, &eff)?;
        let listener = TcpListener::bind((BIND_HOST, 0))
            .map_err(|e| ChekovError::io("binding a proxy port", e))?;
        let port = listener
            .local_addr()
            .map_err(|e| ChekovError::io("reading the proxy port", e))?
            .port();
        Ok(Session {
            dir: ctx.config.agent_dir(self.agent.slug()),
            eff,
            listener,
            port,
        })
    }

    /// Generated settings in a chekov-owned config dir, carrying the user's
    /// MCP servers, hooks, plugins, and permissions forward — and mirroring
    /// local-directory plugins so `enabledPlugins` resolves in the session.
    fn write_settings(&self, ctx: &Ctx, session: &Session) -> Result<(), ChekovError> {
        std::fs::create_dir_all(&session.dir)
            .map_err(|e| ChekovError::io(format!("creating {}", session.dir.display()), e))?;
        let source = self.agent.read_user_settings();
        let text = render_settings_json(
            &LocalSession {
                model: &session.eff.name,
                ctx_size: session.eff.ctx_size,
                proxy_port: session.port,
                api_key: &ctx.config.file.server.api_key,
            },
            source.as_ref(),
        );
        let path = session.dir.join("settings.json");
        std::fs::write(&path, text)
            .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))?;
        Self::inject_claude_json(session, source.as_ref())?;
        sync_local_plugins(&session.dir, source.as_ref())
    }

    /// Write the carried-forward MCP servers into the config dir's
    /// `.claude.json`, the file Claude Code actually reads them from under
    /// `CLAUDE_CONFIG_DIR` (`settings.json` is ignored for MCP definitions).
    ///
    /// Claude owns this file and rewrites it on launch, so this is a
    /// read-modify-write that preserves its state and only sets `mcpServers`,
    /// run before the child is spawned so the servers exist at startup.
    fn inject_claude_json(session: &Session, source: Option<&Value>) -> Result<(), ChekovError> {
        let servers = mcp_servers_of(source);
        if servers.is_empty() {
            return Ok(());
        }
        let path = session.dir.join(".claude.json");
        let existing = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        let injected = inject_mcp_servers(existing, &servers);
        let mut text = serde_json::to_string_pretty(&injected)
            .map_err(|e| ChekovError::io(format!("serializing {}", path.display()), e.into()))?;
        text.push('\n');
        std::fs::write(&path, text)
            .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))
    }

    /// Serve the proxy in a scoped thread while the agent runs as a child.
    fn bridge(&self, ctx: &Ctx, session: &Session) -> Result<ExitCode, ChekovError> {
        let facade = self.agent.facade(&session.eff.name);
        let upstream = Upstream {
            base_url: ctx.config.base_url(),
            api_key: ctx.config.file.server.api_key.clone(),
        };
        std::thread::scope(|scope| {
            // Fire-and-forget by construction: the accept loop never returns,
            // and the session ends when the agent child does. Process exit
            // reclaims the thread and closes the listener.
            scope.spawn(|| translate_until_exit(&session.listener, facade.as_ref(), &upstream));
            self.spawn_agent(&session.dir)
        })
    }

    fn spawn_agent(&self, dir: &Path) -> Result<ExitCode, ChekovError> {
        let binary = self.agent.binary();
        let status = std::process::Command::new(binary)
            .args(&self.args)
            .env(self.agent.config_dir_var(), dir)
            .status()
            .map_err(|_| ChekovError::AgentBinaryMissing {
                binary: binary.to_owned(),
            })?;
        Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }
}

/// Start the model server when it is not already running.
fn ensure_server_up(ctx: &Ctx, eff: &Effective) -> Result<(), ChekovError> {
    if server::live_pid(&ctx.config).is_some() {
        return Ok(());
    }
    eprintln!(
        "chekov launch: local server not running — starting '{}'",
        eff.name
    );
    let pid = server::spawn_daemon(&ctx.config, eff)?;
    server::write_run_state(&ctx.config, &eff.name)?;
    eprintln!("chekov launch: server started (pid {pid})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Banner, render_banner, render_summary};

    #[test]
    fn summary_names_the_config_dir_mechanism() {
        let out = render_summary("claude", "minimax-m2.7", Path::new("/r/agents/claude"));
        assert!(
            out.contains("CLAUDE_CONFIG_DIR='/r/agents/claude'"),
            "{out}"
        );
        assert!(out.contains("minimax-m2.7"), "{out}");
    }

    #[test]
    fn proxy_banner_gives_both_exports_and_the_upstream() {
        let out = render_banner(&Banner {
            agent: "claude",
            port: 8787,
            model: "minimax-m2.7",
            upstream: "http://127.0.0.1:8080",
        });
        assert!(out.contains("http://127.0.0.1:8787"), "{out}");
        assert!(out.contains("http://127.0.0.1:8080"), "{out}");
        assert!(out.contains("ANTHROPIC_BASE_URL"), "{out}");
        assert!(out.contains("ANTHROPIC_MODEL='minimax-m2.7'"), "{out}");
    }
}
