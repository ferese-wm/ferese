//! Password storage has a fixed address and is wiped before its locked mapping is released.
use std::io;

use memmap2::{Advice, MmapMut};

const CAPACITY: usize = 4096;

pub(super) struct Password {
    bytes: MmapMut,
    length: usize,
}

impl Password {
    /// Fails if the mapping cannot be locked or excluded from core dumps.
    pub(super) fn try_new() -> io::Result<Self> {
        let bytes = MmapMut::map_anon(CAPACITY)?;
        bytes.lock()?;
        bytes.advise(Advice::DontDump)?;
        Ok(Self { bytes, length: 0 })
    }

    /// Rejects controls and oversized input without changing the existing password.
    #[must_use]
    pub(super) fn push_str(&mut self, input: &str) -> bool {
        if input.chars().any(char::is_control) || input.len() >= CAPACITY - self.length {
            return false;
        }
        let end = self.length + input.len();
        self.bytes[self.length..end].copy_from_slice(input.as_bytes());
        self.length = end;
        true
    }

    /// Reads the private worker pipe directly into locked storage, without a String copy.
    pub(super) fn read_from(reader: &mut impl io::Read) -> io::Result<Self> {
        let mut length = [0; 2];
        reader.read_exact(&mut length)?;
        let length = usize::from(u16::from_le_bytes(length));
        if length == 0 || length >= CAPACITY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid password length",
            ));
        }
        let mut password = Self::try_new()?;
        reader.read_exact(&mut password.bytes[..length])?;
        let text = std::str::from_utf8(&password.bytes[..length])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid password encoding"))?;
        if text.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid password input",
            ));
        }
        password.length = length;
        Ok(password)
    }

    pub(super) fn pop(&mut self) {
        use zeroize::Zeroize;
        if let Some((start, _)) = self.as_str().char_indices().next_back() {
            self.bytes[start..self.length].zeroize();
            self.length = start;
        }
    }

    pub(super) fn clear(&mut self) {
        use zeroize::Zeroize;
        self.bytes[..].zeroize();
        self.length = 0;
    }

    #[must_use]
    pub(super) fn is_empty(&self) -> bool {
        self.length == 0
    }

    #[must_use]
    pub(super) fn mask(&self) -> String {
        "•".repeat(self.as_str().chars().count())
    }

    #[must_use]
    pub(super) fn as_bytes_with_nul(&self) -> &[u8] {
        &self.bytes[..=self.length]
    }

    #[must_use]
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.length])
            .expect("password edits preserve UTF-8 boundaries")
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        self.clear();
        // Unmapping releases the memory lock after the wipe.
    }
}

#[cfg(test)]
mod tests {
    use super::Password;

    #[test]
    fn editing_wipes_removed_bytes_and_keeps_a_terminated_utf8_secret() {
        let mut password = Password::try_new().expect("small anonymous mapping can be locked");
        assert!(password.push_str("aé🦀"));
        assert_eq!(password.mask(), "•••");
        password.pop();
        assert_eq!(password.as_bytes_with_nul(), b"a\xc3\xa9\0");
        assert!(
            password.bytes[password.length..]
                .iter()
                .all(|byte| *byte == 0)
        );
        password.clear();
        assert!(password.is_empty());
        assert!(password.bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn worker_pipe_is_bounded_and_validated() {
        let mut valid = &b"\x03\x00a\xc3\xa9"[..];
        let password = Password::read_from(&mut valid).expect("valid UTF-8 worker input");
        assert_eq!(password.as_bytes_with_nul(), b"a\xc3\xa9\0");
        for bytes in [
            &b"\x00\x00"[..],
            &b"\xff\xff"[..],
            &b"\x01\x00\0"[..],
            &b"\x01\x00\xff"[..],
            &b"\x03\x00a"[..],
        ] {
            assert!(Password::read_from(&mut &*bytes).is_err());
        }
    }

    #[test]
    fn rejected_edits_leave_the_secret_unchanged() {
        let mut password = Password::try_new().expect("small anonymous mapping can be locked");
        assert!(password.push_str("test"));
        for input in ["bad\0input", "\n", &"x".repeat(super::CAPACITY)] {
            assert!(!password.push_str(input));
            assert_eq!(password.as_bytes_with_nul(), b"test\0");
        }
        password.clear();
        assert!(password.push_str(&"x".repeat(super::CAPACITY - 1)));
        assert!(!password.push_str("x"));
        assert_eq!(password.as_bytes_with_nul().len(), super::CAPACITY);
    }
}
