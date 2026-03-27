/// Splits batched stdin bytes into individual escape sequences and paste events.
/// Handles multi-byte UTF-8, CSI/SS3 sequences, and bracketed paste.
pub struct StdinBuffer {
    buf: Vec<u8>,
    in_paste: bool,
    paste_buf: Vec<u8>,
}

pub enum StdinEvent {
    /// A single key or escape sequence.
    Sequence(Vec<u8>),
    /// Bracketed paste content (decoded to UTF-8).
    Paste(String),
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

impl Default for StdinBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl StdinBuffer {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(256),
            in_paste: false,
            paste_buf: Vec::new(),
        }
    }

    /// Process incoming bytes and return parsed events.
    pub fn process(&mut self, data: &[u8]) -> Vec<StdinEvent> {
        self.buf.extend_from_slice(data);
        let mut events = Vec::new();

        while !self.buf.is_empty() {
            if self.in_paste {
                if let Some(end_pos) = find_subsequence(&self.buf, PASTE_END) {
                    self.paste_buf.extend_from_slice(&self.buf[..end_pos]);
                    let content = String::from_utf8_lossy(&self.paste_buf).into_owned();
                    events.push(StdinEvent::Paste(content));
                    self.paste_buf.clear();
                    self.in_paste = false;
                    self.buf.drain(..end_pos + PASTE_END.len());
                } else {
                    // Paste not complete yet — buffer everything
                    self.paste_buf.extend_from_slice(&self.buf);
                    self.buf.clear();
                    break;
                }
            } else if self.buf.starts_with(PASTE_START) {
                self.buf.drain(..PASTE_START.len());
                self.in_paste = true;
            } else if self.buf[0] == 0x1B {
                // Escape sequence
                if self.buf.len() == 1 {
                    // Lone ESC — could be start of sequence or actual Escape key.
                    // In a real implementation, we'd use a timeout. For now, emit it.
                    events.push(StdinEvent::Sequence(vec![0x1B]));
                    self.buf.drain(..1);
                } else if self.buf.len() >= 2 && self.buf[1] == b'[' {
                    // CSI sequence: ESC [ params final
                    if let Some(end) = find_csi_end(&self.buf[2..]) {
                        let seq_len = 2 + end + 1;
                        let seq = self.buf[..seq_len].to_vec();
                        events.push(StdinEvent::Sequence(seq));
                        self.buf.drain(..seq_len);
                    } else {
                        // Incomplete CSI — wait for more data
                        break;
                    }
                } else if self.buf.len() >= 2 && self.buf[1] == b'O' {
                    // SS3 sequence: ESC O <char>
                    if self.buf.len() >= 3 {
                        let seq = self.buf[..3].to_vec();
                        events.push(StdinEvent::Sequence(seq));
                        self.buf.drain(..3);
                    } else {
                        break;
                    }
                } else if self.buf.len() >= 2 && self.buf[1] == b'_' {
                    // APC sequence: ESC _ ... BEL/ST
                    if let Some(end) = find_apc_end(&self.buf[2..]) {
                        let seq_len = 2 + end + 1;
                        let seq = self.buf[..seq_len].to_vec();
                        events.push(StdinEvent::Sequence(seq));
                        self.buf.drain(..seq_len);
                    } else {
                        break;
                    }
                } else {
                    // Two-byte escape (e.g., ESC + letter for Alt combinations)
                    let seq = self.buf[..2].to_vec();
                    events.push(StdinEvent::Sequence(seq));
                    self.buf.drain(..2);
                }
            } else {
                // Regular byte(s) — could be UTF-8 multi-byte
                let ch_len = utf8_char_len(self.buf[0]);
                if self.buf.len() >= ch_len {
                    let seq = self.buf[..ch_len].to_vec();
                    events.push(StdinEvent::Sequence(seq));
                    self.buf.drain(..ch_len);
                } else {
                    // Incomplete UTF-8 — wait for more data
                    break;
                }
            }
        }

        events
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_csi_end(data: &[u8]) -> Option<usize> {
    for (i, &b) in data.iter().enumerate() {
        if (0x40..=0x7E).contains(&b) {
            return Some(i);
        }
    }
    None
}

fn find_apc_end(data: &[u8]) -> Option<usize> {
    for (i, &b) in data.iter().enumerate() {
        if b == 0x07 {
            return Some(i);
        }
        // ST = ESC + backslash
        if b == 0x5C && i > 0 && data[i - 1] == 0x1B {
            return Some(i);
        }
    }
    None
}

fn utf8_char_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1, // invalid, consume one byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_ascii() {
        let mut buf = StdinBuffer::new();
        let events = buf.process(b"a");
        assert_eq!(events.len(), 1);
        match &events[0] {
            StdinEvent::Sequence(s) => assert_eq!(s, b"a"),
            _ => panic!("expected Sequence"),
        }
    }

    #[test]
    fn csi_sequence() {
        let mut buf = StdinBuffer::new();
        // Up arrow: ESC [ A
        let events = buf.process(b"\x1b[A");
        assert_eq!(events.len(), 1);
        match &events[0] {
            StdinEvent::Sequence(s) => assert_eq!(s, b"\x1b[A"),
            _ => panic!("expected Sequence"),
        }
    }

    #[test]
    fn bracketed_paste() {
        let mut buf = StdinBuffer::new();
        let events = buf.process(b"\x1b[200~hello world\x1b[201~");
        assert_eq!(events.len(), 1);
        match &events[0] {
            StdinEvent::Paste(s) => assert_eq!(s, "hello world"),
            _ => panic!("expected Paste"),
        }
    }

    #[test]
    fn split_delivery() {
        let mut buf = StdinBuffer::new();
        // CSI sequence split across two deliveries
        let events1 = buf.process(b"\x1b[");
        assert!(events1.is_empty());
        let events2 = buf.process(b"A");
        assert_eq!(events2.len(), 1);
    }

    #[test]
    fn utf8_multibyte() {
        let mut buf = StdinBuffer::new();
        let events = buf.process("ñ".as_bytes());
        assert_eq!(events.len(), 1);
        match &events[0] {
            StdinEvent::Sequence(s) => assert_eq!(s, "ñ".as_bytes()),
            _ => panic!("expected Sequence"),
        }
    }
}
