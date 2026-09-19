//! The compiled-in fixture-v1 tree: every file `build.rs` found under
//! `fixtures/fixture-v1/`, as `(fixture-relative path, text)`, sorted.
//!
//! The hidden tests are in here too — they are excluded from prompts by the
//! manifest and the materializer, never by this table.

include!(concat!(env!("OUT_DIR"), "/fixture_v1_files.rs"));

#[cfg(test)]
mod tests {
    use super::FILES;

    fn has(path: &str) -> bool {
        FILES.iter().any(|(p, _)| *p == path)
    }

    #[test]
    fn the_table_carries_the_manifest_the_crate_and_every_hidden_test() {
        for path in [
            "manifest.toml",
            "Cargo.toml",
            "Cargo.lock",
            "src/lib.rs",
            "src/store/mod.rs",
            "hidden/store_limited_full.rs",
            "hidden/near_miss_api.rs",
            "hidden/invariant_exact.rs",
            "hidden/lifetime_knot.rs",
        ] {
            assert!(has(path), "{path} missing from FILES");
        }
    }

    #[test]
    fn nothing_from_target_or_the_readme_is_embedded_and_paths_are_sorted() {
        for (path, _) in FILES {
            assert!(!path.starts_with("target/"), "{path} is build output");
            assert!(!path.contains('\\'), "{path} must use forward slashes");
            assert_ne!(*path, "README.md");
            assert_ne!(*path, ".gitignore");
            assert!(
                path.strip_suffix(".in").is_none(),
                "{path} keeps its `.in` suffix — build.rs must strip it so the \
                 materialized tree gets the real filename"
            );
        }
        let paths: Vec<&str> = FILES.iter().map(|(p, _)| *p).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "build.rs sorts the table");
    }
}
