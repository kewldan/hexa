//! Built-in template library (STRC-23) and format auto-detection (STRC-09).
//!
//! Every entry is an embedded `.hexpat` source plus the metadata used to
//! offer it for a file: magic signatures, file extensions and MIME types.
//! [`detect`] ranks the entries that fit the first bytes of a file and its
//! name; [`program`] parses an entry so it can be evaluated.

use serde::{Deserialize, Serialize};

use crate::{Error, Program, Result};

/// Stable identifier of a library entry (`"png"`, `"elf"`, ...).
pub type TemplateId = &'static str;

/// A signature identifying a format: `bytes` stored at `offset`.
#[derive(Debug, Clone, Copy)]
pub struct Magic {
    pub offset: u64,
    pub bytes: &'static [u8],
}

impl Magic {
    const fn at(offset: u64, bytes: &'static [u8]) -> Self {
        Self { offset, bytes }
    }

    /// Whether `head` (the first bytes of the data) carries this signature.
    pub fn matches(&self, head: &[u8]) -> bool {
        let Ok(start) = usize::try_from(self.offset) else {
            return false;
        };
        start
            .checked_add(self.bytes.len())
            .and_then(|end| head.get(start..end))
            .is_some_and(|s| s == self.bytes)
    }

    /// Detection confidence: longer signatures are more specific, and a
    /// signature away from offset 0 is less likely to match by chance.
    fn score(&self) -> u32 {
        let len = u32::try_from(self.bytes.len()).unwrap_or(u32::MAX / 32);
        MAGIC_SCORE + len.saturating_mul(10) + if self.offset == 0 { 0 } else { 5 }
    }
}

/// One built-in template.
#[derive(Debug, Clone, Copy)]
pub struct LibraryEntry {
    pub id: TemplateId,
    pub name: &'static str,
    pub description: &'static str,
    /// Lower-case file extensions without the dot.
    pub extensions: &'static [&'static str],
    pub mime: &'static [&'static str],
    /// Alternative signatures; any of them identifies the format.
    pub magic: &'static [Magic],
    /// Template source in the native (`.hexpat`) dialect.
    pub source: &'static str,
}

/// Serializable summary of a library entry for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub extensions: Vec<String>,
    pub mime: Vec<String>,
}

impl From<&LibraryEntry> for LibraryInfo {
    fn from(e: &LibraryEntry) -> Self {
        Self {
            id: e.id.to_string(),
            name: e.name.to_string(),
            description: e.description.to_string(),
            extensions: e.extensions.iter().map(ToString::to_string).collect(),
            mime: e.mime.iter().map(ToString::to_string).collect(),
        }
    }
}

/// Base score of a magic match; any magic match outranks an extension match.
const MAGIC_SCORE: u32 = 1000;
/// Score of an extension match without a magic match.
const EXTENSION_SCORE: u32 = 500;
/// Bonus when both the magic and the extension match.
const BOTH_BONUS: u32 = 50;

const ENTRIES: &[LibraryEntry] = &[
    LibraryEntry {
        id: "png",
        name: "PNG image",
        description: "Portable Network Graphics: signature and chunks with IHDR, PLTE, text, \
                      time, APNG and CRC-32 checksums decoded",
        extensions: &["png", "apng"],
        mime: &["image/png", "image/apng"],
        magic: &[Magic::at(0, b"\x89PNG\r\n\x1a\n")],
        source: include_str!("png.hexpat"),
    },
    LibraryEntry {
        id: "jpeg",
        name: "JPEG image",
        description: "JPEG/JFIF/Exif: marker segments with SOF, SOS, DHT, DQT, APP0 (JFIF) and \
                      APP1 (Exif/TIFF IFD0) decoded, scan data up to EOI",
        extensions: &["jpg", "jpeg", "jpe", "jfif"],
        mime: &["image/jpeg"],
        magic: &[Magic::at(0, b"\xff\xd8\xff")],
        source: include_str!("jpeg.hexpat"),
    },
    LibraryEntry {
        id: "gif",
        name: "GIF image",
        description: "Graphics Interchange Format: header, logical screen descriptor, colour \
                      tables, extensions and image blocks",
        extensions: &["gif"],
        mime: &["image/gif"],
        magic: &[Magic::at(0, b"GIF87a"), Magic::at(0, b"GIF89a")],
        source: include_str!("gif.hexpat"),
    },
    LibraryEntry {
        id: "bmp",
        name: "BMP image",
        description: "Windows bitmap: file header, DIB header (core to V5), bit masks, palette \
                      and pixel data",
        extensions: &["bmp", "dib"],
        mime: &["image/bmp"],
        magic: &[Magic::at(0, b"BM")],
        source: include_str!("bmp.hexpat"),
    },
    LibraryEntry {
        id: "ico",
        name: "Windows icon",
        description: "ICO/CUR: icon directory with embedded PNG or DIB images",
        extensions: &["ico", "cur"],
        mime: &["image/vnd.microsoft.icon", "image/x-icon"],
        magic: &[Magic::at(0, b"\0\0\x01\0"), Magic::at(0, b"\0\0\x02\0")],
        source: include_str!("ico.hexpat"),
    },
    LibraryEntry {
        id: "wav",
        name: "WAVE audio",
        description: "RIFF/WAVE: chunks with the fmt (incl. WAVE_FORMAT_EXTENSIBLE), LIST/INFO \
                      and data chunks decoded",
        extensions: &["wav", "wave"],
        mime: &["audio/wav", "audio/x-wav", "audio/vnd.wave"],
        magic: &[Magic::at(8, b"WAVE")],
        source: include_str!("wav.hexpat"),
    },
    LibraryEntry {
        id: "mp3",
        name: "MP3 audio",
        description: "MPEG audio: ID3v2 tag with frames, MPEG frame headers (bit fields, bitrate, \
                      sample rate, frame length) and ID3v1 tag",
        extensions: &["mp3"],
        mime: &["audio/mpeg", "audio/mp3"],
        magic: &[
            Magic::at(0, b"ID3"),
            Magic::at(0, b"\xff\xfb"),
            Magic::at(0, b"\xff\xfa"),
            Magic::at(0, b"\xff\xf3"),
            Magic::at(0, b"\xff\xf2"),
            Magic::at(0, b"\xff\xe3"),
        ],
        source: include_str!("mp3.hexpat"),
    },
    LibraryEntry {
        id: "mp4",
        name: "MP4 / QuickTime",
        description: "ISO base media file format: boxes with 64-bit sizes, nested containers \
                      (moov, trak, mdia, ...) and common headers and sample tables decoded",
        extensions: &[
            "mp4", "m4a", "m4v", "m4b", "mov", "qt", "3gp", "3g2", "heic", "heif", "avif",
        ],
        mime: &[
            "video/mp4",
            "audio/mp4",
            "video/quicktime",
            "video/3gpp",
            "image/heic",
            "image/avif",
        ],
        magic: &[
            Magic::at(4, b"ftyp"),
            Magic::at(4, b"moov"),
            Magic::at(4, b"mdat"),
        ],
        source: include_str!("mp4.hexpat"),
    },
    LibraryEntry {
        id: "pdf",
        name: "PDF document",
        description: "Portable Document Format: header, last startxref, cross-reference table \
                      and trailer dictionary",
        extensions: &["pdf"],
        mime: &["application/pdf"],
        magic: &[Magic::at(0, b"%PDF-")],
        source: include_str!("pdf.hexpat"),
    },
    LibraryEntry {
        id: "sqlite",
        name: "SQLite database",
        description: "SQLite 3 database: 100-byte header and b-tree page headers with cell pointers",
        extensions: &["sqlite", "sqlite3", "db", "db3", "sdb"],
        mime: &["application/vnd.sqlite3", "application/x-sqlite3"],
        magic: &[Magic::at(0, b"SQLite format 3\0")],
        source: include_str!("sqlite.hexpat"),
    },
    LibraryEntry {
        id: "gpt",
        name: "GPT disk",
        description: "GUID partition table: protective MBR, GPT header at LBA 1 (512- or \
                      4096-byte sectors), partition entries and backup header",
        extensions: &["img", "raw", "dd", "iso"],
        mime: &[],
        magic: &[Magic::at(512, b"EFI PART"), Magic::at(4096, b"EFI PART")],
        source: concat!(include_str!("mbr_types.hexpat"), include_str!("gpt.hexpat")),
    },
    LibraryEntry {
        id: "mbr",
        name: "MBR disk",
        description: "Master boot record: partition table with CHS/LBA fields and the extended \
                      boot record chain of logical partitions",
        extensions: &["img", "raw", "dd", "mbr"],
        mime: &[],
        magic: &[Magic::at(510, b"\x55\xaa")],
        source: concat!(include_str!("mbr_types.hexpat"), include_str!("mbr.hexpat")),
    },
    LibraryEntry {
        id: "fat",
        name: "FAT file system",
        description: "FAT12/16/32 volume: boot sector with BPB and FAT32 extension, FSInfo, \
                      first FAT and root directory entries (incl. long names)",
        extensions: &["img", "ima", "vfd", "fat"],
        mime: &[],
        magic: &[
            Magic::at(54, b"FAT12   "),
            Magic::at(54, b"FAT16   "),
            Magic::at(54, b"FAT     "),
            Magic::at(82, b"FAT32   "),
        ],
        source: concat!(include_str!("time.hexpat"), include_str!("fat.hexpat")),
    },
    LibraryEntry {
        id: "ntfs",
        name: "NTFS file system",
        description: "NTFS volume: boot sector and the first MFT records with standard \
                      information, file name and other attribute headers",
        extensions: &["img", "ntfs"],
        mime: &[],
        magic: &[Magic::at(3, b"NTFS    ")],
        source: concat!(include_str!("time.hexpat"), include_str!("ntfs.hexpat")),
    },
    LibraryEntry {
        id: "ext4",
        name: "ext2/3/4 file system",
        description: "ext2/3/4 volume: superblock at 1024 with feature flags and crc32c, block \
                      group descriptors and the root inode with its extent tree",
        extensions: &["img", "ext2", "ext3", "ext4"],
        mime: &[],
        magic: &[Magic::at(1080, b"\x53\xef")],
        source: concat!(include_str!("time.hexpat"), include_str!("ext4.hexpat")),
    },
    LibraryEntry {
        id: "zip",
        name: "ZIP archive",
        description: "ZIP (also JAR, APK, OOXML, ODF, EPUB): local file headers with data, \
                      central directory, ZIP64 records and end of central directory, CRC-32 \
                      ranges for stored entries",
        extensions: &[
            "zip", "jar", "war", "ear", "apk", "aab", "ipa", "xpi", "whl", "nupkg", "docx", "xlsx",
            "pptx", "odt", "ods", "odp", "epub", "kmz", "aar", "vsix",
        ],
        mime: &[
            "application/zip",
            "application/java-archive",
            "application/epub+zip",
        ],
        magic: &[
            Magic::at(0, b"PK\x03\x04"),
            Magic::at(0, b"PK\x05\x06"),
            Magic::at(0, b"PK\x07\x08"),
        ],
        source: concat!(include_str!("time.hexpat"), include_str!("zip.hexpat")),
    },
    LibraryEntry {
        id: "gzip",
        name: "GZIP file",
        description: "GZIP member: header with optional extra, name, comment and header CRC, \
                      deflate stream and CRC-32/ISIZE trailer",
        extensions: &["gz", "gzip", "tgz", "svgz"],
        mime: &["application/gzip", "application/x-gzip"],
        magic: &[Magic::at(0, b"\x1f\x8b\x08")],
        source: concat!(include_str!("time.hexpat"), include_str!("gzip.hexpat")),
    },
    LibraryEntry {
        id: "tar",
        name: "TAR archive",
        description: "ustar/GNU/pax archive: 512-byte headers with octal fields parsed, header \
                      checksum verified, file data and block padding",
        extensions: &["tar"],
        mime: &["application/x-tar"],
        magic: &[Magic::at(257, b"ustar")],
        source: concat!(include_str!("time.hexpat"), include_str!("tar.hexpat")),
    },
    LibraryEntry {
        id: "elf",
        name: "ELF binary",
        description: "Executable and Linkable Format (32/64-bit, both byte orders): header, \
                      program headers, section headers with names, symbols and dynamic table",
        extensions: &["elf", "so", "o", "ko", "axf", "prx", "bin"],
        mime: &[
            "application/x-elf",
            "application/x-executable",
            "application/x-sharedlib",
        ],
        magic: &[Magic::at(0, b"\x7fELF")],
        source: include_str!("elf.hexpat"),
    },
    LibraryEntry {
        id: "pe",
        name: "PE executable",
        description: "Portable Executable (PE32/PE32+): DOS header and stub, NT headers, optional \
                      header with data directories, section table, imports and exports",
        extensions: &[
            "exe", "dll", "sys", "efi", "ocx", "scr", "cpl", "drv", "mui", "winmd",
        ],
        mime: &[
            "application/vnd.microsoft.portable-executable",
            "application/x-msdownload",
        ],
        magic: &[Magic::at(0, b"MZ")],
        source: concat!(include_str!("time.hexpat"), include_str!("pe.hexpat")),
    },
    LibraryEntry {
        id: "java_class",
        name: "Java class file",
        description: "JVM class file: constant pool (all tags, double-slot Long/Double), access \
                      flags, fields, methods and attributes with Code decoded",
        extensions: &["class"],
        mime: &["application/java-vm", "application/x-java-class"],
        magic: &[Magic::at(0, b"\xca\xfe\xba\xbe")],
        source: include_str!("java_class.hexpat"),
    },
    LibraryEntry {
        id: "macho",
        name: "Mach-O binary",
        description: "Mach-O (32/64-bit, both byte orders) and universal binaries: header and \
                      load commands (segments, sections, dylibs, symtab, UUID, build version)",
        extensions: &["dylib", "bundle", "macho", "kext"],
        mime: &["application/x-mach-binary"],
        magic: &[
            Magic::at(0, b"\xfe\xed\xfa\xce"),
            Magic::at(0, b"\xfe\xed\xfa\xcf"),
            Magic::at(0, b"\xce\xfa\xed\xfe"),
            Magic::at(0, b"\xcf\xfa\xed\xfe"),
            Magic::at(0, b"\xca\xfe\xba\xbe"),
        ],
        source: include_str!("macho.hexpat"),
    },
    LibraryEntry {
        id: "dex",
        name: "Android DEX",
        description: "Dalvik executable: header with Adler-32/SHA-1 checksum ranges, string table \
                      with MUTF-8 data, type ids and map list",
        extensions: &["dex", "odex"],
        mime: &["application/vnd.android.dex"],
        magic: &[Magic::at(0, b"dex\n")],
        source: include_str!("dex.hexpat"),
    },
    LibraryEntry {
        id: "wasm",
        name: "WebAssembly module",
        description: "WebAssembly binary: header and sections with LEB128 sizes, custom section \
                      names and exports decoded",
        extensions: &["wasm"],
        mime: &["application/wasm"],
        magic: &[Magic::at(0, b"\0asm")],
        source: include_str!("wasm.hexpat"),
    },
    LibraryEntry {
        id: "pcap",
        name: "pcap capture",
        description: "libpcap capture file: file header and packet records with Ethernet, \
                      802.1Q, IPv4, TCP and UDP headers decoded",
        extensions: &["pcap", "cap", "dmp"],
        mime: &["application/vnd.tcpdump.pcap"],
        magic: &[
            Magic::at(0, b"\xd4\xc3\xb2\xa1"),
            Magic::at(0, b"\xa1\xb2\xc3\xd4"),
            Magic::at(0, b"\x4d\x3c\xb2\xa1"),
            Magic::at(0, b"\xa1\xb2\x3c\x4d"),
        ],
        source: concat!(
            include_str!("time.hexpat"),
            include_str!("net.hexpat"),
            include_str!("pcap.hexpat")
        ),
    },
    LibraryEntry {
        id: "ethernet",
        name: "Ethernet frame",
        description: "Ethernet II frame (with 802.1Q tag) and the IPv4/TCP/UDP headers inside; \
                      apply to a packet payload",
        extensions: &[],
        mime: &[],
        magic: &[],
        source: concat!(include_str!("net.hexpat"), "\nbe Ethernet ethernet @ 0;\n"),
    },
    LibraryEntry {
        id: "ipv4",
        name: "IPv4 packet",
        description: "IPv4 header (checksum range marked) with TCP or UDP payload",
        extensions: &[],
        mime: &[],
        magic: &[],
        source: concat!(include_str!("net.hexpat"), "\nbe IPv4 ipv4 @ 0;\n"),
    },
    LibraryEntry {
        id: "tcp",
        name: "TCP segment",
        description: "TCP header with flags and options, then payload",
        extensions: &[],
        mime: &[],
        magic: &[],
        source: concat!(include_str!("net.hexpat"), "\nbe Tcp tcp @ 0;\n"),
    },
    LibraryEntry {
        id: "udp",
        name: "UDP datagram",
        description: "UDP header and payload",
        extensions: &[],
        mime: &[],
        magic: &[],
        source: concat!(include_str!("net.hexpat"), "\nbe Udp udp @ 0;\n"),
    },
];

/// Every built-in template, in a stable order.
pub fn entries() -> &'static [LibraryEntry] {
    ENTRIES
}

/// Summaries of every built-in template.
pub fn list() -> Vec<LibraryInfo> {
    ENTRIES.iter().map(LibraryInfo::from).collect()
}

/// Looks up an entry by id.
pub fn get(id: &str) -> Option<&'static LibraryEntry> {
    ENTRIES.iter().find(|e| e.id == id)
}

/// Parses the source of entry `id`.
pub fn program(id: &str) -> Result<Program> {
    let entry =
        get(id).ok_or_else(|| Error::invalid(format!("unknown library template `{id}`")))?;
    crate::parse(entry.source)
}

/// Suggests templates for data starting with `head` (any length, possibly
/// empty) and named `file_name` (STRC-09). Magic matches come first, ranked
/// by specificity; extension matches (case-insensitive) follow. Ids are
/// unique and ordered by decreasing confidence.
pub fn detect(head: &[u8], file_name: Option<&str>) -> Vec<TemplateId> {
    let ext = file_name.and_then(extension);
    let mut scored: Vec<(u32, usize, TemplateId)> = ENTRIES
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let magic = e
                .magic
                .iter()
                .filter(|m| m.matches(head))
                .map(Magic::score)
                .max();
            let by_ext = ext
                .as_deref()
                .is_some_and(|x| e.extensions.iter().any(|y| y.eq_ignore_ascii_case(x)));
            let score = match (magic, by_ext) {
                (Some(m), true) => m + BOTH_BONUS,
                (Some(m), false) => m,
                (None, true) => EXTENSION_SCORE,
                (None, false) => return None,
            };
            Some((score, i, e.id))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, id)| id).collect()
}

/// Lower-case extension of the last path component, if any.
fn extension(file_name: &str) -> Option<String> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let (stem, ext) = base.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Severity;

    #[test]
    fn every_source_parses_cleanly() {
        for e in entries() {
            let (_, diags) = crate::parse_with(e.source, &crate::ParseOptions::default());
            let errors: Vec<_> = diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .collect();
            assert!(errors.is_empty(), "{}: {errors:#?}", e.id);
        }
    }

    #[test]
    fn ids_are_unique_and_lookup_works() {
        let mut ids: Vec<_> = entries().iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), entries().len());
        for e in entries() {
            assert_eq!(get(e.id).map(|x| x.id), Some(e.id));
            assert!(program(e.id).is_ok(), "{}", e.id);
            for x in e.extensions {
                assert_eq!(*x, x.to_ascii_lowercase(), "{}", e.id);
            }
        }
        assert!(get("nope").is_none());
        assert!(program("nope").is_err());
        assert_eq!(list().len(), entries().len());
    }

    #[test]
    fn extension_parsing() {
        assert_eq!(extension("a/b/c.PNG").as_deref(), Some("png"));
        assert_eq!(extension("C:\\x\\y.tar.gz").as_deref(), Some("gz"));
        assert_eq!(extension(".hidden"), None);
        assert_eq!(extension("noext"), None);
        assert_eq!(extension("trailing."), None);
    }

    #[test]
    fn magic_bounds_never_panic() {
        let m = Magic::at(u64::MAX, b"x");
        assert!(!m.matches(b"xxxx"));
        assert!(!Magic::at(2, b"abc").matches(b"zzab"));
        assert!(Magic::at(2, b"ab").matches(b"zzab"));
        assert_eq!(detect(&[], None).len(), 0);
        assert_eq!(detect(&[], Some("")).len(), 0);
    }
}
