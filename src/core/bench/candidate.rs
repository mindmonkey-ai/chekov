#[cfg(test)]
mod tests {
    use super::Candidate;

    #[test]
    fn a_candidate_names_its_model_and_pid() {
        let candidate = Candidate {
            eff: crate::core::registry::Effective {
                name: "m".into(),
                ctx_size: 4096,
                flags: vec![],
                entry: crate::core::registry::ModelEntry {
                    repo: "o/r".into(),
                    quant: "Q8_0".into(),
                    revision: "abc123def4567890".into(),
                    path: "models/m@abc123def456".into(),
                    first_shard: "m.gguf".into(),
                    hermes_ok: false,
                    ctx_size: None,
                    extra_flags: vec![],
                    role: None,
                },
            },
            pid: 42,
        };
        assert_eq!((candidate.eff.name.as_str(), candidate.pid), ("m", 42));
    }
}
