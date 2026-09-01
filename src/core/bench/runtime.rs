//! A declared foreign runtime (spec 2026-08-31 §2, §4).
//!
//! Parsed from `--runtime <name>@<version>`, stored on the stamp as
//! `<name> <version>`, and made ready by listing `/v1/models` — chekov
//! never launches, installs, or probes a foreign server's identity; it
//! prints what was declared and what is served, and measures.

use serde_json::Value;

use crate::core::hub::HttpClient;
use crate::error::ChekovError;

/// One declared runtime. `@` is a CLI spelling; `stored` is the stamp's.
#[derive(Debug)]
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
        let (name, version) = value
            .rsplit_once('@')
            .ok_or_else(|| refuse("missing '@'"))?;
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

/// Foreign readiness (spec §4).
///
/// One plain `GET /v1/models`; a 200 with a `data` array is ready and its
/// `id`s are returned FOR PRINTING — chekov cannot know how a foreign server
/// names the weights, so it reports and lets the human read. Anything else
/// is `EndpointDown`.
pub fn foreign_ready(http: &dyn HttpClient, base_url: &str) -> Result<Vec<String>, ChekovError> {
    let url = format!("{base_url}/v1/models");
    let body = http.get(&url).map_err(|e| ChekovError::EndpointDown {
        url: url.clone(),
        reason: e.to_string(),
    })?;
    let parsed: Value = serde_json::from_str(&body).map_err(|_| ChekovError::EndpointDown {
        url: url.clone(),
        reason: "/v1/models did not return JSON".to_owned(),
    })?;
    let data = parsed
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ChekovError::EndpointDown {
            url,
            reason: "/v1/models reply has no `data` array".to_owned(),
        })?;
    Ok(data
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect())
}

/// Which served id names the subject on the wire (finding (a) —
/// chekov addresses a foreign server by what it serves, never by its own
/// registry name, which mlx-lm routes on and 404s trying to download).
///
/// A given `--served-model` wins verbatim; absent it, exactly one served id
/// is unambiguous; zero or several is a refusal, never a guess.
pub fn served_model(flag: Option<&str>, ids: &[String]) -> Result<String, ChekovError> {
    if let Some(name) = flag {
        return Ok(name.to_owned());
    }
    match ids {
        [only] => Ok(only.clone()),
        _ => Err(ChekovError::RuntimeServedModelRequired { count: ids.len() }),
    }
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
        assert_eq!(
            super::foreign_ready(&empty, "http://h:1").unwrap(),
            Vec::<String>::new()
        );
        let shapeless = CannedModels("not json");
        let err = super::foreign_ready(&shapeless, "http://h:1").unwrap_err();
        assert!(matches!(err, ChekovError::EndpointDown { .. }));
    }

    /// A transport that never reaches HTTP 200 at all — the server is not
    /// started, or `/v1/models` 404s/401s. `UreqClient::get` reports this as
    /// `HubRequestFailed` (spec I2); readiness must still surface it as
    /// `EndpointDown` naming the URL, per §4/§8.
    struct FailingGet(&'static str);
    impl HttpClient for FailingGet {
        fn get(&self, url: &str) -> Result<String, ChekovError> {
            Err(ChekovError::HubRequestFailed {
                url: url.to_owned(),
                reason: self.0.to_owned(),
            })
        }
        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("readiness never POSTs")
        }
    }

    #[test]
    fn a_transport_failure_is_endpoint_down_not_a_hub_error() {
        let http = FailingGet("connection refused");
        let err = super::foreign_ready(&http, "http://h:1").unwrap_err();
        assert!(
            matches!(&err, ChekovError::EndpointDown { url, reason }
                if url == "http://h:1/v1/models" && reason.contains("connection refused")),
            "{err}"
        );
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn a_given_flag_wins_over_a_single_different_served_id() {
        let served = ids(&["other-id"]);
        assert_eq!(
            super::served_model(Some("wanted-id"), &served).unwrap(),
            "wanted-id"
        );
    }

    #[test]
    fn a_single_served_id_is_auto_selected_without_a_flag() {
        let served = ids(&["only-id"]);
        assert_eq!(super::served_model(None, &served).unwrap(), "only-id");
    }

    #[test]
    fn zero_served_ids_without_a_flag_refuses_naming_the_count() {
        let err = super::served_model(None, &[]).unwrap_err();
        assert!(
            matches!(err, ChekovError::RuntimeServedModelRequired { count: 0 }),
            "{err}"
        );
        assert!(err.to_string().contains("0 model id(s)"), "{err}");
    }

    #[test]
    fn two_served_ids_without_a_flag_refuses_naming_both() {
        let served = ids(&["a", "b"]);
        let err = super::served_model(None, &served).unwrap_err();
        assert!(
            matches!(err, ChekovError::RuntimeServedModelRequired { count: 2 }),
            "{err}"
        );
    }

    #[test]
    fn a_flag_wins_even_over_many_served_ids() {
        let served = ids(&["a", "b", "c"]);
        assert_eq!(
            super::served_model(Some("wanted-id"), &served).unwrap(),
            "wanted-id"
        );
    }
}
