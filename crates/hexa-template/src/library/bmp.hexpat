#pragma description BMP / DIB image
#pragma endian little

enum Compression : u32 {
    RGB = 0,
    RLE8 = 1,
    RLE4 = 2,
    Bitfields = 3,
    JPEG = 4,
    PNG = 5,
    AlphaBitfields = 6,
    CMYK = 11,
    CMYKRLE8 = 12,
    CMYKRLE4 = 13
};

enum ColorSpaceType : u32 {
    CalibratedRGB = 0,
    sRGB = 0x73524742,
    WindowsColorSpace = 0x57696E20,
    ProfileLinked = 0x4C494E4B,
    ProfileEmbedded = 0x4D424544
};

enum Intent : u32 {
    Business = 1,
    Graphics = 2,
    Images = 4,
    AbsoluteColorimetric = 8
};

struct FileHeader {
    char signature[2] [[comment("BM")]];
    u32 file_size;
    u16 reserved1;
    u16 reserved2;
    u32 pixel_data_offset;
} [[color("808080")]];

struct CieXyz {
    s32 x [[comment("2.30 fixed point")]];
    s32 y;
    s32 z;
};

struct InfoHeader {
    u32 header_size;
    if (header_size == 12) {
        // BITMAPCOREHEADER (OS/2 1.x)
        u16 width;
        u16 height;
        u16 planes;
        u16 bits_per_pixel;
    } else {
        s32 width;
        s32 height [[comment("negative = top-down")]];
        u16 planes;
        u16 bits_per_pixel;
        Compression compression;
        u32 image_size;
        s32 x_pixels_per_meter;
        s32 y_pixels_per_meter;
        u32 colors_used;
        u32 colors_important;
        if (header_size >= 52) {
            u32 red_mask [[format("hex")]];
            u32 green_mask [[format("hex")]];
            u32 blue_mask [[format("hex")]];
        }
        if (header_size >= 56) u32 alpha_mask [[format("hex")]];
        if (header_size >= 108) {
            ColorSpaceType color_space;
            CieXyz red_endpoint;
            CieXyz green_endpoint;
            CieXyz blue_endpoint;
            u32 gamma_red;
            u32 gamma_green;
            u32 gamma_blue;
        }
        if (header_size >= 124) {
            Intent intent;
            u32 profile_data_offset;
            u32 profile_size;
            u32 reserved;
        }
    }
    if ($ < addressof(this) + header_size) u8 unknown[addressof(this) + header_size - $];
    auto row_stride = (width * bits_per_pixel + 31) / 32 * 4;
} [[color("2E86C1")]];

struct RgbQuad {
    u8 blue;
    u8 green;
    u8 red;
    u8 reserved;
} [[format("bmp_color_text")]];

fn bmp_color_text(auto c) {
    return std::format("#{:02X}{:02X}{:02X}", c.red, c.green, c.blue);
};

fn bmp_palette_size() {
    if (info.header_size == 12 || info.bits_per_pixel > 8) return 0;
    if (info.colors_used != 0) return info.colors_used;
    return 1 << info.bits_per_pixel;
};

FileHeader file;
InfoHeader info;
if (info.header_size == 40
        && (info.compression == Compression::Bitfields || info.compression == Compression::AlphaBitfields))
    u32 color_masks[info.compression == Compression::AlphaBitfields ? 4 : 3] [[format("hex")]];
RgbQuad palette[bmp_palette_size()];
u8 pixel_data[std::mem::size() - file.pixel_data_offset] @ file.pixel_data_offset;
