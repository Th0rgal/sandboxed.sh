// Diagnostic fixture: pre-fix vs 1b704fea merge, equal clone/append work.
// rustc scripts/perf/stream_merge_compare.rs -o /tmp/merge-compare
// Debug build; adversarial cases are not end-to-end inference timings.
fn old_overlap(existing: &str, incoming: &str) -> usize {
    let max_chars = existing.chars().count().min(incoming.chars().count());
    for overlap_chars in (1..=max_chars).rev() {
        let existing_start = existing
            .char_indices()
            .nth(existing.chars().count() - overlap_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        let incoming_end = incoming
            .char_indices()
            .nth(overlap_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(incoming.len());
        if existing[existing_start..] == incoming[..incoming_end] {
            return incoming_end;
        }
    }
    0
}

pub(crate) fn old_merge(buffer: &mut String, fragment: &str) {
    if fragment.is_empty() {
        return;
    }
    if buffer.is_empty() || fragment.starts_with(buffer.as_str()) {
        *buffer = fragment.to_string();
        return;
    }
    if buffer.starts_with(fragment) {
        return;
    }

    let overlap = old_overlap(buffer, fragment);
    buffer.push_str(&fragment[overlap..]);
}

fn new_overlap(existing: &str, incoming: &str) -> usize {
    let p = incoming.as_bytes();
    if p.is_empty() {
        return 0;
    }
    let mut pi = vec![0; p.len()];
    for i in 1..p.len() {
        let mut j = pi[i - 1];
        while j > 0 && p[i] != p[j] {
            j = pi[j - 1]
        }
        if p[i] == p[j] {
            j += 1
        }
        pi[i] = j;
    }
    let start = existing.len().saturating_sub(p.len());
    let mut j = 0;
    for &b in &existing.as_bytes()[start..] {
        while j > 0 && (j == p.len() || b != p[j]) {
            j = pi[j - 1]
        }
        if b == p[j] {
            j += 1
        }
    }
    j
}

pub(crate) fn new_merge(buffer: &mut String, fragment: &str) {
    if fragment.is_empty() {
        return;
    }
    if buffer.is_empty() || fragment.starts_with(buffer.as_str()) {
        *buffer = fragment.to_string();
        return;
    }
    if buffer.starts_with(fragment) {
        return;
    }

    let overlap = new_overlap(buffer, fragment);
    buffer.push_str(&fragment[overlap..]);
}

fn main() {
    for (n, m) in [(10000, 20), (100000, 20), (100000, 2000), (1000000, 2000)] {
        let base = "évaluation français propriétés distinctes\n".repeat(n / 42 + 1);
        let fragment = "z".repeat(m);
        let mut outcomes = vec![];
        for (name, f) in [
            ("old", old_merge as fn(&mut String, &str)),
            ("new", new_merge as fn(&mut String, &str)),
        ] {
            let start = std::time::Instant::now();
            let mut b = base.clone();
            f(&mut b, &fragment);
            println!(
                "{} bytes={} fragment={} elapsed_us={}",
                name,
                base.len(),
                m,
                start.elapsed().as_micros()
            );
            outcomes.push(b);
        }
        assert_eq!(outcomes[0], outcomes[1]);
    }
}
