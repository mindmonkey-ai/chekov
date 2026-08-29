//! Typed failure classes (§C.2/§C.3). Every variant's Display message must
//! state what failed AND the exact remediation command — enforced by tests.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ChekovError {
    #[error(
        "invalid pull spec '{spec}' — accepted forms: org/repo, org/repo:QUANT, \
         org/repo:QUANT@rev, org/repo@rev, or https://huggingface.co/org/repo \
         (e.g. `chekov pull unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL`)"
    )]
    InvalidPullSpec { spec: String },

    #[error(
        "no quant tag given for {repo} and there is no silent default.\n\
         Available tags, {available}\n\n\
         re-run: chekov pull {repo}:<QUANT>"
    )]
    NoQuantSpecified { repo: String, available: String },

    #[error(
        "quant tag '{quant}' not found in {repo}.\n\
         Available tags, {available}\n\n\
         re-run: chekov pull {repo}:<QUANT>"
    )]
    QuantNotFound {
        quant: String,
        repo: String,
        available: String,
    },

    #[error(
        "model '{name}' is not in the registry — run `chekov list` to see \
         registered models or `chekov pull <spec>` to add one"
    )]
    UnknownModel { name: String },

    #[error(
        "no active model is set — pick one with `chekov use <name>` \
         (see `chekov list` for registered models)"
    )]
    NoActiveModel,

    #[error(
        "registry {path} is corrupt ({reason}) — restore it from a backup or \
         delete it and re-register models with `chekov pull <spec>`"
    )]
    RegistryCorrupt { path: PathBuf, reason: String },

    #[error(
        "config {path} is invalid ({reason}) — fix the named key or delete the \
         file to fall back to built-in defaults"
    )]
    ConfigInvalid { path: PathBuf, reason: String },

    #[error(
        "first shard for model '{name}' is missing at {path} — the download is \
         incomplete or was moved; re-run `chekov pull` for this model to restore it"
    )]
    MissingShard { name: String, path: PathBuf },

    #[error(
        "port {port} is already in use by another process — run `chekov status` \
         to see if it is a chekov-managed server, then `chekov stop` it or free \
         the port before retrying"
    )]
    PortOccupied { port: u16 },

    #[error(
        "agent binary '{binary}' was not found on PATH — install it, or run \
         `chekov launch {binary} --print` to get the config directory and start \
         it yourself"
    )]
    AgentBinaryMissing { binary: String },

    #[error(
        "GPU wired limit is {actual_mb} MB but {required_mb} MB is required — \
         run: sudo sysctl iogpu.wired_limit_mb={required_mb} \
         then re-run this command to verify (chekov never executes sudo itself)"
    )]
    WiredLimitLow { actual_mb: u64, required_mb: u64 },

    #[error(
        "a llama-server is already running with '{running}' but this launch would \
         advertise '{requested}' to the agent — every token would come from \
         '{running}' at its context, not '{requested}'; \
         run `chekov restart {requested}` to swap, or re-run without --model \
         to use the running one"
    )]
    ServerModelMismatch { running: String, requested: String },

    #[error(
        "a llama-server is running but chekov has no record of which model it \
         loaded — its identity and context cannot be verified; \
         run `chekov stop` then `chekov run <name>`, or `chekov restart <name>`, \
         so the session is served by a known model"
    )]
    ServerModelUnknown,

    #[error(
        "~/.hermes/config.yaml indents its `providers:` entries with {indent} \
         spaces; chekov's merge only understands 2, and guessing would corrupt \
         a config it is contractually forbidden to clobber — add the `chekov:` \
         provider by hand, or reformat the file to 2-space indentation and retry"
    )]
    HermesShapeUnsupported { indent: usize },

    #[error(
        "this model requires {required_mb} MB of wired GPU memory but this Mac has \
         only {ram_mb} MB of RAM — no sysctl can satisfy that; \
         lower `[limits] wired_limit_mb` in {config_path} to match a model this \
         machine can hold, or pull a smaller quant"
    )]
    WiredLimitUnreachable {
        required_mb: u64,
        ram_mb: u64,
        config_path: std::path::PathBuf,
    },

    #[error(
        "a chekov-managed llama-server is already running (pid {pid}) — \
         `chekov stop` it or use `chekov restart [name]` to swap in one motion"
    )]
    ServerAlreadyRunning { pid: i32 },

    #[error("no chekov-managed server is running — start one with `chekov run`")]
    ServerNotRunning,

    #[error(
        "model '{name}' has effective ctx {ctx} but hermes_ok requires at least \
         {floor} — raise ctx_size in models.toml (or set hermes_ok = false) and \
         re-run `chekov integrate hermes`"
    )]
    CtxBelowHermesFloor { name: String, ctx: u32, floor: u32 },

    #[error(
        "endpoint {url} is not answering ({reason}) — check `chekov status` and \
         the log tail it prints, or restart with `chekov restart`"
    )]
    EndpointDown { url: String, reason: String },

    #[error(
        "the server at {url} answered HTTP {status} instead of a result ({reason}) — \
         it is up and reachable; the request is what to fix, not the server. \
         `chekov show` prints the flags and template in effect, and \
         logs/llama-server.log has the server's own words"
    )]
    UpstreamRefused {
        url: String,
        status: u16,
        reason: String,
    },

    #[error(
        "llama-server (pid {pid}) exited while chekov waited for it to become \
         ready — read the tail of logs/llama-server.log"
    )]
    ServerDiedWhileLoading { pid: i32 },

    #[error(
        "the server loaded n_ctx {server} but the effective config says {config} — \
         a bench against the wrong context would be recorded under a config the \
         server is not running; `chekov restart` and re-run"
    )]
    PropsCtxMismatch { server: u32, config: u32 },

    #[error(
        "the upstream response carries no timings object — chekov never invents \
         a measurement; rebuild the engine (`chekov update --engine`) and retry"
    )]
    BenchNoTimings,

    #[error("bench fixture {}: {reason}", path.display())]
    FixtureInvalid {
        path: std::path::PathBuf,
        reason: String,
    },

    #[error("bench run {}: {reason}", path.display())]
    BenchRunInvalid {
        path: std::path::PathBuf,
        reason: String,
    },

    #[error(
        "bench stamp mismatch on '{field}' ({a} vs {b}) — llama.cpp does not \
         guarantee bit-identical results across configurations (GPU reduction \
         kernels pick different accumulation orders and float addition is not \
         associative), so determinism holds only inside one pinned \
         configuration; re-bench under a matching stamp and compare those runs"
    )]
    BenchStampMismatch { field: String, a: String, b: String },

    #[error(
        "the server is running '{running}' but bench was asked for '{resolved}' \
         — bench never stops a server it did not start, and never records one \
         model's numbers under another's name; `chekov stop` first, or bench \
         just the running model"
    )]
    BenchWrongModel { running: String, resolved: String },

    #[error(
        "--resume names one run id, which pins one stamp and one model — \
         re-run with a single candidate (drop --models or name exactly the \
         resumed model)"
    )]
    BenchResumeNeedsOneCandidate,

    #[error(
        "the compiled-in probe set is invalid ({reason}) — this is a chekov \
         build defect; report it rather than working around it"
    )]
    BenchProbeSetInvalid { reason: String },

    #[error(
        "llama-server's own --help does not list '{flag}' — a routine \
         `chekov update --engine` may have removed it upstream (removed flags \
         terminate startup); fix `extra_flags`/defaults in models.toml and re-run"
    )]
    BenchFlagUnknown { flag: String },

    #[error(
        "Metal has not released the previous model's memory ({free_mib} MiB free, \
         want {want_mib}) — wait a few seconds and re-run, or check for other \
         GPU processes"
    )]
    BenchBudgetNotReleased { free_mib: u64, want_mib: u64 },

    #[error(
        "degenerate output detected: {reason} — this matches the known GGUF \
         corruption class; re-pull the model shards with `chekov pull` and re-run \
         `chekov doctor`"
    )]
    DegenerateOutput { reason: String },

    #[error(
        "license text for '{name}' changed between revisions (old: {old}, new: \
         {new}) — review the diff above and re-run `chekov update --model` to \
         confirm explicitly; chekov never repoints past a license change silently"
    )]
    LicenseChanged {
        name: String,
        old: PathBuf,
        new: PathBuf,
    },

    #[error(
        "refusing to remove model '{name}': {reason} — switch away with \
         `chekov use <other>` (and `chekov stop` if running) first"
    )]
    RemovalRefused { name: String, reason: String },

    #[error("{action} was not confirmed — re-run and answer 'y' to proceed")]
    ConfirmationDeclined { action: String },

    #[error(
        "'{action}' needs an interactive terminal by design — chekov never \
         pre-approves a change like this, and stdin here is not a tty so there \
         is no answer to read; run it from a terminal"
    )]
    ConfirmationRequiresTerminal { action: String },

    #[error(
        "refusing to write hermes config: {reason} — re-run \
         `chekov integrate hermes` and confirm explicitly once resolved"
    )]
    HermesConfigUnsafe { reason: String },

    #[error(
        "request to {url} failed ({reason}) — check network access to \
         huggingface.co and retry `chekov pull`"
    )]
    HubRequestFailed { url: String, reason: String },

    #[error(
        "download from {repo} failed ({reason}) — re-run `chekov pull` to resume; \
         completed shards are kept"
    )]
    DownloadFailed { repo: String, reason: String },

    #[error(
        "engine step '{step}' failed ({reason}) — re-run `chekov setup` after \
         fixing the cause; the build is resumable"
    )]
    EngineStepFailed { step: String, reason: String },

    #[error(
        "setup is incomplete: {remaining} — finish the printed steps and re-run \
         `chekov setup` to verify"
    )]
    SetupIncomplete { remaining: String },

    #[error(
        "`chekov update` needs an explicit target — pass --engine, --model, or \
         --all (add --dry-run to preview)"
    )]
    UpdateFlagsMissing,

    #[error(
        "proxy rejected a malformed agent request: {reason} — the agent sent \
         something this facade does not understand; re-run with `chekov launch \
         claude --proxy-only` and check the agent's base URL points at the proxy port"
    )]
    ProxyBadRequest { reason: String },

    #[error(
        "proxy could not reach the local server at {url}: {reason} — run \
         `chekov status` to confirm it is up, then `chekov run`"
    )]
    ProxyUpstreamFailed { url: String, reason: String },

    #[error(
        "the upstream stream ended with an error frame ({reason}) — that turn was \
         never answered, so it is recorded unavailable rather than graded; check \
         `chekov status` and the tail of logs/llama-server.log"
    )]
    BenchStreamFailed { reason: String },

    #[error("{context}: {source} — check the path exists and is writable, then retry")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl ChekovError {
    /// Wrap an IO error with the operation that failed (§C.3 context rule).
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ChekovError;

    #[test]
    fn a_refusal_says_the_server_answered_and_never_prescribes_a_restart() {
        // A 400 is an answer. "not answering … restart" sent a whole
        // diagnosis the wrong way once; the message must carry the status,
        // the server's own words, and a remediation aimed at the REQUEST.
        let msg = ChekovError::UpstreamRefused {
            url: "http://127.0.0.1:8080/v1/chat/completions".into(),
            status: 400,
            reason: "Failed to initialize samplers: std::exception".into(),
        }
        .to_string();
        assert!(msg.contains("400"), "no status in: {msg}");
        assert!(
            msg.contains("Failed to initialize samplers"),
            "no server words in: {msg}"
        );
        assert!(
            !msg.contains("restart"),
            "must not prescribe a restart: {msg}"
        );
        assert!(!msg.contains("not answering"), "it DID answer: {msg}");
        assert!(msg.contains("chekov show"), "no remediation in: {msg}");
        assert!(msg.contains("llama-server.log"), "no log pointer in: {msg}");
    }

    #[test]
    fn missing_shard_names_pull_remediation() {
        let err = ChekovError::MissingShard {
            name: "minimax-m2.7".into(),
            path: "/x/shard.gguf".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("chekov pull"), "no remediation in: {msg}");
        assert!(msg.contains("/x/shard.gguf"), "no path in: {msg}");
    }

    #[test]
    fn an_unreachable_wired_limit_names_the_config_file_not_an_impossible_sysctl() {
        let msg = ChekovError::WiredLimitUnreachable {
            required_mb: 200_000,
            ram_mb: 32_768,
            config_path: "/r/config.toml".into(),
        }
        .to_string();
        assert!(
            !msg.contains("sudo sysctl"),
            "must not hand the user a sudo command the machine can never satisfy: {msg}"
        );
        assert!(msg.contains("/r/config.toml"), "no config path in: {msg}");
        assert!(msg.contains("wired_limit_mb"), "no tunable named in: {msg}");
        assert!(
            msg.contains("32768") || msg.contains("32,768"),
            "no RAM in: {msg}"
        );
    }

    #[test]
    fn port_occupied_points_at_status_and_stop() {
        let msg = ChekovError::PortOccupied { port: 8080 }.to_string();
        assert!(msg.contains("8080"), "no port in: {msg}");
        assert!(msg.contains("chekov status"), "no remediation in: {msg}");
    }

    #[test]
    fn agent_binary_missing_offers_the_print_fallback() {
        let msg = ChekovError::AgentBinaryMissing {
            binary: "claude".to_owned(),
        }
        .to_string();
        assert!(msg.contains("claude"), "no binary in: {msg}");
        assert!(msg.contains("--print"), "no remediation in: {msg}");
    }

    #[test]
    fn wired_limit_prints_exact_sudo_command() {
        let err = ChekovError::WiredLimitLow {
            actual_mb: 100,
            required_mb: 200_000,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("sudo sysctl iogpu.wired_limit_mb=200000"),
            "no sudo cmd in: {msg}"
        );
    }

    #[test]
    fn no_quant_lists_available_choices() {
        let err = ChekovError::NoQuantSpecified {
            repo: "unsloth/X-GGUF".into(),
            available: "UD-Q4_K_XL, UD-Q5_K_XL".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("UD-Q5_K_XL"), "choices missing in: {msg}");
    }

    #[test]
    fn hermes_floor_names_the_gate() {
        let err = ChekovError::CtxBelowHermesFloor {
            name: "m".into(),
            ctx: 4096,
            floor: 65536,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("65536") && msg.contains("4096"),
            "limits missing in: {msg}"
        );
    }
}
