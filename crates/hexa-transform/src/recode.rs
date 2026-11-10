//! Text recoding between code pages (XFRM-08), via `hexa_core::encoding`.

use hexa_core::encoding;

use crate::error::{Result, TransformError};

/// Checks that `id` is a known encoding id (see `hexa_core::encoding::ENCODINGS`).
pub fn check_encoding(id: &str) -> Result<()> {
    encoding::find(id)
        .map(|_| ())
        .ok_or_else(|| TransformError::Text(format!("unknown encoding '{id}'")))
}

/// Decodes `data` as `from` (invalid sequences become U+FFFD) and encodes
/// the text as `to`. Characters `to` cannot represent are an error.
pub fn recode(data: &[u8], from: &str, to: &str) -> Result<Vec<u8>> {
    check_encoding(from)?;
    check_encoding(to)?;
    let text = encoding::decode(data, from).map_err(|e| TransformError::Text(e.to_string()))?;
    encoding::encode(&text, to).map_err(|e| TransformError::Text(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_to_cp1251_and_back() {
        let utf8 = "Привет, мир".as_bytes();
        let cp = recode(utf8, "utf-8", "windows-1251").unwrap();
        assert_eq!(cp[0], 0xCF);
        assert_eq!(cp.len(), "Привет, мир".chars().count());
        assert_eq!(recode(&cp, "windows-1251", "utf-8").unwrap(), utf8);
    }

    #[test]
    fn utf16_and_errors() {
        let le = recode(b"Hi", "ascii", "utf-16le").unwrap();
        assert_eq!(le, [b'H', 0, b'i', 0]);
        assert_eq!(
            recode(&le, "utf-16le", "utf-16be").unwrap(),
            [0, b'H', 0, b'i']
        );
        assert!(recode("ж".as_bytes(), "utf-8", "ascii").is_err());
        assert!(recode(b"x", "nope", "utf-8").is_err());
    }
}
