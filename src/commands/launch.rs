//! `chekov launch <agent>` — start an agent against the local model.
//!
//! The proxy runs in a thread of this process and the agent runs as a child,
//! so the translator dies with the session: nothing is left listening after
//! the agent exits.
//!
//! Claude settings reach the agent through a chekov-owned config directory.
//! Codex keeps its own home and receives local-provider CLI overrides.
//! Claude Code writes its settings-file env block over the
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
use crate::core::proxy::codex;
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
    /// Agent to launch: claude or codex.
    #[arg(value_enum)]
    pub agent: AgentKind,
    /// Model to serve; defaults to the active model.
    #[arg(long)]
    pub model: Option<String>,
    /// Run only the protocol translator in the foreground — no agent child,
    /// no generated settings. Prints client configuration instructions.
    #[arg(long)]
    pub proxy_only: bool,
    /// Listen port for `--proxy-only` (ignored in a full launch).
    #[arg(long, default_value_t = DEFAULT_PROXY_PORT)]
    pub port: u16,
    /// Preview the agent command (and Claude config dir) without starting a session.
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
/// is a session thread whose failure must not mask the agent's exit code.
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
        match self.agent {
            AgentKind::Claude => {
                self.write_settings(ctx, &session)?;
                eprint!(
                    "{}",
                    render_summary("claude", &session.eff.name, &session.dir)
                );
            }
            AgentKind::Codex => {
                eprintln!("chekov launch: codex against '{}'", session.eff.name);
                if self.print {
                    let mut args =
                        codex::launch_args(&session.eff.name, session.eff.ctx_size, session.port);
                    args.extend(self.args.iter().cloned());
                    eprintln!("{}", codex::shell_command(&args));
                }
            }
        }
        if self.print {
            eprintln!(
                "chekov launch: preview only; run without --print to start the session proxy"
            );
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
        match self.agent {
            AgentKind::Claude => eprint!(
                "{}",
                render_banner(&Banner {
                    agent: facade.name(),
                    port: self.port,
                    model: &model,
                    upstream: &upstream.base_url,
                })
            ),
            AgentKind::Codex => {
                let eff = reg.effective(&model)?;
                let args = codex::launch_args(&model, eff.ctx_size, self.port);
                eprintln!(
                    "chekov launch --proxy-only: codex on http://{BIND_HOST}:{} -> {} as '{model}'",
                    self.port, upstream.base_url
                );
                eprintln!("{}", codex::shell_command(&args));
            }
        }
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
        if !self.print {
            ensure_server_up(ctx, &eff)?;
        }
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
        // 0700: this directory holds settings.json and .claude.json, both of
        // which carry the server API key.
        std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
            .create(&session.dir)
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
        write_private(&path, &text)?;
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
        write_private(&path, &text)
    }

    /// The binary's exit reclaims this listener after the child finishes.
    fn bridge(&self, ctx: &Ctx, session: &Session) -> Result<ExitCode, ChekovError> {
        let facade = self.agent.facade(&session.eff.name);
        let upstream = Upstream {
            base_url: ctx.config.base_url(),
            api_key: ctx.config.file.server.api_key.clone(),
        };
        let listener = session
            .listener
            .try_clone()
            .map_err(|e| ChekovError::io("cloning the session proxy listener", e))?;
        // A scoped thread would join the endless accept loop and hang on exit.
        std::thread::Builder::new()
            .name("chekov-session-proxy".to_owned())
            .spawn(move || translate_until_exit(&listener, facade.as_ref(), &upstream))
            .map_err(|e| ChekovError::io("starting the session proxy", e))?;
        self.spawn_agent(session)
    }

    fn agent_command(&self, session: &Session) -> std::process::Command {
        let mut command = std::process::Command::new(self.agent.binary());
        match self.agent {
            AgentKind::Claude => {
                command.env(self.agent.config_dir_var(), &session.dir);
            }
            AgentKind::Codex => {
                command.args(codex::launch_args(
                    &session.eff.name,
                    session.eff.ctx_size,
                    session.port,
                ));
            }
        }
        command.args(&self.args);
        command
    }

    fn spawn_agent(&self, session: &Session) -> Result<ExitCode, ChekovError> {
        let binary = self.agent.binary();
        let status =
            self.agent_command(session)
                .status()
                .map_err(|_| ChekovError::AgentBinaryMissing {
                    binary: binary.to_owned(),
                })?;
        let code = status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1);
        Ok(ExitCode::from(code))
    }
}

/// Start the model server when it is not already running.
fn ensure_server_up(ctx: &Ctx, eff: &Effective) -> Result<(), ChekovError> {
    if server::live_pid(&ctx.config).is_some() {
        return verify_serves(ctx, &eff.name);
    }
    // Same four refusal gates `run` applies — launch must not be a back door
    // around them (§C.2). The ServerAlreadyRunning arm is unreachable here:
    // the live_pid check above already returned.
    super::run::preflight(ctx, eff)?;
    eprintln!(
        "chekov launch: local server not running — starting '{}'",
        eff.name
    );
    let pid = server::spawn_daemon(&ctx.config, eff)?;
    server::write_run_state(&ctx.config, &eff.name)?;
    eprintln!("chekov launch: server started (pid {pid})");
    Ok(())
}

/// Write a file that carries the server API key. Created 0600 rather than
/// chmod'd afterwards — a chmod leaves a window in which any local process can
/// read the credential.
fn write_private(path: &Path, text: &str) -> Result<(), ChekovError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))?;
    file.write_all(text.as_bytes())
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))
}

/// A live server must be serving the model this launch is about to advertise.
/// Adopting an unverified upstream would make the agent's declared model and
/// context window a fiction (§C.2 — nothing degrades silently).
fn verify_serves(ctx: &Ctx, requested: &str) -> Result<(), ChekovError> {
    match server::read_run_state(&ctx.config) {
        Some(running) if running == requested => Ok(()),
        Some(running) => Err(ChekovError::ServerModelMismatch {
            running,
            requested: requested.to_owned(),
        }),
        None => Err(ChekovError::ServerModelUnknown),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Banner, ensure_server_up, render_banner, render_summary};
    use crate::commands::Ctx;
    use crate::core::config::Config;
    use crate::core::hub::{HttpClient, JsonRequest};
    use crate::core::registry::{Effective, ModelEntry};
    use crate::error::ChekovError;

    struct NoHttp;

    #[test]
    fn codex_launch_preserves_agent_arguments() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([
            "chekov",
            "launch",
            "codex",
            "--model",
            "test-model",
            "--",
            "exec",
            "hello",
        ])
        .expect("Codex is a supported launch target");
        let crate::cli::Cmd::Launch(cmd) = cli.cmd else {
            panic!("expected launch");
        };
        assert_eq!(cmd.agent.binary(), "codex");
        assert_eq!(cmd.model.as_deref(), Some("test-model"));
        assert_eq!(cmd.args, ["exec", "hello"]);
    }

    fn codex_preview(tag: &str) -> (Ctx, super::LaunchCmd) {
        let ctx = scratch_ctx(tag);
        let mut entry = any_effective().entry;
        entry.ctx_size = Some(524_288);
        let registry = crate::core::registry::Registry {
            active: Some("test-model".to_owned()),
            models: [("test-model".to_owned(), entry)].into(),
            ..Default::default()
        };
        registry
            .save(&ctx.config.registry_path())
            .expect("registry");
        let cmd = super::LaunchCmd {
            agent: crate::core::proxy::AgentKind::Codex,
            model: None,
            proxy_only: false,
            port: 8787,
            print: true,
            args: vec!["exec".to_owned(), "hello".to_owned()],
        };
        (ctx, cmd)
    }

    #[test]
    fn codex_preview_writes_metadata_for_the_selected_model() {
        use crate::commands::Command;
        let (ctx, cmd) = codex_preview("chekov-test-codex-catalog");
        cmd.run(&ctx).expect("preview");
        let files: Vec<_> = std::fs::read_dir(ctx.config.agent_dir("codex"))
            .expect("generated catalog directory")
            .map(|entry| entry.expect("catalog entry").path())
            .collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(&files[0]).expect("catalog");
        let catalog: serde_json::Value = serde_json::from_str(&text).expect("catalog JSON");
        assert_eq!(catalog["models"].as_array().expect("models").len(), 1);
        let model = &catalog["models"][0];
        assert_eq!(model["slug"], "test-model");
        assert_eq!(model["context_window"], 524_288);
        assert_eq!(model["max_context_window"], 524_288);
        assert_eq!(model["input_modalities"], serde_json::json!(["text"]));
        assert_eq!(model["supports_reasoning_summary_parameter"], false);
        assert!(
            !model["base_instructions"]
                .as_str()
                .expect("instructions")
                .is_empty()
        );
    }

    #[test]
    fn codex_command_loads_catalog_without_replacing_user_home() {
        let (ctx, cmd) = codex_preview("chekov-test-codex-catalog-command");
        let session = cmd.resolve(&ctx).expect("session");
        let command = cmd.agent_command(&session);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().expect("UTF-8"))
            .collect();
        assert_eq!(&args[args.len() - 2..], ["exec", "hello"]);
        let settings = args[..args.len() - 2]
            .chunks_exact(2)
            .map(|pair| pair[1])
            .collect::<Vec<_>>()
            .join("\n");
        let config: toml::Table = toml::from_str(&settings).expect("TOML overrides");
        let path = config.get("model_catalog_json").expect("catalog override");
        assert_eq!(
            Path::new(path.as_str().expect("catalog path")).parent(),
            Some(session.dir.as_path())
        );
        assert!(command.get_envs().all(|(key, _)| key != "CODEX_HOME"));
    }

    #[test]
    fn codex_catalog_write_failure_aborts_the_launch() {
        use crate::commands::Command;
        let (ctx, cmd) = codex_preview("chekov-test-codex-catalog-error");
        std::fs::create_dir_all(ctx.config.root.join("agents")).expect("agents directory");
        std::fs::write(ctx.config.agent_dir("codex"), "obstruction").expect("obstruction");
        assert!(
            cmd.run(&ctx).is_err(),
            "missing metadata must not silently fall back"
        );
    }

    impl HttpClient for NoHttp {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            unreachable!("launch preflight never fetches")
        }
        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("launch preflight never posts")
        }
    }

    fn scratch_ctx(tag: &str) -> Ctx {
        let root = std::env::temp_dir().join(tag);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        Ctx {
            config: Config::load(&root).expect("defaults"),
            http: Box::new(NoHttp),
        }
    }

    fn any_effective() -> Effective {
        Effective {
            name: "test-model".to_owned(),
            ctx_size: 4096,
            flags: Vec::new(),
            entry: ModelEntry {
                repo: "org/repo-GGUF".to_owned(),
                quant: "Q4_K_M".to_owned(),
                revision: "0123456789ab".to_owned(),
                path: "models/test-model@0123456789ab".to_owned(),
                first_shard: "Q4_K_M/test-model-Q4_K_M.gguf".to_owned(),
                hermes_ok: false,
                ctx_size: None,
                extra_flags: Vec::new(),
                role: None,
            },
        }
    }

    /// Put a live server in front of `ensure_server_up`: a pidfile naming this
    /// test process (so `process_alive` reports true) and a run-state marker.
    fn with_running_server(ctx: &Ctx, running_model: Option<&str>) {
        let logs = ctx.config.logs_dir();
        std::fs::create_dir_all(&logs).expect("logs dir");
        std::fs::write(ctx.config.pidfile(), format!("{}\n", std::process::id())).expect("pidfile");
        if let Some(name) = running_model {
            std::fs::write(logs.join("chekov.model"), format!("{name}\n")).expect("run state");
        }
    }

    #[test]
    fn files_carrying_the_api_key_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join("chekov-test-launch-perms");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        let path = root.join("settings.json");
        super::write_private(&path, "{\"ANTHROPIC_AUTH_TOKEN\":\"secret\"}\n").expect("write");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "settings.json carries the server api key and is written into a \
             shared temp-ish tree; any local process could read it at {mode:o}"
        );
    }

    #[test]
    fn adopting_a_server_serving_a_different_model_is_refused() {
        let ctx = scratch_ctx("chekov-test-launch-mismatch");
        with_running_server(&ctx, Some("other-model"));
        let err = ensure_server_up(&ctx, &any_effective())
            .expect_err("launch must not advertise a model the server is not serving");
        let msg = err.to_string();
        assert!(
            msg.contains("other-model"),
            "must name what is running: {msg}"
        );
        assert!(
            msg.contains("test-model"),
            "must name what was requested: {msg}"
        );
        assert!(
            msg.contains("chekov restart"),
            "every refusal names its remediation command: {msg}"
        );
    }

    #[test]
    fn adopting_a_server_of_unknown_identity_is_refused() {
        let ctx = scratch_ctx("chekov-test-launch-unknown");
        with_running_server(&ctx, None);
        let err = ensure_server_up(&ctx, &any_effective())
            .expect_err("an upstream chekov cannot identify must not be adopted silently");
        let msg = err.to_string();
        assert!(
            msg.contains("no record") || msg.contains("cannot be verified"),
            "must state that the running model is unknown: {msg}"
        );
        assert!(
            msg.contains("chekov restart") || msg.contains("chekov stop"),
            "every refusal names its remediation command: {msg}"
        );
    }

    #[test]
    fn ensure_server_up_refuses_with_a_named_error_when_the_engine_is_not_built() {
        let ctx = scratch_ctx("chekov-test-launch-preflight");
        let err = ensure_server_up(&ctx, &any_effective())
            .expect_err("launch must refuse when llama-server is missing");
        assert!(
            matches!(err, ChekovError::SetupIncomplete { .. }),
            "launch must fail the same refusal gate `run` does, got: {err:?}"
        );
    }

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
