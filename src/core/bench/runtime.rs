//! A declared foreign runtime (spec 2026-08-31 §2, §4): parsed from
//! `--runtime <name>@<version>`, stored on the stamp as `<name> <version>`,
//! and made ready by listing `/v1/models` — chekov never launches, installs,
//! or probes a foreign server's identity; it prints what was declared and
//! what is served, and measures.

use serde_json::Value;

use crate::core::hub::HttpClient;
use crate::error::ChekovError;

/// One declared runtime. `@` is a CLI spelling; `stored` is the stamp's.
pub struct RuntimeSpec {
    pub name: String,
    pub version: String,
}

impl RuntimeSpec {
    /// Split on the LAST `@`; name `[a-z0-9][a-z0-9._-]*`, version non-empty
    /// with no whitespace. Every refusal names the value and the reason.
    pub fn parse(value: &str) -> Result<Self, ChekovError> {
        let refuse = |reason: &str| ChekovError::RuntimeFlagInvalid {
            value: value.to_owned(),
            reason: reason.to_owned(),
        };
        let (name, version) = value.rsplit_once('@').ok_or_else(|| refuse("missing '@'"))?;
        if name.is_empty() {
            return Err(refuse("empty name"));
        }
        if !name_ok(name) {
            return Err(refuse("name must be lowercase [a-z0-9._-]"));
        }
        if version.is_empty() {
            return Err(refuse("empty version"));
        }
        if version.chars().any(char::is_whitespace) {
            return Err(refuse("version contains whitespace"));
        }
        Ok(Self {
            name: name.to_owned(),
            version: version.to_owned(),
        })
    }

    /// The stamp's spelling: `<name> <version>`.
    #[must_use]
    pub fn stored(&self) -> String {
        format!("{} {}", self.name, self.version)
    }
}

fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    let first_fits = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_fits
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-_".contains(c))
}

/// Foreign readiness (spec §4): one plain `GET /v1/models`; a 200 with a
/// `data` array is ready and its `id`s are returned FOR PRINTING — chekov
/// cannot know how a foreign server names the weights, so it reports and
/// lets the human read. Anything else is `EndpointDown`.
pub fn foreign_ready(http: &dyn HttpClient, base_url: &str) -> Result<Vec<String>, ChekovError> {
    let url = format!("{base_url}/v1/models");
    let body = http.get(&url)?;
    let parsed: Value = serde_json::from_str(&body).map_err(|_| ChekovError::EndpointDown {
        url: url.clone(),
        reason: "/v1/models did not return JSON".to_owned(),
    })?;
    let data = parsed
        .get("data")
        .and_then(Value::as_array)
        .ok_or(ChekovError::EndpointDown {
            url,
            reason: "/v1/models reply has no `data` array".to_owned(),
        })?;
    Ok(data
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::RuntimeSpec;
    use crate::core::hub::{HttpClient, JsonRequest};
    use crate::error::ChekovError;

    #[test]
    fn a_runtime_spec_parses_name_at_version_and_stores_with_a_space() {
        let spec = RuntimeSpec::parse("mtplx@0.4.1").unwrap();
        assert_eq!(spec.name, "mtplx");
        assert_eq!(spec.version, "0.4.1");
        assert_eq!(spec.stored(), "mtplx 0.4.1");
    }

    #[test]
    fn the_last_at_sign_splits_so_an_at_in_the_name_is_refused() {
        let err = RuntimeSpec::parse("mlx-lm@v0.2@rc1").unwrap_err();
        assert!(err.to_string().contains("lowercase"));
    }

    #[test]
    fn each_malformed_spelling_is_refused_with_its_reason() {
        for (value, needle) in [
            ("mtplx", "missing '@'"),
            ("@0.4.1", "empty name"),
            ("MTPLX@1", "lowercase"),
            ("m tplx@1", "lowercase"),
            ("mtplx@", "empty version"),
            ("mtplx@0 4", "whitespace"),
        ] {
            let err = RuntimeSpec::parse(value).unwrap_err();
            let text = err.to_string();
            assert!(
                text.contains(needle) && text.contains(value),
                "{value}: {text}"
            );
        }
    }

    struct CannedModels(&'static str);
    impl HttpClient for CannedModels {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            Ok(self.0.to_owned())
        }
        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("readiness never POSTs")
        }
    }

    #[test]
    fn foreign_readiness_lists_the_served_ids() {
        let http = CannedModels(r#"{"object":"list","data":[{"id":"a"},{"id":"b"}]}"#);
        let ids = super::foreign_ready(&http, "http://h:1").unwrap();
        assert_eq!(ids, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn an_empty_list_is_ready_and_a_shapeless_reply_is_not() {
        let empty = CannedModels(r#"{"data":[]}"#);
        assert_eq!(super::foreign_ready(&empty, "http://h:1").unwrap(), Vec::<String>::new());
        let shapeless = CannedModels("not json");
        let err = super::foreign_ready(&shapeless, "http://h:1").unwrap_err();
        assert!(matches!(err, ChekovError::EndpointDown { .. }));
    }
}
