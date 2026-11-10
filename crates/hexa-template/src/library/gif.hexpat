#pragma description GIF image (87a / 89a)
#pragma endian little

enum BlockType : u8 {
    Extension = 0x21,
    ImageDescriptor = 0x2C,
    Trailer = 0x3B
};

enum ExtensionLabel : u8 {
    PlainText = 0x01,
    GraphicControl = 0xF9,
    Comment = 0xFE,
    Application = 0xFF
};

enum DisposalMethod : u8 {
    Unspecified = 0,
    DoNotDispose = 1,
    RestoreBackground = 2,
    RestorePrevious = 3
};

bitfield ScreenFlags {
    global_table_size : 3 [[comment("table has 2^(n+1) entries")]];
    bool sorted : 1;
    color_resolution : 3;
    bool global_table : 1;
};

bitfield ImageFlags {
    local_table_size : 3;
    reserved : 2;
    bool sorted : 1;
    bool interlaced : 1;
    bool local_table : 1;
};

bitfield GraphicControlFlags {
    bool transparent_color : 1;
    bool user_input : 1;
    DisposalMethod disposal : 3;
    reserved : 3;
};

struct Header {
    char signature[3];
    char version[3];
} [[color("808080")]];

struct LogicalScreenDescriptor {
    u16 width;
    u16 height;
    ScreenFlags flags;
    u8 background_color_index;
    u8 pixel_aspect_ratio;
};

struct Color {
    u8 red;
    u8 green;
    u8 blue;
} [[format("gif_color_text")]];

fn gif_color_text(auto c) {
    return std::format("#{:02X}{:02X}{:02X}", c.red, c.green, c.blue);
};

struct SubBlock {
    u8 block_size;
    u8 data[block_size];
};

struct DataSubBlocks {
    SubBlock blocks[until(_.block_size == 0)];
};

struct GraphicControlExtension {
    u8 block_size;
    GraphicControlFlags flags;
    u16 delay_time [[comment("hundredths of a second")]];
    u8 transparent_color_index;
    u8 terminator;
};

struct ApplicationExtension {
    u8 block_size;
    char identifier[8];
    char authentication_code[3];
    DataSubBlocks data;
};

struct PlainTextExtension {
    u8 block_size;
    u16 grid_left;
    u16 grid_top;
    u16 grid_width;
    u16 grid_height;
    u8 cell_width;
    u8 cell_height;
    u8 foreground_color_index;
    u8 background_color_index;
    DataSubBlocks text;
};

struct ImageDescriptor {
    u16 left;
    u16 top;
    u16 width;
    u16 height;
    ImageFlags flags;
    if (flags.local_table) Color local_color_table[1 << (flags.local_table_size + 1)];
    u8 lzw_minimum_code_size;
    DataSubBlocks image_data;
};

struct Block {
    BlockType type [[color("E67E22")]];
    match (type) {
        (BlockType::Extension): {
            ExtensionLabel label;
            match (label) {
                (ExtensionLabel::GraphicControl): GraphicControlExtension graphic_control;
                (ExtensionLabel::Application): ApplicationExtension application;
                (ExtensionLabel::PlainText): PlainTextExtension plain_text;
                (ExtensionLabel::Comment): DataSubBlocks comment;
                (_): DataSubBlocks data;
            }
        }
        (BlockType::ImageDescriptor): ImageDescriptor image;
        (BlockType::Trailer): break;
        (_): {
            std::assert_warn(false, "unknown GIF block type");
            break;
        }
    }
};

Header header;
LogicalScreenDescriptor screen;
if (screen.flags.global_table)
    Color global_color_table[1 << (screen.flags.global_table_size + 1)];
Block blocks[while(!std::mem::eof())];
