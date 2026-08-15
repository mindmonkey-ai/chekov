//! Pull-spec grammar: `org/repo[:QUANT][@rev]`, plus full HF URLs normalized
//! to `org/repo` (§4.1 of the bootstrap prompt). Newtypes per §C.4.

use crate::error::ChekovError;

const HF_URL_PREFIX: &str = "https://huggingface.co/";

/// A validated `org/repo` Hugging Face repository id (§C.4 newtype).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoId(String);

impl RepoId {
    fn parse(raw: &str, spec: &str) -> Result<Self, ChekovError> {
        let invalid = || ChekovError::InvalidPullSpec {
            spec: spec.to_owned(),
        };
        let (org, name) = raw.split_once('/').ok_or_else(invalid)?;
        let valid_part = |p: &str| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        };
        if valid_part(org) && valid_part(name) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(invalid())
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Repo tail after the org: `unsloth/X-GGUF` → `X-GGUF`.
    fn name_part(&self) -> &str {
        self.0
            .split_once('/')
            .map_or(self.0.as_str(), |(_, name)| name)
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A parsed pull spec: repo, optional quant tag, optional revision pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullSpec {
    pub repo: RepoId,
    pub quant: Option<String>,
    pub revision: Option<String>,
}

impl PullSpec {
    /// Parse `org/repo[:QUANT][@rev]` or `https://huggingface.co/org/repo`.
    pub fn parse(input: &str) -> Result<Self, ChekovError> {
        let invalid = || ChekovError::InvalidPullSpec {
            spec: input.to_owned(),
        };
        let body = input
            .strip_prefix(HF_URL_PREFIX)
            .map_or(input, |rest| rest.trim_end_matches('/'));
        let (body, revision) = match body.split_once('@') {
            Some((_, "")) => return Err(invalid()),
            Some((head, rev)) => (head, Some(rev.to_owned())),
            None => (body, None),
        };
        let (body, quant) = match body.split_once(':') {
            Some((_, "")) => return Err(invalid()),
            Some((head, tag)) => (head, Some(tag.to_owned())),
            None => (body, None),
        };
        Ok(Self {
            repo: RepoId::parse(body, input)?,
            quant,
            revision,
        })
    }

    /// Derived short name: repo tail, `-GGUF` suffix stripped, lowercased.
    #[must_use]
    pub fn short_name(&self) -> String {
        let name = self.repo.name_part();
        let name = name
            .strip_suffix("-GGUF")
            .or_else(|| name.strip_suffix("-gguf"))
            .unwrap_or(name);
        name.to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::PullSpec;

    #[test]
    fn parses_bare_repo() {
        let spec = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF").expect("valid spec");
        assert_eq!(spec.repo.as_str(), "unsloth/MiniMax-M2.7-GGUF");
        assert_eq!(spec.quant, None);
        assert_eq!(spec.revision, None);
    }

    #[test]
    fn parses_quant_tag() {
        let spec = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL").expect("valid spec");
        assert_eq!(spec.quant.as_deref(), Some("UD-Q5_K_XL"));
    }

    #[test]
    fn parses_quant_and_revision() {
        let spec = PullSpec::parse("org/repo:UD-Q4_K_XL@abc123def456").expect("valid spec");
        assert_eq!(spec.quant.as_deref(), Some("UD-Q4_K_XL"));
        assert_eq!(spec.revision.as_deref(), Some("abc123def456"));
    }

    #[test]
    fn parses_revision_without_quant() {
        let spec = PullSpec::parse("org/repo@abc123").expect("valid spec");
        assert_eq!(spec.quant, None);
        assert_eq!(spec.revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn normalizes_hf_url() {
        let spec = PullSpec::parse("https://huggingface.co/unsloth/MiniMax-M2.7-GGUF")
            .expect("valid URL form");
        assert_eq!(spec.repo.as_str(), "unsloth/MiniMax-M2.7-GGUF");
    }

    #[test]
    fn rejects_malformed_specs() {
        for bad in [
            "norepo",
            "a/b/c",
            "org/repo:",
            "org/repo@",
            "/repo",
            "org/",
            "",
        ] {
            let msg = PullSpec::parse(bad).expect_err("should reject").to_string();
            assert!(
                msg.contains("org/repo"),
                "no accepted-forms hint for {bad:?}: {msg}"
            );
        }
    }

    #[test]
    fn short_name_strips_gguf_and_lowercases() {
        let spec = PullSpec::parse("unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL").expect("valid spec");
        assert_eq!(spec.short_name(), "minimax-m2.7");
    }
}
