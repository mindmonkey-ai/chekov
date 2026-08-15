//! Hugging Face hub access behind a mockable seam (§8.2): all metadata goes
//! through `HttpClient`, so tests inject fakes; only the shard download itself
//! uses `hf-hub` (network, untested by design).

use serde::Deserialize;

use crate::core::pullspec::RepoId;
use crate::error::ChekovError;

/// One JSON POST: url, body, optional bearer token.
#[derive(Debug, Clone)]
pub struct JsonRequest {
    pub url: String,
    pub body: String,
    pub bearer: Option<String>,
}

/// The HTTP boundary (§C.6). Implemented by `UreqClient` for real use and by
/// canned fakes in tests — no test ever touches the network.
pub trait HttpClient {
    fn get(&self, url: &str) -> Result<String, ChekovError>;
    fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError>;
}

/// HF model-info API response — only the fields chekov reads.
/// `deny_unknown_fields` deliberately NOT set here: this is a third-party API
/// whose schema grows fields routinely; §C.7 targets our own config surface.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoSnapshot {
    pub sha: String,
    #[serde(rename = "siblings")]
    pub files: Vec<RepoFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoFile {
    pub rfilename: String,
}

/// What `pull` will do for one spec: which files, which shard loads first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullPlan {
    pub quant: String,
    pub files: Vec<String>,
    pub first_shard: String,
}

/// Fetch repo metadata (sha + file list) at `revision` (default `main`).
pub fn fetch_snapshot(
    http: &dyn HttpClient,
    repo: &RepoId,
    revision: Option<&str>,
) -> Result<RepoSnapshot, ChekovError> {
    let _ = (http, repo, revision);
    todo!("cycle 3 red")
}

/// Decide what to download: requires a quant tag (no silent default), errors
/// with the available choices when it is absent or wrong (§4.1).
pub fn plan_pull(
    snapshot: &RepoSnapshot,
    repo: &RepoId,
    quant: Option<&str>,
) -> Result<PullPlan, ChekovError> {
    let _ = (snapshot, repo, quant);
    todo!("cycle 3 red")
}

/// Distinct quant tags present in a repo's GGUF files (subdir- and
/// flat-filename styles).
#[must_use]
pub fn available_quants(snapshot: &RepoSnapshot) -> Vec<String> {
    let _ = snapshot;
    todo!("cycle 3 red")
}

#[cfg(test)]
mod tests {
    use super::{HttpClient, JsonRequest, RepoSnapshot, available_quants, fetch_snapshot, plan_pull};
    use crate::core::pullspec::PullSpec;
    use crate::error::ChekovError;

    struct FakeHttp {
        body: String,
    }

    impl HttpClient for FakeHttp {
        fn get(&self, url: &str) -> Result<String, ChekovError> {
            assert!(url.contains("api/models"), "unexpected url: {url}");
            Ok(self.body.clone())
        }

        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("hub metadata never POSTs")
        }
    }

    const API_JSON: &str = r#"{
        "sha": "0123456789abcdef0123456789abcdef01234567",
        "modelId": "unsloth/MiniMax-M2.7-GGUF",
        "siblings": [
            {"rfilename": "README.md"},
            {"rfilename": "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00004.gguf"},
            {"rfilename": "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00002-of-00004.gguf"},
            {"rfilename": "UD-Q4_K_XL/MiniMax-M2.7-UD-Q4_K_XL-00001-of-00003.gguf"},
            {"rfilename": "MiniMax-M2.7-Q8_0.gguf"}
        ]
    }"#;

    fn snapshot() -> RepoSnapshot {
        let repo = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF").expect("spec").repo;
        let fake = FakeHttp { body: API_JSON.into() };
        fetch_snapshot(&fake, &repo, None).expect("parse fixture")
    }

    #[test]
    fn fetch_parses_sha_and_files() {
        let snap = snapshot();
        assert!(snap.sha.starts_with("0123456789ab"));
        assert_eq!(snap.files.len(), 5);
    }

    #[test]
    fn plan_selects_quant_files_and_first_shard() {
        let snap = snapshot();
        let repo = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF").expect("spec").repo;
        let plan = plan_pull(&snap, &repo, Some("UD-Q5_K_XL")).expect("quant exists");
        assert_eq!(plan.files.len(), 2);
        assert!(plan.first_shard.ends_with("00001-of-00004.gguf"), "{}", plan.first_shard);
    }

    #[test]
    fn plan_without_quant_lists_choices() {
        let snap = snapshot();
        let repo = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF").expect("spec").repo;
        let msg = plan_pull(&snap, &repo, None).expect_err("no silent default").to_string();
        assert!(msg.contains("UD-Q5_K_XL") && msg.contains("Q8_0"), "choices missing: {msg}");
    }

    #[test]
    fn plan_with_wrong_quant_lists_choices() {
        let snap = snapshot();
        let repo = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF").expect("spec").repo;
        let msg = plan_pull(&snap, &repo, Some("Q2_K")).expect_err("unknown quant").to_string();
        assert!(msg.contains("Q2_K") && msg.contains("UD-Q4_K_XL"), "bad message: {msg}");
    }

    #[test]
    fn quants_cover_subdir_and_flat_styles() {
        let quants = available_quants(&snapshot());
        assert!(quants.contains(&"UD-Q5_K_XL".to_owned()), "{quants:?}");
        assert!(quants.contains(&"UD-Q4_K_XL".to_owned()), "{quants:?}");
        assert!(quants.contains(&"Q8_0".to_owned()), "{quants:?}");
    }
}
