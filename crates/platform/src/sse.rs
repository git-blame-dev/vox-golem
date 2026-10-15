//! Shared, byte-oriented SSE record framing. Limits are supplied by each route.

#[derive(Default)]
pub(crate) struct Boundary {
    swallow_lf: bool,
}

impl Boundary {
    pub(crate) fn separator(&mut self, buffer: &mut Vec<u8>) -> Option<(usize, usize)> {
        if self.swallow_lf {
            let first = *buffer.first()?;
            if first == b'\n' {
                buffer.remove(0);
            }
            self.swallow_lf = false;
        }
        let mut position = 0;
        while position < buffer.len() {
            if !matches!(buffer[position], b'\r' | b'\n') {
                position += 1;
                continue;
            }
            let first_len = if buffer[position] == b'\r' && buffer.get(position + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            };
            let blank = position + first_len;
            if matches!(buffer.get(blank), Some(b'\r' | b'\n')) {
                let second_len = if buffer[blank] == b'\r' && buffer.get(blank + 1) == Some(&b'\n')
                {
                    2
                } else {
                    1
                };
                // A CR terminates a line immediately. If its optional LF has
                // not arrived, remember to consume it across the next chunk.
                self.swallow_lf = buffer[blank] == b'\r' && blank + second_len == buffer.len();
                return Some((position, first_len + second_len));
            }
            position = blank;
        }
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
