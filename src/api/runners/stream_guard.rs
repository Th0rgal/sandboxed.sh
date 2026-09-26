//! Bounded, advisory repetition detection. Never run this work in the stream reader.
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MAX_WINDOW: usize = 4096;
const COMPARISON_BUDGET: usize = 2_000_000;
const TIME_BUDGET: Duration = Duration::from_millis(100);

#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    Clear,
    Repeated { needle: String, evidence: String },
    Inconclusive,
}

pub(super) fn tail(blocks: &BTreeMap<u32, String>, limit: usize) -> String {
    let mut chars: Vec<char> = blocks
        .values()
        .rev()
        .flat_map(|s| s.chars().rev())
        .take(limit.min(MAX_WINDOW))
        .collect();
    chars.reverse();
    chars.into_iter().collect()
}

#[derive(Clone, Copy)]
struct Occurrences {
    exemplar: usize,
    first: usize,
    end: usize,
    count: usize,
    noise: bool,
}

pub(crate) fn detect(
    text: &str,
    window: usize,
    min_len: usize,
    repeats: usize,
    cancel: &CancellationToken,
) -> Verdict {
    detect_budgeted(
        text,
        window,
        min_len,
        repeats,
        cancel,
        TIME_BUDGET,
        COMPARISON_BUDGET,
    )
}

fn detect_budgeted(
    text: &str,
    window: usize,
    min_len: usize,
    repeats: usize,
    cancel: &CancellationToken,
    time_budget: Duration,
    comparison_budget: usize,
) -> Verdict {
    let started = Instant::now();
    if cancel.is_cancelled() || time_budget.is_zero() {
        return Verdict::Inconclusive;
    }
    if min_len == 0 || repeats < 2 || window == 0 {
        return Verdict::Clear;
    }
    if min_len > 256 {
        return Verdict::Inconclusive;
    }
    let mut chars: Vec<char> = text.chars().rev().take(window.min(MAX_WINDOW)).collect();
    chars.reverse();
    let n = chars.len();
    if n < min_len.saturating_mul(repeats) {
        return Verdict::Clear;
    }
    let max_len = min_len.saturating_mul(2).min(256).min(n / repeats);
    // Rolling fingerprints are only an index. Every equal fingerprint is checked
    // against the original Unicode characters before it can count as a repeat.
    const BASE: u64 = 1_000_003;
    let mut prefix = vec![0u64; n + 1];
    let mut powers = vec![1u64; max_len + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i]
            .wrapping_mul(BASE)
            .wrapping_add(chars[i] as u64 + 1);
    }
    for i in 1..=max_len {
        powers[i] = powers[i - 1].wrapping_mul(BASE);
    }
    // A longer repeated substring necessarily has a repeated min-length
    // prefix. Eliminate unique prose before exploring candidate lengths.
    let hash_at = |start: usize, len: usize| {
        prefix[start + len].wrapping_sub(prefix[start].wrapping_mul(powers[len]))
    };
    let mut prefix_counts = HashMap::<u64, usize>::with_capacity(n);
    for start in 0..=n - min_len {
        *prefix_counts.entry(hash_at(start, min_len)).or_default() += 1;
    }
    let eligible: Vec<usize> = (0..=n - min_len)
        .filter(|&start| prefix_counts[&hash_at(start, min_len)] >= repeats)
        .collect();
    if eligible.is_empty() {
        return Verdict::Clear;
    }
    let mut comparisons = 0;
    let mut candidates = 0;
    for len in min_len..=max_len {
        let mut seen: HashMap<u64, Occurrences> = HashMap::with_capacity(eligible.len());
        for &start in &eligible {
            if start + len > n {
                break;
            }
            candidates += 1;
            if candidates % 128 == 0 && (cancel.is_cancelled() || started.elapsed() >= time_budget)
            {
                return Verdict::Inconclusive;
            }
            let hash = prefix[start + len].wrapping_sub(prefix[start].wrapping_mul(powers[len]));
            let Some(previous) = seen.get_mut(&hash) else {
                seen.insert(
                    hash,
                    Occurrences {
                        exemplar: start,
                        first: start,
                        end: start + len,
                        count: 1,
                        noise: false,
                    },
                );
                continue;
            };
            if previous.noise {
                continue;
            }
            for offset in 0..len {
                comparisons += 1;
                if comparisons > comparison_budget {
                    return Verdict::Inconclusive;
                }
                if chars[previous.exemplar + offset] != chars[start + offset] {
                    // Collision: fail open rather than guess or build an unbounded bucket.
                    return Verdict::Inconclusive;
                }
            }
            if start < previous.end {
                continue;
            } // Count whole, non-overlapping occurrences.
            if start <= previous.end.saturating_add(len) {
                previous.count += 1;
            } else {
                previous.count = 1;
                previous.first = start;
            }
            previous.end = start + len;
            if previous.count >= repeats {
                let needle: String = chars[start..start + len].iter().collect();
                let words: std::collections::HashSet<_> = needle
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|word| word.chars().count() >= 4)
                    .map(str::to_ascii_lowercase)
                    .collect();
                previous.noise = words.len() < 2;
                if !previous.noise {
                    return Verdict::Repeated {
                        needle,
                        evidence: chars[previous.first..start + len].iter().collect(),
                    };
                }
            }
        }
    }
    Verdict::Clear
}

fn analysis_slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(2))).clone()
}

/// One job per turn, at most two for the backend. No queue of stale snapshots.
pub(super) struct Guard {
    job: Option<tokio::task::JoinHandle<Verdict>>,
    cancel: CancellationToken,
    last_attempt: Option<Instant>,
    pub dirty: bool,
}
impl Guard {
    pub fn new() -> Self {
        Self {
            job: None,
            cancel: CancellationToken::new(),
            last_attempt: None,
            dirty: false,
        }
    }
    pub fn reset(&mut self) {
        self.cancel.cancel();
        let last_attempt = self.last_attempt;
        *self = Self::new();
        self.last_attempt = last_attempt;
    }
    pub fn start(&mut self, text: String, window: usize, min_len: usize, repeats: usize) {
        if !self.dirty
            || self.job.is_some()
            || self
                .last_attempt
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.last_attempt = Some(Instant::now());
        let Ok(permit) = analysis_slots().try_acquire_owned() else {
            tracing::debug!("Repetition analysis skipped: workers busy");
            return;
        };
        self.dirty = false;
        let cancel = self.cancel.clone();
        self.job = Some(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let start = Instant::now();
            let result = detect(&text, window, min_len, repeats, &cancel);
            tracing::debug!(
                elapsed_us = start.elapsed().as_micros() as u64,
                inconclusive = matches!(result, Verdict::Inconclusive),
                "Repetition analysis finished"
            );
            if matches!(result, Verdict::Inconclusive) && !cancel.is_cancelled() {
                tracing::warn!("Repetition analysis inconclusive; stream remains available");
            }
            result
        }));
    }
    pub async fn result(&mut self) -> Verdict {
        let Some(job) = self.job.as_mut() else {
            return std::future::pending().await;
        };
        let result = job.await.unwrap_or(Verdict::Inconclusive);
        self.job = None;
        result
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn generous(text: &str, min: usize, repeats: usize) -> Verdict {
        detect_budgeted(
            text,
            MAX_WINDOW,
            min,
            repeats,
            &CancellationToken::new(),
            Duration::from_secs(5),
            usize::MAX,
        )
    }
    // Deliberately simple exhaustive oracle, only used on small generated inputs.
    fn oracle(text: &str, min: usize, repeats: usize) -> bool {
        let chars: Vec<char> = text.chars().collect();
        for len in min..=min * 2 {
            if len > chars.len() {
                continue;
            }
            for start in 0..=chars.len() - len {
                let needle = &chars[start..start + len];
                let s: String = needle.iter().collect();
                let words: std::collections::HashSet<_> = s
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.chars().count() >= 4)
                    .map(str::to_ascii_lowercase)
                    .collect();
                if words.len() < 2 {
                    continue;
                }
                let mut pos = 0;
                let mut end = 0;
                let mut count = 0;
                while pos + len <= chars.len() {
                    if &chars[pos..pos + len] == needle {
                        count = if count == 0 || pos > end + len {
                            1
                        } else {
                            count + 1
                        };
                        if count >= repeats {
                            return true;
                        }
                        end = pos + len;
                        pos = end;
                    } else {
                        pos += 1;
                    }
                }
            }
        }
        false
    }
    #[test]
    fn matches_exhaustive_oracle_on_unicode_and_partial_repeats() {
        for phrase in [
            "alpha bravo ",
            "élève modèle ",
            "alpha alpha ",
            "yes, ",
            "表格 alpha bravo ",
        ] {
            for copies in 1..=4 {
                for gap in [
                    "",
                    "x",
                    "something unrelated separates these sections entirely",
                ] {
                    let text = (0..copies)
                        .map(|i| format!("{phrase}{}", if i == 1 { gap } else { "" }))
                        .collect::<String>();
                    assert_eq!(
                        matches!(generous(&text, 8, 3), Verdict::Repeated { .. }),
                        oracle(&text, 8, 3),
                        "{text}"
                    );
                }
            }
        }
    }
    #[test]
    fn report_is_fast_in_debug_without_false_positive() {
        let mut text = String::new();
        for i in 0..60 {
            text.push_str(&format!("| Garantie {i} | modèle vérifié dans Source/Correspondence{i}.lean | résultat distinct {} |\n",i*17));
        }
        let start = Instant::now();
        assert_eq!(
            detect(&text, 4096, 40, 3, &CancellationToken::new()),
            Verdict::Clear
        );
        assert!(start.elapsed() < Duration::from_millis(250));
    }
    #[test]
    fn deadlines_and_comparison_budget_are_not_matches() {
        let text = "Yielding pending your choice between the three options. ".repeat(20);
        assert_eq!(
            detect_budgeted(
                &text,
                4096,
                40,
                3,
                &CancellationToken::new(),
                Duration::ZERO,
                usize::MAX
            ),
            Verdict::Inconclusive
        );
        assert_eq!(
            detect_budgeted(
                &text,
                4096,
                40,
                3,
                &CancellationToken::new(),
                Duration::from_secs(5),
                0
            ),
            Verdict::Inconclusive
        );
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            detect_budgeted(
                &text,
                4096,
                40,
                3,
                &token,
                Duration::from_secs(5),
                usize::MAX
            ),
            Verdict::Inconclusive
        );
    }
    #[test]
    fn evidence_is_exact_and_tail_is_ordered_and_bounded() {
        let phrase = "Yielding pending your choice between the three options. ";
        let text = phrase.repeat(10);
        let Verdict::Repeated { needle, evidence } = generous(&text, 40, 3) else {
            panic!("missing repeat")
        };
        assert!(text.contains(&evidence));
        assert!(evidence.contains(&needle));
        let blocks = BTreeMap::from([(2, "finé".into()), (1, "début".into())]);
        assert_eq!(tail(&blocks, 6), "utfiné");
    }
    #[tokio::test(flavor = "current_thread")]
    async fn slow_worker_does_not_block_stream_or_cancellation_and_reset_discards_it() {
        let mut guard = Guard::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        guard.job = Some(tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Verdict::Clear
        }));
        started_rx.await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);
        tx.send("tool_result").await.unwrap();
        tx.send("terminal_result").await.unwrap();
        let start = Instant::now();
        for expected in ["tool_result", "terminal_result"] {
            tokio::select! {
                result=guard.result()=>panic!("worker unexpectedly completed: {result:?}"),
                value=rx.recv()=>assert_eq!(value,Some(expected)),
            }
        }
        let cancel = CancellationToken::new();
        cancel.cancel();
        tokio::select! { _=cancel.cancelled()=>{}, _=guard.result()=>panic!("worker won") }
        assert!(start.elapsed() < Duration::from_millis(250));
        guard.reset();
        assert!(guard.job.is_none());
        release_tx.send(()).unwrap();
    }
    #[tokio::test]
    async fn concurrency_and_cadence_are_bounded_without_queuing() {
        let slots = analysis_slots();
        let one = slots.clone().try_acquire_owned().unwrap();
        let two = slots.clone().try_acquire_owned().unwrap();
        let mut guard = Guard::new();
        guard.dirty = true;
        guard.start("normal text".into(), 4096, 40, 3);
        assert!(guard.job.is_none());
        assert!(guard.dirty);
        drop(one);
        drop(two);
        guard.last_attempt = Some(Instant::now() - Duration::from_secs(2));
        guard.start("normal text".into(), 4096, 40, 3);
        assert!(guard.job.is_some());
        assert_eq!(guard.result().await, Verdict::Clear);
        guard.dirty = true;
        guard.start("new text".into(), 4096, 40, 3);
        assert!(guard.job.is_none()); // No second analysis in the same second.
        guard.reset();
        guard.dirty = true;
        guard.start("replacement".into(), 4096, 40, 3);
        assert!(guard.job.is_none()); // A block reset cannot defeat the rate limit.
    }

    #[tokio::test]
    async fn bounded_reader_preserves_order_and_wakes_when_receiver_closes() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);
        let reader = tokio::task::spawn_blocking(move || {
            for i in 0..1000 {
                if tx.blocking_send(i).is_err() {
                    return;
                }
            }
        });
        for i in 0..300 {
            assert_eq!(rx.recv().await, Some(i));
        }
        rx.close();
        drop(rx);
        tokio::time::timeout(Duration::from_millis(250), reader)
            .await
            .unwrap()
            .unwrap();
    }
}
