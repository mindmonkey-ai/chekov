//! Live candidate discovery against the Hugging Face API.
//!
//! There is deliberately no compiled-in catalog. A vendored list rots visibly
//! — ramalama's `shortnames.conf` still maps `codellama` to repos with no
//! upload since 2024 — and chekov already knows what is registered locally.
//! This layer only runs when the user asks for it with `--refresh`; an
//! offline-first tool that silently reaches for the network on an ordinary
//! invocation is a surprise, and a recommendation that changed because of a
//! background fetch is not reproducible.

use serde::Deserialize;

use crate::core::hub::HttpClient;
use crate::error::ChekovError;

/// One row of the list endpoint, with `expand[]=gguf`.
#[derive(Debug, Clone, Deserialize)]
pub struct Listed {
    pub id: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub gguf: Option<GgufMeta>,
}

/// The `gguf` object the list endpoint returns per repo.
#[derive(Debug, Clone, Deserialize)]
pub struct GgufMeta {
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub chat_template: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
}

/// The discovery query.
///
/// `expand[]=gguf` on the LIST endpoint returns the full chat template for
/// every row, so the tool-parser cascade runs across a whole page with no
/// per-repo follow-up. (`expand[]` is NOT supported on
/// `GET /api/models/{repo}` — chekov's revision-pinned `?blobs=true` call is a
/// different endpoint and stays as it is.)
#[must_use]
pub fn discovery_url(limit: u32) -> String {
    format!(
        "https://huggingface.co/api/models?filter=gguf&filter=text-generation\
         &expand[]=gguf&expand[]=downloads&sort=downloads&direction=-1&limit={limit}"
    )
}

/// Fetch one page of candidates.
///
/// Sorted by downloads only to bound which repos are worth a size lookup —
/// never to rank them. Popularity is not tool-capability: the most-downloaded
/// GGUF text-generation repos include one with a 506-character template and no
/// tool markup at all.
pub fn discover(http: &dyn HttpClient, limit: u32) -> Result<Vec<Listed>, ChekovError> {
    let url = discovery_url(limit);
    let body = http.get(&url)?;
    serde_json::from_str(&body).map_err(|e| ChekovError::HubRequestFailed {
        url,
        reason: format!("unexpected list-endpoint shape: {e}"),
    })
}

/// Repos this tool will never usefully run, filtered before any size lookup so
/// they do not each cost a request.
#[must_use]
pub fn worth_sizing(listed: &Listed) -> bool {
    let id = listed.id.to_ascii_lowercase();
    !id.contains("-draft") && !id.contains("mmproj") && listed.gguf.is_some()
}

#[cfg(test)]
mod tests {
    use super::{Listed, discovery_url, worth_sizing};

    #[test]
    fn the_discovery_query_expands_gguf_on_the_list_endpoint() {
        let u = discovery_url(100);
        assert!(u.contains("expand[]=gguf"), "{u}");
        assert!(u.contains("filter=gguf"), "{u}");
        assert!(u.contains("limit=100"), "{u}");
        assert!(
            !u.contains("/api/models/"),
            "expand[] is unsupported on the per-repo endpoint: {u}"
        );
    }

    #[test]
    fn a_row_without_gguf_metadata_is_not_worth_a_request() {
        let bare: Listed = serde_json::from_str(r#"{"id":"x/y","downloads":5}"#).expect("parse");
        assert!(!worth_sizing(&bare));
    }

    #[test]
    fn draft_repos_are_skipped_before_costing_a_lookup() {
        let d: Listed = serde_json::from_str(r#"{"id":"x/y-draft-GGUF","downloads":5,"gguf":{}}"#)
            .expect("parse");
        assert!(!worth_sizing(&d));
        let ok: Listed =
            serde_json::from_str(r#"{"id":"unsloth/Qwen3.8-27B-GGUF","gguf":{}}"#).expect("parse");
        assert!(worth_sizing(&ok));
    }

    #[test]
    fn the_list_shape_parses_with_fields_absent() {
        // Every expand[] field is optional in practice; a missing one must not
        // fail the whole page.
        let rows: Vec<Listed> = serde_json::from_str(
            r#"[{"id":"a/b"},{"id":"c/d","downloads":9,"gguf":{"architecture":"qwen3moe","chat_template":"x","context_length":262144}}]"#,
        )
        .expect("parse");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1].gguf.as_ref().and_then(|g| g.context_length),
            Some(262_144)
        );
    }
}
