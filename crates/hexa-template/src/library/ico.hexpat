#pragma description Windows icon / cursor (ICO / CUR)
#pragma endian little

enum ImageType : u16 {
    Icon = 1,
    Cursor = 2
};

struct BitmapInfoHeader {
    u32 header_size;
    s32 width;
    s32 height [[comment("twice the icon height: XOR mask + AND mask")]];
    u16 planes;
    u16 bits_per_pixel;
    u32 compression;
    u32 image_size;
    s32 x_pixels_per_meter;
    s32 y_pixels_per_meter;
    u32 colors_used;
    u32 colors_important;
};

struct DibImage {
    BitmapInfoHeader header;
    u8 pixel_data[];
};

struct DirectoryEntry {
    u8 width [[comment("0 = 256 pixels")]];
    u8 height [[comment("0 = 256 pixels")]];
    u8 color_count;
    u8 reserved;
    if (parent.type == ImageType::Cursor) {
        u16 hotspot_x;
        u16 hotspot_y;
    } else {
        u16 color_planes;
        u16 bits_per_pixel;
    }
    u32 image_size;
    u32 image_offset;
    auto pixel_width = width == 0 ? 256 : width;
    auto pixel_height = height == 0 ? 256 : height;
    if (image_size >= 8 && std::mem::read_unsigned(image_offset, 4, "big") == 0x89504E47)
        u8 png_image[image_size] @ image_offset [[comment("embedded PNG stream")]];
    else
        DibImage dib_image @ image_offset [[size(image_size)]];
};

struct IconDirectory {
    u16 reserved;
    ImageType type;
    u16 image_count;
    DirectoryEntry entries[image_count];
};

IconDirectory icon @ 0;
