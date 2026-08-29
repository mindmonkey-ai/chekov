//! Deterministic, stratified task sampling: the same HEAD always yields the
//! same set, and a large file cannot dominate it.

use super::TaskTier;
use super::masker::Candidate;
use crate::core::hash::sha256_hex;

pub struct FileCandidates {
    /// Worktree-relative path, forward slashes.
    pub path: String,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub path: String,
    pub candidate: Candidate,
    pub id: String,
}

#[derive(Debug, Default)]
pub struct TaskSet {
    pub picked: Vec<Picked>,
    /// "`function_body`: 5 of 8 requested (repo has 5 candidates)" — printed,
    /// never filled from the other tier.
    pub shortfall: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub in_file: usize,
    pub function_body: usize,
}

/// Two-thirds `in_file` rounded up, the remainder `function_body`.
#[must_use]
pub fn quota(total: u32) -> Quota {
    let total = usize::try_from(total).unwrap_or(0);
    let in_file = (total * 2).div_ceil(3);
    Quota {
        in_file,
        function_body: total - in_file,
    }
}

/// The first 8 bytes of `sha256("chekov-codebase-v1:" + head)`.
#[must_use]
pub fn seed_from_head(head_sha: &str) -> u64 {
    let hex = sha256_hex(format!("chekov-codebase-v1:{head_sha}").as_bytes());
    u64::from_str_radix(&hex[..16], 16).unwrap_or(0x9E37_79B9_7F4A_7C15)
}

/// xorshift64* — small, fast, and part of the task-set identity (changing it
/// changes every set, which the corpus id records).
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).unwrap_or(1)).unwrap_or(0)
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

#[must_use]
pub fn sample(mut files: Vec<FileCandidates>, quota: Quota, seed: u64) -> TaskSet {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut rng = Rng::new(seed);
    let mut set = TaskSet::default();
    for (tier, want) in [
        (TaskTier::InFile, quota.in_file),
        (TaskTier::FunctionBody, quota.function_body),
    ] {
        let mut lanes = per_file_lanes(&files, tier, &mut rng);
        let picked = round_robin(&mut lanes, want);
        let have: usize = files
            .iter()
            .map(|f| f.candidates.iter().filter(|c| c.tier == tier).count())
            .sum();
        if picked.len() < want {
            set.shortfall.push(format!(
                "{}: {} of {want} requested (repo has {have} candidates)",
                tier.label(),
                picked.len()
            ));
        }
        set.picked.extend(picked);
    }
    set
}

/// One shuffled lane of candidates per file (files in seeded order).
fn per_file_lanes(
    files: &[FileCandidates],
    tier: TaskTier,
    rng: &mut Rng,
) -> Vec<(String, Vec<Candidate>)> {
    let mut lanes: Vec<(String, Vec<Candidate>)> = files
        .iter()
        .map(|f| {
            let mut lane: Vec<Candidate> = f
                .candidates
                .iter()
                .filter(|c| c.tier == tier)
                .cloned()
                .collect();
            rng.shuffle(&mut lane);
            (f.path.clone(), lane)
        })
        .filter(|(_, lane)| !lane.is_empty())
        .collect();
    rng.shuffle(&mut lanes);
    lanes
}

/// Take one candidate per file per pass until `want` are picked or every
/// lane is empty.
fn round_robin(lanes: &mut [(String, Vec<Candidate>)], want: usize) -> Vec<Picked> {
    let mut picked = Vec::new();
    while picked.len() < want {
        let before = picked.len();
        for (path, lane) in lanes.iter_mut() {
            if picked.len() == want {
                break;
            }
            if let Some(candidate) = lane.pop() {
                let id = task_id(path, &candidate);
                picked.push(Picked {
                    path: path.clone(),
                    candidate,
                    id,
                });
            }
        }
        if picked.len() == before {
            break;
        }
    }
    picked
}

/// `<tier>-<sha256(path)[..6]>-L<line>` — stable across runs on one HEAD.
#[must_use]
pub fn task_id(path: &str, candidate: &Candidate) -> String {
    let digest = sha256_hex(path.as_bytes());
    format!(
        "{}-{}-L{}",
        candidate.tier.label(),
        &digest[..6],
        candidate.line
    )
}

/// SHA-256 over the ids in order, first 12 hex — the set's identity.
#[must_use]
pub fn task_set_hash(set: &TaskSet) -> String {
    let ids: Vec<&str> = set.picked.iter().map(|p| p.id.as_str()).collect();
    sha256_hex(ids.join("\n").as_bytes())[..12].to_owned()
}

#[cfg(test)]
mod tests {
    use super::{FileCandidates, Quota, quota, sample, seed_from_head, task_id, task_set_hash};
    use crate::core::bench::codebase::TaskTier;
    use crate::core::bench::codebase::masker::Candidate;

    fn cand(tier: TaskTier, line: usize) -> Candidate {
        Candidate {
            tier,
            byte_range: line * 10..line * 10 + 5,
            line,
            doc_comment: None,
        }
    }

    fn files(n_files: usize, per_file: usize) -> Vec<FileCandidates> {
        (0..n_files)
            .map(|f| FileCandidates {
                path: format!("src/f{f}.rs"),
                candidates: (1..=per_file)
                    .flat_map(|l| {
                        [
                            cand(TaskTier::InFile, l),
                            cand(TaskTier::FunctionBody, 100 + l),
                        ]
                    })
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn quota_is_two_thirds_in_file_rounded_up() {
        assert_eq!((quota(24).in_file, quota(24).function_body), (16, 8));
        assert_eq!((quota(12).in_file, quota(12).function_body), (8, 4));
        assert_eq!((quota(5).in_file, quota(5).function_body), (4, 1));
    }

    #[test]
    fn the_same_head_yields_the_same_set_and_a_different_head_does_not() {
        let a = sample(files(4, 10), quota(24), seed_from_head("abc123"));
        let b = sample(files(4, 10), quota(24), seed_from_head("abc123"));
        let c = sample(files(4, 10), quota(24), seed_from_head("def456"));
        let ids = |s: &super::TaskSet| s.picked.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&a), ids(&b));
        assert_ne!(ids(&a), ids(&c));
        assert_eq!(task_set_hash(&a), task_set_hash(&b));
        assert_ne!(task_set_hash(&a), task_set_hash(&c));
        assert_eq!(a.picked.len(), 24);
    }

    #[test]
    fn selection_is_stratified_across_files() {
        // 8 files × plenty of candidates: 24 picks must touch every file
        // rather than draining the first.
        let set = sample(files(8, 20), quota(24), 7);
        let mut per_file = std::collections::BTreeMap::new();
        for p in &set.picked {
            *per_file.entry(p.path.clone()).or_insert(0) += 1;
        }
        assert_eq!(per_file.len(), 8, "{per_file:?}");
        assert!(per_file.values().all(|&n| n <= 4), "{per_file:?}");
    }

    #[test]
    fn a_short_tier_is_reported_not_filled_from_the_other() {
        let mut only_in_file = files(2, 3);
        for f in &mut only_in_file {
            f.candidates.retain(|c| c.tier == TaskTier::InFile);
        }
        let set = sample(
            only_in_file,
            Quota {
                in_file: 4,
                function_body: 8,
            },
            1,
        );
        assert_eq!(set.picked.len(), 4);
        assert_eq!(
            set.shortfall,
            vec!["function_body: 0 of 8 requested (repo has 0 candidates)"]
        );
    }

    #[test]
    fn task_ids_are_stable_and_readable() {
        let id = task_id("src/lib.rs", &cand(TaskTier::FunctionBody, 42));
        assert!(id.starts_with("function_body-"), "{id}");
        assert!(id.ends_with("-L42"), "{id}");
        assert_eq!(id, task_id("src/lib.rs", &cand(TaskTier::FunctionBody, 42)));
        assert_ne!(
            id,
            task_id("src/main.rs", &cand(TaskTier::FunctionBody, 42))
        );
    }
}
