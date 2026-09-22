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

pub(crate) fn merge_stream_fragment(buffer: &mut String, fragment: &str) {
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

    let overlap = fast_overlap(buffer, fragment);
    buffer.push_str(&fragment[overlap..]);
}

fn fast_overlap(existing: &str, incoming: &str) -> usize {
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
fn main() {
    let alphabet = ["a", "b", "é", "🙂"];
    let mut strings = vec![String::new()];
    let mut level = vec![String::new()];
    for _ in 0..4 {
        let mut next = vec![];
        for x in &level {
            for c in alphabet {
                next.push(format!("{x}{c}"))
            }
        }
        strings.extend(next.clone());
        level = next;
    }
    for a in &strings {
        for b in &strings {
            assert_eq!(old_overlap(a, b), fast_overlap(a, b), "{:?} {:?}", a, b)
        }
    }
    println!("oracle_pairs={}", strings.len() * strings.len());
    for (n, m) in [(10000, 20), (100000, 20), (100000, 2000), (1000000, 2000)] {
        let base = "évaluation français propriétés distinctes\n".repeat(n / 42 + 1);
        let fragment = "z".repeat(m);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            std::hint::black_box(fast_overlap(&base, &fragment));
        }
        println!(
            "bytes={} fragment={} fast_avg_us={}",
            base.len(),
            m,
            start.elapsed().as_micros() / 100
        );
    }
}
