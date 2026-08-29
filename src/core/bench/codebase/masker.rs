//! Mask selection without a parser.
//!
//! A `fn` signature by regex-free scanning, its body by brace balance,
//! statements inside it by the same scanner. Cruder than AST boundaries and
//! labelled as such; a span that fails its own balance check is discarded,
//! never approximated.

use std::ops::Range;

use super::TaskTier;

const BODY_LINES: std::ops::RangeInclusive<usize> = 3..=40;
const SPAN_LINES: std::ops::RangeInclusive<usize> = 1..=8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub tier: TaskTier,
    pub byte_range: Range<usize>,
    /// 1-based first line of the span.
    pub line: usize,
    /// The `///` block directly above the function, when there is one.
    pub doc_comment: Option<Range<usize>>,
}

pub trait MaskSource {
    /// Every candidate span, in source order. Never a malformed span.
    fn candidates(&self, text: &str) -> Vec<Candidate>;
}

pub struct RustBraceMasker;

impl MaskSource for RustBraceMasker {
    fn candidates(&self, text: &str) -> Vec<Candidate> {
        let mut out = Vec::new();
        for sig in fn_signatures(text) {
            let Some(body) = body_after(text, sig.end) else {
                continue;
            };
            let interior = body.start + 1..body.end - 1;
            if BODY_LINES.contains(&line_count(&text[interior.clone()])) {
                out.push(Candidate {
                    tier: TaskTier::FunctionBody,
                    byte_range: interior.clone(),
                    line: line_of(text, interior.start + 1),
                    doc_comment: doc_comment_before(text, line_start(text, sig.start)),
                });
            }
            out.extend(statement_spans(text, &interior));
        }
        out
    }
}

/// `fn name(` or `fn name<` at the start of a line (after visibility and
/// qualifiers). Returns the byte range of `fn` through the name.
fn fn_signatures(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        let rest = strip_qualifiers(trimmed);
        if let Some(after_fn) = rest.strip_prefix("fn ")
            && let Some(name_len) = ident_len(after_fn)
            && matches!(
                after_fn[name_len..].trim_start().chars().next(),
                Some('(' | '<')
            )
        {
            let start = offset + lead + (trimmed.len() - rest.len());
            found.push(start..start + 3 + name_len);
        }
        offset += line.len();
    }
    found
}

fn strip_qualifiers(mut s: &str) -> &str {
    loop {
        let before = s;
        for q in [
            "pub(crate) ",
            "pub(super) ",
            "pub ",
            "async ",
            "unsafe ",
            "const ",
            "extern \"C\" ",
        ] {
            s = s.strip_prefix(q).unwrap_or(s);
        }
        if s == before {
            return s;
        }
    }
}

fn ident_len(s: &str) -> Option<usize> {
    let n = s
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count();
    (n > 0 && !s.starts_with(|c: char| c.is_ascii_digit())).then_some(n)
}

/// The balanced `{…}` that follows a signature: the first `{` after the
/// signature's parameter list, closed by its matching `}`. Range includes
/// both braces.
fn body_after(text: &str, from: usize) -> Option<Range<usize>> {
    let open = from + text[from..].find('{')?;
    // A `where` clause or return type never contains `{`; a `;` first means
    // a trait method without a body.
    if text[from..open].contains(';') {
        return None;
    }
    let close = matching_close(text, open)?;
    Some(open..close + 1)
}

/// Index of the `}` matching the `{` at `open`, skipping strings, chars and
/// comments.
fn matching_close(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0_i64;
    let mut scanner = Scanner::new(&text[open..]);
    while let Some((i, c)) = scanner.next_code_char() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Statement spans inside a body: each line-aligned run that ends at a `;`
/// or a balanced `}` at depth 0 relative to the body, 1–8 lines, balanced.
fn statement_spans(text: &str, body: &Range<usize>) -> Vec<Candidate> {
    let mut spans = Vec::new();
    let mut start = body.start;
    let mut depth = 0_i64;
    let mut scanner = Scanner::new(&text[body.clone()]);
    while let Some((i, c)) = scanner.next_code_char() {
        let at = body.start + i;
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            _ => {}
        }
        let end = at + 1;
        let ends_statement =
            depth == 0 && (c == ';' || (c == '}' && !continues_with_else(text, end)));
        if !ends_statement {
            continue;
        }
        push_span(text, start..end, &mut spans);
        start = end;
    }
    spans
}

/// Whether an `else` (or `else if`) keyword follows immediately (across
/// whitespace) at `at` — the closing `}` just scanned belongs to an `if`
/// block that isn't done yet, so it must not end the statement.
fn continues_with_else(text: &str, at: usize) -> bool {
    let Some(after) = text[at..].trim_start().strip_prefix("else") else {
        return false;
    };
    after
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn push_span(text: &str, raw: Range<usize>, spans: &mut Vec<Candidate>) {
    let span = trim_to_lines(text, raw);
    let content = &text[span.clone()];
    if content.trim().is_empty() || !SPAN_LINES.contains(&line_count(content)) {
        return;
    }
    if balance(content) != Some(0) {
        return;
    }
    spans.push(Candidate {
        tier: TaskTier::InFile,
        byte_range: span.clone(),
        line: line_of(text, span.start),
        doc_comment: None,
    });
}

/// Widen `raw` to whole lines: back to the previous `\n` (exclusive), forward
/// to the next `\n` (exclusive).
fn trim_to_lines(text: &str, raw: Range<usize>) -> Range<usize> {
    let start = line_start(text, raw.start);
    let end = text[raw.end..]
        .find('\n')
        .map_or(text.len(), |i| raw.end + i);
    let leading_blank = text[start..end].len() - text[start..end].trim_start().len();
    let start = start
        + text[start..end][..leading_blank]
            .rfind('\n')
            .map_or(0, |i| i + 1);
    start..end
}

/// Byte index of the start of the line containing `at`. When `at` itself is
/// a `\n` — the common case right after a brace or a prior statement's
/// terminator — that newline already marks the boundary, so it counts as
/// "at or before `at`" rather than being skipped past.
fn line_start(text: &str, at: usize) -> usize {
    let search_end = (at + 1).min(text.len());
    text[..search_end].rfind('\n').map_or(0, |i| i + 1)
}

fn line_count(s: &str) -> usize {
    s.trim_matches('\n').lines().count()
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// The `///` block whose last line is directly above `sig_start` (blank
/// lines break adjacency).
fn doc_comment_before(text: &str, sig_start: usize) -> Option<Range<usize>> {
    let head = &text[..sig_start];
    let mut lines: Vec<(usize, &str)> = Vec::new();
    // `head` ends with the `\n` that terminates its last line (guaranteed by
    // `line_start`, which only ever hands us a line boundary); that `\n` is
    // consumed by `rsplit_terminator` rather than yielded, so line offsets
    // start one byte short of `head.len()`.
    let mut end = head.len().saturating_sub(1);
    for line in head.rsplit_terminator('\n') {
        let start = end - line.len();
        if line.trim_start().starts_with("///") {
            lines.push((start, line));
            end = start.saturating_sub(1);
        } else if line.trim().is_empty() && lines.is_empty() {
            return None;
        } else {
            break;
        }
    }
    let first = lines.last()?.0;
    Some(first..head.len())
}

/// Balance of `{}`/`[]`/`()` outside strings, chars and comments. `None`
/// when a closer arrives with nothing open.
#[must_use]
pub fn balance(text: &str) -> Option<i64> {
    let mut depth = 0_i64;
    let mut scanner = Scanner::new(text);
    while let Some((_, c)) = scanner.next_code_char() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    Some(depth)
}

/// Yields code characters only: string literals (plain, escaped, raw `r#…#`),
/// char literals, `//` and `/* */` comments are consumed whole.
struct Scanner<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    const fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    fn next_code_char(&mut self) -> Option<(usize, char)> {
        loop {
            let rest = &self.text[self.pos..];
            let c = rest.chars().next()?;
            let at = self.pos;
            if let Some(skip) = literal_len(rest) {
                self.pos += skip;
                continue;
            }
            self.pos += c.len_utf8();
            return Some((at, c));
        }
    }
}

/// Length of a string/char/comment literal starting at `rest`, or `None`.
fn literal_len(rest: &str) -> Option<usize> {
    if rest.starts_with("//") {
        return Some(rest.find('\n').map_or(rest.len(), |i| i));
    }
    if rest.starts_with("/*") {
        return Some(rest.find("*/").map_or(rest.len(), |i| i + 2));
    }
    if let Some(raw) = rest.strip_prefix('r') {
        let hashes = raw.chars().take_while(|c| *c == '#').count();
        if raw[hashes..].starts_with('"') {
            let close = format!("\"{}", "#".repeat(hashes));
            let body = &raw[hashes + 1..];
            return Some(
                1 + hashes + 1 + body.find(&close).map_or(body.len(), |i| i + close.len()),
            );
        }
    }
    if rest.starts_with('"') {
        return Some(quoted_len(rest, '"'));
    }
    if rest.starts_with('\'') && rest.len() >= 3 && char_literal(rest) {
        return Some(quoted_len(rest, '\''));
    }
    None
}

/// `'a'`, `'\n'`, `'\u{1F600}'` — but not a lifetime `'a `.
fn char_literal(rest: &str) -> bool {
    let inner = &rest[1..];
    let escaped = inner.starts_with('\\');
    let after = if escaped {
        inner[1..].find('\'').map(|i| i + 2)
    } else {
        inner.chars().next().map(char::len_utf8)
    };
    after.is_some_and(|n| inner[n..].starts_with('\''))
}

fn quoted_len(rest: &str, quote: char) -> usize {
    let mut escaped = false;
    for (i, c) in rest.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return i + 1;
        }
    }
    rest.len()
}

#[cfg(test)]
mod tests {
    use super::{Candidate, MaskSource, RustBraceMasker, balance};
    use crate::core::bench::codebase::TaskTier;

    const SRC: &str = r#"//! module doc
use std::fmt;

/// Adds two numbers.
/// Second doc line.
pub fn add(a: i32, b: i32) -> i32 {
    let sum = a + b;
    let text = "not { a brace";
    // nor } this one
    sum
}

fn tiny() -> i32 { 1 }

pub(crate) fn branchy(flag: bool) -> &'static str {
    if flag {
        "yes"
    } else {
        "no"
    }
}
"#;

    fn tier(cands: &[Candidate], tier: TaskTier) -> Vec<&Candidate> {
        cands.iter().filter(|c| c.tier == tier).collect()
    }

    #[test]
    fn a_body_is_masked_between_its_braces_ignoring_braces_in_strings_and_comments() {
        let cands = RustBraceMasker.candidates(SRC);
        let bodies = tier(&cands, TaskTier::FunctionBody);
        assert_eq!(
            bodies.len(),
            2,
            "add and branchy; tiny is one line: {cands:?}"
        );
        let add = &bodies[0];
        let gold = &SRC[add.byte_range.clone()];
        assert!(gold.starts_with("\n    let sum"), "{gold:?}");
        assert!(gold.trim_end().ends_with("sum"), "{gold:?}");
        assert!(
            !gold.contains("pub fn add"),
            "the signature is context, not gold"
        );
        assert_eq!(add.line, 7, "1-based first line of the span");
        let doc = add.doc_comment.clone().expect("adjacent /// block");
        assert!(SRC[doc].starts_with("/// Adds two numbers."));
        assert!(
            bodies[1].doc_comment.is_none(),
            "branchy has no doc comment"
        );
    }

    #[test]
    fn in_file_spans_are_whole_balanced_statements() {
        let cands = RustBraceMasker.candidates(SRC);
        let spans: Vec<String> = tier(&cands, TaskTier::InFile)
            .iter()
            .map(|c| SRC[c.byte_range.clone()].trim().to_owned())
            .collect();
        assert!(spans.contains(&"let sum = a + b;".to_owned()), "{spans:?}");
        assert!(
            spans
                .iter()
                .any(|s| s.starts_with("if flag {") && s.ends_with('}')),
            "an if with its blocks is one span: {spans:?}"
        );
        assert!(spans.iter().all(|s| balance(s) == Some(0)), "{spans:?}");
    }

    #[test]
    fn a_two_line_body_and_a_41_line_body_are_not_candidates() {
        use std::fmt::Write as _;

        let lines = (0..41).fold(String::new(), |mut acc, i| {
            let _ = writeln!(acc, "    let v{i} = {i};");
            acc
        });
        let long_body = format!("fn long() {{\n{lines}}}\n");
        let src = format!("fn two() {{\n    1\n}}\n{long_body}");
        let bodies = RustBraceMasker.candidates(&src);
        assert!(
            !bodies.iter().any(|c| c.tier == TaskTier::FunctionBody),
            "2-line and 41-line bodies are out of range: {bodies:?}"
        );
    }

    #[test]
    fn balance_skips_strings_chars_and_comments() {
        assert_eq!(balance("{ \"}\" '}' // }\n /* } */ }"), Some(0));
        assert_eq!(balance("r#\"}\"# {"), Some(1));
        assert_eq!(balance("}"), None, "a closer with no opener");
        assert_eq!(balance("fn f() { let x = [1, (2)]; }"), Some(0));
    }
}
