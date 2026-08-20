use std::collections::VecDeque;

const TRUNCATION_MARKER: &[u8] = b"\n... [AtrisBridge output truncated; tail preserved] ...\n";
const MAX_HEAD_BYTES: usize = 128 * 1024;

#[derive(Debug)]
pub(crate) struct TailPreservingBuffer {
    max_bytes: usize,
    head_target: usize,
    tail_capacity: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: usize,
    forced_truncated: bool,
}

impl TailPreservingBuffer {
    pub(crate) fn new(max_bytes: usize) -> Self {
        let head_target = if max_bytes == 0 {
            0
        } else {
            (max_bytes / 8).max(1).min(MAX_HEAD_BYTES).min(max_bytes)
        };
        Self {
            max_bytes,
            head_target,
            tail_capacity: max_bytes.saturating_sub(head_target),
            head: Vec::with_capacity(head_target),
            tail: VecDeque::with_capacity(max_bytes.saturating_sub(head_target)),
            total_bytes: 0,
            forced_truncated: false,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());

        let head_remaining = self.head_target.saturating_sub(self.head.len());
        let head_take = head_remaining.min(bytes.len());
        if head_take > 0 {
            self.head.extend_from_slice(&bytes[..head_take]);
        }

        let remaining = &bytes[head_take..];
        if remaining.is_empty() || self.tail_capacity == 0 {
            return;
        }
        if remaining.len() >= self.tail_capacity {
            self.tail.clear();
            self.tail.extend(
                remaining[remaining.len() - self.tail_capacity..]
                    .iter()
                    .copied(),
            );
            return;
        }

        let overflow = self
            .tail
            .len()
            .saturating_add(remaining.len())
            .saturating_sub(self.tail_capacity);
        for _ in 0..overflow {
            self.tail.pop_front();
        }
        self.tail.extend(remaining.iter().copied());
    }

    pub(crate) fn mark_truncated(&mut self) {
        self.forced_truncated = true;
    }

    pub(crate) fn is_truncated(&self) -> bool {
        self.forced_truncated || self.total_bytes > self.max_bytes
    }

    pub(crate) fn finish(self) -> (Vec<u8>, bool) {
        let truncated = self.is_truncated();
        if self.max_bytes == 0 {
            return (Vec::new(), truncated);
        }
        if !truncated {
            let mut bytes = self.head;
            bytes.extend(self.tail);
            return (bytes, false);
        }
        if self.max_bytes <= TRUNCATION_MARKER.len() {
            return (TRUNCATION_MARKER[..self.max_bytes].to_vec(), true);
        }

        let head_budget = self
            .head
            .len()
            .min(self.max_bytes.saturating_sub(TRUNCATION_MARKER.len()));
        let tail_budget = self
            .max_bytes
            .saturating_sub(head_budget)
            .saturating_sub(TRUNCATION_MARKER.len());
        let tail_skip = self.tail.len().saturating_sub(tail_budget);

        let mut bytes = Vec::with_capacity(self.max_bytes);
        bytes.extend_from_slice(&self.head[..head_budget]);
        bytes.extend_from_slice(TRUNCATION_MARKER);
        bytes.extend(self.tail.into_iter().skip(tail_skip));
        (bytes, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_under_limit_is_preserved_exactly() {
        let mut capture = TailPreservingBuffer::new(64);
        capture.push(b"first ");
        capture.push(b"second");

        let (bytes, truncated) = capture.finish();
        assert!(!truncated);
        assert_eq!(bytes, b"first second");
    }

    #[test]
    fn truncated_content_preserves_head_and_latest_tail() {
        let mut capture = TailPreservingBuffer::new(96);
        capture.push(b"HEAD-0123456789");
        capture.push(&vec![b'x'; 160]);
        capture.push(b"-LATEST-TAIL");

        let (bytes, truncated) = capture.finish();
        assert!(truncated);
        assert!(bytes.len() <= 96);
        assert!(bytes.starts_with(b"HEAD-"));
        assert!(bytes
            .windows(TRUNCATION_MARKER.len())
            .any(|window| window == TRUNCATION_MARKER));
        assert!(bytes.ends_with(b"-LATEST-TAIL"));
    }

    #[test]
    fn forced_truncation_marks_incomplete_capture() {
        let mut capture = TailPreservingBuffer::new(96);
        capture.push(b"partial output");
        capture.mark_truncated();

        let (bytes, truncated) = capture.finish();
        assert!(truncated);
        assert!(bytes
            .windows(TRUNCATION_MARKER.len())
            .any(|window| window == TRUNCATION_MARKER));
    }

    #[test]
    fn zero_sized_capture_stays_empty() {
        let mut capture = TailPreservingBuffer::new(0);
        capture.push(b"ignored");
        let (bytes, truncated) = capture.finish();
        assert!(truncated);
        assert!(bytes.is_empty());
    }
}
