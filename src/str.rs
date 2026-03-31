/// Extension trait for safe string truncation at character boundaries.
///
/// Both methods avoid the "byte index is not a char boundary" panic that
/// occurs when naively slicing `&s[..n]` with a value derived from a byte
/// count or an unchecked user-supplied limit.
pub trait StrExt {
    /// Truncate to at most `n` **characters** (Unicode scalar values).
    ///
    /// Returns the original string unchanged if it is already short enough.
    fn truncate_chars(&self, n: usize) -> &str;

    /// Truncate to at most `n` **bytes**, snapping back to the nearest valid
    /// char boundary if `n` falls inside a multi-byte character.
    ///
    /// Returns the original string unchanged if it is already short enough.
    fn truncate_bytes(&self, n: usize) -> &str;
}

impl StrExt for str {
    fn truncate_chars(&self, n: usize) -> &str {
        match self.char_indices().nth(n) {
            Some((i, _)) => &self[..i],
            None => self,
        }
    }

    fn truncate_bytes(&self, n: usize) -> &str {
        &self[..self.floor_char_boundary(n.min(self.len()))]
    }
}
