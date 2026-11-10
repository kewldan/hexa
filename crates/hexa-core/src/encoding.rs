//! Text encodings for the text column, typing and search (DISP-09, EDIT-02).

use serde::Serialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EncodingKind {
    SingleByte,
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodingInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    pub kind: EncodingKind,
}

const fn info(
    id: &'static str,
    name: &'static str,
    group: &'static str,
    kind: EncodingKind,
) -> EncodingInfo {
    EncodingInfo {
        id,
        name,
        group,
        kind,
    }
}

use EncodingKind::{SingleByte as SB, Utf8, Utf16Be, Utf16Le, Utf32Be, Utf32Le};

/// Every supported encoding. Single-byte ones other than the built-in tables
/// are resolved through `encoding_rs` by their WHATWG label.
pub const ENCODINGS: &[EncodingInfo] = &[
    info("ascii", "ASCII", "Unicode", SB),
    info("latin1", "Latin-1 (ISO-8859-1)", "Western", SB),
    info("utf-8", "UTF-8", "Unicode", Utf8),
    info("utf-16le", "UTF-16 LE", "Unicode", Utf16Le),
    info("utf-16be", "UTF-16 BE", "Unicode", Utf16Be),
    info("utf-32le", "UTF-32 LE", "Unicode", Utf32Le),
    info("utf-32be", "UTF-32 BE", "Unicode", Utf32Be),
    info("ebcdic-037", "EBCDIC 037", "IBM", SB),
    info("cp437", "CP437 (DOS, box drawing)", "DOS", SB),
    info("ibm866", "CP866 (DOS Cyrillic)", "DOS", SB),
    info(
        "windows-1250",
        "Windows-1250 Central European",
        "Windows",
        SB,
    ),
    info("windows-1251", "Windows-1251 Cyrillic", "Windows", SB),
    info("windows-1252", "Windows-1252 Western", "Windows", SB),
    info("windows-1253", "Windows-1253 Greek", "Windows", SB),
    info("windows-1254", "Windows-1254 Turkish", "Windows", SB),
    info("windows-1255", "Windows-1255 Hebrew", "Windows", SB),
    info("windows-1256", "Windows-1256 Arabic", "Windows", SB),
    info("windows-1257", "Windows-1257 Baltic", "Windows", SB),
    info("windows-1258", "Windows-1258 Vietnamese", "Windows", SB),
    info("windows-874", "Windows-874 Thai", "Windows", SB),
    info("koi8-r", "KOI8-R", "Cyrillic", SB),
    info("koi8-u", "KOI8-U", "Cyrillic", SB),
    info("iso-8859-2", "ISO-8859-2 Central European", "ISO", SB),
    info("iso-8859-3", "ISO-8859-3 South European", "ISO", SB),
    info("iso-8859-4", "ISO-8859-4 Baltic", "ISO", SB),
    info("iso-8859-5", "ISO-8859-5 Cyrillic", "ISO", SB),
    info("iso-8859-6", "ISO-8859-6 Arabic", "ISO", SB),
    info("iso-8859-7", "ISO-8859-7 Greek", "ISO", SB),
    info("iso-8859-8", "ISO-8859-8 Hebrew", "ISO", SB),
    info("iso-8859-10", "ISO-8859-10 Nordic", "ISO", SB),
    info("iso-8859-13", "ISO-8859-13 Baltic", "ISO", SB),
    info("iso-8859-14", "ISO-8859-14 Celtic", "ISO", SB),
    info("iso-8859-15", "ISO-8859-15 Western (€)", "ISO", SB),
    info("iso-8859-16", "ISO-8859-16 South-Eastern", "ISO", SB),
    info("macintosh", "Mac OS Roman", "Mac", SB),
    info("x-mac-cyrillic", "Mac OS Cyrillic", "Mac", SB),
];

pub fn find(id: &str) -> Option<&'static EncodingInfo> {
    ENCODINGS.iter().find(|e| e.id.eq_ignore_ascii_case(id))
}

const CP437_LOW: [char; 32] = [
    '\0', '☺', '☻', '♥', '♦', '♣', '♠', '•', '◘', '○', '◙', '♂', '♀', '♪', '♫', '☼', '►', '◄', '↕',
    '‼', '¶', '§', '▬', '↨', '↑', '↓', '→', '←', '∟', '↔', '▲', '▼',
];

const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»\
░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀\
αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{A0}";

const EBCDIC_037_CONTROLS: [u32; 64] = [
    0x00, 0x01, 0x02, 0x03, 0x9C, 0x09, 0x86, 0x7F, 0x97, 0x8D, 0x8E, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x9D, 0x85, 0x08, 0x87, 0x18, 0x19, 0x92, 0x8F, 0x1C, 0x1D, 0x1E, 0x1F,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x0A, 0x17, 0x1B, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x05, 0x06, 0x07,
    0x90, 0x91, 0x16, 0x93, 0x94, 0x95, 0x96, 0x04, 0x98, 0x99, 0x9A, 0x9B, 0x14, 0x15, 0x9E, 0x1A,
];

const EBCDIC_037_HIGH: &str = " \u{A0}âäàáãåçñ¢.<(+|&éêëèíîïìß!$*);¬-/ÂÄÀÁÃÅÇÑ¦,%_>?øÉÊËÈÍÎÏÌ`:#@'=\"\
Øabcdefghi«»ðýþ±°jklmnopqrªºæ¸Æ¤µ~stuvwxyz¡¿ÐÝÞ®^£¥·©§¶¼½¾[]¯¨´×\
{ABCDEFGHI\u{AD}ôöòóõ}JKLMNOPQR¹ûüùúÿ\\÷STUVWXYZ²ÔÖÒÓÕ0123456789³ÛÜÙÚ\u{9F}";

/// Byte → character table of a single-byte encoding; `None` marks bytes the
/// encoding does not define.
pub fn table(id: &str) -> Result<Vec<Option<char>>> {
    let e = find(id).ok_or_else(|| Error::invalid(format!("unknown encoding {id}")))?;
    if e.kind != EncodingKind::SingleByte {
        return Err(Error::invalid(format!(
            "{id} is not a single-byte encoding"
        )));
    }
    let t: Vec<Option<char>> = match e.id {
        "ascii" => (0u8..=255)
            .map(|b| (b < 0x80).then_some(char::from(b)))
            .collect(),
        "latin1" => (0u8..=255).map(|b| Some(char::from(b))).collect(),
        "cp437" => {
            let mut v: Vec<Option<char>> = CP437_LOW.iter().map(|&c| Some(c)).collect();
            v.extend((0x20u8..0x7F).map(|b| Some(char::from(b))));
            v.push(Some('⌂'));
            v.extend(CP437_HIGH.chars().map(Some));
            v
        }
        "ebcdic-037" => {
            let mut v: Vec<Option<char>> = EBCDIC_037_CONTROLS
                .iter()
                .map(|&c| char::from_u32(c))
                .collect();
            v.extend(EBCDIC_037_HIGH.chars().map(Some));
            v
        }
        label => {
            let enc = encoding_rs::Encoding::for_label(label.as_bytes())
                .ok_or_else(|| Error::invalid(format!("unknown encoding {label}")))?;
            (0u8..=255)
                .map(|b| {
                    if b < 0x80 {
                        return Some(char::from(b));
                    }
                    let one = [b];
                    let (s, had_errors) = enc.decode_without_bom_handling(&one);
                    let c = s.chars().next()?;
                    (!had_errors && c != '\u{FFFD}').then_some(c)
                })
                .collect()
        }
    };
    debug_assert_eq!(t.len(), 256);
    Ok(t)
}

/// Encodes `text`; characters the encoding cannot represent are an error.
pub fn encode(text: &str, id: &str) -> Result<Vec<u8>> {
    let e = find(id).ok_or_else(|| Error::invalid(format!("unknown encoding {id}")))?;
    Ok(match e.kind {
        EncodingKind::Utf8 => text.as_bytes().to_vec(),
        EncodingKind::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        EncodingKind::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        EncodingKind::Utf32Le => text
            .chars()
            .flat_map(|c| u32::from(c).to_le_bytes())
            .collect(),
        EncodingKind::Utf32Be => text
            .chars()
            .flat_map(|c| u32::from(c).to_be_bytes())
            .collect(),
        EncodingKind::SingleByte => {
            let t = table(id)?;
            let mut reverse = std::collections::HashMap::new();
            for (b, c) in t.iter().enumerate() {
                if let Some(c) = c {
                    reverse.entry(*c).or_insert(b as u8);
                }
            }
            text.chars()
                .map(|c| {
                    reverse
                        .get(&c)
                        .copied()
                        .ok_or_else(|| Error::invalid(format!("'{c}' cannot be encoded in {id}")))
                })
                .collect::<Result<_>>()?
        }
    })
}

/// Decodes bytes into text, replacing invalid sequences.
pub fn decode(bytes: &[u8], id: &str) -> Result<String> {
    let e = find(id).ok_or_else(|| Error::invalid(format!("unknown encoding {id}")))?;
    Ok(match e.kind {
        EncodingKind::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        EncodingKind::Utf16Le | EncodingKind::Utf16Be => {
            let units = bytes.as_chunks::<2>().0.iter().map(|c| {
                let a = [c[0], c[1]];
                if e.kind == EncodingKind::Utf16Le {
                    u16::from_le_bytes(a)
                } else {
                    u16::from_be_bytes(a)
                }
            });
            char::decode_utf16(units)
                .map(|r| r.unwrap_or('\u{FFFD}'))
                .collect()
        }
        EncodingKind::Utf32Le | EncodingKind::Utf32Be => bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let a = [c[0], c[1], c[2], c[3]];
                let v = if e.kind == EncodingKind::Utf32Le {
                    u32::from_le_bytes(a)
                } else {
                    u32::from_be_bytes(a)
                };
                char::from_u32(v).unwrap_or('\u{FFFD}')
            })
            .collect(),
        EncodingKind::SingleByte => {
            let t = table(id)?;
            bytes
                .iter()
                .map(|&b| t[b as usize].unwrap_or('\u{FFFD}'))
                .collect()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_single_byte_tables_have_256_entries() {
        for e in ENCODINGS
            .iter()
            .filter(|e| e.kind == EncodingKind::SingleByte)
        {
            let t = table(e.id).unwrap_or_else(|err| panic!("{}: {err}", e.id));
            assert_eq!(t.len(), 256, "{}", e.id);
        }
    }

    #[test]
    fn known_mappings() {
        assert_eq!(table("cp437").unwrap()[0xC9], Some('╔'));
        assert_eq!(table("cp437").unwrap()[0x01], Some('☺'));
        assert_eq!(table("ebcdic-037").unwrap()[0xC1], Some('A'));
        assert_eq!(table("ebcdic-037").unwrap()[0xF0], Some('0'));
        assert_eq!(table("windows-1251").unwrap()[0xC0], Some('А'));
        assert_eq!(table("koi8-r").unwrap()[0xC1], Some('а'));
        assert_eq!(table("ibm866").unwrap()[0x80], Some('А'));
    }

    #[test]
    fn roundtrip() {
        for id in [
            "utf-8",
            "utf-16le",
            "utf-16be",
            "utf-32be",
            "windows-1251",
            "koi8-r",
        ] {
            let b = encode("Привет", id).unwrap();
            assert_eq!(decode(&b, id).unwrap(), "Привет", "{id}");
        }
        assert!(encode("Привет", "ascii").is_err());
        assert_eq!(encode("HI", "ebcdic-037").unwrap(), [0xC8, 0xC9]);
    }
}
