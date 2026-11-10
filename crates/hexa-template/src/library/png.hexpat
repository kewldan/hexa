#pragma description PNG image (ISO/IEC 15948)
#pragma endian big

enum ColorType : u8 {
    Grayscale = 0,
    RGB = 2,
    Indexed = 3,
    GrayscaleAlpha = 4,
    RGBA = 6
};

enum CompressionMethod : u8 { Deflate = 0 };
enum FilterMethod : u8 { Adaptive = 0 };
enum InterlaceMethod : u8 { None = 0, Adam7 = 1 };
enum PhysUnit : u8 { Unknown = 0, Meter = 1 };
enum RenderingIntent : u8 { Perceptual = 0, RelativeColorimetric = 1, Saturation = 2, AbsoluteColorimetric = 3 };
enum DisposeOp : u8 { None = 0, Background = 1, Previous = 2 };
enum BlendOp : u8 { Source = 0, Over = 1 };

struct ImageHeader {
    u32 width;
    u32 height;
    u8 bit_depth;
    ColorType color_type;
    CompressionMethod compression;
    FilterMethod filter;
    InterlaceMethod interlace;
} [[color("2E86C1")]];

struct PaletteEntry {
    u8 red;
    u8 green;
    u8 blue;
} [[format("png_rgb_text")]];

struct PhysicalDimensions {
    u32 pixels_per_unit_x;
    u32 pixels_per_unit_y;
    PhysUnit unit;
};

struct Chromaticities {
    u32 white_point_x;
    u32 white_point_y;
    u32 red_x;
    u32 red_y;
    u32 green_x;
    u32 green_y;
    u32 blue_x;
    u32 blue_y;
};

struct LastModified {
    u16 year;
    u8 month;
    u8 day;
    u8 hour;
    u8 minute;
    u8 second;
} [[format("png_time_text")]];

struct TextualData {
    string keyword;
    char text[];
};

struct CompressedText {
    string keyword;
    CompressionMethod compression;
    u8 compressed_text[];
};

struct InternationalText {
    string keyword;
    bool compressed;
    CompressionMethod compression;
    string language_tag;
    string translated_keyword;
    if (compressed) u8 compressed_text[];
    else char text[];
};

struct AnimationControl {
    u32 num_frames;
    u32 num_plays [[comment("0 = loop forever")]];
};

struct FrameControl {
    u32 sequence_number;
    u32 width;
    u32 height;
    u32 x_offset;
    u32 y_offset;
    u16 delay_num;
    u16 delay_den;
    DisposeOp dispose_op;
    BlendOp blend_op;
};

struct Chunk {
    u32 length;
    char type[4] [[color("E67E22")]];
    match (type) {
        ("IHDR"): ImageHeader header [[size(length)]];
        ("PLTE"): PaletteEntry palette[length / 3];
        ("pHYs"): PhysicalDimensions physical [[size(length)]];
        ("cHRM"): Chromaticities chromaticities [[size(length)]];
        ("gAMA"): u32 gamma [[comment("gamma * 100000")]];
        ("sRGB"): RenderingIntent rendering_intent;
        ("tIME"): LastModified last_modified [[size(length)]];
        ("tEXt"): TextualData text [[size(length)]];
        ("zTXt"): CompressedText compressed_text [[size(length)]];
        ("iTXt"): InternationalText international_text [[size(length)]];
        ("acTL"): AnimationControl animation_control [[size(length)]];
        ("fcTL"): FrameControl frame_control [[size(length)]];
        ("fdAT"): {
            u32 sequence_number;
            u8 frame_data[length - 4];
        }
        ("IEND"): {}
        (_): u8 data[length];
    }
    u32 crc [[format("hex"), color("27AE60"),
        checksum("crc32", addressof(type), addressof(crc)),
        comment("CRC-32 of the chunk type and data")]];
    if (type == "IEND") break;
} [[format("png_chunk_text")]];

fn png_rgb_text(auto c) {
    return std::format("#{:02X}{:02X}{:02X}", c.red, c.green, c.blue);
};

fn png_time_text(auto t) {
    return std::format("{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second);
};

fn png_chunk_text(auto c) {
    return std::format("{} ({} bytes)", c.type, c.length);
};

u8 signature[8] [[color("808080"), comment("\\x89 P N G \\r \\n \\x1A \\n")]];
assert(std::mem::read_unsigned(0, 4, "big") == 0x89504E47, "not a PNG file: bad signature");
Chunk chunks[while(!std::mem::eof())];
