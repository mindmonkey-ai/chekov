//! Ranking candidates for "which model should I actually run".
//!
//! Gates first, then sort. A rejected candidate is always reported with its
//! reason — a recommender that silently drops options teaches the user nothing
//! about why their machine cannot take something.

use crate::core::frontier::{Fit, fit_for};
use crate::core::toolparser::ToolParser;

/// What the model is being asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Backing a coding agent: tool calls are load-bearing.
    Agent,
    /// Plain chat: the tool parser does not matter.
    Chat,
}

/// One thing that could be run.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub quant: String,
    pub total_bytes: Option<u64>,
    pub parser: ToolParser,
}

/// The outcome for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Ranked, with any caveats that lowered it.
    Ranked { notes: Vec<String> },
    /// Excluded, with the reason printed rather than the row dropped.
    Rejected { reason: String },
}

/// A candidate plus its outcome, in presentation order.
#[derive(Debug, Clone)]
pub struct Ranked {
    pub candidate: Candidate,
    pub verdict: Verdict,
    pub fit: Fit,
}

/// Gate, then sort.
#[must_use]
pub fn rank(candidates: Vec<Candidate>, budget_mib: u64, role: Role) -> Vec<Ranked> {
    let mut out: Vec<Ranked> = candidates
        .into_iter()
        .map(|c| judge(c, budget_mib, role))
        .collect();
    out.sort_by_key(|r| sort_key(r, role));
    out
}

/// One candidate's verdict. Rejections carry the numbers, not just a word.
fn judge(candidate: Candidate, budget_mib: u64, role: Role) -> Ranked {
    let fit = fit_for(candidate.total_bytes, budget_mib);
    let verdict = match fit {
        Fit::Unknown => Verdict::Rejected {
            reason: "size unknown — its weights are not on disk and no header \
                     could be read, so it cannot be sized against this machine"
                .to_owned(),
        },
        Fit::Exceeds => Verdict::Rejected {
            reason: format!(
                "exceeds the budget: needs {} MiB against {budget_mib} MiB",
                candidate.total_bytes.unwrap_or(0) / (1024 * 1024)
            ),
        },
        Fit::Fits | Fit::Tight => Verdict::Ranked {
            notes: notes_for(&candidate, fit, role),
        },
    };
    Ranked {
        candidate,
        verdict,
        fit,
    }
}

/// Caveats that lower a candidate without disqualifying it.
fn notes_for(candidate: &Candidate, fit: Fit, role: Role) -> Vec<String> {
    let mut notes = Vec::new();
    if role == Role::Agent && !candidate.parser.is_specialized() {
        notes.push(
            "no dedicated tool parser — llama.cpp falls back to the generic \
             autoparser, which often works but is not a contract"
                .to_owned(),
        );
    }
    if fit == Fit::Tight {
        notes.push("tight: over 85% of the budget, leaving little headroom".to_owned());
    }
    notes
}

/// Rejected last; then specialized parsers first for agents; then comfortable
/// before tight; then the largest model that still fits.
fn sort_key(r: &Ranked, role: Role) -> (u8, u8, u8, std::cmp::Reverse<u64>) {
    let rejected = u8::from(matches!(r.verdict, Verdict::Rejected { .. }));
    let untooled = u8::from(role == Role::Agent && !r.candidate.parser.is_specialized());
    let tight = u8::from(r.fit == Fit::Tight);
    (
        rejected,
        untooled,
        tight,
        std::cmp::Reverse(r.candidate.total_bytes.unwrap_or(0)),
    )
}

#[cfg(test)]
mod tests {
    use super::{Candidate, Role, Verdict, rank};
    use crate::core::frontier::Fit;
    use crate::core::toolparser::ToolParser;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn c(name: &str, gib: u64, parser: ToolParser) -> Candidate {
        Candidate {
            name: name.to_owned(),
            quant: "Q8_0".to_owned(),
            total_bytes: Some(gib * GIB),
            parser,
        }
    }

    /// 100 GiB budget expressed in MiB.
    const BUDGET: u64 = 100 * 1024;

    #[test]
    fn a_model_that_exceeds_the_budget_is_rejected_with_its_reason() {
        let out = rank(
            vec![c("huge", 200, ToolParser::Qwen3Coder)],
            BUDGET,
            Role::Agent,
        );
        let r = out.first().expect("the row is reported, not dropped");
        match &r.verdict {
            Verdict::Rejected { reason } => {
                assert!(
                    reason.contains("exceeds"),
                    "the reason must be legible: {reason}"
                );
            }
            Verdict::Ranked { .. } => panic!("a model larger than the machine cannot be ranked"),
        }
        assert_eq!(r.fit, Fit::Exceeds);
    }

    #[test]
    fn an_unsizeable_candidate_is_rejected_not_optimistically_ranked() {
        let mut unknown = c("mystery", 1, ToolParser::Qwen3Coder);
        unknown.total_bytes = None;
        let out = rank(vec![unknown], BUDGET, Role::Agent);
        assert!(
            matches!(out[0].verdict, Verdict::Rejected { .. }),
            "an unknown size must never be recommended"
        );
    }

    #[test]
    fn fallthrough_is_downranked_for_agents_not_refused() {
        // The author's own MiniMax-M2.7 falls through and works; refusing it
        // would be wrong. It ranks BELOW an equivalent specialized model.
        let out = rank(
            vec![
                c("falls-through", 40, ToolParser::AutoparserFallthrough),
                c("specialized", 40, ToolParser::Qwen3Coder),
            ],
            BUDGET,
            Role::Agent,
        );
        assert_eq!(out[0].candidate.name, "specialized");
        assert_eq!(out[1].candidate.name, "falls-through");
        match &out[1].verdict {
            Verdict::Ranked { notes } => assert!(
                notes.iter().any(|n| n.contains("autoparser")),
                "the caveat must be stated, not silent: {notes:?}"
            ),
            Verdict::Rejected { .. } => panic!("downrank, never refuse — it works"),
        }
    }

    #[test]
    fn for_plain_chat_the_tool_parser_does_not_matter() {
        let out = rank(
            vec![
                c("falls-through", 60, ToolParser::AutoparserFallthrough),
                c("specialized", 40, ToolParser::Qwen3Coder),
            ],
            BUDGET,
            Role::Chat,
        );
        assert_eq!(
            out[0].candidate.name, "falls-through",
            "with no tool requirement the larger model wins"
        );
        match &out[0].verdict {
            Verdict::Ranked { notes } => assert!(notes.is_empty(), "no caveat applies: {notes:?}"),
            Verdict::Rejected { .. } => panic!("nothing disqualifies it"),
        }
    }

    #[test]
    fn among_equals_the_larger_model_that_still_fits_wins() {
        let out = rank(
            vec![
                c("small", 20, ToolParser::Qwen3Coder),
                c("large", 60, ToolParser::Qwen3Coder),
            ],
            BUDGET,
            Role::Agent,
        );
        assert_eq!(out[0].candidate.name, "large");
    }

    #[test]
    fn a_tight_fit_ranks_below_a_comfortable_one() {
        let out = rank(
            vec![
                c("tight", 95, ToolParser::Qwen3Coder),
                c("comfortable", 60, ToolParser::Qwen3Coder),
            ],
            BUDGET,
            Role::Agent,
        );
        assert_eq!(
            out[0].candidate.name, "comfortable",
            "headroom beats raw size once a model is in the tight band"
        );
        assert_eq!(out[1].fit, Fit::Tight);
    }
}
