//! The deterministic scoring ladder, tiers 1–5 (spec §8).
//!
//! Cheapest to strongest, every tier reported separately, never collapsed.
//! Tiers 6–7 (compile, covering test) are slice B and report `skipped`,
//! never pass.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::masker::balance;
use super::{CodebaseTask, TaskTier};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Exact,
    EditSim,
    IdentF1,
    Parse,
    Symbols,
    Compile,
    Test,
}

impl Tier {
    pub const ALL: [Self; 7] = [
        Self::Exact,
        Self::EditSim,
        Self::IdentF1,
        Self::Parse,
        Self::Symbols,
        Self::Compile,
        Self::Test,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::EditSim => "edit_sim",
            Self::IdentF1 => "ident_f1",
            Self::Parse => "parse",
            Self::Symbols => "symbols",
            Self::Compile => "compile",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Score {
    Value(f64),
    Skipped(&'static str),
}

const EXEC_SKIPPED: &str = "slice B (--allow-exec)";
const BODY_SKIPPED: &str = "function_body: tiers 1-2 punish valid alternatives";

/// Rust keywords — never identifiers.
const KEYWORDS: [&str; 52] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// Names any Rust program may use without declaring: the prelude, common
/// std types and the methods the ladder would otherwise call fabricated.
const PRELUDE: [&str; 60] = [
    "Some",
    "None",
    "Ok",
    "Err",
    "Option",
    "Result",
    "Vec",
    "String",
    "Box",
    "Rc",
    "Arc",
    "str",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "f32",
    "f64",
    "bool",
    "char",
    "format",
    "println",
    "eprintln",
    "vec",
    "iter",
    "into_iter",
    "map",
    "filter",
    "collect",
    "unwrap",
    "unwrap_or",
    "expect",
    "len",
    "is_empty",
    "push",
    "clone",
    "to_owned",
    "to_string",
    "as_str",
    "as_ref",
    "and_then",
    "map_err",
    "ok_or",
    "get",
    "insert",
    "contains",
    "join",
    "trim",
    "lines",
    "chars",
    "new",
    "default",
];

#[derive(Debug, Clone, Default)]
pub struct Symbols(pub BTreeSet<String>);

/// What a prediction's identifiers are checked against.
///
/// Besides the prelude and the gold's own bindings: the repo's
/// declarations and the task file's `use` targets. Bundled to keep
/// `symbols` at 3 parameters.
pub struct Known<'a> {
    pub repo: &'a Symbols,
    pub file_uses: &'a [String],
}

pub struct Scored<'a> {
    pub task: &'a CodebaseTask,
    pub prediction: &'a str,
    pub symbols: &'a Symbols,
}

#[must_use]
pub fn score_all(s: &Scored) -> Vec<(Tier, Score)> {
    let t = s.task;
    let file_uses = file_use_symbols(&format!("{}{}", t.prefix, t.suffix));
    let known = Known {
        repo: s.symbols,
        file_uses: &file_uses,
    };
    let line_level = t.tier == TaskTier::InFile;
    let gated = |v: f64| {
        if line_level {
            Score::Value(v)
        } else {
            Score::Skipped(BODY_SKIPPED)
        }
    };
    vec![
        (Tier::Exact, gated(exact(&t.gold, s.prediction))),
        (Tier::EditSim, gated(edit_sim(&t.gold, s.prediction))),
        (Tier::IdentF1, Score::Value(ident_f1(&t.gold, s.prediction))),
        (
            Tier::Parse,
            Score::Value(parse(&t.prefix, s.prediction, &t.suffix)),
        ),
        (
            Tier::Symbols,
            Score::Value(symbols(s.prediction, &t.gold, &known)),
        ),
        (Tier::Compile, Score::Skipped(EXEC_SKIPPED)),
        (Tier::Test, Score::Skipped(EXEC_SKIPPED)),
    ]
}

fn normalise(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[must_use]
pub fn exact(gold: &str, pred: &str) -> f64 {
    if normalise(gold) == normalise(pred) {
        1.0
    } else {
        0.0
    }
}

/// `1 − lev / max(len)` over whitespace-normalised text; two-row DP.
#[must_use]
pub fn edit_sim(gold: &str, pred: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (
        normalise(gold).chars().collect(),
        normalise(pred).chars().collect(),
    );
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut previous_row: Vec<usize> = (0..=b.len()).collect();
    let mut current_row = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current_row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current_row[j + 1] = (previous_row[j] + cost)
                .min(previous_row[j + 1] + 1)
                .min(current_row[j] + 1);
        }
        std::mem::swap(&mut previous_row, &mut current_row);
    }
    1.0 - as_f64(previous_row[b.len()]) / as_f64(longest)
}

fn as_f64(n: usize) -> f64 {
    u32::try_from(n).map_or(f64::MAX, f64::from)
}

/// `[A-Za-z_][A-Za-z0-9_]*` tokens minus keywords, deduplicated, in order.
#[must_use]
pub fn identifiers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        let ident_char = c.is_ascii_alphanumeric() || c == '_';
        if ident_char {
            cur.push(c);
            continue;
        }
        let word = std::mem::take(&mut cur);
        let starts_ok = word.starts_with(|w: char| w.is_ascii_alphabetic() || w == '_');
        if starts_ok && !KEYWORDS.contains(&word.as_str()) && !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

#[must_use]
pub fn ident_f1(gold: &str, pred: &str) -> f64 {
    let g: BTreeSet<String> = identifiers(gold).into_iter().collect();
    let p: BTreeSet<String> = identifiers(pred).into_iter().collect();
    if g.is_empty() && p.is_empty() {
        return 1.0;
    }
    let overlap = as_f64(g.intersection(&p).count());
    if overlap == 0.0 {
        return 0.0;
    }
    let (precision, recall) = (overlap / as_f64(p.len()), overlap / as_f64(g.len()));
    2.0 * precision * recall / (precision + recall)
}

#[must_use]
pub fn parse(prefix: &str, pred: &str, suffix: &str) -> f64 {
    if balance(&format!("{prefix}{pred}{suffix}")) == Some(0) {
        1.0
    } else {
        0.0
    }
}

/// Fraction of the prediction's identifiers that exist.
///
/// Checked against the repo's declarations, the file's `use` targets, the
/// prelude, and the gold's own bindings. Empty prediction scores 0 — it
/// referenced nothing that exists.
#[must_use]
pub fn symbols(pred: &str, gold: &str, known: &Known) -> f64 {
    let idents = identifiers(pred);
    if idents.is_empty() {
        return 0.0;
    }
    let gold_bindings = identifiers(gold);
    let exists = |id: &String| {
        known.repo.0.contains(id)
            || known.file_uses.contains(id)
            || PRELUDE.contains(&id.as_str())
            || gold_bindings.contains(id)
    };
    as_f64(idents.iter().filter(|id| exists(id)).count()) / as_f64(idents.len())
}

/// Declaration names across the repo.
///
/// `fn`/`struct`/`enum`/`trait`/`type`/`const`/`static`/`mod` names,
/// struct fields (`name: Type,`), enum variants (a capitalised identifier
/// on its own line inside `enum {}`).
#[must_use]
pub fn repo_symbols(files: &[(String, String)]) -> Symbols {
    let mut set = BTreeSet::new();
    for (_, text) in files {
        for line in text.lines() {
            collect_declarations(line, &mut set);
        }
    }
    Symbols(set)
}

fn collect_declarations(line: &str, set: &mut BTreeSet<String>) {
    let words: Vec<&str> = line
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    for (i, w) in words.iter().enumerate() {
        if matches!(
            *w,
            "fn" | "struct" | "enum" | "trait" | "type" | "const" | "static" | "mod"
        ) && let Some(name) = words.get(i + 1)
        {
            set.insert((*name).to_owned());
        }
    }
    collect_members(line.trim(), set);
}

/// A struct field (`name: Type,`) or an enum variant (a capitalised
/// identifier at line start followed by `,`/`(`/`{`/space).
fn collect_members(trimmed: &str, set: &mut BTreeSet<String>) {
    if let Some((name, _)) = trimmed.split_once(':')
        && !name.contains(' ')
        && !name.is_empty()
    {
        set.insert(name.trim_start_matches("pub ").to_owned());
    }
    let variant: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if variant.starts_with(|c: char| c.is_ascii_uppercase())
        && trimmed[variant.len()..].starts_with([',', '(', '{', ' '])
    {
        set.insert(variant);
    }
}

/// The last path segment of each `use` line: `use std::collections::HashMap;`
/// → `HashMap`; `use a::{B, C};` → `B`, `C`.
#[must_use]
pub fn file_use_symbols(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("use "))
        .flat_map(|rest| {
            rest.trim_end_matches(';')
                .split(['{', '}', ','])
                .map(|seg| seg.rsplit("::").next().unwrap_or(seg).trim().to_owned())
                .filter(|s| !s.is_empty() && s != "self" && s != "*")
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        Known, Score, Scored, Symbols, Tier, edit_sim, exact, ident_f1, identifiers, parse,
        repo_symbols, score_all, symbols,
    };
    use crate::core::bench::codebase::{CodebaseTask, Excluded, TaskTier};

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn exact_ignores_whitespace_only_differences() {
        assert!(approx(exact("let x = 1;", "let  x =\n1;"), 1.0));
        assert!(approx(exact("let x = 1;", "let x = 2;"), 0.0));
    }

    #[test]
    fn edit_similarity_is_one_minus_normalised_levenshtein() {
        assert!(approx(edit_sim("abc", "abc"), 1.0));
        assert!((edit_sim("kitten", "sitting") - (1.0 - 3.0 / 7.0)).abs() < 1e-9);
        assert!(approx(edit_sim("", ""), 1.0));
    }

    #[test]
    fn identifier_f1_catches_a_wrong_api() {
        assert!(
            (ident_f1("self.log.apply_entry(e)", "self.log.append_entry(e)") - 2.0 / 3.0).abs()
                < 1e-9
        );
        assert!(approx(ident_f1("fn x() {}", "fn x() {}"), 1.0));
        assert_eq!(
            identifiers("let mut x = foo(y); return"),
            vec!["x", "foo", "y"],
            "keywords are not identifiers"
        );
    }

    #[test]
    fn parse_gate_is_balance_of_the_whole_file() {
        assert!(approx(parse("fn f() {", "let a = [1];", "}"), 1.0));
        assert!(approx(parse("fn f() {", "let a = [1;", "}"), 0.0));
    }

    #[test]
    fn symbols_scores_a_fabricated_identifier_down_and_a_gold_binding_up() {
        let known = repo_symbols(&[(
            "src/a.rs".into(),
            "pub struct Ledger {\n    balance: i64,\n}\npub fn apply_entry(l: &Ledger) {}\nenum E {\n    Credit,\n    Debit,\n}\n".into(),
        )]);
        assert!(
            known.0.contains("apply_entry")
                && known.0.contains("balance")
                && known.0.contains("Credit")
        );
        let uses = vec!["HashMap".to_owned()];
        let known_ctx = Known {
            repo: &known,
            file_uses: &uses,
        };
        assert!(approx(symbols("apply_entry(balance)", "", &known_ctx), 1.0));
        assert!(approx(symbols("frobnicate(l)", "", &known_ctx), 0.0));
        assert!(
            approx(
                symbols("let total = 1; total", "let total = 1;", &known_ctx),
                1.0
            ),
            "gold-introduced binding"
        );
        assert!(
            approx(symbols("HashMap::new()", "", &known_ctx), 1.0),
            "a `use` target exists"
        );
        assert!(approx(symbols("Some(1)", "", &known_ctx), 1.0), "prelude");
    }

    fn task(tier: TaskTier) -> CodebaseTask {
        CodebaseTask {
            id: "t".into(),
            tier,
            file: "src/a.rs".into(),
            line: 1,
            gold: "let a = 1;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "n/a: same-file".into(),
            },
        }
    }

    #[test]
    fn all_seven_tiers_are_reported_and_the_exec_tiers_are_skipped() {
        let known = Symbols(BTreeSet::default());
        let t = task(TaskTier::InFile);
        let scores = score_all(&Scored {
            task: &t,
            prediction: "let a = 1;",
            symbols: &known,
        });
        assert_eq!(scores.len(), 7);
        assert!(matches!(scores[0], (Tier::Exact, Score::Value(v)) if approx(v, 1.0)));
        assert!(matches!(scores[5], (Tier::Compile, Score::Skipped(_))));
        assert!(matches!(scores[6], (Tier::Test, Score::Skipped(_))));
        let body = task(TaskTier::FunctionBody);
        let scores = score_all(&Scored {
            task: &body,
            prediction: "let a = 1;",
            symbols: &known,
        });
        assert!(
            matches!(scores[0], (Tier::Exact, Score::Skipped(_))),
            "tiers 1-2 skip on bodies"
        );
        assert!(matches!(scores[2], (Tier::IdentF1, Score::Value(_))));
    }
}
