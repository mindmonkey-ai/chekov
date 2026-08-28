//! Per-candidate server lifecycle pieces (spec §7.3) — the pure parts.
//!
//! The orchestration (who launches, who tears down) lives in the bench
//! command; this module holds what can be tested without a process: flag
//! hygiene against the binary's own `--help`, the Metal residency env, the
//! budget-release policy, and the plan-as-data the confirm gate prints.

use std::path::Path;

/// Argv flags the built binary's own `--help` does not mention.
///
/// chekov tracks tip-of-master with no pin, and upstream REMOVES flags behind
/// an `arg_removed()` handler that terminates startup — this catches that
/// before a spawn, not as a cryptic exit in the server log. Value tokens
/// (paths, `q8_0`, numbers) are never flagged: only `-`-prefixed tokens are.
#[must_use]
pub fn unknown_flags(argv: &[String], help_text: &str) -> Vec<String> {
    argv.iter()
        .filter(|token| token.starts_with('-'))
        .filter(|flag| !help_text.contains(flag.as_str()))
        .cloned()
        .collect()
}

/// The binary's own `--help`, for `unknown_flags`. `None` when it cannot be
/// captured — the caller states that loudly rather than trusting or refusing
/// an unverifiable argv.
#[must_use]
pub fn server_help(engine_dir: &Path) -> Option<String> {
    let binary = crate::core::engine::server_binary(engine_dir);
    let out = std::process::Command::new(binary)
        .arg("--help")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::unknown_flags;

    /// Shape of real `llama-server --help` output: short+long pairs, values.
    const HELP: &str = "\
  -m,    --model FNAME                 model path\n\
  -c,    --ctx-size N                  size of the prompt context\n\
  -np,   --parallel N                  number of parallel sequences\n\
  -fa,   --flash-attn [on|off|auto]    set Flash Attention use\n\
  -ctk,  --cache-type-k TYPE           KV cache data type for K\n\
  -ctv,  --cache-type-v TYPE           KV cache data type for V\n\
         --jinja                       use jinja template for chat\n\
         --reasoning-format FORMAT     controls thought tags\n\
         --host HOST                   ip address to listen on\n\
         --port PORT                   port to listen on\n\
         --api-key KEY                 API key to use for authentication\n\
         --temp N                      temperature\n\
         --top-p N                     top-p sampling\n\
         --top-k N                     top-k sampling\n";

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| (*t).to_owned()).collect()
    }

    #[test]
    fn a_clean_argv_raises_nothing() {
        let args = argv(&[
            "-m",
            "model.gguf",
            "--ctx-size",
            "262144",
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
            "--api-key",
            "k",
            "--jinja",
            "--flash-attn",
            "on",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q8_0",
            "-np",
            "1",
            "--reasoning-format",
            "none",
            "--temp",
            "0.6",
            "--top-p",
            "0.95",
            "--top-k",
            "20",
        ]);
        assert_eq!(unknown_flags(&args, HELP), Vec::<String>::new());
    }

    #[test]
    fn an_upstream_removed_flag_is_caught_before_the_spawn() {
        // --draft-max was REMOVED upstream behind arg_removed(), which
        // terminates startup — the failure this check exists to front-run.
        let args = argv(&["-m", "model.gguf", "--draft-max", "16"]);
        assert_eq!(unknown_flags(&args, HELP), vec!["--draft-max".to_owned()]);
    }

    #[test]
    fn values_and_paths_are_never_flagged() {
        // q8_0, file paths, numbers — only `-`-prefixed tokens are flags.
        let args = argv(&["--cache-type-k", "q8_0", "-m", "/x/-weird-dir/m.gguf"]);
        assert_eq!(unknown_flags(&args, HELP), Vec::<String>::new());
    }
}
