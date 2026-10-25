//! Shared, byte-oriented SSE record framing. Limits are supplied by each route.

#[derive(Default)]
pub(crate) struct Boundary {
    swallow_lf: bool,
    position: usize,
    #[cfg(test)]
    examined_bytes: usize,
}

impl Boundary {
    fn byte(&mut self, buffer: &[u8], position: usize) -> Option<u8> {
        let byte = buffer.get(position).copied();
        #[cfg(test)]
        if byte.is_some() {
            self.examined_bytes += 1;
        }
        byte
    }

    /// After `None`, only append to the retained buffer. After `Some((end, len))`,
    /// drain exactly `..end + len` before calling again. Replace this boundary
    /// with `Default::default()` when clearing or replacing the buffer.
    /// A pending optional LF after a record-ending CR is removed here.
    pub(crate) fn separator(&mut self, buffer: &mut Vec<u8>) -> Option<(usize, usize)> {
        if self.swallow_lf {
            let first = self.byte(buffer, 0)?;
            if first == b'\n' {
                buffer.remove(0);
            }
            self.swallow_lf = false;
        }
        let mut position = self.position;
        while position < buffer.len() {
            if !matches!(self.byte(buffer, position), Some(b'\r' | b'\n')) {
                position += 1;
                continue;
            }
            let first_len = if self.byte(buffer, position) == Some(b'\r')
                && self.byte(buffer, position + 1) == Some(b'\n')
            {
                2
            } else {
                1
            };
            let blank = position + first_len;
            if blank == buffer.len() {
                // The next byte decides whether this line ending starts the
                // separator (and whether a trailing CR includes an LF).
                self.position = position;
                return None;
            }
            if matches!(self.byte(buffer, blank), Some(b'\r' | b'\n')) {
                let second_len = if self.byte(buffer, blank) == Some(b'\r')
                    && self.byte(buffer, blank + 1) == Some(b'\n')
                {
                    2
                } else {
                    1
                };
                // A CR terminates a line immediately. If its optional LF has
                // not arrived, remember to consume it across the next chunk.
                self.swallow_lf =
                    self.byte(buffer, blank) == Some(b'\r') && blank + second_len == buffer.len();
                self.position = 0;
                return Some((position, first_len + second_len));
            }
            position = blank;
        }
        self.position = position;
        None
    }
}

pub(crate) fn data(record: &str) -> String {
    data_if_present(record).unwrap_or_default()
}

/// Distinguish records without data fields from an explicitly empty data field.
pub(crate) fn data_if_present(record: &str) -> Option<String> {
    let record = record.strip_prefix('\u{feff}').unwrap_or(record);
    let normalized = record.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .peekable();
    lines.peek()?;
    Some(lines.collect::<Vec<_>>().join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_record_examines_linear_bytes() {
        let mut boundary = Boundary::default();
        let mut buffer = Vec::new();
        for _ in 0..4096 {
            buffer.push(b'x');
            assert_eq!(boundary.separator(&mut buffer), None);
        }
        buffer.extend_from_slice(b"\r\n\r\n");
        assert_eq!(boundary.separator(&mut buffer), Some((4096, 4)));
        assert!(
            boundary.examined_bytes <= 8 * buffer.len(),
            "examined {} bytes for {} input bytes",
            boundary.examined_bytes,
            buffer.len()
        );
    }

    #[test]
    fn every_line_ending_pair_and_chunk_partition() {
        for first in ["\r", "\n", "\r\n"] {
            for second in ["\r", "\n", "\r\n"] {
                // Adjacent CR + LF is one line ending, not an empty line.
                let extra = if first == "\r" && second == "\n" {
                    "\n"
                } else {
                    ""
                };
                let stream = format!("é{first}{second}{extra}z{first}{second}{extra}");
                let bytes = stream.as_bytes();
                for partition in 0..1usize << (bytes.len() - 1) {
                    let mut boundary = Boundary::default();
                    let mut buffer = Vec::new();
                    let mut records = Vec::new();
                    let mut start = 0;
                    for end in 1..=bytes.len() {
                        if end != bytes.len() && partition & (1 << (end - 1)) == 0 {
                            continue;
                        }
                        buffer.extend_from_slice(&bytes[start..end]);
                        start = end;
                        while let Some((end, len)) = boundary.separator(&mut buffer) {
                            records.push(String::from_utf8(buffer[..end].to_vec()).unwrap());
                            buffer.drain(..end + len);
                        }
                        // Empty network chunks must not change framing state.
                        assert_eq!(boundary.separator(&mut buffer), None);
                    }
                    assert_eq!(
                        records,
                        ["é", "z"],
                        "{first:?}/{second:?}, partition {partition}"
                    );
                    assert!(buffer.is_empty());
                }
            }
        }
    }

    #[test]
    fn empty_records_and_explicit_buffer_reset() {
        let mut boundary = Boundary::default();
        let mut buffer = b"\r\r".to_vec();
        assert_eq!(boundary.separator(&mut buffer), Some((0, 2)));
        buffer.drain(..2);
        assert_eq!(boundary.separator(&mut buffer), None);
        buffer.extend_from_slice(b"\npartial");
        assert_eq!(boundary.separator(&mut buffer), None);
        assert_eq!(buffer, b"partial");
        buffer.clear();
        boundary = Boundary::default();
        buffer.extend_from_slice(b"x\n\n");
        assert_eq!(boundary.separator(&mut buffer), Some((1, 2)));
    }
}
