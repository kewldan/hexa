#pragma description JPEG / JFIF / Exif image (ITU T.81)
#pragma endian big

enum Marker : u8 {
    TEM = 0x01,
    SOF0 = 0xC0, SOF1 = 0xC1, SOF2 = 0xC2, SOF3 = 0xC3,
    DHT = 0xC4,
    SOF5 = 0xC5, SOF6 = 0xC6, SOF7 = 0xC7,
    JPG = 0xC8,
    SOF9 = 0xC9, SOF10 = 0xCA, SOF11 = 0xCB,
    DAC = 0xCC,
    SOF13 = 0xCD, SOF14 = 0xCE, SOF15 = 0xCF,
    RST0 = 0xD0, RST1 = 0xD1, RST2 = 0xD2, RST3 = 0xD3,
    RST4 = 0xD4, RST5 = 0xD5, RST6 = 0xD6, RST7 = 0xD7,
    SOI = 0xD8, EOI = 0xD9, SOS = 0xDA, DQT = 0xDB, DNL = 0xDC, DRI = 0xDD,
    DHP = 0xDE, EXP = 0xDF,
    APP0 = 0xE0, APP1 = 0xE1, APP2 = 0xE2, APP3 = 0xE3, APP4 = 0xE4, APP5 = 0xE5,
    APP6 = 0xE6, APP7 = 0xE7, APP8 = 0xE8, APP9 = 0xE9, APP10 = 0xEA, APP11 = 0xEB,
    APP12 = 0xEC, APP13 = 0xED, APP14 = 0xEE, APP15 = 0xEF,
    JPG0 = 0xF0 ... 0xFD,
    COM = 0xFE
};

enum DensityUnit : u8 { NoUnits = 0, DotsPerInch = 1, DotsPerCm = 2 };

enum TiffType : u16 {
    Byte = 1, Ascii = 2, Short = 3, Long = 4, Rational = 5, SByte = 6, Undefined = 7,
    SShort = 8, SLong = 9, SRational = 10, Float = 11, Double = 12
};

enum TiffTag : u16 {
    ImageWidth = 0x0100, ImageLength = 0x0101, BitsPerSample = 0x0102, Compression = 0x0103,
    PhotometricInterpretation = 0x0106, ImageDescription = 0x010E, Make = 0x010F,
    Model = 0x0110, StripOffsets = 0x0111, Orientation = 0x0112, SamplesPerPixel = 0x0115,
    XResolution = 0x011A, YResolution = 0x011B, ResolutionUnit = 0x0128, Software = 0x0131,
    DateTime = 0x0132, Artist = 0x013B, JPEGInterchangeFormat = 0x0201,
    JPEGInterchangeFormatLength = 0x0202, YCbCrPositioning = 0x0213, Copyright = 0x8298,
    ExposureTime = 0x829A, FNumber = 0x829D, ExifIFD = 0x8769, ISOSpeed = 0x8827,
    GPSInfoIFD = 0x8825, ExifVersion = 0x9000, DateTimeOriginal = 0x9003,
    DateTimeDigitized = 0x9004, FocalLength = 0x920A, MakerNote = 0x927C, UserComment = 0x9286,
    ColorSpace = 0xA001, PixelXDimension = 0xA002, PixelYDimension = 0xA003
};

bitfield SamplingFactors {
    horizontal : 4;
    vertical : 4;
};

bitfield TableSelectors {
    dc_table : 4;
    ac_table : 4;
};

bitfield TableInfo {
    table_class : 4;
    table_id : 4;
};

bitfield QuantInfo {
    precision : 4;
    table_id : 4;
};

bitfield ApproximationBits {
    high : 4;
    low : 4;
};

struct FrameComponent {
    u8 component_id;
    SamplingFactors sampling;
    u8 quant_table;
};

struct StartOfFrame {
    u8 precision;
    u16 height;
    u16 width;
    u8 num_components;
    FrameComponent components[num_components];
};

struct ScanComponent {
    u8 component_id;
    TableSelectors tables;
};

struct StartOfScan {
    u8 num_components;
    ScanComponent components[num_components];
    u8 spectral_start;
    u8 spectral_end;
    ApproximationBits approximation;
};

struct QuantTable {
    QuantInfo info;
    if (info.precision == 0) u8 values[64];
    else u16 values[64];
};

fn jpeg_sum16(auto counts) {
    u32 total = 0;
    for (u8 i = 0, i < 16, i += 1) total += counts[i];
    return total;
};

struct HuffmanTable {
    TableInfo info [[comment("class 0 = DC, 1 = AC")]];
    u8 code_counts[16];
    u8 symbols[jpeg_sum16(code_counts)];
};

struct JfifThumbnailPixel {
    u8 red;
    u8 green;
    u8 blue;
};

struct App0 {
    char identifier[5];
    if (identifier == "JFIF") {
        u8 version_major;
        u8 version_minor;
        DensityUnit density_unit;
        u16 x_density;
        u16 y_density;
        u8 thumbnail_width;
        u8 thumbnail_height;
        JfifThumbnailPixel thumbnail[thumbnail_width * thumbnail_height];
    } else {
        u8 data[];
    }
};

struct IfdEntry {
    TiffTag tag;
    TiffType type;
    u32 count;
    u32 value_or_offset;
    // Offsets are relative to the TIFF header (`tiff_start`).
    if (type == TiffType::Ascii && count > 4
            && tiff_start + value_or_offset + count <= std::mem::base_address() + std::mem::size())
        char text[count] @ tiff_start + value_or_offset;
};

struct Ifd {
    u16 entry_count;
    IfdEntry entries[entry_count];
    u32 next_ifd_offset;
};

struct TiffHeader {
    char byte_order[2] [[comment("II = little endian, MM = big endian")]];
    u16 magic [[comment("42")]];
    u32 ifd0_offset;
    Ifd ifd0 @ tiff_start + ifd0_offset;
};

struct App1 {
    char identifier[6];
    if (identifier == "Exif") {
        tiff_start = $;
        if (std::mem::read_unsigned($, 2) == 0x4949) le TiffHeader tiff;
        else be TiffHeader tiff;
        u8 exif_data[];
    } else {
        u8 data[];
    }
};

fn jpeg_find_eoi(u64 from) {
    s64 occurrence = 0;
    while (true) {
        s64 at = std::mem::find_sequence(occurrence, 0xFF, 0xD9);
        if (at < 0 || at >= from) return at;
        occurrence += 1;
    }
};

struct Segment {
    u8 prefix [[format("hex"), comment("always 0xFF")]];
    Marker marker [[color("E67E22")]];
    if (marker == Marker::SOI || marker == Marker::EOI || marker == Marker::TEM
            || (marker >= 0xD0 && marker <= 0xD7)) {
        // Stand-alone marker without a length field.
    } else {
        u16 length;
        match (marker) {
            (Marker::SOF0 | Marker::SOF1 | Marker::SOF2 | Marker::SOF3 | Marker::SOF5
                    | Marker::SOF6 | Marker::SOF7 | Marker::SOF9 | Marker::SOF10
                    | Marker::SOF11 | Marker::SOF13 | Marker::SOF14 | Marker::SOF15):
                StartOfFrame frame [[size(length - 2)]];
            (Marker::DHT): HuffmanTable huffman_tables[] [[size(length - 2)]];
            (Marker::DQT): QuantTable quant_tables[] [[size(length - 2)]];
            (Marker::DRI): u16 restart_interval;
            (Marker::SOS): StartOfScan scan [[size(length - 2)]];
            (Marker::APP0): App0 app0 [[size(length - 2)]];
            (Marker::APP1): App1 app1 [[size(length - 2)]];
            (Marker::COM): char comment[length - 2];
            (_): u8 data[length - 2];
        }
        if (marker == Marker::SOS) {
            s64 eoi = jpeg_find_eoi($);
            if (eoi < 0) eoi = std::mem::base_address() + std::mem::size();
            u8 image_data[eoi - $] [[comment("entropy-coded scan data")]];
        }
    }
    if (marker == Marker::EOI) break;
};

// Absolute offset of the Exif TIFF header; IFD offsets are relative to it.
u64 tiff_start = 0;
Segment segments[while(!std::mem::eof())];
