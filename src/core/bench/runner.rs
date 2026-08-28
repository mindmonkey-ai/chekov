//! Server readiness and the `/props` assertion.
//!
//! Readiness is `/health` AND the pid: a server that dies while loading must
//! fail as "died" (go read the log), never as a timeout (keep waiting).

use std::time::Duration;

use serde_json::Value;

use crate::core::config::BenchSection;
use crate::core::hub::HttpClient;
use crate::error::ChekovError;

/// What readiness watches: the health endpoint and the process behind it.
pub struct ReadyTarget {
    pub base_url: String,
    pub pid: i32,
}

/// Poll budget, from `[bench]` config.
#[derive(Debug, Clone, Copy)]
pub struct ReadyPolicy {
    pub max_polls: u32,
    pub interval: Duration,
}

impl From<&BenchSection> for ReadyPolicy {
    fn from(bench: &BenchSection) -> Self {
        Self {
            max_polls: bench.ready_max_polls,
            interval: Duration::from_millis(bench.ready_interval_ms),
        }
    }
}

/// Wait until `/health` answers, watching the pid between polls.
///
/// `/health` is public (no api key), so the plain seam `get` suffices;
/// 503-while-loading surfaces as `Err` and the poll continues.
pub fn wait_ready(
    http: &dyn HttpClient,
    target: &ReadyTarget,
    policy: ReadyPolicy,
) -> Result<(), ChekovError> {
    let url = format!("{}/health", target.base_url);
    for _ in 0..policy.max_polls {
        if !crate::core::server::process_alive(target.pid) {
            return Err(ChekovError::ServerDiedWhileLoading { pid: target.pid });
        }
        if http.get(&url).is_ok() {
            return Ok(());
        }
        std::thread::sleep(policy.interval);
    }
    Err(ChekovError::EndpointDown {
        url,
        reason: format!("not ready after {} polls", policy.max_polls),
    })
}

/// How the `/props` body is fetched — `serve::get_bearer` in production
/// (the endpoint sits behind `--api-key`), a canned closure in tests.
pub type PropsFetch<'a> = dyn Fn() -> Result<String, ChekovError> + 'a;

/// The context the server ACTUALLY loaded, asserted against the config's
/// intent — a bench under the wrong context would be recorded under a config
/// the server is not running.
pub fn assert_props_ctx(fetch: &PropsFetch, expected: u32) -> Result<u32, ChekovError> {
    let raw = fetch()?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| ChekovError::EndpointDown {
        url: "/props".to_owned(),
        reason: format!("/props is not JSON: {e}"),
    })?;
    let n_ctx = parsed
        .pointer("/default_generation_settings/n_ctx")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| ChekovError::EndpointDown {
            url: "/props".to_owned(),
            reason: "no default_generation_settings.n_ctx in /props".to_owned(),
        })?;
    if n_ctx == expected {
        Ok(n_ctx)
    } else {
        Err(ChekovError::PropsCtxMismatch {
            server: n_ctx,
            config: expected,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::{ReadyPolicy, ReadyTarget, assert_props_ctx, wait_ready};
    use crate::core::hub::{HttpClient, JsonRequest};
    use crate::error::ChekovError;

    /// /health that answers 503-as-error `failures_left` times, then 200.
    struct FlakyHealth {
        failures_left: RefCell<u32>,
    }

    impl HttpClient for FlakyHealth {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            let mut left = self.failures_left.borrow_mut();
            if *left == 0 {
                return Ok(r#"{"status":"ok"}"#.into());
            }
            *left -= 1;
            Err(ChekovError::EndpointDown {
                url: "fake".into(),
                reason: "503 while loading".into(),
            })
        }

        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("readiness never POSTs")
        }
    }

    fn own_pid() -> i32 {
        i32::try_from(std::process::id()).expect("pid fits")
    }

    fn instant_policy(max_polls: u32) -> ReadyPolicy {
        ReadyPolicy {
            max_polls,
            interval: Duration::ZERO,
        }
    }

    #[test]
    fn readiness_waits_through_loading_then_succeeds() {
        let http = FlakyHealth {
            failures_left: RefCell::new(2),
        };
        let target = ReadyTarget {
            base_url: "http://fake".into(),
            pid: own_pid(),
        };
        wait_ready(&http, &target, instant_policy(5)).expect("ready on the third poll");
    }

    #[test]
    fn a_dead_pid_fails_as_died_not_as_a_timeout() {
        // The server exiting during load must be reported as its own failure —
        // a timeout message would send the user waiting instead of to the log.
        let http = FlakyHealth {
            failures_left: RefCell::new(u32::MAX),
        };
        let target = ReadyTarget {
            base_url: "http://fake".into(),
            pid: 99_999_999,
        };
        let err = wait_ready(&http, &target, instant_policy(5)).expect_err("died");
        assert!(matches!(
            err,
            ChekovError::ServerDiedWhileLoading { pid: 99_999_999 }
        ));
    }

    #[test]
    fn readiness_gives_up_after_the_poll_budget() {
        let http = FlakyHealth {
            failures_left: RefCell::new(u32::MAX),
        };
        let target = ReadyTarget {
            base_url: "http://fake".into(),
            pid: own_pid(),
        };
        let err = wait_ready(&http, &target, instant_policy(3)).expect_err("budget spent");
        assert!(matches!(err, ChekovError::EndpointDown { .. }));
    }

    fn props(n_ctx: u64) -> String {
        serde_json::json!({"default_generation_settings": {"n_ctx": n_ctx}}).to_string()
    }

    #[test]
    fn a_matching_props_ctx_passes() {
        let body = props(131_072);
        let got = assert_props_ctx(&|| Ok(body.clone()), 131_072).expect("matches");
        assert_eq!(got, 131_072);
    }

    #[test]
    fn a_mismatched_props_ctx_is_refused_naming_both_numbers() {
        // The server loaded something other than what the registry intended —
        // benching it would attribute the numbers to a config that is not running.
        let body = props(65_536);
        let err = assert_props_ctx(&|| Ok(body.clone()), 131_072).expect_err("mismatch");
        assert!(matches!(
            err,
            ChekovError::PropsCtxMismatch {
                server: 65_536,
                config: 131_072
            }
        ));
    }

    #[test]
    fn props_without_n_ctx_is_loud_rather_than_assumed() {
        let err = assert_props_ctx(&|| Ok(r#"{"total_slots": 4}"#.into()), 131_072)
            .expect_err("no n_ctx");
        assert!(err.to_string().contains("n_ctx"), "{err}");
    }
}
