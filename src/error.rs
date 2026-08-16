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
        "GPU wired limit is {actual_mb} MB but {required_mb} MB is required — \
         run: sudo sysctl iogpu.wired_limit_mb={required_mb} \
         then re-run this command to verify (chekov never executes sudo itself)"
    )]
    WiredLimitLow { actual_mb: u64, required_mb: u64 },

    #[error(
        "a chekov-managed llama-server is already running (pid {pid}) — \
         `chekov stop` it or use `chekov restart [name]` to swap in one motion"
    )]
    ServerAlreadyRunning { pid: i32 },

    #[error("no chekov-managed server is running — start one with `chekov run --daemon`")]
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
    fn port_occupied_points_at_status_and_stop() {
        let msg = ChekovError::PortOccupied { port: 8080 }.to_string();
        assert!(msg.contains("8080"), "no port in: {msg}");
        assert!(msg.contains("chekov status"), "no remediation in: {msg}");
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
