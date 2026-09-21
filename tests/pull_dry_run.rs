//! Integration: `chekov pull --dry-run` plans against a fake HF API and
//! writes nothing — no registry, no model dir, no network (§8.2, prompt §2.4).

use std::process::ExitCode;

use chekov::commands::pull::{NewModel, PullCmd};
use chekov::commands::{Command, Ctx};
use chekov::core::config::Config;
use chekov::core::hub::{HttpClient, JsonRequest};
use chekov::core::registry::Registry;
use chekov::error::ChekovError;

struct FakeHub;

const SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";
const SHARD: &str = "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00004.gguf";
const BASE_LICENSE_URL: &str = "https://example.test/base-license";

impl HttpClient for FakeHub {
    fn get(&self, url: &str) -> Result<String, ChekovError> {
        if url == BASE_LICENSE_URL {
            return Ok("base license terms".to_owned());
        }
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
        model_loc: None,
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
        model_loc: None,
    };
    let msg = cmd.run(&ctx).expect_err("no silent default").to_string();
    assert!(msg.contains("UD-Q5_K_XL"), "choices missing: {msg}");
}

#[test]
fn verified_noop_can_snapshot_a_supplied_base_license() {
    let root = std::env::temp_dir().join("chekov-test-pull-noop-license");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch");
    let model = NewModel {
        name: "repair".into(),
        repo: "unsloth/MiniMax-M2.7-GGUF".into(),
        quant: "UD-Q5_K_XL".into(),
        sha: SHA.into(),
        first_shard: SHARD.into(),
        location: None,
    };
    let dir = root.join(model.registry_path());
    std::fs::create_dir_all(dir.join("UD-Q5_K_XL")).expect("model dir");
    std::fs::write(dir.join(SHARD), "weights").expect("shard");
    let mut registry = Registry::default();
    registry.models.insert(model.name.clone(), model.entry());
    registry.save(&root.join("models.toml")).expect("registry");
    let ctx = Ctx {
        config: Config::load(&root).expect("defaults"),
        http: Box::new(FakeHub),
    };
    PullCmd {
        spec: "unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL".into(),
        name: Some(model.name),
        dry_run: false,
        license_url: Some(BASE_LICENSE_URL.into()),
        model_loc: None,
    }
    .run(&ctx)
    .expect("verified no-op succeeds");
    assert_eq!(
        std::fs::read_to_string(dir.join("LICENSE.base.snapshot")).expect("base snapshot"),
        "base license terms"
    );
}
