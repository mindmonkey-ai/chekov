//! Grading reads the ANTHROPIC artifact — the same bytes the agent would
//! parse. A body the translator could not produce fails the probe; it never
//! grades as an empty (merely unhelpful) reply.

use serde_json::Value;

use super::fixture::FixtureProbe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grade {
    Pass,
    Fail { reason: String },
}

#[must_use]
pub fn grade(anthropic_body: &str, probe: &FixtureProbe) -> Grade {
    let Ok(parsed) = serde_json::from_str::<Value>(anthropic_body) else {
        return Grade::Fail {
            reason: "artifact is not JSON".to_owned(),
        };
    };
    let Some(text) = parsed.pointer("/content/0/text").and_then(Value::as_str) else {
        return Grade::Fail {
            reason: "no content[0].text in the artifact — a translation failure, \
                     not an empty reply"
                .to_owned(),
        };
    };
    if text.trim().is_empty() {
        return Grade::Fail {
            reason: "empty reply".to_owned(),
        };
    }
    let lower = text.to_lowercase();
    for expected in &probe.expect_contains {
        if !lower.contains(&expected.to_lowercase()) {
            return Grade::Fail {
                reason: format!("missing expected substring {expected:?}"),
            };
        }
    }
    Grade::Pass
}

#[cfg(test)]
mod tests {
    use super::{Grade, grade};
    use crate::core::bench::fixture::FixtureProbe;

    fn probe(expect: &[&str]) -> FixtureProbe {
        FixtureProbe {
            id: "p".into(),
            prompt: "q".into(),
            max_tokens: 32,
            expect_contains: expect.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn anthropic(text: &str) -> String {
        serde_json::json!({
            "type": "message",
            "content": [{"type": "text", "text": text}],
        })
        .to_string()
    }

    #[test]
    fn a_reply_containing_every_expected_substring_passes_case_insensitively() {
        assert!(matches!(
            grade(&anthropic("Hello, World"), &probe(&["hello", "world"])),
            Grade::Pass
        ));
    }

    #[test]
    fn a_missing_substring_fails_naming_it() {
        match grade(&anthropic("hi"), &probe(&["hello"])) {
            Grade::Fail { reason } => assert!(reason.contains("hello"), "{reason}"),
            Grade::Pass => panic!("must fail"),
        }
    }

    #[test]
    fn an_empty_reply_fails_rather_than_passing_an_expectation_free_probe() {
        assert!(matches!(
            grade(&anthropic("  "), &probe(&[])),
            Grade::Fail { .. }
        ));
    }

    #[test]
    fn a_body_without_content_text_fails_as_a_translation_problem_not_an_empty_reply() {
        // A grader that reads a broken artifact as "the model said nothing"
        // would score a broken server as a merely unhelpful model.
        let broken = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
        match grade(broken, &probe(&[])) {
            Grade::Fail { reason } => assert!(reason.contains("content"), "{reason}"),
            Grade::Pass => panic!("must fail"),
        }
    }
}
