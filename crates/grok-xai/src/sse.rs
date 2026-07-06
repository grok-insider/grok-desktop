/// Minimal bounded decoder for the `data` fields used by xAI SSE responses.
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
    maximum: usize,
}

impl SseDecoder {
    pub(crate) const fn new(maximum: usize) -> Self {
        Self {
            buffer: Vec::new(),
            maximum,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ()> {
        if self.buffer.len().saturating_add(bytes.len()) > self.maximum {
            return Err(());
        }
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some((end, delimiter)) = delimiter(&self.buffer) {
            let block = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..delimiter);
            let text = std::str::from_utf8(&block).map_err(|_| ())?;
            let mut data = Vec::new();
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("data:") {
                    data.push(value.strip_prefix(' ').unwrap_or(value));
                }
            }
            if !data.is_empty() {
                events.push(data.join("\n"));
            }
        }
        Ok(events)
    }

    pub(crate) fn has_pending_data(&self) -> bool {
        !self.buffer.iter().all(u8::is_ascii_whitespace)
    }
}

fn delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_) | None, Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_fragmented_crlf_and_multiline_data() {
        let mut decoder = SseDecoder::new(100);
        assert!(decoder.push(b"data: one\r\n").expect("first").is_empty());
        assert_eq!(
            decoder
                .push(b"data: two\r\n\r\ndata: three\n\n")
                .expect("second"),
            vec!["one\ntwo", "three"]
        );
        assert!(!decoder.has_pending_data());
    }

    #[test]
    fn rejects_unbounded_event() {
        let mut decoder = SseDecoder::new(4);
        assert!(decoder.push(b"12345").is_err());
    }
}
