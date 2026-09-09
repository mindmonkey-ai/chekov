//! The `tool_loop` probe's canned environment and driver (tool-loop design
//! §4–§5): a repository as a map, every tool answer a pure function of the
//! case and the calls so far, and a loop that stops at a terminal state.

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
        env.answer(&edit("src/a.rs", "3", "4")).expect("answered");
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
