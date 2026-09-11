//! Opt-in, seeded two-hop traces. Version the generator when its task or grading changes.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::runner::{self, ProbeWire};
use super::store::{GradeRow, RunHead, RunLog, RunWriter, Task, TaskRow, Transport};
use crate::core::hub::JsonRequest;
use crate::core::proxy::http::HttpRequest;
use crate::error::ChekovError;

const SUITE: &str = "long_ctx_trace";
const ANSWER_TOKENS: u32 = 256;
const PLACEMENTS: [(usize, usize); 4] = [(6, 44), (22, 57), (41, 9), (57, 25)];
const INSTRUCTION: &str = "Follow the function pointer in the named constant to its function. Return only the decimal integer returned by that function, with no prose or code fence.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "PlanData")]
pub struct Plan {
    version: u32,
    lengths: Vec<u32>,
    seed: u32,
    max_tokens: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanData {
    version: u32,
    lengths: Vec<u32>,
    seed: u32,
    max_tokens: u32,
}

impl TryFrom<PlanData> for Plan {
    type Error = ChekovError;

    fn try_from(data: PlanData) -> Result<Self, Self::Error> {
        let plan = Self::new(&data.lengths, data.seed)?;
        if data.version != 1 || data.max_tokens != ANSWER_TOKENS {
            return Err(invalid("unsupported trace version or answer budget"));
        }
        Ok(plan)
    }
}

impl Plan {
    pub(crate) fn new(lengths: &[u32], seed: u32) -> Result<Self, ChekovError> {
        if lengths.is_empty() || lengths.len() > 16 {
            return Err(invalid("provide between one and sixteen lengths"));
        }
        if lengths
            .iter()
            .any(|length| !(1024..=1_048_576).contains(length))
        {
            return Err(invalid(
                "lengths must be between 1024 and 1048576 estimated tokens",
            ));
        }
        if lengths.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(invalid("lengths must be distinct and in increasing order"));
        }
        Ok(Self {
            version: 1,
            lengths: lengths.to_vec(),
            seed,
            max_tokens: ANSWER_TOKENS,
        })
    }

    pub(crate) fn optional(lengths: &[u32], seed: u32) -> Result<Option<Self>, ChekovError> {
        if lengths.is_empty() {
            return Ok(None);
        }
        Self::new(lengths, seed).map(Some)
    }

    pub(crate) fn wrap_hash(&self, base: &str) -> String {
        let canonical = format!(
            "{base}|{SUITE}|{}|{:?}|{}|{}|{INSTRUCTION}|{PLACEMENTS:?}",
            self.version, self.lengths, self.seed, self.max_tokens
        );
        crate::core::hash::sha256_hex(canonical.as_bytes())[..12].to_owned()
    }

    pub(crate) fn with_seed(&self, seed: u32) -> Self {
        Self {
            seed,
            ..self.clone()
        }
    }

    pub(crate) fn check_context(&self, context: u32) -> Result<(), ChekovError> {
        if self
            .lengths
            .iter()
            .any(|length| length.saturating_add(self.max_tokens) > context)
        {
            return Err(invalid(&format!(
                "trace lengths {:?} plus {} answer tokens exceed context {context}; shorten the length list or increase the model context",
                self.lengths, self.max_tokens
            )));
        }
        Ok(())
    }

    pub(crate) fn estimate_secs(&self) -> u64 {
        self.lengths
            .iter()
            .map(|length| {
                8 * ((u64::from(*length) + 4096).div_ceil(100)
                    + u64::from(self.max_tokens).div_ceil(20))
            })
            .sum()
    }

    pub(crate) fn plan_line(&self, candidates: usize) -> String {
        format!(
            "+ long_ctx_trace: {:?} estimated prompt tokens; {} crossings per model, {} model(s); full-prefill estimate ~{} s (100 prompt tok/s, 20 answer tok/s); local template/token counts checked before inference; foreign lengths remain unverified\n",
            self.lengths,
            self.lengths.len() * 8,
            candidates,
            self.estimate_secs().saturating_mul(candidates as u64)
        )
    }

    fn key(&self, length: u32, placement: usize) -> Key {
        let digest = crate::core::hash::sha256_hex(
            format!("trace-v1|{}|{length}|{placement}", self.seed).as_bytes(),
        );
        Key {
            requested_length: length,
            placement,
            digest,
        }
    }
}

fn invalid(reason: &str) -> ChekovError {
    ChekovError::LongContextInvalid {
        reason: reason.to_owned(),
    }
}

pub(crate) fn assert_same(a: &RunHead, b: &RunHead) -> Result<(), ChekovError> {
    if a.long_ctx_trace == b.long_ctx_trace {
        return Ok(());
    }
    Err(ChekovError::BenchStampMismatch {
        field: SUITE.to_owned(),
        a: format!("{:?}", a.long_ctx_trace),
        b: format!("{:?}", b.long_ctx_trace),
    })
}

struct Key {
    requested_length: u32,
    placement: usize,
    digest: String,
}

impl Key {
    fn id(&self) -> String {
        format!("trace-{}-{}", self.requested_length, self.placement)
    }
    fn anchor(&self) -> String {
        format!("ANCHOR_{}", &self.digest[..12])
    }
    fn function(&self) -> String {
        format!("node_{}", &self.digest[12..24])
    }
    fn gold(&self) -> String {
        let value = self.digest[24..40].bytes().fold(0_u64, |value, byte| {
            value.wrapping_mul(31).wrapping_add(u64::from(byte))
        });
        (10_000_000 + value % 90_000_000).to_string()
    }

    fn request(&self) -> HttpRequest {
        let corpus = self.corpus();
        super::probes::anthropic_post(&json!({
            "model": "claude-sonnet-4", "max_tokens": ANSWER_TOKENS,
            "messages": [{"role": "user", "content": format!("{INSTRUCTION}\n\n{corpus}\nQuery: {}\n", self.anchor())}],
        }))
    }

    fn corpus(&self) -> String {
        let (anchor_at, function_at) = PLACEMENTS[self.placement];
        let filler = "lorem ".repeat(self.requested_length as usize / 64);
        (0..64).fold(String::new(), |mut corpus, position| {
            let declaration = if position == anchor_at {
                format!(
                    "const {}: fn() -> u64 = {};",
                    self.anchor(),
                    self.function()
                )
            } else if position == function_at {
                format!("fn {}() -> u64 {{ {} }}", self.function(), self.gold())
            } else {
                self.distractor(position)
            };
            let _ = writeln!(corpus, "// {filler}\n{declaration}");
            corpus
        })
    }

    fn distractor(&self, position: usize) -> String {
        let rank = position * 17 % 64;
        let hash = crate::core::hash::sha256_hex(
            format!("{}|decoy|{}", self.digest, rank % 32).as_bytes(),
        );
        let value = hash.bytes().fold(0_u64, |value, byte| {
            value.wrapping_mul(17).wrapping_add(u64::from(byte))
        });
        let value = 10_000_000 + value % 90_000_000;
        let value = if value.to_string() == self.gold() {
            10_000_000 + (value + 1) % 90_000_000
        } else {
            value
        };
        if rank < 32 {
            format!(
                "const ANCHOR_{}: fn() -> u64 = node_{};",
                &hash[..12],
                &hash[12..24]
            )
        } else {
            format!("fn node_{}() -> u64 {{ {value} }}", &hash[12..24])
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    requested_length: u32,
    placement: usize,
    prompt_tokens: Option<u64>,
    expected_prompt_tokens: Option<u64>,
    context_tokens: Option<u32>,
    answer: String,
    expected: String,
    finish_reason: Option<String>,
    truncated: bool,
}

impl Row {
    fn new(key: &Key, context: Option<u32>) -> Self {
        Self {
            requested_length: key.requested_length,
            placement: key.placement,
            prompt_tokens: None,
            expected_prompt_tokens: None,
            context_tokens: context,
            answer: String::new(),
            expected: key.gold(),
            finish_reason: None,
            truncated: false,
        }
    }

    fn verified(&self) -> bool {
        match (
            self.prompt_tokens,
            self.expected_prompt_tokens,
            self.context_tokens,
        ) {
            (Some(observed), Some(expected), Some(context)) => {
                observed > 0
                    && observed == expected
                    && observed.saturating_add(u64::from(ANSWER_TOKENS)) <= u64::from(context)
                    && !self.truncated
            }
            _ => false,
        }
    }

    fn verdict(&self) -> GradeRow {
        if self.truncated {
            return GradeRow::unavailable("server reported truncated context".into());
        }
        if let Some((expected, observed)) = self
            .expected_prompt_tokens
            .zip(self.prompt_tokens)
            .filter(|(expected, observed)| expected != observed)
        {
            return GradeRow::unavailable(format!(
                "prompt length unverified: tokenized {expected}, server reported {observed}"
            ));
        }
        if self.finish_reason.as_deref() != Some("end_turn") {
            return GradeRow::fail(format!(
                "answer truncated or incomplete: {:?}",
                self.finish_reason
            ));
        }
        if self.answer.trim() == self.expected {
            return GradeRow::pass();
        }
        GradeRow::fail(format!(
            "exact answer mismatch: expected {}, received {:?}",
            self.expected, self.answer
        ))
    }
}

struct Session<'a> {
    wire: &'a ProbeWire<'a>,
    plan: &'a Plan,
    context: Option<u32>,
    done: &'a [(String, String, Transport)],
}

struct Pass<'a> {
    session: &'a Session<'a>,
    transport: Transport,
}

pub(crate) fn run(
    writer: &mut RunWriter,
    wire: &ProbeWire,
    done: &[(String, String, Transport)],
) -> Result<(), ChekovError> {
    let log = RunLog::load(writer.dir())?;
    let Some(plan) = &log.head.long_ctx_trace else {
        return Ok(());
    };
    let context = (log.head.stamp.ctx > 0).then_some(log.head.stamp.ctx);
    let session = Session {
        wire,
        plan,
        context,
        done,
    };
    for length in &plan.lengths {
        for placement in 0..PLACEMENTS.len() {
            run_case(writer, &session, &plan.key(*length, placement))?;
        }
    }
    Ok(())
}

fn run_case(writer: &mut RunWriter, session: &Session, key: &Key) -> Result<(), ChekovError> {
    let id = key.id();
    for transport in [Transport::Buffered, Transport::Streamed] {
        if session
            .done
            .iter()
            .any(|(suite, recorded, door)| suite == SUITE && *recorded == id && *door == transport)
        {
            continue;
        }
        record(writer, &Pass { session, transport }, key)?;
    }
    Ok(())
}

fn record(writer: &mut RunWriter, pass: &Pass, key: &Key) -> Result<(), ChekovError> {
    let mut row = Row::new(key, pass.session.context);
    let grade = match answer(pass, key, &mut row) {
        Ok(()) => row.verdict(),
        Err(error) => GradeRow::unavailable(error.to_string()),
    };
    let mut measure = super::codebase::run::empty_measure();
    measure.prompt_n = row.prompt_tokens.unwrap_or(0);
    let task = Task {
        suite: SUITE.into(),
        task_id: key.id(),
        measure,
        grade: Some(grade),
        transport: pass.transport,
        codebase: None,
        judge: None,
        tool_loop: None,
    };
    writer.append_trace(task, row)
}

fn answer(pass: &Pass, key: &Key, row: &mut Row) -> Result<(), ChekovError> {
    let wire = pass.session.wire;
    let mut request = runner::prepare_trace(wire, &key.request(), pass.transport)?;
    if let Some(context) = row.context_tokens {
        let expected = prompt_tokens(wire, &request)?;
        row.expected_prompt_tokens = Some(expected);
        if expected.saturating_add(u64::from(pass.session.plan.max_tokens)) > u64::from(context) {
            return Err(invalid(&format!(
                "requested length {} tokenizes to {expected} plus {} answer tokens, exceeding server context {context}; use a shorter --long-ctx-trace length or restart with a larger context",
                key.requested_length, pass.session.plan.max_tokens
            )));
        }
        let mut body: Value = serde_json::from_str(&request.body).map_err(bad_reply)?;
        body["cache_prompt"] = json!(false);
        body["n_keep"] = json!(-1);
        request.body = body.to_string();
    }
    let response = wire.http.post_json(&request)?;
    record_counts(row, &response, pass.transport);
    let body = runner::translate_trace(wire, &response, pass.transport)?;
    let parsed: Value = serde_json::from_str(&body).map_err(bad_reply)?;
    row.finish_reason = parsed["stop_reason"].as_str().map(str::to_owned);
    row.answer = super::grade::artifact_text(&body).map_err(|grade| match grade {
        super::grade::Grade::Fail { reason } => bad_reply(reason),
        super::grade::Grade::Pass => bad_reply("invalid grading outcome"),
    })?;
    Ok(())
}

fn bad_reply(error: impl std::fmt::Display) -> ChekovError {
    ChekovError::ProxyBadRequest {
        reason: format!("long_ctx_trace response: {error}"),
    }
}

fn prompt_tokens(wire: &ProbeWire, request: &JsonRequest) -> Result<u64, ChekovError> {
    let applied = wire.http.post_json(&JsonRequest {
        url: format!("{}/apply-template", wire.upstream.base_url),
        body: request.body.clone(),
        bearer: request.bearer.clone(),
    })?;
    let applied: Value = serde_json::from_str(&applied).map_err(bad_reply)?;
    let prompt = applied["prompt"]
        .as_str()
        .ok_or_else(|| bad_reply("/apply-template omitted prompt"))?;
    let tokenized = wire.http.post_json(&JsonRequest {
        url: format!("{}/tokenize", wire.upstream.base_url),
        bearer: request.bearer.clone(),
        body: json!({"content": prompt, "add_special": false, "parse_special": true}).to_string(),
    })?;
    let tokenized: Value = serde_json::from_str(&tokenized).map_err(bad_reply)?;
    let tokens = tokenized["tokens"]
        .as_array()
        .ok_or_else(|| bad_reply("/tokenize omitted tokens"))?;
    if tokens.is_empty() {
        return Err(bad_reply("/tokenize returned no tokens"));
    }
    Ok(tokens.len() as u64)
}

fn record_counts(row: &mut Row, response: &str, transport: Transport) {
    let frames: Vec<Value> = match transport {
        Transport::Buffered => serde_json::from_str(response).into_iter().collect(),
        Transport::Streamed => response
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str(data.trim()).ok())
            .collect(),
    };
    row.prompt_tokens = frames
        .iter()
        .filter_map(|frame| frame["usage"]["prompt_tokens"].as_u64())
        .next_back();
    row.truncated = frames.iter().any(|frame| frame["truncated"] == true);
}

#[derive(Default)]
struct Scores {
    passed: usize,
    measured: usize,
    missing: usize,
    duplicate: usize,
    unavailable: usize,
    unverified: usize,
    prompt_tokens: Vec<u64>,
}

impl Scores {
    const fn complete(&self) -> bool {
        self.passed == PLACEMENTS.len()
            && self.missing == 0
            && self.duplicate == 0
            && self.unavailable == 0
            && self.unverified == 0
    }

    fn cell(&self) -> String {
        let observed = match (
            self.prompt_tokens.iter().min(),
            self.prompt_tokens.iter().max(),
        ) {
            (Some(min), Some(max)) => format!("{min}..{max}"),
            _ => "unknown".into(),
        };
        format!(
            "{}/{}; observed {observed}; missing {}, duplicate {}, unavailable {}, unverified {}",
            self.passed,
            self.measured,
            self.missing,
            self.duplicate,
            self.unavailable,
            self.unverified
        )
    }
}

fn rows_for<'a>(log: &'a RunLog, key: &Key, door: Transport) -> Vec<&'a TaskRow> {
    let id = key.id();
    log.rows
        .iter()
        .filter(|row| row.suite == SUITE && row.task_id == id && row.transport == door)
        .collect()
}

fn scores(log: &RunLog, length: u32, door: Transport) -> Scores {
    let mut scores = Scores::default();
    let Some(plan) = &log.head.long_ctx_trace else {
        return scores;
    };
    for placement in 0..PLACEMENTS.len() {
        let key = plan.key(length, placement);
        let rows = rows_for(log, &key, door);
        match rows.as_slice() {
            [] => scores.missing += 1,
            [row] => score_row(&mut scores, row, &key),
            _ => scores.duplicate += 1,
        }
    }
    scores
}

fn score_row(scores: &mut Scores, row: &TaskRow, key: &Key) {
    let Some(evidence) = &row.long_ctx_trace else {
        scores.unverified += 1;
        return;
    };
    if evidence.requested_length != key.requested_length
        || evidence.placement != key.placement
        || evidence.expected != key.gold()
    {
        scores.unverified += 1;
        return;
    }
    scores.prompt_tokens.extend(evidence.prompt_tokens);
    if !evidence.verified() {
        scores.unverified += 1;
    }
    let Some(grade) = &row.grade else {
        scores.unavailable += 1;
        return;
    };
    let recomputed = evidence.verdict();
    if grade.unavailable || recomputed.unavailable {
        scores.unavailable += 1;
        return;
    }
    scores.measured += 1;
    scores.passed += usize::from(grade.pass && recomputed.pass);
}

pub(crate) fn render(log: &RunLog) -> String {
    let Some(plan) = &log.head.long_ctx_trace else {
        return String::new();
    };
    let mut out = String::from(
        "long_ctx_trace: exact two-hop answers; four placements per length and transport\n",
    );
    let mut contiguous = true;
    let mut best = None;
    for length in &plan.lengths {
        let buffered = scores(log, *length, Transport::Buffered);
        let streamed = scores(log, *length, Transport::Streamed);
        let _ = writeln!(
            out,
            "  requested {length}: buffered {}; streamed {}",
            buffered.cell(),
            streamed.cell()
        );
        contiguous &= buffered.complete() && streamed.complete();
        if contiguous {
            best = buffered
                .prompt_tokens
                .iter()
                .chain(&streamed.prompt_tokens)
                .max()
                .copied();
        }
    }
    out.push_str(&recommendation(best, plan.max_tokens));
    out.push_str(&details(log, plan));
    out
}

fn recommendation(best: Option<u64>, answer_tokens: u32) -> String {
    best.map_or_else(
        || "  recommended ctx_size: N/A (no complete, verified contiguous range at >=90% on both transports)\n".into(),
        |tokens| format!("  recommended ctx_size: {} (tested lower bound, {tokens} observed prompt tokens + {answer_tokens} answer reserve; not a model maximum; models.toml unchanged)\n",
            tokens.saturating_add(u64::from(answer_tokens))))
}

fn details(log: &RunLog, plan: &Plan) -> String {
    let mut out = String::new();
    for (row, grade) in log
        .rows
        .iter()
        .filter(|row| row.suite == SUITE)
        .filter_map(|row| {
            row.grade
                .as_ref()
                .filter(|grade| !grade.pass)
                .map(|grade| (row, grade))
        })
    {
        let _ = writeln!(
            out,
            "  {} {:?}: {}",
            row.task_id,
            row.transport,
            grade.reason.as_deref().unwrap_or("ungraded")
        );
    }
    for length in &plan.lengths {
        for placement in 0..PLACEMENTS.len() {
            out.push_str(&asymmetry(log, &plan.key(*length, placement)));
        }
    }
    out
}

fn asymmetry(log: &RunLog, key: &Key) -> String {
    let (buffered, streamed) = (
        rows_for(log, key, Transport::Buffered),
        rows_for(log, key, Transport::Streamed),
    );
    let ([a], [b]) = (buffered.as_slice(), streamed.as_slice()) else {
        return String::new();
    };
    let (Some(a), Some(b)) = (&a.grade, &b.grade) else {
        return String::new();
    };
    if a.unavailable || b.unavailable || a.pass == b.pass {
        return String::new();
    }
    format!(
        "  long_ctx_trace asymmetry {}: buffered {}, streamed {}\n",
        key.id(),
        verdict_word(a),
        verdict_word(b)
    )
}

const fn verdict_word(grade: &GradeRow) -> &str {
    if grade.pass { "PASS" } else { "FAIL" }
}

pub(crate) fn compare(a: &RunLog, b: &RunLog) -> String {
    if a.head.long_ctx_trace.is_none() && b.head.long_ctx_trace.is_none() {
        return String::new();
    }
    format!(
        "long_ctx_trace {}:\n{}long_ctx_trace {}:\n{}",
        a.head.model,
        render(a),
        b.head.model,
        render(b)
    )
}
