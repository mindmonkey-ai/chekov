//! The `tool_loop` probe's canned environment and driver.
//!
//! Tool-loop design §4–§5: a repository as a map, every tool answer a pure
//! function of the case and the calls so far, and a loop that stops at a
//! terminal state.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::core::bench::grade::ToolUse;
use crate::core::bench::probeset::{Goal, LoopCase, ToolDef, canned_text};
use crate::core::bench::store::LoopEnd;

/// The canned repository for one case, and the goal it is judged against.
///
/// No clock, no randomness, no filesystem: two environments fed the same
/// calls hold the same state, which is what makes the probe deterministic
/// for a deterministic model.
pub struct ToolEnv<'a> {
    case: &'a LoopCase,
    files: BTreeMap<String, String>,
}

impl<'a> ToolEnv<'a> {
    #[must_use]
    pub fn new(case: &'a LoopCase) -> Self {
        let files = case
            .files
            .iter()
            .map(|f| (f.path.clone(), f.text.clone()))
            .collect();
        Self { case, files }
    }

    /// Answer one call, or end the loop: a tool outside the palette, or a
    /// call missing a key the tool's schema requires, is not answered.
    pub fn answer(&mut self, call: &ToolUse) -> Result<String, LoopEnd> {
        let tool = self
            .case
            .tools
            .iter()
            .find(|t| t.name == call.name)
            .ok_or_else(|| LoopEnd::FabricatedTool {
                name: call.name.clone(),
            })?;
        if let Some(key) = missing_key(tool, &call.input) {
            return Err(LoopEnd::MalformedCall {
                name: call.name.clone(),
                key,
            });
        }
        Ok(match call.name.as_str() {
            "read_file" => self.read_file(arg(&call.input, "path")),
            "list_dir" => self.list_dir(arg(&call.input, "path")),
            "grep" => self.grep(arg(&call.input, "pattern"), arg(&call.input, "path")),
            "edit_file" => self.edit_file(&call.input),
            "run_tests" => self.run_tests(),
            other => format!("tool '{other}' is offered but has no canned behaviour"),
        })
    }

    fn read_file(&self, path: &str) -> String {
        self.files
            .get(path)
            .cloned()
            .unwrap_or_else(|| format!("no such file: {path}"))
    }

    /// Direct children only; a subdirectory is named with a trailing slash.
    fn list_dir(&self, path: &str) -> String {
        let prefix = dir_prefix(path);
        let entries: BTreeSet<String> = self
            .files
            .keys()
            .filter_map(|p| p.strip_prefix(&prefix))
            .map(|rest| match rest.split_once('/') {
                Some((dir, _)) => format!("{dir}/"),
                None => rest.to_owned(),
            })
            .collect();
        if entries.is_empty() {
            return format!("no such directory: {path}");
        }
        entries.into_iter().collect::<Vec<_>>().join("\n")
    }

    /// Plain substring, never a regex: no prompt asks for one, and a model
    /// that sends `.` means a dot.
    fn grep(&self, pattern: &str, path: &str) -> String {
        let prefix = dir_prefix(path);
        let hits: Vec<String> = self
            .files
            .iter()
            .filter(|(p, _)| p.starts_with(&prefix) || p.as_str() == path)
            .flat_map(|(p, text)| {
                text.lines()
                    .enumerate()
                    .filter(|(_, line)| line.contains(pattern))
                    .map(move |(i, line)| format!("{p}:{}: {line}", i + 1))
            })
            .collect();
        if hits.is_empty() {
            "no matches".to_owned()
        } else {
            hits.join("\n")
        }
    }

    /// Claude Code's own `Edit` contract: exactly one occurrence, or say why.
    fn edit_file(&mut self, input: &Value) -> String {
        let (path, old, new) = (arg(input, "path"), arg(input, "old"), arg(input, "new"));
        let Some(text) = self.files.get(path) else {
            return format!("no such file: {path}");
        };
        let count = text.matches(old).count();
        let edited = text.replacen(old, new, 1);
        match count {
            0 => format!("old text not found in {path}"),
            1 => {
                self.files.insert(path.to_owned(), edited);
                format!("edited {path}")
            }
            n => format!("old text occurs {n} times in {path}; make it unique"),
        }
    }

    /// The canned failure until the goal is met — the same words every time,
    /// never the answer.
    fn run_tests(&self) -> String {
        match &self.case.goal {
            Goal::Edited {
                tests_fail: Some(fail),
                ..
            } if !self.goal_met() => fail.clone(),
            Goal::Edited { .. } | Goal::Unchanged { .. } => "ok. 1 passed".to_owned(),
        }
    }

    fn goal_met(&self) -> bool {
        match &self.case.goal {
            Goal::Edited {
                file,
                contains_any,
                untouched,
                ..
            } => {
                self.files
                    .get(file)
                    .is_some_and(|t| contains_any.iter().any(|w| t.contains(w.as_str())))
                    && untouched.iter().all(|p| self.unchanged(p))
            }
            Goal::Unchanged { .. } => self.case.files.iter().all(|f| self.unchanged(&f.path)),
        }
    }

    fn unchanged(&self, path: &str) -> bool {
        self.files.get(path).map(String::as_str) == canned_text(self.case, path)
    }

    /// The end state when the model stops talking: met, or what was wanted.
    #[must_use]
    pub fn finish(&self, final_text: &str) -> LoopEnd {
        let mentioned = match &self.case.goal {
            Goal::Unchanged { reply_mentions } => final_text
                .to_lowercase()
                .contains(&reply_mentions.to_lowercase()),
            Goal::Edited { .. } => true,
        };
        if self.goal_met() && mentioned {
            LoopEnd::GoalMet
        } else {
            LoopEnd::GoalUnmet {
                wanted: self.wanted(),
            }
        }
    }

    fn wanted(&self) -> String {
        match &self.case.goal {
            Goal::Edited {
                file, contains_any, ..
            } => format!(
                "{file} containing {}",
                contains_any
                    .iter()
                    .map(|w| format!("{w:?}"))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            Goal::Unchanged { reply_mentions } => {
                format!("no file changed and a reply naming {reply_mentions}")
            }
        }
    }
}

fn arg<'v>(input: &'v Value, key: &str) -> &'v str {
    input.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The first `required` key of the tool's schema the call did not supply as
/// a string, if any — read off the schema, never a hard-coded list.
fn missing_key(tool: &ToolDef, input: &Value) -> Option<String> {
    let schema: Value = serde_json::from_str(&tool.input_schema).unwrap_or(Value::Null);
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|key| input.get(key).and_then(Value::as_str).is_none())
        .map(str::to_owned)
}

/// `src` → `src/`; the root (`""`, `.`) → `""`.
fn dir_prefix(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::ToolEnv;
    use crate::core::bench::grade::ToolUse;
    use crate::core::bench::probeset::{LoopCase, ProbeSet, parse};
    use crate::core::bench::store::LoopEnd;

    const TOOLS: &str = r#"
[[tool_loop.tools]]
name = "read_file"
description = "d"
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "list_dir"
description = "d"
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "grep"
description = "d"
input_schema = '{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern","path"]}'
[[tool_loop.tools]]
name = "edit_file"
description = "d"
input_schema = '{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}'
"#;

    /// Two files under `src/`, an edited goal on `src/a.rs`, a test tool.
    pub(super) fn edited_set() -> ProbeSet {
        parse(&format!(
            "version = 0\nloop_system = \"s\"\ntool_emit = []\ninstruction = []\n\
             [[tool_loop]]\nid = \"tl-x\"\nprompt = \"p\"\n\
             [[tool_loop.files]]\npath = \"src/a.rs\"\ntext = \"const A: u32 = 3;\\nconst B: u32 = 3;\\n\"\n\
             [[tool_loop.files]]\npath = \"src/sub/b.rs\"\ntext = \"fn b() {{}}\\n\"\n\
             [tool_loop.goal]\nkind = \"edited\"\nfile = \"src/a.rs\"\n\
             contains_any = [\"const A: u32 = 5;\"]\nuntouched = [\"src/sub/b.rs\"]\n\
             tests_fail = \"test a ... FAILED: expected 5\"\n{TOOLS}\
             [[tool_loop.tools]]\nname = \"run_tests\"\ndescription = \"d\"\n\
             input_schema = '{{\"type\":\"object\",\"properties\":{{\"filter\":{{\"type\":\"string\"}}}},\"required\":[\"filter\"]}}'\n"
        ))
        .expect("valid")
    }

    pub(super) fn unchanged_set() -> ProbeSet {
        parse(&format!(
            "version = 0\nloop_system = \"s\"\ntool_emit = []\ninstruction = []\n\
             [[tool_loop]]\nid = \"tl-u\"\nprompt = \"p\"\n\
             [[tool_loop.files]]\npath = \"src/a.rs\"\ntext = \"const A: u32 = 3;\\n\"\n\
             [tool_loop.goal]\nkind = \"unchanged\"\nreply_mentions = \"src/legacy.rs\"\n{TOOLS}"
        ))
        .expect("valid")
    }

    fn case(set: &ProbeSet) -> &LoopCase {
        &set.tool_loop[0]
    }

    fn call(name: &str, input: Value) -> ToolUse {
        ToolUse {
            id: "t1".into(),
            name: name.into(),
            input,
        }
    }

    fn edit(path: &str, old: &str, new: &str) -> ToolUse {
        call("edit_file", json!({"path": path, "old": old, "new": new}))
    }

    #[test]
    fn read_file_answers_the_text_or_names_the_missing_path() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("read_file", json!({"path": "src/a.rs"})))
                .expect("answered"),
            "const A: u32 = 3;\nconst B: u32 = 3;\n"
        );
        assert_eq!(
            env.answer(&call("read_file", json!({"path": "src/zz.rs"})))
                .expect("answered"),
            "no such file: src/zz.rs"
        );
    }

    #[test]
    fn list_dir_shows_direct_children_and_marks_directories() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("list_dir", json!({"path": "src"})))
                .expect("answered"),
            "a.rs\nsub/"
        );
        assert_eq!(
            env.answer(&call("list_dir", json!({"path": "."})))
                .expect("answered"),
            "src/"
        );
        assert_eq!(
            env.answer(&call("list_dir", json!({"path": "docs"})))
                .expect("answered"),
            "no such directory: docs"
        );
    }

    #[test]
    fn grep_is_a_substring_match_over_a_path_prefix_or_one_file() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("grep", json!({"pattern": ": u32", "path": "src"})))
                .expect("answered"),
            "src/a.rs:1: const A: u32 = 3;\nsrc/a.rs:2: const B: u32 = 3;"
        );
        assert_eq!(
            env.answer(&call(
                "grep",
                json!({"pattern": ".", "path": "src/sub/b.rs"})
            ))
            .expect("answered"),
            "no matches",
            "a dot is a literal dot, not a regex"
        );
    }

    #[test]
    fn edit_file_replaces_exactly_one_occurrence_and_names_zero_or_many() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&edit("src/a.rs", "= 3;\nconst B", "= 5;\nconst B"))
                .expect("answered"),
            "edited src/a.rs"
        );
        assert_eq!(
            env.answer(&call("read_file", json!({"path": "src/a.rs"})))
                .expect("answered"),
            "const A: u32 = 5;\nconst B: u32 = 3;\n"
        );
        assert_eq!(
            env.answer(&edit("src/a.rs", "nope", "x"))
                .expect("answered"),
            "old text not found in src/a.rs"
        );
        assert_eq!(
            env.answer(&edit("src/a.rs", "u32", "u64"))
                .expect("answered"),
            "old text occurs 2 times in src/a.rs; make it unique"
        );
        assert_eq!(
            env.answer(&edit("src/zz.rs", "a", "b")).expect("answered"),
            "no such file: src/zz.rs"
        );
    }

    #[test]
    fn run_tests_fails_with_the_canned_line_until_the_goal_is_met() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        let tests = call("run_tests", json!({"filter": ""}));
        assert_eq!(
            env.answer(&tests).expect("answered"),
            "test a ... FAILED: expected 5"
        );
        env.answer(&edit("src/a.rs", "A: u32 = 3", "A: u32 = 5"))
            .expect("answered");
        assert_eq!(env.answer(&tests).expect("answered"), "ok. 1 passed");
        assert_eq!(env.finish("done"), LoopEnd::GoalMet);
    }

    #[test]
    fn an_untouched_file_that_changed_keeps_the_goal_unmet() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        env.answer(&edit("src/a.rs", "A: u32 = 3", "A: u32 = 5"))
            .expect("answered");
        env.answer(&edit("src/sub/b.rs", "fn b", "fn c"))
            .expect("answered");
        assert_eq!(
            env.finish("done"),
            LoopEnd::GoalUnmet {
                wanted: "src/a.rs containing \"const A: u32 = 5;\"".into()
            }
        );
    }

    #[test]
    fn a_tool_outside_the_palette_or_a_call_missing_a_required_key_ends_the_loop() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("delete_file", json!({"path": "src/a.rs"})))
                .expect_err("fabricated"),
            LoopEnd::FabricatedTool {
                name: "delete_file".into()
            }
        );
        assert_eq!(
            env.answer(&call("edit_file", json!({"path": "src/a.rs", "old": "x"})))
                .expect_err("malformed"),
            LoopEnd::MalformedCall {
                name: "edit_file".into(),
                key: "new".into()
            }
        );
    }

    #[test]
    fn an_unchanged_goal_is_met_only_when_nothing_changed_and_the_reply_names_the_path() {
        let set = unchanged_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("read_file", json!({"path": "src/legacy.rs"})))
                .expect("answered"),
            "no such file: src/legacy.rs"
        );
        assert_eq!(
            env.finish("There is no SRC/LEGACY.RS in this repository."),
            LoopEnd::GoalMet
        );
        assert_eq!(
            env.finish("Fixed it."),
            LoopEnd::GoalUnmet {
                wanted: "no file changed and a reply naming src/legacy.rs".into()
            }
        );
        env.answer(&edit("src/a.rs", "= 3;", "= 4;"))
            .expect("answered");
        assert!(matches!(
            env.finish("no src/legacy.rs here"),
            LoopEnd::GoalUnmet { .. }
        ));
    }

    #[test]
    fn two_environments_fed_the_same_calls_hold_the_same_state() {
        let set = edited_set();
        let (mut one, mut two) = (ToolEnv::new(case(&set)), ToolEnv::new(case(&set)));
        for env in [&mut one, &mut two] {
            env.answer(&edit("src/a.rs", "A: u32 = 3", "A: u32 = 5"))
                .expect("answered");
        }
        let read = call("read_file", json!({"path": "src/a.rs"}));
        assert_eq!(one.answer(&read), two.answer(&read));
        assert_eq!(one.finish(""), two.finish(""));
    }
}
