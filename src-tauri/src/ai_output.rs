const TRUNCATION_MARKER: &[u8] = b"\n... [AtrisBridge output truncated; tail preserved] ...\n";
const MAX_HEAD_BYTES: usize = 128 * 1024;

pub(crate) fn append_tail_preserving(
    stored: &mut Vec<u8>,
    truncated: &mut bool,
    bytes: &[u8],
    max: usize,
) {
    if bytes.is_empty() {
        return;
    }
    if max == 0 {
        stored.clear();
        *truncated = true;
        return;
    }

    if !*truncated && stored.len().saturating_add(bytes.len()) <= max {
        stored.extend_from_slice(bytes);
        return;
    }

    if !*truncated {
        let mut combined = Vec::with_capacity(stored.len().saturating_add(bytes.len()));
        combined.extend_from_slice(stored);
        combined.extend_from_slice(bytes);
        *stored = truncate_with_tail(&combined, max);
        *truncated = true;
        return;
    }

    if max <= TRUNCATION_MARKER.len() {
        stored.clear();
        stored.extend_from_slice(&TRUNCATION_MARKER[..max]);
        return;
    }

    let marker_index = find_marker(stored).unwrap_or_else(|| stored.len().min(head_budget(max)));
    let head = stored[..marker_index].to_vec();
    let existing_tail_start = marker_index
        .saturating_add(TRUNCATION_MARKER.len())
        .min(stored.len());
    let mut tail = Vec::with_capacity(
        stored
            .len()
            .saturating_sub(existing_tail_start)
            .saturating_add(bytes.len()),
    );
    tail.extend_from_slice(&stored[existing_tail_start..]);
    tail.extend_from_slice(bytes);

    let head_budget = head.len().min(head_budget(max));
    let tail_budget = max
        .saturating_sub(head_budget)
        .saturating_sub(TRUNCATION_MARKER.len());
    let tail_start = tail.len().saturating_sub(tail_budget);

    stored.clear();
    stored.extend_from_slice(&head[..head_budget]);
    stored.extend_from_slice(TRUNCATION_MARKER);
    stored.extend_from_slice(&tail[tail_start..]);
}

fn truncate_with_tail(bytes: &[u8], max: usize) -> Vec<u8> {
    if bytes.len() <= max {
        return bytes.to_vec();
    }
    if max <= TRUNCATION_MARKER.len() {
        return TRUNCATION_MARKER[..max].to_vec();
    }

    let head_budget = head_budget(max).min(bytes.len());
    let tail_budget = max
        .saturating_sub(head_budget)
        .saturating_sub(TRUNCATION_MARKER.len());
    let tail_start = bytes.len().saturating_sub(tail_budget);

    let mut result = Vec::with_capacity(max);
    result.extend_from_slice(&bytes[..head_budget]);
    result.extend_from_slice(TRUNCATION_MARKER);
    result.extend_from_slice(&bytes[tail_start..]);
    result
}

fn head_budget(max: usize) -> usize {
    (max / 8)
        .max(1)
        .min(MAX_HEAD_BYTES)
        .min(max.saturating_sub(TRUNCATION_MARKER.len()))
}

fn find_marker(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(TRUNCATION_MARKER.len())
        .position(|window| window == TRUNCATION_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_under_limit_is_preserved_exactly() {
        let mut stored = Vec::new();
        let mut truncated = false;
        append_tail_preserving(&mut stored, &mut truncated, b"first ", 64);
        append_tail_preserving(&mut stored, &mut truncated, b"second", 64);

        assert_eq!(stored, b"first second");
        assert!(!truncated);
    }

    #[test]
    fn overflow_preserves_head_and_latest_tail() {
        let mut stored = Vec::new();
        let mut truncated = false;
        append_tail_preserving(&mut stored, &mut truncated, b"HEAD-0123456789", 96);
        append_tail_preserving(&mut stored, &mut truncated, &vec![b'x'; 160], 96);
        append_tail_preserving(&mut stored, &mut truncated, b"-LATEST-TAIL", 96);

        assert!(truncated);
        assert!(stored.len() <= 96);
        assert!(stored.starts_with(b"HEAD-"));
        assert!(find_marker(&stored).is_some());
        assert!(stored.ends_with(b"-LATEST-TAIL"));
    }

    #[test]
    fn repeated_overflow_keeps_the_newest_tail() {
        let mut stored = Vec::new();
        let mut truncated = false;
        append_tail_preserving(&mut stored, &mut truncated, &vec![b'a'; 120], 80);
        append_tail_preserving(&mut stored, &mut truncated, b"old-tail", 80);
        append_tail_preserving(&mut stored, &mut truncated, b"new-tail", 80);

        assert!(truncated);
        assert!(stored.len() <= 80);
        assert!(stored.ends_with(b"new-tail"));
    }

    #[test]
    fn zero_sized_output_is_empty_and_truncated() {
        let mut stored = Vec::new();
        let mut truncated = false;
        append_tail_preserving(&mut stored, &mut truncated, b"ignored", 0);

        assert!(stored.is_empty());
        assert!(truncated);
    }
}
