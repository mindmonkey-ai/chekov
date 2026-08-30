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

const EXEC_SKIPPED: &str = "slice B2 (--allow-exec)";
const BODY_SKIPPED: &str = "function_body: tiers 1-2 punish valid alternatives";
const SYMBOLS_AT_RUN_TIME: &str = "symbols: needs the worktree, scored at run time";

/// Rust keywords — never identifiers.
pub(super) const KEYWORDS: [&str; 52] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// Names any Rust program may use without declaring: the prelude, common
/// std types and the methods the ladder would otherwise call fabricated.
pub(super) const PRELUDE: &[&str] = &[
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
    "display",
    "unwrap_or_else",
    "ok_or_else",
    "enumerate",
    "zip",
    "rev",
    "take",
    "skip",
    "find",
    "any",
    "all",
    "sum",
    "count",
    "min",
    "max",
    "sort",
    "sort_by",
    "dedup",
    "extend",
    "retain",
    "drain",
    "split",
    "starts_with",
    "ends_with",
    "strip_prefix",
    "strip_suffix",
    "parse",
    "from",
    "into",
    "try_from",
    "try_into",
    "as_bytes",
    "to_vec",
    "Debug",
    "Display",
    "Default",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "Hash",
    "Path",
    "PathBuf",
    "std",
    "core",
    "self",
    "Self",
    "super",
    "crate",
    "Iterator",
    "IntoIterator",
    "From",
    "Into",
    "AsRef",
    "Fn",
    "FnMut",
    "FnOnce",
    "Send",
    "Sync",
    "Sized",
    "Drop",
    "matches",
    "assert",
    "assert_eq",
    "debug_assert",
    "todo",
    "unreachable",
    "panic",
    "write",
    "writeln",
    "dbg",
    "env",
    "fs",
    "io",
    "fmt",
    "mem",
    "ptr",
    "cmp",
    "ops",
    "collections",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "VecDeque",
    "Cow",
    "Cell",
    "RefCell",
    "Mutex",
    "RwLock",
    "Duration",
    "Instant",
];

#[derive(Debug, Clone, Default)]
pub struct Symbols(pub BTreeSet<String>);

/// What a prediction's identifiers are checked against.
///
/// Besides the prelude and the gold's own bindings: the repo's
/// declarations, the task file's `use` targets, and the task's own context.
/// Bundled to keep `symbols` at 3 parameters.
pub struct Known<'a> {
    pub repo: &'a Symbols,
    pub file_uses: &'a [String],
    /// The prefix and suffix the model was shown. A name already on the page
    /// trivially exists — the probe is about cross-file and API names, not
    /// about the local binding two lines up.
    pub context: &'a str,
}

pub struct Scored<'a> {
    pub task: &'a CodebaseTask,
    pub prediction: &'a str,
    pub symbols: &'a Symbols,
    /// The extra file's text on the with-extra arm, `""` otherwise. The model
    /// was shown that file, so its names exist for it (§6); the without arm
    /// is scored against the page it actually saw.
    pub extra: &'a str,
}

/// The text tiers 1–4 are scored from.
///
/// A live task and a stored row look identical here, which is the point: both
/// go through `stored_tier`, so the run and the re-read can never disagree
/// about what a tier skipped or why.
pub struct StoredText<'a> {
    pub tier: TaskTier,
    pub gold: &'a str,
    pub prediction: &'a str,
    pub prefix: &'a str,
    pub suffix: &'a str,
}

/// One tier over stored text. Tier 5 needs the repo's symbol set, which the
/// worktree took with it, so it is scored at run time and skipped here.
///
/// Tiers 1–4 read the fill trimmed to the gold's line count (§6, amended
/// 2026-08-30). Tier 5 keeps the whole prediction: what it asks is which
/// identifiers the model emitted, and it emitted all of them.
#[must_use]
pub fn stored_tier(tier: Tier, text: &StoredText) -> Score {
    // A cross-file span is a statement, exactly as an `in_file` span is: same
    // mask shape, one right answer expected, so tiers 1-2 mean the same thing
    // there. Only `function_body`, where many different bodies are correct,
    // skips them.
    let line_level = text.tier != TaskTier::FunctionBody;
    let fill = trimmed_to_gold(text.gold, text.prediction);
    match tier {
        Tier::Exact if line_level => Score::Value(exact(text.gold, &fill)),
        Tier::EditSim if line_level => Score::Value(edit_sim(text.gold, &fill)),
        Tier::Exact | Tier::EditSim => Score::Skipped(BODY_SKIPPED),
        Tier::IdentF1 => Score::Value(ident_f1(text.gold, &fill)),
        Tier::Parse => Score::Value(parse(text.prefix, &fill, text.suffix)),
        Tier::Symbols => Score::Skipped(SYMBOLS_AT_RUN_TIME),
        Tier::Compile | Tier::Test => Score::Skipped(EXEC_SKIPPED),
    }
}

/// The first `gold.lines().count()` lines of the prediction, ending the way
/// the gold ends.
///
/// `n_predict` is generous — 36 tokens per gold line, floored at 64 — so a
/// model that answers the span correctly and then keeps writing the rest of
/// the function was being scored on the run-on, not on the answer. The mask
/// asked for the gold's lines; the lines past them belong to the suffix the
/// model was already shown, and grading them measured the token budget.
fn trimmed_to_gold(gold: &str, prediction: &str) -> String {
    let kept: String = prediction
        .split_inclusive('\n')
        .take(gold.lines().count())
        .collect();
    match (gold.ends_with('\n'), kept.ends_with('\n')) {
        (true, false) if !kept.is_empty() => format!("{kept}\n"),
        (false, true) => kept.trim_end_matches('\n').to_owned(),
        _ => kept,
    }
}

#[must_use]
pub fn score_all(s: &Scored) -> Vec<(Tier, Score)> {
    let t = s.task;
    let context = format!("{}{}{}", t.prefix, t.suffix, s.extra);
    let file_uses = file_use_symbols(&context);
    let known = Known {
        repo: s.symbols,
        file_uses: &file_uses,
        context: &context,
    };
    let text = StoredText {
        tier: t.tier,
        gold: &t.gold,
        prediction: s.prediction,
        prefix: &t.prefix,
        suffix: &t.suffix,
    };
    [Tier::Exact, Tier::EditSim, Tier::IdentF1, Tier::Parse]
        .into_iter()
        .map(|tier| (tier, stored_tier(tier, &text)))
        .chain([
            (
                Tier::Symbols,
                Score::Value(symbols(s.prediction, &t.gold, &known)),
            ),
            (Tier::Compile, Score::Skipped(EXEC_SKIPPED)),
            (Tier::Test, Score::Skipped(EXEC_SKIPPED)),
        ])
        .collect()
}

/// Runs of whitespace collapsed to one space, trimmed. `pub(super)` because
/// rule (b) asks whether another file contains the gold's text, and "the
/// same code, differently indented" is the same answer.
pub(super) fn normalise(s: &str) -> String {
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

/// A count as a float, exactly — a count too large for `u32` saturates
/// rather than rounding to a number that was never measured.
pub(crate) fn as_f64(n: usize) -> f64 {
    u32::try_from(n).map_or(f64::MAX, f64::from)
}

/// `[A-Za-z_][A-Za-z0-9_]*` tokens minus keywords, deduplicated, in order.
///
/// Code only: the words inside string and char literals and comments are
/// prose, not identifiers, and counting them made a comment naming an API
/// look like a call to it.
#[must_use]
pub fn identifiers(text: &str) -> Vec<String> {
    let code = code_only(text);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in code.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
            continue;
        }
        let word = std::mem::take(&mut cur);
        let starts_ok = word.starts_with(|w: char| w.is_ascii_alphabetic() || w == '_');
        if starts_ok && !KEYWORDS.contains(&word.as_str()) && seen.insert(word.clone()) {
            out.push(word);
        }
    }
    out
}

/// `text` with every string, char, and comment literal blanked out, so a
/// scanner walks code bytes only.
///
/// Byte lengths and line breaks survive the blanking: the cross-file index
/// reads declarations out of this and then reports the OFFSET of one, so an
/// offset into the result has to be an offset into the original.
pub(super) fn code_only(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for range in super::masker::literal_ranges(text) {
        out.push_str(&text[pos..range.start]);
        blank_into(&text[range.clone()], &mut out);
        pos = range.end;
    }
    out.push_str(&text[pos..]);
    out
}

/// One space per BYTE of the literal, and a newline for a newline — the
/// offsets and the line structure of the original both survive.
fn blank_into(literal: &str, out: &mut String) {
    for c in literal.chars() {
        match c {
            '\n' => out.push('\n'),
            _ => out.extend(std::iter::repeat_n(' ', c.len_utf8())),
        }
    }
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
    let context: BTreeSet<String> = identifiers(known.context).into_iter().collect();
    let exists = |id: &String| {
        known.repo.0.contains(id)
            || known.file_uses.contains(id)
            || PRELUDE.contains(&id.as_str())
            || gold_bindings.contains(id)
            || context.contains(id)
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
    declaration_names(line, set);
    collect_members(line.trim(), set);
}

/// The `fn`/`struct`/`enum`/`trait`/`type`/`const`/`static`/`mod` name on
/// this line, if any.
///
/// Split from `collect_declarations` because the cross-file index wants
/// declarations only: a struct field and an enum variant are not call sites,
/// so indexing them would make every `name:` in the repository look like a
/// definition to cross a file for.
pub(super) fn declaration_names(line: &str, set: &mut BTreeSet<String>) {
    names_after(line, &DECLARATION_KEYWORDS, set);
}

/// The `struct`/`enum` names on this line — the subset of
/// `declaration_names` that a `{` can open a literal for.
///
/// The cross-file index keeps this apart so a `{` after a name in a type
/// position (`-> Widget {`, `impl Widget {`) is told from a struct literal.
pub(super) fn type_declaration_names(line: &str, set: &mut BTreeSet<String>) {
    names_after(line, &["struct", "enum"], set);
}

const DECLARATION_KEYWORDS: [&str; 8] = [
    "fn", "struct", "enum", "trait", "type", "const", "static", "mod",
];

/// The cross-file index's declaration names: the same list without `mod`.
///
/// Amended 2026-08-30 (§3.2). `mod agents;` declares a FILE, not a symbol a
/// span can call: indexing it made every `agents::` path in the repository
/// look like a cross-file first use of a name nothing defines. Tier 5's
/// declaration list keeps `mod` — there the question is only whether a name
/// the model emitted exists.
pub(super) fn cross_file_declaration_names(line: &str, set: &mut BTreeSet<String>) {
    names_after(line, &CROSS_FILE_KEYWORDS, set);
}

const CROSS_FILE_KEYWORDS: [&str; 7] = ["fn", "struct", "enum", "trait", "type", "const", "static"];

/// The word following any of `keywords` on this line.
fn names_after(line: &str, keywords: &[&str], set: &mut BTreeSet<String>) {
    let words: Vec<&str> = line
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    for (i, w) in words.iter().enumerate() {
        if keywords.contains(w)
            && let Some(name) = words.get(i + 1)
        {
            set.insert((*name).to_owned());
        }
    }
}

/// Words a line-shaped scan mistakes for a declaration: `Self::x` and
/// `crate::y` split at their `:` like a field, `Some(v)` and `Ok(v)` read
/// like enum variants. None of them is a name this repo declares.
const NOT_DECLARED: [&str; 8] = [
    "Self", "crate", "self", "super", "Some", "None", "Ok", "Err",
];

/// A struct field (`name: Type,`) or an enum variant (a capitalised
/// identifier at line start followed by `,`/`(`/`{`/space).
fn collect_members(trimmed: &str, set: &mut BTreeSet<String>) {
    if let Some((name, _)) = trimmed.split_once(':') {
        let name = strip_visibility(name).trim();
        if !name.is_empty() && !name.contains(' ') && !NOT_DECLARED.contains(&name) {
            set.insert(name.to_owned());
        }
    }
    let variant: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if variant.starts_with(|c: char| c.is_ascii_uppercase())
        && !NOT_DECLARED.contains(&variant.as_str())
        && trimmed[variant.len()..].starts_with([',', '(', '{', ' '])
    {
        set.insert(variant);
    }
}

/// Strips a leading `pub` / `pub(crate)` / `pub(super)` / … visibility
/// keyword, so a field name split off `pub balance: i64,` isn't rejected
/// for containing a space. Guards against `public_key` false-matching
/// `pub` by requiring the keyword be followed by whitespace or `(`.
pub(super) fn strip_visibility(s: &str) -> &str {
    let s = s.trim_start();
    let Some(after_pub) = s.strip_prefix("pub") else {
        return s;
    };
    if !after_pub.starts_with([' ', '(']) {
        return s;
    }
    let after_paren = after_pub
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
        .map_or(after_pub, |(_, tail)| tail);
    after_paren.trim_start()
}

/// The last token of each `use` line's segments, alias included.
///
/// `use std::collections::HashMap;` → `HashMap`; `use a::{B, C};` → `B`,
/// `C`; `use foo::Bar as Baz;` → `Baz`; `use a::{B as C, D};` → `C`, `D`.
#[must_use]
pub fn file_use_symbols(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("use "))
        .flat_map(|rest| {
            rest.trim_end_matches(';')
                .split(['{', '}', ','])
                .map(use_target_name)
                .filter(|s| !s.is_empty() && s != "self" && s != "*")
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The bound name a single `use` segment introduces: the last `::`
/// segment, then the last whitespace-separated token of that (so an
/// `as` alias wins over the original path name).
fn use_target_name(seg: &str) -> String {
    let after_path = seg.rsplit("::").next().unwrap_or(seg).trim();
    after_path
        .split_whitespace()
        .last()
        .unwrap_or(after_path)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        Known, Score, Scored, Symbols, Tier, edit_sim, exact, file_use_symbols, ident_f1,
        identifiers, parse, repo_symbols, score_all, symbols,
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
            "pub struct Ledger {\n    balance: i64,\n    pub owner: String,\n}\npub fn apply_entry(l: &Ledger) {}\nenum E {\n    Credit,\n    Debit,\n}\n".into(),
        )]);
        assert!(
            known.0.contains("apply_entry")
                && known.0.contains("balance")
                && known.0.contains("owner")
                && known.0.contains("Credit")
        );
        assert!(
            !known.0.contains("Self") && !known.0.contains("Some"),
            "a line-shaped scan must not call the language's own words declarations"
        );
        let uses = vec!["HashMap".to_owned()];
        let known_ctx = Known {
            repo: &known,
            file_uses: &uses,
            context: "",
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

    #[test]
    fn a_name_only_the_extra_file_carries_exists_on_the_with_arm_and_not_without() {
        let t = task(TaskTier::CrossFileFirst, "let a = 1;");
        let scored = |extra| {
            score_all(&Scored {
                task: &t,
                prediction: "let a = build(1);",
                symbols: &Symbols(BTreeSet::new()),
                extra,
            })
            .into_iter()
            .find_map(|(tier, s)| match (tier, s) {
                (Tier::Symbols, Score::Value(v)) => Some(v),
                _ => None,
            })
            .expect("tier 5 has a value")
        };
        assert!(
            scored("pub fn build(n: u32) -> u32 { n }\n") > scored(""),
            "the extra file makes its names exist"
        );
    }

    /// The tier the trim exists for: `n_predict` bought the model 64 tokens
    /// for a one-line span, and it spent them all. The answer is the first
    /// line; the rest is the suffix it was already shown.
    #[test]
    fn a_fill_that_answers_the_gold_and_then_runs_on_is_scored_on_the_answer() {
        let t = task(TaskTier::InFile, "let a = 1;");
        let run_on = "let a = 1;\n    let b = 2;\n    let c = 3;\n}\n";
        let scores = score_all(&Scored {
            task: &t,
            prediction: run_on,
            symbols: &Symbols(BTreeSet::new()),
            extra: "",
        });
        let value = |want| {
            scores
                .iter()
                .find_map(|(tier, s)| match (*tier == want, s) {
                    (true, Score::Value(v)) => Some(*v),
                    _ => None,
                })
                .expect("the tier is reported")
        };
        assert!(approx(value(Tier::Exact), 1.0), "{:?}", value(Tier::Exact));
        assert!(approx(value(Tier::EditSim), 1.0));
        assert!(approx(value(Tier::IdentF1), 1.0));
        assert!(
            approx(value(Tier::Parse), 1.0),
            "the trimmed fill balances where the run-on's extra brace does not"
        );
    }

    /// A fill shorter than the gold is every line the model wrote — the trim
    /// takes nothing away, and the miss stays a miss.
    #[test]
    fn a_fill_shorter_than_the_gold_is_scored_as_it_stands() {
        let t = task(TaskTier::FunctionBody, "let x = 1;\n    x + y\n");
        let scores = score_all(&Scored {
            task: &t,
            prediction: "let x = 2;",
            symbols: &Symbols(BTreeSet::new()),
            extra: "",
        });
        let f1 = scores
            .iter()
            .find_map(|(tier, s)| match (*tier == Tier::IdentF1, s) {
                (true, Score::Value(v)) => Some(*v),
                _ => None,
            })
            .expect("function_body is scored on tier 3");
        assert!(f1 < 1.0 && f1 > 0.0, "{f1}");
    }

    #[test]
    fn a_cross_file_span_is_scored_on_tiers_one_and_two_like_an_in_file_span() {
        let t = task(TaskTier::CrossFileFirst, "let a = build(1);");
        let scores = score_all(&Scored {
            task: &t,
            prediction: "let a = build(1);",
            symbols: &Symbols(BTreeSet::new()),
            extra: "",
        });
        for want in [Tier::Exact, Tier::EditSim] {
            let score = scores
                .iter()
                .find_map(|(tier, s)| (*tier == want).then_some(*s))
                .expect("the tier is reported");
            assert!(
                matches!(score, Score::Value(v) if approx(v, 1.0)),
                "{want:?} {score:?}"
            );
        }
    }

    #[test]
    fn identifiers_are_code_only_never_prose_in_a_literal_or_a_comment() {
        assert_eq!(
            identifiers("foo(\"Setting up llama\") // cpp"),
            vec!["foo"],
            "a comment naming an API is not a call to it"
        );
    }

    #[test]
    fn a_name_the_model_could_read_in_its_own_context_exists() {
        let repo = Symbols(BTreeSet::default());
        let known = Known {
            repo: &repo,
            file_uses: &[],
            context: "let entry = read_dir()?;\nlet dir = entry.path();\n",
        };
        assert!(
            approx(symbols("entry.dir", "", &known), 1.0),
            "a binding on the same page is not a fabrication"
        );
    }

    #[test]
    fn file_use_symbols_resolves_to_the_alias_when_one_is_given() {
        assert_eq!(file_use_symbols("use foo::Bar as Baz;"), vec!["Baz"]);
        assert_eq!(file_use_symbols("use a::{B as C, D};"), vec!["C", "D"]);
        assert_eq!(
            file_use_symbols("use std::collections::HashMap;"),
            vec!["HashMap"]
        );
    }

    fn task(tier: TaskTier, gold: &str) -> CodebaseTask {
        CodebaseTask {
            id: "t".into(),
            tier,
            file: "src/a.rs".into(),
            line: 1,
            gold: gold.into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "n/a: same-file".into(),
                cfg_test_lines: 0,
                cross_file_withheld: 0,
            },
            name: None,
            also_first_uses: Vec::new(),
            extra: None,
            extra_text: String::new(),
        }
    }

    #[test]
    fn all_seven_tiers_are_reported_and_the_exec_tiers_are_skipped() {
        let known = Symbols(BTreeSet::default());
        let t = task(TaskTier::InFile, "let a = 1;");
        let scores = score_all(&Scored {
            task: &t,
            prediction: "let a = 1;",
            symbols: &known,
            extra: "",
        });
        assert_eq!(scores.len(), 7);
        assert!(matches!(scores[0], (Tier::Exact, Score::Value(v)) if approx(v, 1.0)));
        assert!(matches!(scores[5], (Tier::Compile, Score::Skipped(_))));
        assert!(matches!(scores[6], (Tier::Test, Score::Skipped(_))));
        let body = task(TaskTier::FunctionBody, "let a = 1;");
        let scores = score_all(&Scored {
            task: &body,
            prediction: "let a = 1;",
            symbols: &known,
            extra: "",
        });
        assert!(
            matches!(scores[0], (Tier::Exact, Score::Skipped(_))),
            "tiers 1-2 skip on bodies"
        );
        assert!(matches!(scores[2], (Tier::IdentF1, Score::Value(_))));
    }
}
