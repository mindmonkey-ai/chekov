//! Integration: `chekov pull --dry-run` plans against a fake HF API and
//! writes nothing — no registry, no model dir, no network (§8.2, prompt §2.4).

use std::process::ExitCode;

use chekov::commands::pull::PullCmd;
use chekov::commands::{Command, Ctx};
use chekov::core::config::Config;
use chekov::core::hub::{HttpClient, JsonRequest};
use chekov::error::ChekovError;

struct FakeHub;

impl HttpClient for FakeHub {
    fn get(&self, url: &str) -> Result<String, ChekovError> {
        assert!(
            url.contains("api/models/unsloth/MiniMax-M2.7-GGUF"),
            "unexpected url: {url}"
        );
        Ok(r#"{
            "sha": "fedcba9876543210fedcba9876543210fedcba98",
            "siblings": [
                {"rfilename": "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00004.gguf"},
                {"rfilename": "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00002-of-00004.gguf"}
            ]
        }"#
        .to_owned())
    }

    fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
        unreachable!("pull never POSTs")
    }
}

#[test]
fn dry_run_plans_without_writing_anything() {
    let root = std::env::temp_dir().join("chekov-test-pull-dry");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch");
    let ctx = Ctx {
        config: Config::load(&root).expect("defaults"),
        http: Box::new(FakeHub),
    };
    let cmd = PullCmd {
        spec: "unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL".into(),
        name: None,
        dry_run: true,
        license_url: None,
    };
    let code = cmd.run(&ctx).expect("dry run succeeds");
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        !root.join("models.toml").exists(),
        "dry run must not register"
    );
    assert!(
        !root.join("models").exists(),
        "dry run must not create dirs"
    );
}

#[test]
fn dry_run_without_quant_errors_with_choices() {
    let root = std::env::temp_dir().join("chekov-test-pull-noquant");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch");
    let ctx = Ctx {
        config: Config::load(&root).expect("defaults"),
        http: Box::new(FakeHub),
    };
    let cmd = PullCmd {
        spec: "unsloth/MiniMax-M2.7-GGUF".into(),
        name: None,
        dry_run: true,
        license_url: None,
    };
    let msg = cmd.run(&ctx).expect_err("no silent default").to_string();
    assert!(msg.contains("UD-Q5_K_XL"), "choices missing: {msg}");
}
