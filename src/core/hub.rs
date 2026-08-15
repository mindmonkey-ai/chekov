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

/// Production client: blocking `ureq` (no async runtime, prompt §2.1).
pub struct UreqClient;

impl HttpClient for UreqClient {
    fn get(&self, url: &str) -> Result<String, ChekovError> {
        let mut response = ureq::get(url)
            .call()
            .map_err(|e| ChekovError::HubRequestFailed {
                url: url.to_owned(),
                reason: e.to_string(),
            })?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|e| ChekovError::HubRequestFailed {
                url: url.to_owned(),
                reason: e.to_string(),
            })
    }

    fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError> {
        let mut builder = ureq::post(&req.url).header("Content-Type", "application/json");
        if let Some(token) = &req.bearer {
            builder = builder.header("Authorization", &format!("Bearer {token}"));
        }
        let mut response =
            builder
                .send(&req.body)
                .map_err(|e| ChekovError::EndpointDown {
                    url: req.url.clone(),
                    reason: e.to_string(),
                })?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|e| ChekovError::EndpointDown {
                url: req.url.clone(),
                reason: e.to_string(),
            })
    }
}

/// HF model-info API response — only the fields chekov reads.
///
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
    let rev = revision.unwrap_or("main");
    let url = format!("https://huggingface.co/api/models/{repo}/revision/{rev}");
    let body = http.get(&url)?;
    serde_json::from_str(&body).map_err(|e| ChekovError::HubRequestFailed {
        url,
        reason: format!("unexpected response shape: {e}"),
    })
}

/// Decide what to download: requires a quant tag (no silent default), errors
/// with the available choices when it is absent or wrong (§4.1).
pub fn plan_pull(
    snapshot: &RepoSnapshot,
    repo: &RepoId,
    quant: Option<&str>,
) -> Result<PullPlan, ChekovError> {
    let available = available_quants(snapshot).join(", ");
    let Some(quant) = quant else {
        return Err(ChekovError::NoQuantSpecified {
            repo: repo.to_string(),
            available,
        });
    };
    let files: Vec<String> = snapshot
        .files
        .iter()
        .map(|f| f.rfilename.as_str())
        .filter(|f| derived_quant(f).as_deref() == Some(quant))
        .map(ToOwned::to_owned)
        .collect();
    if files.is_empty() {
        return Err(ChekovError::QuantNotFound {
            quant: quant.to_owned(),
            repo: repo.to_string(),
            available,
        });
    }
    let first_shard = files
        .iter()
        .find(|f| f.contains("-00001-of-"))
        .unwrap_or(&files[0])
        .clone();
    Ok(PullPlan {
        quant: quant.to_owned(),
        files,
        first_shard,
    })
}

/// Distinct quant tags present in a repo's GGUF files (subdir- and
/// flat-filename styles).
#[must_use]
pub fn available_quants(snapshot: &RepoSnapshot) -> Vec<String> {
    let mut quants: Vec<String> = snapshot
        .files
        .iter()
        .filter_map(|f| derived_quant(&f.rfilename))
        .collect();
    quants.sort();
    quants.dedup();
    quants
}

/// The quant tag a GGUF path belongs to: its directory when subdir-style
/// (`UD-Q5_K_XL/model-....gguf`), else the tag embedded in the filename
/// (`model-Q8_0.gguf`). One source of truth for matching AND listing, so an
/// ambiguous spec like `Q5_K_XL` can never select `UD-Q5_K_XL` files.
fn derived_quant(path: &str) -> Option<String> {
    let stem = path.strip_suffix(".gguf")?;
    if let Some((dir, _)) = path.split_once('/') {
        return quant_like(dir).then(|| dir.to_owned());
    }
    let stem = strip_shard_suffix(stem);
    stem.char_indices()
        .filter(|&(_, c)| c == '-')
        .map(|(i, _)| &stem[i + 1..])
        .find(|cand| quant_like(cand))
        .map(ToOwned::to_owned)
}

/// Heuristic for quant-tag tokens: `UD-Q5_K_XL`, `IQ4_XS`, `Q8_0`, `BF16`…
fn quant_like(token: &str) -> bool {
    let core = token.strip_prefix("UD-").unwrap_or(token);
    let q_digit = |t: &str| {
        t.strip_prefix("IQ")
            .or_else(|| t.strip_prefix('Q'))
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    };
    q_digit(core) || matches!(core, "BF16" | "F16" | "F32")
}

/// Strip a trailing `-00001-of-00004` shard marker if present.
fn strip_shard_suffix(stem: &str) -> &str {
    let Some(idx) = stem.len().checked_sub(15) else {
        return stem;
    };
    let tail = &stem[idx..];
    let shardish = tail.starts_with('-')
        && tail[1..6].bytes().all(|b| b.is_ascii_digit())
        && &tail[6..10] == "-of-"
        && tail[10..].bytes().all(|b| b.is_ascii_digit());
    if shardish { &stem[..idx] } else { stem }
}

#[cfg(test)]
mod tests {
    use super::{
        HttpClient, JsonRequest, RepoSnapshot, available_quants, fetch_snapshot, plan_pull,
    };
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
        let repo = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF")
            .expect("spec")
            .repo;
        let fake = FakeHttp {
            body: API_JSON.into(),
        };
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
        let repo = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF")
            .expect("spec")
            .repo;
        let plan = plan_pull(&snap, &repo, Some("UD-Q5_K_XL")).expect("quant exists");
        assert_eq!(plan.files.len(), 2);
        assert!(
            plan.first_shard.ends_with("00001-of-00004.gguf"),
            "{}",
            plan.first_shard
        );
    }

    #[test]
    fn plan_without_quant_lists_choices() {
        let snap = snapshot();
        let repo = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF")
            .expect("spec")
            .repo;
        let msg = plan_pull(&snap, &repo, None)
            .expect_err("no silent default")
            .to_string();
        assert!(
            msg.contains("UD-Q5_K_XL") && msg.contains("Q8_0"),
            "choices missing: {msg}"
        );
    }

    #[test]
    fn plan_with_wrong_quant_lists_choices() {
        let snap = snapshot();
        let repo = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF")
            .expect("spec")
            .repo;
        let msg = plan_pull(&snap, &repo, Some("Q2_K"))
            .expect_err("unknown quant")
            .to_string();
        assert!(
            msg.contains("Q2_K") && msg.contains("UD-Q4_K_XL"),
            "bad message: {msg}"
        );
    }

    #[test]
    fn quants_cover_subdir_and_flat_styles() {
        let quants = available_quants(&snapshot());
        assert!(quants.contains(&"UD-Q5_K_XL".to_owned()), "{quants:?}");
        assert!(quants.contains(&"UD-Q4_K_XL".to_owned()), "{quants:?}");
        assert!(quants.contains(&"Q8_0".to_owned()), "{quants:?}");
    }
}
