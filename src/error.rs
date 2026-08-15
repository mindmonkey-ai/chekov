//! Typed failure classes (§C.2/§C.3). Every variant's Display message must
//! state what failed AND the exact remediation command — enforced by tests.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ChekovError {
    #[error("unimplemented")]
    InvalidPullSpec { spec: String },

    #[error("unimplemented")]
    NoQuantSpecified { repo: String, available: String },

    #[error("unimplemented")]
    QuantNotFound { quant: String, repo: String, available: String },

    #[error("unimplemented")]
    UnknownModel { name: String },

    #[error("unimplemented")]
    RegistryCorrupt { path: PathBuf, reason: String },

    #[error("unimplemented")]
    ConfigInvalid { path: PathBuf, reason: String },

    #[error("unimplemented")]
    MissingShard { name: String, path: PathBuf },

    #[error("unimplemented")]
    PortOccupied { port: u16 },

    #[error("unimplemented")]
    WiredLimitLow { actual_mb: u64, required_mb: u64 },

    #[error("unimplemented")]
    ServerAlreadyRunning { pid: i32 },

    #[error("unimplemented")]
    ServerNotRunning,

    #[error("unimplemented")]
    CtxBelowHermesFloor { name: String, ctx: u32, floor: u32 },

    #[error("unimplemented")]
    EndpointDown { url: String, reason: String },

    #[error("unimplemented")]
    DegenerateOutput { reason: String },

    #[error("unimplemented")]
    LicenseChanged { name: String, old: PathBuf, new: PathBuf },

    #[error("unimplemented")]
    RemovalRefused { name: String, reason: String },

    #[error("unimplemented")]
    ConfirmationDeclined { action: String },

    #[error("unimplemented")]
    HermesConfigUnsafe { reason: String },

    #[error("unimplemented")]
    HubRequestFailed { url: String, reason: String },

    #[error("unimplemented")]
    DownloadFailed { repo: String, reason: String },

    #[error("unimplemented")]
    EngineStepFailed { step: String, reason: String },

    #[error("unimplemented")]
    SetupIncomplete { remaining: String },

    #[error("unimplemented")]
    UpdateFlagsMissing,

    #[error("unimplemented")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
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
        let err = ChekovError::WiredLimitLow { actual_mb: 100, required_mb: 200_000 };
        let msg = err.to_string();
        assert!(msg.contains("sudo sysctl iogpu.wired_limit_mb=200000"), "no sudo cmd in: {msg}");
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
        assert!(msg.contains("65536") && msg.contains("4096"), "limits missing in: {msg}");
    }
}
