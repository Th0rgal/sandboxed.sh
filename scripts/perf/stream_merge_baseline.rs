fn suffix_prefix_overlap_len(existing: &str, incoming: &str) -> usize {
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

    let overlap = suffix_prefix_overlap_len(buffer, fragment);
    buffer.push_str(&fragment[overlap..]);
}

fn main() {
    for (n, m) in [(10000, 20), (100000, 20), (100000, 2000), (1000000, 2000)] {
        let base = "évaluation français propriétés distinctes\n".repeat(n / 42 + 1);
        let fragment = "z".repeat(m);
        let start = std::time::Instant::now();
        let mut b = base.clone();
        merge_stream_fragment(&mut b, &fragment);
        println!(
            "bytes={} fragment={} elapsed_us={}",
            base.len(),
            m,
            start.elapsed().as_micros()
        );
    }
}
