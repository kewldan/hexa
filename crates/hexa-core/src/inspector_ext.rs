//! Extended inspector types (INSP-04, 05, 08…13).
//!
//! Read-only interpretations of the bytes under the cursor that do not fit the
//! fixed-width numeric rows of [`crate::inspector`]: dates, GUIDs, network
//! addresses, variable-length integers, strings, BCD, fixed point and colours.

use serde::Serialize;

use crate::inspector::Endian;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtRow {
    pub key: &'static str,
    pub group: &'static str,
    pub label: String,
    pub value: Option<String>,
    pub size: usize,
    /// CSS colour for colour rows (INSP-11).
    pub swatch: Option<String>,
}

fn row(
    key: &'static str,
    group: &'static str,
    label: impl Into<String>,
    value: Option<String>,
    size: usize,
) -> ExtRow {
    ExtRow {
        key,
        group,
        label: label.into(),
        value,
        size,
        swatch: None,
    }
}

fn uint(bytes: &[u8], n: usize, e: Endian) -> Option<u64> {
    let s = bytes.get(..n)?;
    let mut a = [0u8; 8];
    a[..n].copy_from_slice(s);
    if e == Endian::Big {
        a[..n].reverse();
    }
    Some(u64::from_le_bytes(a))
}

// ---------------------------------------------------------------- dates (INSP-09)

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Formats nanoseconds since the Unix epoch as ISO-8601 UTC.
pub fn format_unix_nanos(ns: i128) -> Option<String> {
    let secs = ns.div_euclid(1_000_000_000);
    let frac = ns.rem_euclid(1_000_000_000);
    // Plausible range: years 1601…9999.
    if !(-11_644_473_600..=253_402_300_799).contains(&secs) {
        return None;
    }
    let secs = secs as i64;
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    let base = format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        t / 3600,
        (t / 60) % 60,
        t % 60
    );
    Some(if frac == 0 {
        format!("{base} UTC")
    } else {
        let ms = frac / 1_000_000;
        let rest = frac % 1_000_000;
        if rest == 0 {
            format!("{base}.{ms:03} UTC")
        } else {
            format!("{base}.{frac:09} UTC")
        }
    })
}

fn dates(b: &[u8], e: Endian) -> Vec<ExtRow> {
    let g = "Дата и время";
    let u32v = uint(b, 4, e);
    let u64v = uint(b, 8, e);
    let i64v = u64v.map(|v| v as i64);
    let mut out = vec![
        row(
            "unix32",
            g,
            "Unix (с, 32)",
            u32v.and_then(|v| format_unix_nanos(i128::from(v) * 1_000_000_000)),
            4,
        ),
        row(
            "unix64",
            g,
            "Unix (с, 64)",
            i64v.and_then(|v| format_unix_nanos(i128::from(v) * 1_000_000_000)),
            8,
        ),
        row(
            "unixMs",
            g,
            "Unix (мс)",
            i64v.and_then(|v| format_unix_nanos(i128::from(v) * 1_000_000)),
            8,
        ),
        row(
            "unixUs",
            g,
            "Unix (мкс)",
            i64v.and_then(|v| format_unix_nanos(i128::from(v) * 1_000)),
            8,
        ),
        row(
            "unixNs",
            g,
            "Unix (нс)",
            i64v.and_then(|v| format_unix_nanos(i128::from(v))),
            8,
        ),
        // FILETIME: 100 ns since 1601-01-01.
        row(
            "filetime",
            g,
            "Windows FILETIME",
            u64v.and_then(|v| {
                format_unix_nanos(i128::from(v) * 100 - 11_644_473_600 * 1_000_000_000)
            }),
            8,
        ),
        // .NET ticks: 100 ns since 0001-01-01 (lower 62 bits).
        row(
            "dotnet",
            g,
            ".NET ticks",
            u64v.and_then(|v| {
                let ticks = i128::from(v & 0x3FFF_FFFF_FFFF_FFFF);
                format_unix_nanos(ticks * 100 - 62_135_596_800 * 1_000_000_000)
            }),
            8,
        ),
        // HFS+: seconds since 1904-01-01.
        row(
            "hfs",
            g,
            "HFS+ (с 1904)",
            u32v.and_then(|v| format_unix_nanos((i128::from(v) - 2_082_844_800) * 1_000_000_000)),
            4,
        ),
        // GPS: seconds since 1980-01-06 (without leap seconds).
        row(
            "gps",
            g,
            "GPS (с 1980-01-06)",
            u32v.and_then(|v| format_unix_nanos((i128::from(v) + 315_964_800) * 1_000_000_000)),
            4,
        ),
    ];
    // OLE automation DATE: f64 days since 1899-12-30.
    let ole = u64v
        .map(f64::from_bits)
        .filter(|d| d.is_finite() && d.abs() < 3_000_000.0);
    out.push(row(
        "ole",
        g,
        "OLE DATE",
        ole.and_then(|d| format_unix_nanos(((d - 25_569.0) * 86_400.0 * 1e9) as i128)),
        8,
    ));
    // DOS date/time: time (2 bytes) then date (2 bytes), little-endian.
    let dos = uint(b, 4, Endian::Little).and_then(|v| {
        let time = v & 0xFFFF;
        let date = v >> 16;
        let (y, m, d) = (1980 + (date >> 9), (date >> 5) & 15, date & 31);
        let (h, mi, s) = (time >> 11, (time >> 5) & 63, (time & 31) * 2);
        ((1..=12).contains(&m) && d >= 1 && h < 24 && mi < 60 && s < 60)
            .then(|| format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
    });
    out.push(row("dos", g, "DOS дата/время", dos, 4));
    out
}

// ---------------------------------------------------------------- GUID, net (INSP-10, 12)

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02X}");
            s
        })
}

fn guids(b: &[u8]) -> Vec<ExtRow> {
    let g = "GUID";
    let rfc = b.get(..16).map(|s| {
        format!(
            "{}-{}-{}-{}-{}",
            hex(&s[0..4]),
            hex(&s[4..6]),
            hex(&s[6..8]),
            hex(&s[8..10]),
            hex(&s[10..16])
        )
    });
    let ms = b.get(..16).map(|s| {
        let mut d1 = s[0..4].to_vec();
        d1.reverse();
        let mut d2 = s[4..6].to_vec();
        d2.reverse();
        let mut d3 = s[6..8].to_vec();
        d3.reverse();
        format!(
            "{{{}-{}-{}-{}-{}}}",
            hex(&d1),
            hex(&d2),
            hex(&d3),
            hex(&s[8..10]),
            hex(&s[10..16])
        )
    });
    vec![
        row("guidMs", g, "GUID (Microsoft)", ms, 16),
        row("guidRfc", g, "UUID (RFC 4122)", rfc, 16),
    ]
}

fn net(b: &[u8]) -> Vec<ExtRow> {
    let g = "Сеть";
    let v4 = b
        .get(..4)
        .map(|s| format!("{}.{}.{}.{}", s[0], s[1], s[2], s[3]));
    let v6 = b.get(..16).map(|s| {
        let a: [u8; 16] = s.try_into().expect("16 bytes");
        std::net::Ipv6Addr::from(a).to_string()
    });
    let mac = b.get(..6).map(|s| {
        s.iter()
            .map(|x| format!("{x:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    });
    let port = uint(b, 2, Endian::Big).map(|p| p.to_string());
    vec![
        row("ipv4", g, "IPv4", v4, 4),
        row("ipv6", g, "IPv6", v6, 16),
        row("mac", g, "MAC", mac, 6),
        row("port", g, "Порт (BE)", port, 2),
    ]
}

// ---------------------------------------------------------------- varints (INSP-05)

/// Unsigned LEB128 / protobuf varint: (value, length).
pub fn uleb128(b: &[u8]) -> Option<(u128, usize)> {
    let mut v: u128 = 0;
    for (i, &x) in b.iter().take(19).enumerate() {
        v |= u128::from(x & 0x7F) << (7 * i);
        if x & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// Signed LEB128: (value, length).
pub fn sleb128(b: &[u8]) -> Option<(i128, usize)> {
    let mut v: i128 = 0;
    let mut shift = 0;
    for (i, &x) in b.iter().take(19).enumerate() {
        v |= i128::from(x & 0x7F) << shift;
        shift += 7;
        if x & 0x80 == 0 {
            if shift < 128 && x & 0x40 != 0 {
                v |= -1i128 << shift;
            }
            return Some((v, i + 1));
        }
    }
    None
}

fn varints(b: &[u8]) -> Vec<ExtRow> {
    let g = "Переменная длина";
    let u = uleb128(b);
    let s = sleb128(b);
    let zz = u.map(|(v, n)| (((v >> 1) as i128) ^ -((v & 1) as i128), n));
    vec![
        row(
            "uleb128",
            g,
            "ULEB128 / varint",
            u.map(|(v, _)| v.to_string()),
            u.map_or(1, |x| x.1),
        ),
        row(
            "sleb128",
            g,
            "SLEB128",
            s.map(|(v, _)| v.to_string()),
            s.map_or(1, |x| x.1),
        ),
        row(
            "zigzag",
            g,
            "varint zigzag",
            zz.map(|(v, _)| v.to_string()),
            zz.map_or(1, |x| x.1),
        ),
    ]
}

// ---------------------------------------------------------------- strings (INSP-08)

fn printable(s: &[u8]) -> String {
    let t = String::from_utf8_lossy(s);
    let mut out: String = t
        .chars()
        .take(64)
        .map(|c| if c.is_control() { '·' } else { c })
        .collect();
    if t.chars().count() > 64 {
        out.push('…');
    }
    format!("\"{out}\"")
}

fn strings(b: &[u8], e: Endian) -> Vec<ExtRow> {
    let g = "Строки";
    let cstr = b
        .iter()
        .position(|&x| x == 0)
        .map(|n| (printable(&b[..n]), n + 1));
    let pas = |w: usize| -> (Option<String>, usize) {
        match uint(b, w, e) {
            Some(n) if (n as usize) <= b.len().saturating_sub(w) && n <= 4096 => {
                (Some(printable(&b[w..w + n as usize])), w + n as usize)
            }
            _ => (None, w),
        }
    };
    let (p8, s8) = pas(1);
    let (p16, s16) = pas(2);
    let (p32, s32) = pas(4);
    vec![
        row(
            "cstring",
            g,
            "C-строка",
            cstr.as_ref().map(|x| x.0.clone()),
            cstr.map_or(1, |x| x.1),
        ),
        row("pascal8", g, "Pascal (u8 длина)", p8, s8),
        row("pascal16", g, "Pascal (u16 длина)", p16, s16),
        row("pascal32", g, "Pascal (u32 длина)", p32, s32),
    ]
}

// ---------------------------------------------------------------- misc (INSP-04, 11, 13)

fn misc(b: &[u8], e: Endian, q: (u32, u32)) -> Vec<ExtRow> {
    let mut out = Vec::new();
    let bcd = b.first().map(|&x| {
        let (h, l) = (x >> 4, x & 15);
        if h < 10 && l < 10 {
            format!("{}", h * 10 + l)
        } else {
            "не BCD".into()
        }
    });
    out.push(row("bcd", "Прочее", "BCD (1 байт)", bcd, 1));
    let bcd4 = b.get(..4).and_then(|s| {
        let mut v = 0u64;
        for &x in s {
            let (h, l) = (x >> 4, x & 15);
            if h > 9 || l > 9 {
                return None;
            }
            v = v * 100 + u64::from(h * 10 + l);
        }
        Some(v.to_string())
    });
    out.push(row("bcd4", "Прочее", "BCD (4 байта)", bcd4, 4));
    out.push(row(
        "bool",
        "Прочее",
        "bool",
        b.first().map(|&x| match x {
            0 => "false".into(),
            1 => "true".into(),
            n => format!("true ({n})"),
        }),
        1,
    ));
    // Q-format fixed point (INSP-04): m integer bits incl. sign, n fraction bits.
    let (m, n) = q;
    let bits = (m + n) as usize;
    if (2..=64).contains(&bits) && bits.is_multiple_of(8) {
        let bytes = bits / 8;
        let v = uint(b, bytes, e).map(|raw| {
            let shift = 64 - bits;
            let signed = ((raw << shift) as i64) >> shift;
            let val = signed as f64 / 2f64.powi(n as i32);
            format!("{val}")
        });
        out.push(row("qformat", "Прочее", format!("Q{m}.{n}"), v, bytes));
    }
    // Colours (INSP-11).
    let g = "Цвет";
    let mut color =
        |key: &'static str, label: &str, size: usize, rgba: Option<(u8, u8, u8, u8)>| {
            let mut r = row(
                key,
                g,
                label,
                rgba.map(|(r, g, b, a)| format!("#{r:02X}{g:02X}{b:02X}{a:02X}")),
                size,
            );
            r.swatch = rgba
                .map(|(r, g, b, a)| format!("rgba({r}, {g}, {b}, {:.3})", f64::from(a) / 255.0));
            out.push(r);
        };
    let rgb565 = uint(b, 2, e).map(|v| {
        let r = ((v >> 11) & 31) as u8;
        let g = ((v >> 5) & 63) as u8;
        let bl = (v & 31) as u8;
        (
            (r << 3) | (r >> 2),
            (g << 2) | (g >> 4),
            (bl << 3) | (bl >> 2),
            255,
        )
    });
    color("rgb565", "RGB565", 2, rgb565);
    let rgb888 = b.get(..3).map(|s| (s[0], s[1], s[2], 255));
    color("rgb888", "RGB888", 3, rgb888);
    let quad = b.get(..4).map(|s| (s[0], s[1], s[2], s[3]));
    color("rgba", "RGBA", 4, quad);
    color("bgra", "BGRA", 4, quad.map(|(b0, g, r, a)| (r, g, b0, a)));
    color("argb", "ARGB", 4, quad.map(|(a, r, g, b0)| (r, g, b0, a)));
    out
}

/// All extended rows for the bytes at the cursor (or a selection, INSP-19).
pub fn inspect_ext(bytes: &[u8], endian: Endian, q: (u32, u32)) -> Vec<ExtRow> {
    let mut out = dates(bytes, endian);
    out.extend(guids(bytes));
    out.extend(net(bytes));
    out.extend(varints(bytes));
    out.extend(strings(bytes, endian));
    out.extend(misc(bytes, endian, q));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(rows: &[ExtRow], key: &str) -> Option<String> {
        rows.iter()
            .find(|r| r.key == key)
            .and_then(|r| r.value.clone())
    }

    #[test]
    fn dates_decode() {
        let b = 1_700_000_000u32.to_le_bytes();
        let r = inspect_ext(&b, Endian::Little, (16, 16));
        assert_eq!(val(&r, "unix32").unwrap(), "2023-11-14 22:13:20 UTC");
        // FILETIME of the Unix epoch.
        let ft = 116_444_736_000_000_000u64.to_le_bytes();
        let r = inspect_ext(&ft, Endian::Little, (16, 16));
        assert_eq!(val(&r, "filetime").unwrap(), "1970-01-01 00:00:00 UTC");
        // DOS: 2020-02-29 13:45:30 → date 0x505D, time 0x6DAF.
        let r = inspect_ext(&[0xAF, 0x6D, 0x5D, 0x50], Endian::Little, (16, 16));
        assert_eq!(val(&r, "dos").unwrap(), "2020-02-29 13:45:30");
    }

    #[test]
    fn guid_net_varint_strings() {
        let g: Vec<u8> = (0..16).collect();
        let r = inspect_ext(&g, Endian::Little, (16, 16));
        assert_eq!(
            val(&r, "guidRfc").unwrap(),
            "00010203-0405-0607-0809-0A0B0C0D0E0F"
        );
        assert_eq!(
            val(&r, "guidMs").unwrap(),
            "{03020100-0504-0706-0809-0A0B0C0D0E0F}"
        );
        assert_eq!(val(&r, "ipv4").unwrap(), "0.1.2.3");
        assert_eq!(uleb128(&[0xE5, 0x8E, 0x26]), Some((624_485, 3)));
        assert_eq!(sleb128(&[0xC0, 0xBB, 0x78]), Some((-123_456, 3)));
        let r = inspect_ext(b"\x03abc\x00", Endian::Little, (16, 16));
        assert_eq!(val(&r, "pascal8").unwrap(), "\"abc\"");
        assert_eq!(
            val(&r, "cstring").unwrap(),
            "\"\\u{3}abc\"".replace("\\u{3}", "·")
        );
        let r = inspect_ext(&[0x00, 0x80], Endian::Little, (8, 8));
        assert_eq!(val(&r, "qformat").unwrap(), "-128");
        let r = inspect_ext(&[0x99, 0x12, 0x34, 0x56], Endian::Little, (16, 16));
        assert_eq!(val(&r, "bcd").unwrap(), "99");
        assert_eq!(val(&r, "bcd4").unwrap(), "99123456");
    }
}
