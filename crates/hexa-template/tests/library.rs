//! STRC-23 built-in template library and STRC-09 format detection.

mod common;

use std::fmt::Write as _;

use common::{all_paths, get, int, text, tree};
use hexa_core::Cancel;
use hexa_template::library::{self, LibraryInfo};
use hexa_template::{EvalOptions, TemplateResult, checksum_fields, evaluate};

// ----- helpers --------------------------------------------------------------

/// Evaluates library entry `id` over `bytes` and requires a clean run.
fn eval_entry(id: &str, bytes: &[u8]) -> TemplateResult {
    eval_with(id, bytes, &EvalOptions::default())
}

fn eval_with(id: &str, bytes: &[u8], opts: &EvalOptions) -> TemplateResult {
    let program = library::program(id).unwrap_or_else(|e| panic!("{id}: {e}"));
    let r = evaluate(&program, &tree(bytes), opts, &Cancel::new()).expect("evaluate");
    assert!(
        r.is_ok(),
        "{id}: evaluation errors: {:#?}\nnodes: {:?}",
        r.errors,
        all_paths(&r)
    );
    r
}

fn enum_name(r: &TemplateResult, path: &str) -> String {
    get(r, path)
        .enum_name
        .clone()
        .unwrap_or_else(|| panic!("`{path}` has no enum name: {:?}", get(r, path).value))
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xEDB8_8320
            };
        }
    }
    !crc
}

/// Little byte-buffer builder for hand-made samples.
#[derive(Default)]
struct Buf(Vec<u8>);

impl Buf {
    fn new() -> Self {
        Self::default()
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
    fn pad_to(&mut self, n: usize) -> &mut Self {
        assert!(
            self.0.len() <= n,
            "buffer already at {} > {n}",
            self.0.len()
        );
        self.0.resize(n, 0);
        self
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn be16(&mut self, v: u16) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
    fn be32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
    fn be64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
    fn put_u32(&mut self, at: usize, v: u32) -> &mut Self {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
        self
    }
    fn done(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

fn checksum_paths(r: &TemplateResult) -> Vec<(String, String, u64, u64, Option<i128>)> {
    checksum_fields(r)
        .into_iter()
        .map(|c| (c.path, c.algorithm, c.start, c.end, c.stored))
        .collect()
}

// ----- library API -----------------------------------------------------------

#[test]
fn every_entry_parses_without_errors() {
    for e in library::entries() {
        let p = library::program(e.id);
        assert!(p.is_ok(), "{}: {:?}", e.id, p.err());
        assert!(!e.name.is_empty() && !e.description.is_empty(), "{}", e.id);
    }
}

#[test]
fn list_serializes_camel_case() {
    let list = library::list();
    assert_eq!(list.len(), library::entries().len());
    let png = list.iter().find(|i| i.id == "png").expect("png entry");
    assert_eq!(png.extensions, vec!["png", "apng"]);
    let json = serde_json::to_value(png).expect("json");
    assert_eq!(json["id"], "png");
    assert!(json["mime"].as_array().is_some_and(|m| !m.is_empty()));
    let back: LibraryInfo = serde_json::from_value(json).expect("round trip");
    assert_eq!(&back, png);
}

// ----- PNG -------------------------------------------------------------------

fn png_chunk(out: &mut Buf, kind: &[u8; 4], data: &[u8]) {
    out.be32(u32::try_from(data.len()).expect("len"));
    let start = out.len();
    out.bytes(kind).bytes(data);
    let crc = crc32(&out.0[start..]);
    out.be32(crc);
}

fn png_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Buf::new();
    ihdr.be32(640).be32(480).u8(8).u8(6).u8(0).u8(0).u8(1);
    png_chunk(&mut b, b"IHDR", &ihdr.done());
    png_chunk(&mut b, b"PLTE", &[255, 0, 0, 0, 255, 0]);
    png_chunk(&mut b, b"tEXt", b"Title\0Hello");
    png_chunk(&mut b, b"IDAT", &[0x78, 0x9C, 0x01, 0x02]);
    png_chunk(&mut b, b"IEND", &[]);
    b.done()
}

#[test]
fn png_chunks_and_crc() {
    let data = png_sample();
    let r = eval_entry("png", &data);
    assert_eq!(get(&r, "chunks").len, 5);
    assert_eq!(text(&r, "chunks[0].type"), "IHDR");
    assert_eq!(int(&r, "chunks[0].header.width"), 640);
    assert_eq!(int(&r, "chunks[0].header.height"), 480);
    assert_eq!(enum_name(&r, "chunks[0].header.color_type"), "RGBA");
    assert_eq!(enum_name(&r, "chunks[0].header.interlace"), "Adam7");
    assert_eq!(get(&r, "chunks[1].palette").len, 2);
    assert_eq!(
        get(&r, "chunks[1].palette[1]").value.as_deref(),
        Some("#00FF00")
    );
    assert_eq!(text(&r, "chunks[2].text.keyword"), "Title");
    assert_eq!(text(&r, "chunks[2].text.text"), "Hello");
    assert_eq!(get(&r, "chunks[3].data").size, 4);
    assert_eq!(text(&r, "chunks[4].type"), "IEND");
    let sums = checksum_paths(&r);
    assert_eq!(sums.len(), 5);
    let (path, alg, start, end, stored) = &sums[0];
    assert_eq!(
        (path.as_str(), alg.as_str(), *start, *end),
        ("chunks[0].crc", "crc32", 12, 29)
    );
    assert_eq!(*stored, Some(i128::from(crc32(&data[12..29]))));
    for (_, _, start, end, stored) in &sums {
        let (s, e) = (
            usize::try_from(*start).unwrap(),
            usize::try_from(*end).unwrap(),
        );
        assert_eq!(*stored, Some(i128::from(crc32(&data[s..e]))));
    }
}

// ----- JPEG ------------------------------------------------------------------

fn jpeg_segment(out: &mut Buf, marker: u8, body: &[u8]) {
    out.u8(0xFF)
        .u8(marker)
        .be16(u16::try_from(body.len() + 2).expect("len"))
        .bytes(body);
}

fn jpeg_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(&[0xFF, 0xD8]);
    let mut app0 = Buf::new();
    app0.bytes(b"JFIF\0")
        .u8(1)
        .u8(2)
        .u8(1)
        .be16(72)
        .be16(72)
        .u8(0)
        .u8(0);
    jpeg_segment(&mut b, 0xE0, &app0.done());
    // APP1 Exif with a big-endian TIFF header and one ASCII entry (Make).
    let mut app1 = Buf::new();
    app1.bytes(b"Exif\0\0").bytes(b"MM").be16(42).be32(8);
    app1.be16(1).be16(0x010F).be16(2).be32(6).be32(26).be32(0);
    app1.bytes(b"Canon\0");
    jpeg_segment(&mut b, 0xE1, &app1.done());
    let mut dqt = Buf::new();
    dqt.u8(0x00).bytes(&[1; 64]);
    jpeg_segment(&mut b, 0xDB, &dqt.done());
    let mut sof = Buf::new();
    sof.u8(8).be16(16).be16(32).u8(1).u8(1).u8(0x22).u8(0);
    jpeg_segment(&mut b, 0xC0, &sof.done());
    let mut dht = Buf::new();
    let mut counts = [0u8; 16];
    counts[0] = 2;
    dht.u8(0x10).bytes(&counts).bytes(&[0x00, 0x01]);
    jpeg_segment(&mut b, 0xC4, &dht.done());
    jpeg_segment(&mut b, 0xFE, b"hexa");
    jpeg_segment(&mut b, 0xDA, &[1, 1, 0x00, 0, 63, 0]);
    b.bytes(&[0x12, 0x34, 0xFF, 0x00, 0x56]);
    b.bytes(&[0xFF, 0xD9]);
    b.done()
}

#[test]
fn jpeg_segments() {
    let r = eval_entry("jpeg", &jpeg_sample());
    assert_eq!(get(&r, "segments").len, 9);
    assert_eq!(enum_name(&r, "segments[0].marker"), "SOI");
    assert_eq!(text(&r, "segments[1].app0.identifier"), "JFIF");
    assert_eq!(
        enum_name(&r, "segments[1].app0.density_unit"),
        "DotsPerInch"
    );
    assert_eq!(text(&r, "segments[2].app1.tiff.byte_order"), "MM");
    assert_eq!(int(&r, "segments[2].app1.tiff.ifd0.entry_count"), 1);
    assert_eq!(
        enum_name(&r, "segments[2].app1.tiff.ifd0.entries[0].tag"),
        "Make"
    );
    assert_eq!(
        text(&r, "segments[2].app1.tiff.ifd0.entries[0].text"),
        "Canon"
    );
    assert_eq!(get(&r, "segments[3].quant_tables").len, 1);
    assert_eq!(enum_name(&r, "segments[4].marker"), "SOF0");
    assert_eq!(int(&r, "segments[4].frame.width"), 32);
    assert_eq!(int(&r, "segments[4].frame.height"), 16);
    assert_eq!(
        int(&r, "segments[4].frame.components[0].sampling.horizontal"),
        2
    );
    assert_eq!(get(&r, "segments[5].huffman_tables[0].symbols").len, 2);
    assert_eq!(int(&r, "segments[5].huffman_tables[0].info.table_class"), 1);
    assert_eq!(text(&r, "segments[6].comment"), "hexa");
    assert_eq!(enum_name(&r, "segments[7].marker"), "SOS");
    assert_eq!(get(&r, "segments[7].image_data").size, 5);
    assert_eq!(enum_name(&r, "segments[8].marker"), "EOI");
}

// ----- GIF -------------------------------------------------------------------

fn gif_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(b"GIF89a").u16(2).u16(1).u8(0x80).u8(0).u8(0);
    b.bytes(&[0, 0, 0, 255, 255, 255]);
    b.bytes(&[0x21, 0xF9, 4, 0x05, 10, 0, 1, 0]);
    b.bytes(&[0x21, 0xFF, 11])
        .bytes(b"NETSCAPE2.0")
        .bytes(&[3, 1, 0, 0, 0]);
    b.bytes(&[0x2C]).u16(0).u16(0).u16(2).u16(1).u8(0);
    b.bytes(&[2, 2, 0x4C, 0x01, 0]);
    b.u8(0x3B);
    b.done()
}

#[test]
fn gif_blocks() {
    let r = eval_entry("gif", &gif_sample());
    assert_eq!(text(&r, "header.version"), "89a");
    assert_eq!(int(&r, "screen.width"), 2);
    assert_eq!(
        get(&r, "screen.flags.global_table").raw,
        Some(hexa_template::Value::Bool(true))
    );
    assert_eq!(get(&r, "global_color_table").len, 2);
    assert_eq!(
        get(&r, "global_color_table[1]").value.as_deref(),
        Some("#FFFFFF")
    );
    assert_eq!(get(&r, "blocks").len, 4);
    assert_eq!(enum_name(&r, "blocks[0].label"), "GraphicControl");
    assert_eq!(
        enum_name(&r, "blocks[0].graphic_control.flags.disposal"),
        "DoNotDispose"
    );
    assert_eq!(int(&r, "blocks[0].graphic_control.delay_time"), 10);
    assert_eq!(text(&r, "blocks[1].application.identifier"), "NETSCAPE");
    assert_eq!(int(&r, "blocks[2].image.width"), 2);
    assert_eq!(get(&r, "blocks[2].image.image_data.blocks").len, 2);
    assert_eq!(enum_name(&r, "blocks[3].type"), "Trailer");
}

// ----- BMP -------------------------------------------------------------------

fn bmp_sample() -> Vec<u8> {
    let mut b = Buf::new();
    // 2x2, 8 bpp, two palette entries, rows padded to 4 bytes.
    b.bytes(b"BM").u32(0).u32(0).u32(14 + 40 + 8);
    b.u32(40)
        .u32(2)
        .u32(2)
        .u16(1)
        .u16(8)
        .u32(0)
        .u32(8)
        .u32(2835)
        .u32(2835)
        .u32(2)
        .u32(0);
    b.bytes(&[0, 0, 255, 0, 255, 255, 255, 0]);
    b.bytes(&[0, 1, 0, 0, 1, 0, 0, 0]);
    let len = u32::try_from(b.len()).expect("len");
    b.put_u32(2, len);
    b.done()
}

#[test]
fn bmp_headers_palette_and_pixels() {
    let r = eval_entry("bmp", &bmp_sample());
    assert_eq!(text(&r, "file.signature"), "BM");
    assert_eq!(int(&r, "file.file_size"), 70);
    assert_eq!(int(&r, "info.width"), 2);
    assert_eq!(int(&r, "info.bits_per_pixel"), 8);
    assert_eq!(enum_name(&r, "info.compression"), "RGB");
    assert_eq!(int(&r, "info.row_stride"), 4);
    assert_eq!(get(&r, "palette").len, 2);
    assert_eq!(get(&r, "palette[0]").value.as_deref(), Some("#FF0000"));
    let px = get(&r, "pixel_data");
    assert_eq!((px.offset, px.size), (62, 8));
}

// ----- ICO -------------------------------------------------------------------

fn ico_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.u16(0).u16(1).u16(2);
    // Entry 0: 1x1 32-bpp DIB; entry 1: PNG stream.
    b.u8(1).u8(1).u8(0).u8(0).u16(1).u16(32).u32(48).u32(6 + 32);
    b.u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u16(1)
        .u16(32)
        .u32(16)
        .u32(6 + 32 + 48);
    b.u32(40)
        .u32(1)
        .u32(2)
        .u16(1)
        .u16(32)
        .u32(0)
        .u32(8)
        .u32(0)
        .u32(0)
        .u32(0)
        .u32(0);
    b.bytes(&[1, 2, 3, 4, 0, 0, 0, 0]);
    b.bytes(b"\x89PNG\r\n\x1a\n").zeros(8);
    b.done()
}

#[test]
fn ico_directory_with_dib_and_png() {
    let r = eval_entry("ico", &ico_sample());
    assert_eq!(enum_name(&r, "icon.type"), "Icon");
    assert_eq!(get(&r, "icon.entries").len, 2);
    assert_eq!(int(&r, "icon.entries[0].bits_per_pixel"), 32);
    assert_eq!(int(&r, "icon.entries[0].dib_image.header.height"), 2);
    assert_eq!(get(&r, "icon.entries[0].dib_image.pixel_data").size, 8);
    assert_eq!(int(&r, "icon.entries[1].pixel_width"), 256);
    let png = get(&r, "icon.entries[1].png_image");
    assert_eq!((png.offset, png.size), (86, 16));
}

// ----- WAV -------------------------------------------------------------------

fn wav_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(b"RIFF").u32(0).bytes(b"WAVE");
    b.bytes(b"fmt ")
        .u32(40)
        .u16(0xFFFE)
        .u16(2)
        .u32(44100)
        .u32(176_400)
        .u16(4)
        .u16(16);
    b.u16(22).u16(16).u32(3);
    b.u32(1)
        .u16(0)
        .u16(0x10)
        .bytes(&[0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
    b.bytes(b"LIST")
        .u32(4 + 8 + 5 + 1)
        .bytes(b"INFO")
        .bytes(b"INAM")
        .u32(5)
        .bytes(b"song\0")
        .u8(0);
    b.bytes(b"data").u32(8).bytes(&[0, 0, 1, 0, 2, 0, 3, 0]);
    let len = u32::try_from(b.len() - 8).expect("len");
    b.put_u32(4, len);
    b.done()
}

#[test]
fn wav_chunks() {
    let r = eval_entry("wav", &wav_sample());
    assert_eq!(text(&r, "header.wave"), "WAVE");
    assert_eq!(get(&r, "chunks").len, 3);
    assert_eq!(enum_name(&r, "chunks[0].format.format"), "Extensible");
    assert_eq!(int(&r, "chunks[0].format.channels"), 2);
    assert_eq!(int(&r, "chunks[0].format.sample_rate"), 44100);
    assert_eq!(
        enum_name(&r, "chunks[0].format.channel_mask"),
        "FrontLeft | FrontRight"
    );
    assert_eq!(
        get(&r, "chunks[0].format.sub_format").value.as_deref(),
        Some("00000001-0000-0010-8000-00AA00389B71")
    );
    assert_eq!(text(&r, "chunks[1].list.items[0].id"), "INAM");
    assert_eq!(text(&r, "chunks[1].list.items[0].text"), "song");
    assert_eq!(get(&r, "chunks[2].samples").size, 8);
}

// ----- MP3 -------------------------------------------------------------------

fn mp3_sample() -> Vec<u8> {
    let mut b = Buf::new();
    // ID3v2.4 with a TIT2 frame (UTF-8) and padding.
    let mut frame = Buf::new();
    frame.bytes(b"TIT2").be32(6).be16(0).u8(3).bytes(b"Hello");
    let frame = frame.done();
    b.bytes(b"ID3")
        .u8(4)
        .u8(0)
        .u8(0)
        .be32(u32::try_from(frame.len() + 4).expect("len"));
    b.bytes(&frame).zeros(4);
    // Two MPEG-1 Layer III frames, 128 kbit/s, 44100 Hz: 417 bytes each.
    for _ in 0..2 {
        let start = b.len();
        b.bytes(&[0xFF, 0xFB, 0x90, 0x64]);
        b.pad_to(start + 417);
    }
    // ID3v1 tag.
    let start = b.len();
    b.bytes(b"TAG").bytes(b"Title");
    b.pad_to(start + 3 + 30 + 30 + 30 + 4 + 28);
    b.u8(0).u8(7).u8(17);
    b.done()
}

#[test]
fn mp3_id3_and_frames() {
    let r = eval_entry("mp3", &mp3_sample());
    assert_eq!(int(&r, "id3v2.header.version_major"), 4);
    assert_eq!(int(&r, "id3v2.header.tag_size"), 20);
    assert_eq!(get(&r, "id3v2.frames").len, 1);
    assert_eq!(text(&r, "id3v2.frames[0].id"), "TIT2");
    assert_eq!(enum_name(&r, "id3v2.frames[0].encoding"), "UTF8");
    assert_eq!(text(&r, "id3v2.frames[0].text"), "Hello");
    assert_eq!(get(&r, "id3v2.padding_bytes").size, 4);
    assert_eq!(get(&r, "frames").len, 2);
    assert_eq!(enum_name(&r, "frames[0].header.version"), "MPEG1");
    assert_eq!(enum_name(&r, "frames[0].header.layer"), "LayerIII");
    assert_eq!(
        enum_name(&r, "frames[0].header.channel_mode"),
        "JointStereo"
    );
    assert_eq!(int(&r, "frames[0].bitrate_kbps"), 128);
    assert_eq!(int(&r, "frames[0].sample_rate"), 44100);
    assert_eq!(int(&r, "frames[0].frame_length"), 417);
    assert_eq!(get(&r, "frames[1]").size, 417);
    assert_eq!(text(&r, "id3v1.title"), "Title");
    assert_eq!(int(&r, "id3v1.track"), 7);
    assert_eq!(int(&r, "id3v1.genre"), 17);
}

// ----- MP4 -------------------------------------------------------------------

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Buf::new();
    b.be32(u32::try_from(body.len() + 8).expect("len"))
        .bytes(kind)
        .bytes(body);
    b.done()
}

fn mp4_sample() -> Vec<u8> {
    let mut ftyp = Buf::new();
    ftyp.bytes(b"isom").be32(512).bytes(b"isomiso2mp41");
    let mut mvhd = Buf::new();
    mvhd.u8(0)
        .bytes(&[0, 0, 0])
        .be32(0)
        .be32(0)
        .be32(1000)
        .be32(5000);
    mvhd.be32(0x0001_0000)
        .be16(0x0100)
        .zeros(10)
        .zeros(36)
        .zeros(24)
        .be32(2);
    let mut tkhd = Buf::new();
    tkhd.u8(0)
        .bytes(&[0, 0, 3])
        .be32(0)
        .be32(0)
        .be32(1)
        .be32(0)
        .be32(5000);
    tkhd.zeros(8)
        .be16(0)
        .be16(0)
        .be16(0)
        .zeros(2)
        .zeros(36)
        .be32(640 << 16)
        .be32(480 << 16);
    let mut mdhd = Buf::new();
    mdhd.u8(0)
        .zeros(3)
        .be32(0)
        .be32(0)
        .be32(44100)
        .be32(0)
        .be16(0x55C4)
        .be16(0);
    let mut hdlr = Buf::new();
    hdlr.u8(0)
        .zeros(3)
        .be32(0)
        .bytes(b"vide")
        .zeros(12)
        .bytes(b"Video\0");
    let mut stco = Buf::new();
    stco.u8(0).zeros(3).be32(2).be32(100).be32(200);
    let stbl = mp4_box(b"stbl", &mp4_box(b"stco", &stco.done()));
    let minf = mp4_box(b"minf", &stbl);
    let mut mdia_body = mp4_box(b"mdhd", &mdhd.done());
    mdia_body.extend(mp4_box(b"hdlr", &hdlr.done()));
    mdia_body.extend(minf);
    let mut trak_body = mp4_box(b"tkhd", &tkhd.done());
    trak_body.extend(mp4_box(b"mdia", &mdia_body));
    let mut moov_body = mp4_box(b"mvhd", &mvhd.done());
    moov_body.extend(mp4_box(b"trak", &trak_body));
    let mut out = mp4_box(b"ftyp", &ftyp.done());
    out.extend(mp4_box(b"moov", &moov_body));
    // mdat with a 64-bit large size.
    let mut mdat = Buf::new();
    mdat.be32(1)
        .bytes(b"mdat")
        .be64(16 + 4)
        .bytes(&[1, 2, 3, 4]);
    out.extend(mdat.done());
    out
}

#[test]
fn mp4_nested_boxes() {
    let r = eval_entry("mp4", &mp4_sample());
    assert_eq!(get(&r, "boxes").len, 3);
    assert_eq!(text(&r, "boxes[0].file_type.major_brand"), "isom");
    assert_eq!(get(&r, "boxes[0].file_type.compatible_brands").len, 3);
    assert_eq!(text(&r, "boxes[1].type"), "moov");
    assert_eq!(
        int(&r, "boxes[1].children.boxes[0].movie_header.timescale"),
        1000
    );
    assert_eq!(
        int(
            &r,
            "boxes[1].children.boxes[0].movie_header.duration_seconds"
        ),
        5
    );
    let trak = "boxes[1].children.boxes[1].children";
    assert_eq!(
        get(&r, &format!("{trak}.boxes[0].track_header.width"))
            .value
            .as_deref(),
        Some("640.0000")
    );
    let mdia = format!("{trak}.boxes[1].children");
    assert_eq!(
        get(&r, &format!("{mdia}.boxes[0].media_header.language"))
            .value
            .as_deref(),
        Some("und")
    );
    assert_eq!(
        text(&r, &format!("{mdia}.boxes[1].handler.handler_type")),
        "vide"
    );
    let stco = format!("{mdia}.boxes[2].children.boxes[0].children.boxes[0].chunk_offset");
    assert_eq!(int(&r, &format!("{stco}.entry_count")), 2);
    assert_eq!(text(&r, "boxes[2].type"), "mdat");
    assert_eq!(int(&r, "boxes[2].large_size"), 20);
    assert_eq!(get(&r, "boxes[2].media_data").size, 4);
}

// ----- PDF -------------------------------------------------------------------

fn pdf_sample() -> Vec<u8> {
    let mut s = String::from("%PDF-1.7\n%\u{e2}\u{e3}\n");
    let obj1 = s.len();
    s.push_str("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj2 = s.len();
    s.push_str("2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref = s.len();
    s.push_str("xref\n0 3\n0000000000 65535 f \n");
    let _ = write!(s, "{obj1:010} 00000 n \n{obj2:010} 00000 n \n");
    s.push_str("trailer\n<< /Size 3 /Root 1 0 R /Info << /A 1 >> >>\n");
    let _ = write!(s, "startxref\n{xref}\n%%EOF\n");
    s.into_bytes()
}

#[test]
fn pdf_header_xref_and_trailer() {
    let data = pdf_sample();
    let r = eval_entry("pdf", &data);
    assert_eq!(text(&r, "header.signature"), "%PDF-");
    assert_eq!(text(&r, "header.version"), "1.7");
    assert!(r.find("header.binary_marker").is_some());
    let xref_at = i128::try_from(data.windows(4).position(|w| w == b"xref").unwrap()).unwrap();
    assert_eq!(int(&r, "startxref.xref_offset"), xref_at);
    assert_eq!(text(&r, "startxref.eof_marker"), "%%EOF");
    assert_eq!(int(&r, "xref.subsections[0].count"), 3);
    assert_eq!(get(&r, "xref.subsections[0].entries").len, 3);
    assert_eq!(
        get(&r, "xref.subsections[0].entries[0].in_use").raw,
        Some(hexa_template::Value::Bool(false))
    );
    assert_eq!(int(&r, "xref.subsections[0].entries[1].object_offset"), 15);
    assert_eq!(int(&r, "xref.subsections[0].entries[2].generation"), 0);
    assert_eq!(
        text(&r, "xref.trailer.dictionary"),
        "<< /Size 3 /Root 1 0 R /Info << /A 1 >> >>"
    );
}

// ----- SQLite ----------------------------------------------------------------

fn sqlite_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(b"SQLite format 3\0")
        .be16(512)
        .u8(1)
        .u8(1)
        .u8(0)
        .u8(64)
        .u8(32)
        .u8(32);
    b.be32(7)
        .be32(2)
        .be32(0)
        .be32(0)
        .be32(1)
        .be32(4)
        .be32(0)
        .be32(0)
        .be32(1);
    b.be32(0).be32(0).be32(0).zeros(20).be32(7).be32(3_045_001);
    // Page 1 b-tree header: leaf table with one cell.
    b.u8(0x0D).be16(0).be16(1).be16(500).u8(0).be16(500);
    b.pad_to(512);
    // Page 2: interior table page.
    b.u8(0x05)
        .be16(0)
        .be16(2)
        .be16(400)
        .u8(0)
        .be32(3)
        .be16(450)
        .be16(460);
    b.pad_to(1024);
    b.done()
}

#[test]
fn sqlite_header_and_pages() {
    let r = eval_entry("sqlite", &sqlite_sample());
    assert_eq!(text(&r, "header.magic"), "SQLite format 3");
    assert_eq!(int(&r, "header.page_size"), 512);
    assert_eq!(enum_name(&r, "header.text_encoding"), "UTF8");
    assert_eq!(int(&r, "header.sqlite_version_number"), 3_045_001);
    assert_eq!(enum_name(&r, "first_page.page_type"), "LeafTable");
    assert_eq!(int(&r, "first_page.cell_count"), 1);
    assert_eq!(get(&r, "pages").len, 1);
    assert_eq!(enum_name(&r, "pages[0].btree.page_type"), "InteriorTable");
    assert_eq!(int(&r, "pages[0].btree.right_most_pointer"), 3);
    assert_eq!(get(&r, "pages[0].btree.cell_pointers").len, 2);
    assert_eq!(get(&r, "pages[0]").size, 512);
}

// ----- MBR / GPT ---------------------------------------------------------------

fn mbr_entry(b: &mut Buf, status: u8, kind: u8, first_lba: u32, sectors: u32) {
    b.u8(status)
        .bytes(&[1, 2, 0])
        .u8(kind)
        .bytes(&[0xFE, 0xFF, 0xFF])
        .u32(first_lba)
        .u32(sectors);
}

fn mbr_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.zeros(440).u32(0xDEAD_BEEF).u16(0);
    mbr_entry(&mut b, 0x80, 0x83, 2048, 4096);
    mbr_entry(&mut b, 0, 0x05, 8192, 4096);
    b.zeros(32).u16(0xAA55);
    b.pad_to(8192 * 512);
    // First EBR: logical partition at +63, link to a second EBR at +2048.
    b.zeros(446);
    mbr_entry(&mut b, 0, 0x07, 63, 1000);
    mbr_entry(&mut b, 0, 0x05, 2048, 1063);
    b.zeros(32).u16(0xAA55);
    b.pad_to((8192 + 2048) * 512);
    b.zeros(446);
    mbr_entry(&mut b, 0, 0x0B, 63, 1000);
    b.zeros(16 + 32).u16(0xAA55);
    b.done()
}

#[test]
fn mbr_partitions_and_logical_chain() {
    let r = eval_entry("mbr", &mbr_sample());
    assert_eq!(int(&r, "mbr.disk_signature"), 0xDEAD_BEEF);
    assert_eq!(enum_name(&r, "mbr.partitions[0].status"), "Active");
    assert_eq!(enum_name(&r, "mbr.partitions[0].type"), "Linux");
    assert_eq!(int(&r, "mbr.partitions[0].first_lba"), 2048);
    assert_eq!(int(&r, "mbr.partitions[0].first_chs.sector"), 2);
    assert_eq!(int(&r, "mbr.partitions[0].last_chs.cylinder"), 1023);
    assert_eq!(enum_name(&r, "mbr.partitions[2].type"), "Empty");
    assert_eq!(int(&r, "mbr.boot_signature"), 0xAA55);
    assert_eq!(
        enum_name(&r, "logical_partitions.logical_partition.type"),
        "NTFS_exFAT_HPFS"
    );
    assert_eq!(get(&r, "logical_partitions").offset, 8192 * 512);
    assert_eq!(
        enum_name(&r, "logical_partitions.next.logical_partition.type"),
        "FAT32"
    );
    assert_eq!(
        get(&r, "logical_partitions.next").offset,
        (8192 + 2048) * 512
    );
}

fn guid_bytes(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Vec<u8> {
    let mut b = Buf::new();
    b.u32(d1).u16(d2).u16(d3).bytes(&d4);
    b.done()
}

fn gpt_sample() -> Vec<u8> {
    let entries = 4u32;
    let mut part = Buf::new();
    part.bytes(&guid_bytes(
        0xC12A_7328,
        0xF81F,
        0x11D2,
        [0xBA, 0x4B, 0, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
    ));
    part.bytes(&guid_bytes(1, 2, 3, [4, 5, 6, 7, 8, 9, 10, 11]));
    part.u64(34).u64(1057).u64(1);
    for c in "EFI".encode_utf16() {
        part.u16(c);
    }
    part.pad_to(128);
    let mut array = part.done();
    array.resize(128 * entries as usize, 0);
    let mut b = Buf::new();
    b.zeros(446);
    mbr_entry(&mut b, 0, 0xEE, 1, 0xFFFF_FFFF);
    b.zeros(48).u16(0xAA55);
    b.bytes(b"EFI PART").u32(0x0001_0000).u32(92).u32(0).u32(0);
    b.u64(1).u64(0).u64(34).u64(2000);
    b.bytes(&guid_bytes(
        0x1122_3344,
        0x5566,
        0x7788,
        [0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0],
    ));
    b.u64(2).u32(entries).u32(128).u32(crc32(&array));
    b.pad_to(1024).bytes(&array);
    b.done()
}

#[test]
fn gpt_header_and_entries() {
    let data = gpt_sample();
    let r = eval_entry("gpt", &data);
    assert_eq!(
        enum_name(&r, "protective_mbr.partitions[0].type"),
        "GPTProtective"
    );
    assert_eq!(text(&r, "gpt.signature"), "EFI PART");
    assert_eq!(int(&r, "gpt.first_usable_lba"), 34);
    assert_eq!(
        get(&r, "gpt.disk_guid").value.as_deref(),
        Some("11223344-5566-7788-99AA-BBCCDDEEFF00")
    );
    assert_eq!(get(&r, "partitions").len, 4);
    let t = get(&r, "partitions[0].type_guid");
    assert_eq!(
        t.value.as_deref(),
        Some("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
    );
    assert_eq!(t.comment.as_deref(), Some("EFI system partition"));
    assert_eq!(text(&r, "partitions[0].name"), "EFI");
    assert_eq!(int(&r, "partitions[0].last_lba"), 1057);
    assert_eq!(
        get(&r, "partitions[0].attributes.required_partition").raw,
        Some(hexa_template::Value::Bool(true))
    );
    assert_eq!(get(&r, "partitions[1]").value.as_deref(), Some("unused"));
    let sums = checksum_paths(&r);
    assert_eq!(sums.len(), 1);
    let (path, alg, start, end, stored) = &sums[0];
    assert_eq!(
        (path.as_str(), alg.as_str(), *start, *end),
        ("gpt.partition_entries_crc32", "crc32", 1024, 1536)
    );
    assert_eq!(*stored, Some(i128::from(crc32(&data[1024..1536]))));
}

// ----- FAT -------------------------------------------------------------------

fn fat12_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(&[0xEB, 0x3C, 0x90]).bytes(b"MSDOS5.0");
    b.u16(512)
        .u8(1)
        .u16(1)
        .u8(2)
        .u16(16)
        .u16(64)
        .u8(0xF8)
        .u16(1)
        .u16(32)
        .u16(2)
        .u32(0)
        .u32(0);
    b.u8(0x80)
        .u8(0)
        .u8(0x29)
        .u32(0x1234_5678)
        .bytes(b"HEXA DISK  ")
        .bytes(b"FAT12   ");
    b.pad_to(510).u16(0xAA55);
    b.bytes(&[0xF8, 0xFF, 0xFF]).pad_to(1024);
    b.bytes(&[0xF8, 0xFF, 0xFF]).pad_to(1536);
    // Long file name fragment followed by the 8.3 entry.
    b.u8(0x41);
    for c in "hello".encode_utf16() {
        b.u16(c);
    }
    b.u8(0x0F).u8(0).u8(0x55);
    for c in [
        u16::from(b'.'),
        u16::from(b't'),
        u16::from(b'x'),
        u16::from(b't'),
        0,
        0xFFFF,
    ] {
        b.u16(c);
    }
    b.u16(0).u16(0xFFFF).u16(0xFFFF);
    b.bytes(b"HELLO   TXT")
        .u8(0x20)
        .u8(0)
        .u8(0)
        .u16(0)
        .u16(0)
        .u16(0)
        .u16(0);
    b.u16((10 << 11) | (30 << 5) | 0xF)
        .u16(((2024 - 1980) << 9) | (5 << 5) | 0x11);
    b.u16(2).u32(12);
    b.pad_to(2048);
    b.done()
}

#[test]
fn fat12_boot_sector_and_root_directory() {
    let r = eval_entry("fat", &fat12_sample());
    assert_eq!(text(&r, "boot.oem_name"), "MSDOS5.0");
    assert_eq!(int(&r, "boot.bytes_per_sector"), 512);
    assert_eq!(enum_name(&r, "boot.media"), "FixedDisk");
    assert_eq!(text(&r, "boot.volume_label"), "HEXA DISK  ");
    assert_eq!(text(&r, "boot.fs_type"), "FAT12   ");
    assert_eq!(int(&r, "boot.fat_bits"), 12);
    assert_eq!(int(&r, "boot.first_data_sector"), 4);
    assert_eq!(get(&r, "fat").size, 512);
    assert_eq!(get(&r, "root_directory").len, 2);
    assert_eq!(text(&r, "root_directory[0].name_part1"), "hello");
    assert_eq!(text(&r, "root_directory[0].name_part2"), ".txt");
    assert_eq!(text(&r, "root_directory[1].name"), "HELLO   ");
    assert_eq!(text(&r, "root_directory[1].extension"), "TXT");
    assert_eq!(
        get(&r, "root_directory[1].attributes.archive").raw,
        Some(hexa_template::Value::Bool(true))
    );
    assert_eq!(
        get(&r, "root_directory[1].write_time").value.as_deref(),
        Some("10:30:30")
    );
    assert_eq!(
        get(&r, "root_directory[1].write_date").value.as_deref(),
        Some("2024-05-17")
    );
    assert_eq!(int(&r, "root_directory[1].first_cluster"), 2);
    assert_eq!(int(&r, "root_directory[1].file_size"), 12);
}

#[test]
fn fat32_boot_sector_and_fs_info() {
    let mut b = Buf::new();
    b.bytes(&[0xEB, 0x58, 0x90]).bytes(b"mkfs.fat");
    b.u16(512)
        .u8(1)
        .u16(32)
        .u8(2)
        .u16(0)
        .u16(0)
        .u8(0xF8)
        .u16(0)
        .u16(32)
        .u16(64)
        .u32(0)
        .u32(70_000);
    b.u32(600).u16(0).u16(0).u32(2).u16(1).u16(6).zeros(12);
    b.u8(0x80)
        .u8(0)
        .u8(0x29)
        .u32(0xCAFE)
        .bytes(b"NO NAME    ")
        .bytes(b"FAT32   ");
    b.pad_to(510).u16(0xAA55);
    b.u32(0x4161_5252)
        .zeros(480)
        .u32(0x6141_7272)
        .u32(1234)
        .u32(5)
        .zeros(12)
        .u32(0xAA55_0000);
    let r = eval_entry("fat", &b.done());
    assert_eq!(int(&r, "boot.fat_bits"), 32);
    assert_eq!(int(&r, "boot.sectors_per_fat"), 600);
    assert_eq!(int(&r, "boot.root_cluster"), 2);
    assert_eq!(text(&r, "boot.fs_type"), "FAT32   ");
    assert_eq!(int(&r, "fs_info.free_cluster_count"), 1234);
    assert!(r.find("fat").is_none() && r.find("root_directory").is_none());
}

// ----- NTFS ------------------------------------------------------------------

fn ntfs_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(&[0xEB, 0x52, 0x90]).bytes(b"NTFS    ");
    b.u16(512)
        .u8(1)
        .u16(0)
        .zeros(3)
        .u16(0)
        .u8(0xF8)
        .u16(0)
        .u16(63)
        .u16(255)
        .u32(2048)
        .u32(0);
    b.u32(0x0080_0080)
        .u64(100_000)
        .u64(2)
        .u64(50)
        .u8(0xF6)
        .zeros(3)
        .u8(1)
        .zeros(3);
    b.u64(0x0123_4567_89AB_CDEF).u32(0);
    b.pad_to(510).u16(0xAA55);
    b.pad_to(1024);
    // MFT record 0 ($MFT) with $STANDARD_INFORMATION and $FILE_NAME.
    let rec = b.len();
    b.bytes(b"FILE")
        .u16(48)
        .u16(3)
        .u64(0)
        .u16(1)
        .u16(1)
        .u16(56)
        .u16(1)
        .u32(0)
        .u32(1024);
    b.u64(0).u16(3).u16(0).u32(0);
    b.u16(1).u16(0).u16(0).pad_to(rec + 56);
    // $STANDARD_INFORMATION (resident, 48 bytes of value).
    let ft = 133_000_000_000_000_000u64;
    b.u32(0x10)
        .u32(24 + 48)
        .u8(0)
        .u8(0)
        .u16(0)
        .u16(0)
        .u16(0)
        .u32(48)
        .u16(24)
        .u8(0)
        .u8(0);
    b.u64(ft)
        .u64(ft)
        .u64(ft)
        .u64(ft)
        .u32(0x6)
        .u32(0)
        .u32(0)
        .u32(0);
    // $FILE_NAME "$MFT" (resident).
    let name: Vec<u16> = "$MFT".encode_utf16().collect();
    let value_len = 66 + 2 * name.len();
    let attr_len = (24 + value_len).next_multiple_of(8);
    let attr = b.len();
    b.u32(0x30)
        .u32(u32::try_from(attr_len).unwrap())
        .u8(0)
        .u8(0)
        .u16(0)
        .u16(0)
        .u16(1);
    b.u32(u32::try_from(value_len).unwrap()).u16(24).u8(1).u8(0);
    b.u64(5 | (5 << 48))
        .u64(ft)
        .u64(ft)
        .u64(ft)
        .u64(ft)
        .u64(16384)
        .u64(16384)
        .u32(0x6)
        .u32(0);
    b.u8(4).u8(3);
    for c in name {
        b.u16(c);
    }
    b.pad_to(attr + attr_len);
    // $DATA (non-resident).
    b.u32(0x80).u32(72).u8(1).u8(0).u16(0).u16(0).u16(2);
    b.u64(0)
        .u64(15)
        .u16(64)
        .u16(0)
        .zeros(4)
        .u64(16384)
        .u64(16384)
        .u64(16384)
        .u64(0);
    b.u32(0xFFFF_FFFF).u32(0);
    let used = u32::try_from(b.len() - rec).unwrap();
    b.put_u32(rec + 24, used);
    b.pad_to(rec + 1024);
    b.done()
}

#[test]
fn ntfs_boot_sector_and_mft() {
    let r = eval_entry("ntfs", &ntfs_sample());
    assert_eq!(text(&r, "boot.oem_id"), "NTFS    ");
    assert_eq!(int(&r, "boot.cluster_size"), 512);
    assert_eq!(int(&r, "boot.file_record_size"), 1024);
    assert_eq!(int(&r, "boot.mft_offset"), 1024);
    assert_eq!(int(&r, "boot.volume_serial_number"), 0x0123_4567_89AB_CDEF);
    assert_eq!(get(&r, "mft").len, 1);
    assert_eq!(text(&r, "mft[0].signature"), "FILE");
    assert_eq!(enum_name(&r, "mft[0].flags"), "InUse");
    assert_eq!(get(&r, "mft[0].attributes").len, 3);
    assert_eq!(
        enum_name(&r, "mft[0].attributes[0].type"),
        "StandardInformation"
    );
    assert_eq!(
        enum_name(&r, "mft[0].attributes[0].standard_information.attributes"),
        "Hidden | System"
    );
    assert_eq!(
        get(
            &r,
            "mft[0].attributes[0].standard_information.creation_time"
        )
        .value
        .as_deref(),
        Some("2022-06-18 04:26:40 UTC")
    );
    assert_eq!(enum_name(&r, "mft[0].attributes[1].type"), "FileName");
    assert_eq!(text(&r, "mft[0].attributes[1].file_name.name"), "$MFT");
    assert_eq!(
        enum_name(&r, "mft[0].attributes[1].file_name.name_space"),
        "Win32AndDOS"
    );
    assert_eq!(enum_name(&r, "mft[0].attributes[2].type"), "Data");
    assert_eq!(int(&r, "mft[0].attributes[2].real_size"), 16384);
    assert_eq!(enum_name(&r, "mft[0].end_marker"), "End");
    assert_eq!(get(&r, "mft[0]").size, 1024);
}

// ----- ext4 ------------------------------------------------------------------

fn ext4_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.zeros(1024);
    let sb = b.len();
    b.u32(128)
        .u32(8192)
        .u32(0)
        .u32(7000)
        .u32(100)
        .u32(1)
        .u32(0)
        .u32(0)
        .u32(8192)
        .u32(8192);
    b.u32(128)
        .u32(1_700_000_000)
        .u32(1_700_000_000)
        .u16(3)
        .u16(0xFFFF)
        .u16(0xEF53)
        .u16(1);
    b.u16(1).u16(0).u32(0).u32(0).u32(0).u32(1).u16(0).u16(0);
    b.u32(11).u16(256).u16(0).u32(0x3C).u32(0x2C2).u32(0x46B);
    b.bytes(&[0x11; 16]).bytes(b"rootfs").pad_to(sb + 0x3FC);
    b.u32(0xDEAD_BEEF);
    b.pad_to(2048);
    // Group descriptor 0: inode table at block 5.
    b.u32(3)
        .u32(4)
        .u32(5)
        .u16(6000)
        .u16(90)
        .u16(2)
        .u16(0x4)
        .zeros(12);
    b.pad_to(5 * 1024 + 256);
    // Root inode (inode 2): directory with one extent.
    b.u16(0x41ED)
        .u16(0)
        .u32(1024)
        .u32(0)
        .u32(0)
        .u32(1_700_000_000)
        .u32(0)
        .u16(0)
        .u16(3)
        .u32(2);
    b.u32(0x80000).u32(0);
    b.u16(0xF30A).u16(1).u16(4).u16(0).u32(0);
    b.u32(0).u16(1).u16(0).u32(42);
    b.zeros(60 - 24);
    b.pad_to(5 * 1024 + 512);
    b.done()
}

#[test]
fn ext4_superblock_groups_and_root_inode() {
    let data = ext4_sample();
    let r = eval_entry("ext4", &data);
    assert_eq!(int(&r, "superblock.magic"), 0xEF53);
    assert_eq!(get(&r, "superblock").size, 1024);
    assert_eq!(int(&r, "superblock.block_size"), 1024);
    assert_eq!(int(&r, "superblock.inode_size"), 256);
    assert_eq!(enum_name(&r, "superblock.state"), "Clean");
    assert_eq!(
        enum_name(&r, "superblock.feature_compat"),
        "HasJournal | ExtAttr | ResizeInode | DirIndex"
    );
    assert_eq!(
        enum_name(&r, "superblock.feature_incompat"),
        "FileType | Extents | Bit64 | FlexBg"
    );
    assert_eq!(text(&r, "superblock.volume_name"), "rootfs");
    assert_eq!(
        get(&r, "superblock.uuid").value.as_deref(),
        Some("11111111-1111-1111-1111-111111111111")
    );
    assert_eq!(
        get(&r, "superblock.mount_time").value.as_deref(),
        Some("2023-11-14 22:13:20 UTC")
    );
    assert_eq!(int(&r, "superblock.group_count"), 1);
    assert_eq!(int(&r, "group_descriptors[0].inode_table_lo"), 5);
    assert_eq!(get(&r, "root_inode").offset, 5 * 1024 + 256);
    assert_eq!(
        get(&r, "root_inode.mode").value.as_deref(),
        Some("directory 0755")
    );
    assert_eq!(enum_name(&r, "root_inode.flags"), "Extents");
    assert_eq!(int(&r, "root_inode.extent_tree.header.magic"), 0xF30A);
    assert_eq!(int(&r, "root_inode.extent_tree.extents[0].start_block"), 42);
    let sums = checksum_paths(&r);
    assert_eq!(sums.len(), 1);
    assert_eq!(
        (sums[0].1.as_str(), sums[0].2, sums[0].3),
        ("crc32c", 1024, 2044)
    );
}

// ----- ZIP -------------------------------------------------------------------

fn zip_sample() -> Vec<u8> {
    let content = b"hello world\n";
    let crc = crc32(content);
    let name = b"hello.txt";
    let mut b = Buf::new();
    b.u32(0x0403_4B50)
        .u16(20)
        .u16(0)
        .u16(0)
        .u16(0x6000)
        .u16(0x5821)
        .u32(crc);
    b.u32(12).u32(12).u16(9).u16(9).bytes(name);
    b.u16(0x5455).u16(5).u8(1).u32(1_700_000_000);
    b.bytes(content);
    let cd = b.len();
    b.u32(0x0201_4B50)
        .u8(30)
        .u8(3)
        .u16(20)
        .u16(0)
        .u16(0)
        .u16(0x6000)
        .u16(0x5821)
        .u32(crc);
    b.u32(12)
        .u32(12)
        .u16(9)
        .u16(0)
        .u16(4)
        .u16(0)
        .u16(1)
        .u32(0o100_644 << 16)
        .u32(0);
    b.bytes(name).bytes(b"note");
    let cd_size = b.len() - cd;
    b.u32(0x0605_4B50).u16(0).u16(0).u16(1).u16(1);
    b.u32(u32::try_from(cd_size).unwrap())
        .u32(u32::try_from(cd).unwrap())
        .u16(3)
        .bytes(b"end");
    b.done()
}

#[test]
fn zip_local_central_and_end_records() {
    let data = zip_sample();
    let r = eval_entry("zip", &data);
    assert_eq!(get(&r, "files").len, 1);
    assert_eq!(text(&r, "files[0].file_name"), "hello.txt");
    assert_eq!(enum_name(&r, "files[0].compression"), "Stored");
    assert_eq!(
        get(&r, "files[0].version_needed").value.as_deref(),
        Some("2.0")
    );
    assert_eq!(
        get(&r, "files[0].modification_time").value.as_deref(),
        Some("12:00:00")
    );
    assert_eq!(
        get(&r, "files[0].modification_date").value.as_deref(),
        Some("2024-01-01")
    );
    assert_eq!(
        enum_name(&r, "files[0].extra_fields[0].id"),
        "ExtendedTimestamp"
    );
    assert_eq!(get(&r, "files[0].data").offset, 30 + 9 + 9);
    assert_eq!(get(&r, "files[0].data").size, 12);
    assert_eq!(get(&r, "files[0]").value.as_deref(), Some("hello.txt"));
    assert_eq!(int(&r, "end_of_central_directory.total_entries"), 1);
    assert_eq!(text(&r, "end_of_central_directory.comment"), "end");
    assert_eq!(get(&r, "central_directory").len, 1);
    assert_eq!(
        enum_name(&r, "central_directory[0].version_made_by.host"),
        "Unix"
    );
    assert_eq!(text(&r, "central_directory[0].file_comment"), "note");
    let sums = checksum_paths(&r);
    assert_eq!(sums.len(), 2);
    for (_, alg, start, end, stored) in &sums {
        assert_eq!((alg.as_str(), *start, *end), ("crc32", 48, 60));
        assert_eq!(*stored, Some(i128::from(crc32(b"hello world\n"))));
    }
}

#[test]
fn zip_deflated_entry_uses_inflate_checksum() {
    let mut b = Buf::new();
    b.u32(0x0403_4B50)
        .u16(20)
        .u16(0)
        .u16(8)
        .u16(0)
        .u16(0x21)
        .u32(0x1234_5678);
    b.u32(4)
        .u32(10)
        .u16(1)
        .u16(0)
        .bytes(b"a")
        .bytes(&[1, 2, 3, 4]);
    let r = eval_entry("zip", &b.done());
    let sums = checksum_paths(&r);
    assert_eq!(sums.len(), 1);
    assert_eq!(
        (sums[0].1.as_str(), sums[0].2, sums[0].3),
        ("crc32_inflate", 31, 35)
    );
    assert!(r.find("end_of_central_directory").is_none());
}

// ----- GZIP ------------------------------------------------------------------

#[test]
fn gzip_member() {
    let mut b = Buf::new();
    b.bytes(&[0x1F, 0x8B, 8, 0x1C])
        .u32(1_700_000_000)
        .u8(2)
        .u8(3);
    b.u16(6).bytes(b"AB").u16(2).bytes(&[9, 9]);
    b.bytes(b"file.txt\0").bytes(b"hi\0");
    b.bytes(&[0xCB, 0x48, 0xCD, 0xC9, 0xC9, 0x07, 0x00]);
    b.u32(0x3610_A686).u32(5);
    let r = eval_entry("gzip", &b.done());
    assert_eq!(enum_name(&r, "member.method"), "Deflate");
    assert_eq!(
        get(&r, "member.flags.name").raw,
        Some(hexa_template::Value::Bool(true))
    );
    assert_eq!(
        get(&r, "member.modification_time").value.as_deref(),
        Some("2023-11-14 22:13:20 UTC")
    );
    assert_eq!(enum_name(&r, "member.extra_flags"), "MaximumCompression");
    assert_eq!(enum_name(&r, "member.os"), "Unix");
    assert_eq!(text(&r, "member.extra[0].id"), "AB");
    assert_eq!(text(&r, "member.file_name"), "file.txt");
    assert_eq!(text(&r, "member.comment"), "hi");
    assert_eq!(get(&r, "member.compressed_data").size, 7);
    assert_eq!(int(&r, "member.crc32"), 0x3610_A686);
    assert_eq!(int(&r, "member.uncompressed_size"), 5);
    let sums = checksum_paths(&r);
    assert_eq!(sums.len(), 1);
    assert_eq!(
        (sums[0].1.as_str(), sums[0].2, sums[0].3),
        ("crc32_inflate", 30, 37)
    );
}

// ----- TAR -------------------------------------------------------------------

fn tar_header(name: &str, size: usize, kind: u8) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0001750\0");
    h[116..124].copy_from_slice(b"0001750\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"14524770400\0");
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[265..269].copy_from_slice(b"user");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

fn tar_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(&tar_header("dir/", 0, b'5'));
    b.bytes(&tar_header("dir/a.txt", 5, b'0'))
        .bytes(b"hello")
        .pad_to(512 * 3);
    b.zeros(1024);
    b.done()
}

#[test]
fn tar_entries_with_octal_sizes() {
    let r = eval_entry("tar", &tar_sample());
    assert_eq!(get(&r, "entries").len, 2);
    assert_eq!(text(&r, "entries[0].header.name"), "dir/");
    assert_eq!(enum_name(&r, "entries[0].header.type_flag"), "Directory");
    assert_eq!(text(&r, "entries[1].header.name"), "dir/a.txt");
    assert_eq!(enum_name(&r, "entries[1].header.type_flag"), "Regular");
    assert_eq!(int(&r, "entries[1].header.file_size"), 5);
    assert_eq!(
        get(&r, "entries[1].header.size").value.as_deref(),
        Some("5")
    );
    assert_eq!(
        get(&r, "entries[1].header.mode").value.as_deref(),
        Some("0644")
    );
    assert_eq!(
        get(&r, "entries[1].header.modification_time")
            .value
            .as_deref(),
        Some("2023-11-14 22:13:20 UTC")
    );
    assert_eq!(
        get(&r, "entries[1].header.checksum_valid").raw,
        Some(hexa_template::Value::Bool(true))
    );
    assert_eq!(text(&r, "entries[1].header.magic"), "ustar");
    assert_eq!(get(&r, "entries[1].data").size, 5);
    assert_eq!(get(&r, "entries[1]").size, 1024);
    assert_eq!(get(&r, "entries[1]").value.as_deref(), Some("dir/a.txt"));
    assert_eq!(get(&r, "end_of_archive").size, 1024);
}

// ----- executables ---------------------------------------------------------------

fn elf_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(b"\x7fELF").u8(2).u8(1).u8(1).u8(0).zeros(8);
    b.u16(2).u16(62).u32(1).u64(0x40_1000).u64(64).u64(144);
    b.u32(0).u16(64).u16(56).u16(1).u16(64).u16(3).u16(1);
    // PT_LOAD, R+X
    b.u32(1)
        .u32(5)
        .u64(0)
        .u64(0x40_0000)
        .u64(0x40_0000)
        .u64(141)
        .u64(141)
        .u64(0x1000);
    b.bytes(b"\0.shstrtab\0.text\0"); // 120..137
    b.bytes(&[0x90, 0x90, 0x90, 0xC3]); // 137..141
    b.pad_to(144);
    b.zeros(64); // null section
    b.u32(1)
        .u32(3)
        .u64(0)
        .u64(0)
        .u64(120)
        .u64(17)
        .u32(0)
        .u32(0)
        .u64(1)
        .u64(0);
    b.u32(11)
        .u32(1)
        .u64(6)
        .u64(0x40_1000)
        .u64(137)
        .u64(4)
        .u32(0)
        .u32(0)
        .u64(16)
        .u64(0);
    b.done()
}

#[test]
fn elf64_headers_and_sections() {
    let r = eval_entry("elf", &elf_sample());
    assert_eq!(enum_name(&r, "elf.header.ident.class"), "ELF64");
    assert_eq!(enum_name(&r, "elf.header.machine"), "X86_64");
    assert_eq!(enum_name(&r, "elf.header.type"), "Executable");
    assert_eq!(int(&r, "elf.header.entry"), 0x40_1000);
    assert_eq!(enum_name(&r, "elf.program_headers[0].type"), "Load");
    assert_eq!(get(&r, "elf.section_headers").len, 3);
    assert_eq!(text(&r, "elf.section_headers[1].name"), ".shstrtab");
    assert_eq!(text(&r, "elf.section_headers[2].name"), ".text");
    assert_eq!(enum_name(&r, "elf.section_headers[2].type"), "ProgBits");
    assert_eq!(get(&r, "elf.section_headers[2].data").size, 4);
    assert_eq!(
        get(&r, "elf.section_headers[2]").value.as_deref(),
        Some(".text")
    );
}

#[test]
fn elf32_big_endian_header() {
    let mut b = Buf::new();
    b.bytes(b"\x7fELF").u8(1).u8(2).u8(1).u8(0).zeros(8);
    b.be16(2).be16(8).be32(1).be32(0x0040_0100).be32(0).be32(0);
    b.be32(0).be16(52).be16(32).be16(0).be16(40).be16(0).be16(0);
    let r = eval_entry("elf", &b.done());
    assert_eq!(enum_name(&r, "elf.header.ident.data"), "BigEndian");
    assert_eq!(enum_name(&r, "elf.header.machine"), "MIPS");
    assert_eq!(int(&r, "elf.header.entry"), 0x0040_0100);
    assert_eq!(get(&r, "elf.header").size, 52);
}

fn pe_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(b"MZ").pad_to(60).u32(0x40);
    b.bytes(b"PE\0\0");
    b.u16(0x8664)
        .u16(1)
        .u32(1_700_000_000)
        .u32(0)
        .u32(0)
        .u16(240)
        .u16(0x22);
    b.u16(0x20B)
        .u8(14)
        .u8(0)
        .u32(0x200)
        .u32(0)
        .u32(0)
        .u32(0x1000)
        .u32(0x1000);
    b.u64(0x1_4000_0000).u32(0x1000).u32(0x200);
    b.u16(6).u16(0).u16(0).u16(0).u16(6).u16(0).u32(0);
    b.u32(0x2000).u32(0x200).u32(0).u16(3).u16(0x8160);
    b.u64(0x10_0000)
        .u64(0x1000)
        .u64(0x10_0000)
        .u64(0x1000)
        .u32(0)
        .u32(16);
    b.zeros(16 * 8);
    assert_eq!(b.len(), 0x148);
    b.bytes(b".text\0\0\0")
        .u32(0x10)
        .u32(0x1000)
        .u32(0x200)
        .u32(0x200);
    b.u32(0).u32(0).u16(0).u16(0).u32(0x6000_0020);
    b.pad_to(0x200).bytes(&[0xC3]).pad_to(0x400);
    b.done()
}

#[test]
fn pe32_plus_headers_and_sections() {
    let r = eval_entry("pe", &pe_sample());
    assert_eq!(text(&r, "dos_header.magic"), "MZ");
    assert_eq!(int(&r, "dos_header.new_header_offset"), 0x40);
    assert_eq!(enum_name(&r, "nt_headers.file_header.machine"), "AMD64");
    assert_eq!(
        enum_name(&r, "nt_headers.optional_header.magic"),
        "PE32Plus"
    );
    assert_eq!(
        int(&r, "nt_headers.optional_header.image_base"),
        0x1_4000_0000
    );
    assert_eq!(
        enum_name(&r, "nt_headers.optional_header.subsystem"),
        "WindowsCUI"
    );
    assert_eq!(
        get(&r, "nt_headers.optional_header.data_directories").len,
        16
    );
    assert_eq!(text(&r, "sections[0].name"), ".text");
    assert_eq!(get(&r, "sections[0].data").offset, 0x200);
    assert_eq!(
        get(&r, "nt_headers.file_header.time_date_stamp")
            .value
            .as_deref(),
        Some("2023-11-14 22:13:20 UTC")
    );
}

fn macho_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.u32(0xFEED_FACF)
        .u32(0x0100_0007)
        .u32(3)
        .u32(2)
        .u32(2)
        .u32(176)
        .u32(0x85)
        .u32(0);
    b.u32(0x19).u32(152).bytes(b"__TEXT\0\0\0\0\0\0\0\0\0\0");
    b.u64(0x1_0000_0000)
        .u64(0x1000)
        .u64(0)
        .u64(0x1000)
        .u32(5)
        .u32(5)
        .u32(1)
        .u32(0);
    b.bytes(b"__text\0\0\0\0\0\0\0\0\0\0")
        .bytes(b"__TEXT\0\0\0\0\0\0\0\0\0\0");
    b.u64(0x1_0000_0f00)
        .u64(4)
        .u32(0xF00)
        .u32(2)
        .u32(0)
        .u32(0)
        .u32(0x8000_0400);
    b.u32(0).u32(0).u32(0);
    b.u32(0x1B).u32(24).bytes(&[0xAB; 16]);
    b.done()
}

#[test]
fn macho64_load_commands() {
    let r = eval_entry("macho", &macho_sample());
    assert_eq!(int(&r, "macho.header.magic"), 0xFEED_FACF);
    assert_eq!(get(&r, "macho.load_commands").len, 2);
    assert_eq!(enum_name(&r, "macho.load_commands[0].command"), "Segment64");
    assert_eq!(text(&r, "macho.load_commands[0].segment_name"), "__TEXT");
    assert_eq!(
        text(&r, "macho.load_commands[0].sections[0].section_name"),
        "__text"
    );
    assert_eq!(enum_name(&r, "macho.load_commands[1].command"), "UUID");
    assert_eq!(get(&r, "macho.load_commands[1]").size, 24);
}

fn java_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.be32(0xCAFE_BABE).be16(0).be16(52).be16(7);
    b.u8(1).be16(5).bytes(b"Hello");
    b.u8(7).be16(1);
    b.u8(5).be64(1 << 40);
    b.u8(1).be16(16).bytes(b"java/lang/Object");
    b.u8(7).be16(5);
    b.be16(0x0021).be16(2).be16(6).be16(0);
    b.be16(0); // fields
    b.be16(1); // methods
    b.be16(0x0009).be16(1).be16(1).be16(1);
    b.be16(1).be32(8).be16(1).be16(1).be32(0); // attribute named #1 with 8 bytes
    b.be16(0); // class attributes
    b.done()
}

#[test]
fn java_class_constant_pool() {
    let r = eval_entry("java_class", &java_sample());
    assert_eq!(int(&r, "class_file.major_version"), 52);
    assert_eq!(get(&r, "class_file.constant_pool").len, 5);
    assert_eq!(enum_name(&r, "class_file.constant_pool[2].tag"), "Long");
    assert_eq!(int(&r, "class_file.constant_pool[2].value"), 1 << 40);
    assert_eq!(int(&r, "class_file.constant_pool[3].index"), 5);
    assert_eq!(text(&r, "class_file.class_name"), "Hello");
    assert_eq!(text(&r, "class_file.super_class_name"), "java/lang/Object");
    assert_eq!(enum_name(&r, "class_file.access_flags"), "Public | Super");
    assert_eq!(text(&r, "class_file.methods[0].name"), "Hello");
    assert_eq!(get(&r, "class_file.methods[0].attributes[0].info").size, 8);
    assert_eq!(
        get(&r, "class_file.constant_pool[0]").value.as_deref(),
        Some("#1 \"Hello\"")
    );
}

fn dex_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(b"dex\n035\0")
        .u32(0)
        .zeros(20)
        .u32(0xA4)
        .u32(0x70)
        .u32(0x1234_5678);
    b.u32(0).u32(0).u32(0x88); // link, map
    b.u32(2).u32(0x70).u32(1).u32(0x78);
    b.zeros(4 * 8); // proto, field, method, class defs
    b.u32(0x34).u32(0x70);
    assert_eq!(b.len(), 0x70);
    b.u32(0x7C).u32(0x81).u32(0);
    b.u8(3).bytes(b"foo\0").u8(2).bytes(b"hi\0");
    b.pad_to(0x88);
    b.u32(2)
        .u16(0)
        .u16(0)
        .u32(1)
        .u32(0)
        .u16(1)
        .u16(0)
        .u32(2)
        .u32(0x70);
    b.done()
}

#[test]
fn dex_header_strings_and_map() {
    let r = eval_entry("dex", &dex_sample());
    assert_eq!(text(&r, "header.version"), "035");
    assert_eq!(enum_name(&r, "header.endian_tag"), "Little");
    assert_eq!(get(&r, "string_ids").len, 2);
    assert_eq!(text(&r, "string_ids[0].string_data.data"), "foo");
    assert_eq!(int(&r, "string_ids[1].string_data.utf16_size.value"), 2);
    assert_eq!(get(&r, "string_ids[1]").value.as_deref(), Some("hi"));
    assert_eq!(enum_name(&r, "map.list[1].type"), "StringIdItem");
    let sums = checksum_paths(&r);
    assert!(
        sums.iter()
            .any(|c| c.1 == "adler32" && c.2 == 12 && c.3 == 0xA4),
        "{sums:?}"
    );
    assert!(sums.iter().any(|c| c.1 == "sha1" && c.2 == 32));
}

fn wasm_sample() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(b"\0asm").u32(1);
    b.u8(1).u8(4).u8(1).bytes(&[0x60, 0, 0]);
    b.u8(7).u8(8).u8(1).u8(4).bytes(b"main").u8(0).u8(0);
    b.u8(0).u8(7).u8(4).bytes(b"name").bytes(&[1, 2]);
    b.u8(10).bytes(&[0x82, 0x01]).u8(1).zeros(129);
    b.done()
}

#[test]
fn wasm_sections() {
    let r = eval_entry("wasm", &wasm_sample());
    assert_eq!(int(&r, "module.version"), 1);
    assert_eq!(get(&r, "module.sections").len, 4);
    assert_eq!(enum_name(&r, "module.sections[0].id"), "Type");
    assert_eq!(text(&r, "module.sections[1].exports[0].name.text"), "main");
    assert_eq!(text(&r, "module.sections[2].name.text"), "name");
    assert_eq!(int(&r, "module.sections[3].size.value"), 130);
    assert_eq!(get(&r, "module.sections[3]").size, 133);
    assert_eq!(
        get(&r, "module.sections[2]").value.as_deref(),
        Some("custom \"name\"")
    );
}

// ----- network -----------------------------------------------------------------------

fn ip_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = header
        .chunks(2)
        .map(|c| u32::from(u16::from_be_bytes([c[0], c[1]])))
        .sum();
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn ipv4_udp_packet() -> Vec<u8> {
    let mut ip = Buf::new();
    ip.u8(0x45)
        .u8(0)
        .be16(32)
        .be16(1)
        .be16(0x4000)
        .u8(64)
        .u8(17)
        .be16(0);
    ip.bytes(&[192, 168, 0, 1]).bytes(&[192, 168, 0, 2]);
    let mut ip = ip.done();
    let sum = ip_checksum(&ip);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    let mut b = Buf::new();
    b.bytes(&ip)
        .be16(1234)
        .be16(53)
        .be16(12)
        .be16(0)
        .bytes(b"abcd");
    b.done()
}

fn ethernet_frame() -> Vec<u8> {
    let mut b = Buf::new();
    b.bytes(&[0xFF; 6])
        .bytes(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55])
        .be16(0x0800);
    b.bytes(&ipv4_udp_packet());
    b.done()
}

fn pcap_sample() -> Vec<u8> {
    let frame = ethernet_frame();
    let mut b = Buf::new();
    b.u32(0xA1B2_C3D4)
        .u16(2)
        .u16(4)
        .u32(0)
        .u32(0)
        .u32(65535)
        .u32(1);
    let n = frame.len() as u32;
    b.u32(1_700_000_000).u32(0).u32(n).u32(n).bytes(&frame);
    b.u32(1_700_000_001).u32(5).u32(3).u32(60).bytes(&[1, 2, 3]);
    b.done()
}

#[test]
fn pcap_records_with_protocol_layers() {
    let r = eval_entry("pcap", &pcap_sample());
    assert_eq!(enum_name(&r, "pcap.header.network"), "Ethernet");
    assert_eq!(get(&r, "pcap.records").len, 2);
    let eth = "pcap.records[0].ethernet";
    assert_eq!(
        get(&r, &format!("{eth}.source")).value.as_deref(),
        Some("00:11:22:33:44:55")
    );
    assert_eq!(enum_name(&r, &format!("{eth}.ether_type")), "IPv4");
    assert_eq!(int(&r, &format!("{eth}.ipv4.version_ihl.version")), 4);
    assert_eq!(int(&r, &format!("{eth}.ipv4.version_ihl.ihl")), 5);
    assert_eq!(
        int(&r, &format!("{eth}.ipv4.flags_fragment.dont_fragment")),
        1
    );
    assert_eq!(enum_name(&r, &format!("{eth}.ipv4.protocol")), "UDP");
    assert_eq!(
        get(&r, &format!("{eth}.ipv4.source")).value.as_deref(),
        Some("192.168.0.1")
    );
    assert_eq!(int(&r, &format!("{eth}.ipv4.udp.destination_port")), 53);
    assert_eq!(get(&r, &format!("{eth}.ipv4.udp.payload")).len, 4);
    assert_eq!(get(&r, "pcap.records[1]").size, 19);
    assert_eq!(
        get(&r, "pcap.records[1].timestamp_seconds")
            .value
            .as_deref(),
        Some("2023-11-14 22:13:21 UTC")
    );
    let sums = checksum_paths(&r);
    assert!(
        sums.iter()
            .any(|c| c.1 == "internet" && c.2 == 24 + 16 + 14 && c.3 == 24 + 16 + 34),
        "{sums:?}"
    );
}

#[test]
fn standalone_protocol_entries() {
    let r = eval_entry("ethernet", &ethernet_frame());
    assert_eq!(int(&r, "ethernet.ipv4.udp.source_port"), 1234);
    let r = eval_entry("ipv4", &ipv4_udp_packet());
    assert_eq!(
        get(&r, "ipv4.destination").value.as_deref(),
        Some("192.168.0.2")
    );
    assert_eq!(get(&r, "ipv4.udp").size, 12);
    let mut tcp = Buf::new();
    tcp.be16(80)
        .be16(5555)
        .be32(1)
        .be32(2)
        .be16(0x6012)
        .be16(1024)
        .be16(0)
        .be16(0);
    tcp.bytes(&[2, 4, 5, 0xB4]).bytes(b"GET");
    let r = eval_entry("tcp", &tcp.done());
    assert_eq!(int(&r, "tcp.flags.data_offset"), 6);
    assert_eq!(int(&r, "tcp.flags.syn"), 1);
    assert_eq!(int(&r, "tcp.flags.ack"), 1);
    assert_eq!(get(&r, "tcp.options").len, 4);
    assert_eq!(get(&r, "tcp.payload").len, 3);
    let mut udp = Buf::new();
    udp.be16(1).be16(2).be16(10).be16(0xBEEF).bytes(b"ok");
    let r = eval_entry("udp", &udp.done());
    assert_eq!(get(&r, "udp.checksum").value.as_deref(), Some("0xBEEF"));
}

// ----- detection (STRC-09) -----------------------------------------------------------

#[test]
fn detect_by_magic() {
    use library::detect;
    assert_eq!(detect(&png_sample(), None).first(), Some(&"png"));
    assert_eq!(detect(&elf_sample(), None), ["elf"]);
    assert_eq!(detect(&pe_sample(), Some("a.exe")).first(), Some(&"pe"));
    assert_eq!(detect(&wasm_sample(), None), ["wasm"]);
    assert_eq!(detect(&pcap_sample(), None), ["pcap"]);
    assert_eq!(detect(&dex_sample(), None), ["dex"]);
    assert_eq!(detect(&macho_sample(), None), ["macho"]);
    assert_eq!(detect(&tar_sample(), None), ["tar"]);
    assert_eq!(detect(&ext4_sample(), None).first(), Some(&"ext4"));
}

#[test]
fn detect_ranks_specific_magic_and_extension() {
    use library::detect;
    // GPT disks also carry the MBR signature; GPT must win.
    let gpt = detect(&gpt_sample(), None);
    assert_eq!(gpt[0], "gpt");
    assert!(gpt.contains(&"mbr"));
    // CAFEBABE is shared by Java classes and universal Mach-O binaries.
    let java = detect(&java_sample(), Some("Hello.class"));
    assert_eq!(java[..2], ["java_class", "macho"]);
    let fat = detect(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 2], Some("tool.dylib"));
    assert_eq!(fat[0], "macho");
}

#[test]
fn detect_by_extension_and_short_input() {
    use library::detect;
    assert_eq!(detect(b"", Some("report.DOCX")), ["zip"]);
    assert_eq!(detect(b"\0\0", Some("C:\\x\\mod.wasm")), ["wasm"]);
    assert_eq!(detect(b"\x89P", None), [] as [&str; 0]);
    assert_eq!(detect(b"nothing", Some("file.unknownext")), [] as [&str; 0]);
    assert_eq!(detect(&[0u8; 3], Some("noext")), [] as [&str; 0]);
    for len in 0..600 {
        let _ = detect(&gpt_sample()[..len], None);
    }
}
