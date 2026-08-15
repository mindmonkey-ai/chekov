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
        let mut response = builder
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
    /// Byte size from the `?blobs=true` API — the adoption/skip verifier.
    #[serde(default)]
    pub size: Option<u64>,
}

/// One file the plan will fetch, with its expected size when the API knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    pub path: String,
    pub size: Option<u64>,
}

/// What `pull` will do for one spec: which files, which shard loads first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullPlan {
    pub quant: String,
    pub files: Vec<PlanFile>,
    pub first_shard: String,
}

/// Fetch repo metadata (sha + file list) at `revision` (default `main`).
pub fn fetch_snapshot(
    http: &dyn HttpClient,
    repo: &RepoId,
    revision: Option<&str>,
) -> Result<RepoSnapshot, ChekovError> {
    let rev = revision.unwrap_or("main");
    // ?blobs=true adds per-file sizes — required for verified adoption/skip.
    let url = format!("https://huggingface.co/api/models/{repo}/revision/{rev}?blobs=true");
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
    let files: Vec<PlanFile> = snapshot
        .files
        .iter()
        .filter(|f| derived_quant(&f.rfilename).as_deref() == Some(quant))
        .map(|f| PlanFile {
            path: f.rfilename.clone(),
            size: f.size,
        })
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
        .find(|f| f.path.contains("-00001-of-"))
        .unwrap_or(&files[0])
        .path
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

/// Parameters for a revision-pinned shard download.
pub struct DownloadSpec<'a> {
    pub repo: &'a str,
    pub revision: &'a str,
    pub dest: &'a std::path::Path,
    /// `--model-loc` root to adopt pre-downloaded files from (hard links).
    pub adopt_from: Option<&'a std::path::Path>,
}

/// Where a pre-downloaded copy of `rfilename` may live under a `--model-loc`
/// root: hf-cli layout (`<loc>/<repo-tail>/<rfilename>`) first, then flat
/// (`<loc>/<rfilename>`).
#[must_use]
pub fn adoption_candidates(
    model_loc: &std::path::Path,
    repo: &str,
    rfilename: &str,
) -> Vec<std::path::PathBuf> {
    let repo_tail = repo.split_once('/').map_or(repo, |(_, tail)| tail);
    vec![
        model_loc.join(repo_tail).join(rfilename),
        model_loc.join(rfilename),
    ]
}

/// Hard-link `src` to `dest` after verifying its size against `expected`.
///
/// `Ok(true)` = linked; `Ok(false)` = size mismatch, caller must download
/// instead (loudly — a truncated shard is never adopted silently).
pub fn link_verified(
    src: &std::path::Path,
    dest: &std::path::Path,
    expected: Option<u64>,
) -> Result<bool, ChekovError> {
    let meta = std::fs::metadata(src)
        .map_err(|e| ChekovError::io(format!("inspecting {}", src.display()), e))?;
    if expected != Some(meta.len()) {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    // Same-volume hard link is instant and free; copy is the cross-volume
    // fallback. Either way the source stays untouched.
    std::fs::hard_link(src, dest)
        .or_else(|_| std::fs::copy(src, dest).map(|_| ()))
        .map_err(|e| ChekovError::io(format!("linking {}", src.display()), e))?;
    Ok(true)
}

/// Download every planned file into the model directory, revision-pinned.
///
/// Network path — exercised only by real pulls, never by tests (prompt §2.4).
/// hf-hub's blocking wrapper runs its own internal runtime thread; chekov
/// itself stays synchronous.
pub fn download_plan(spec: &DownloadSpec<'_>, plan: &PullPlan) -> Result<(), ChekovError> {
    let failed = |reason: String| ChekovError::DownloadFailed {
        repo: spec.repo.to_owned(),
        reason,
    };
    let (owner, name) = spec
        .repo
        .split_once('/')
        .ok_or_else(|| failed("repo id is missing the org/ prefix".to_owned()))?;
    let client = hf_hub::HFClientSync::new().map_err(|e| failed(e.to_string()))?;
    let repo = client.model(owner, name);
    for file in &plan.files {
        let target = spec.dest.join(&file.path);
        if file_matches(&target, file.size) {
            println!("{} already present (size verified) — skipping", file.path);
            continue;
        }
        if try_adopt(spec, file)? {
            println!(
                "{} adopted from local copy (size verified, hard link)",
                file.path
            );
            continue;
        }
        println!("downloading {} …", file.path);
        repo.download_file()
            .filename(file.path.clone())
            .local_dir(spec.dest.to_path_buf())
            .revision(spec.revision.to_owned())
            .send()
            .map_err(|e| failed(format!("{}: {e}", file.path)))?;
    }
    Ok(())
}

/// True when `path` exists and matches the expected size (unknown size never
/// matches — presence alone is not verification).
fn file_matches(path: &std::path::Path, expected: Option<u64>) -> bool {
    match (std::fs::metadata(path), expected) {
        (Ok(meta), Some(size)) => meta.len() == size,
        _ => false,
    }
}

/// Try each adoption candidate under `adopt_from`; link the first verified
/// one. `Ok(false)` = nothing adoptable, download instead.
fn try_adopt(spec: &DownloadSpec<'_>, file: &PlanFile) -> Result<bool, ChekovError> {
    let Some(loc) = spec.adopt_from else {
        return Ok(false);
    };
    for candidate in adoption_candidates(loc, spec.repo, &file.path) {
        if !candidate.exists() {
            continue;
        }
        if link_verified(&candidate, &spec.dest.join(&file.path), file.size)? {
            return Ok(true);
        }
        eprintln!(
            "warning: {} exists but its size does not match the repo — downloading instead",
            candidate.display()
        );
    }
    Ok(false)
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
            {"rfilename": "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00004.gguf", "size": 8237824},
            {"rfilename": "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00002-of-00004.gguf", "size": 49986025344},
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
        assert_eq!(plan.files[0].size, Some(8_237_824), "sizes must ride along");
        assert!(
            plan.first_shard.ends_with("00001-of-00004.gguf"),
            "{}",
            plan.first_shard
        );
    }

    #[test]
    fn adoption_candidates_prefer_hf_cli_layout() {
        let cands = super::adoption_candidates(
            std::path::Path::new("/Volumes/jane/models"),
            "unsloth/MiniMax-M2.7-GGUF",
            "UD-Q5_K_XL/x.gguf",
        );
        assert_eq!(
            cands[0],
            std::path::PathBuf::from("/Volumes/jane/models/MiniMax-M2.7-GGUF/UD-Q5_K_XL/x.gguf")
        );
        assert_eq!(
            cands[1],
            std::path::PathBuf::from("/Volumes/jane/models/UD-Q5_K_XL/x.gguf")
        );
    }

    #[test]
    fn link_verified_links_matching_and_rejects_mismatch() {
        let dir = std::env::temp_dir().join("chekov-test-adopt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let src = dir.join("src.gguf");
        std::fs::write(&src, vec![7u8; 100]).expect("write");
        let good = dir.join("sub/good.gguf");
        assert!(super::link_verified(&src, &good, Some(100)).expect("link"));
        assert_eq!(std::fs::metadata(&good).expect("linked").len(), 100);
        let bad = dir.join("sub/bad.gguf");
        assert!(!super::link_verified(&src, &bad, Some(999)).expect("mismatch is not an error"));
        assert!(!bad.exists(), "mismatched file must never be adopted");
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
