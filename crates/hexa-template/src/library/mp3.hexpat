#pragma description MP3 audio: ID3v2 tag, MPEG audio frames, ID3v1 tag
#pragma endian big

enum MpegVersion : u8 {
    MPEG25 = 0,
    Reserved = 1,
    MPEG2 = 2,
    MPEG1 = 3
};

enum MpegLayer : u8 {
    Reserved = 0,
    LayerIII = 1,
    LayerII = 2,
    LayerI = 3
};

enum ChannelMode : u8 {
    Stereo = 0,
    JointStereo = 1,
    DualChannel = 2,
    Mono = 3
};

enum Emphasis : u8 {
    None = 0,
    Emphasis50_15 = 1,
    Reserved = 2,
    CCITT_J17 = 3
};

enum TextEncoding : u8 {
    ISO8859_1 = 0,
    UTF16 = 1,
    UTF16BE = 2,
    UTF8 = 3
};

bitfield Id3Flags {
    bool unsynchronisation : 1;
    bool extended_header : 1;
    bool experimental : 1;
    bool footer : 1;
    reserved : 4;
};

// ID3v2 sizes store 7 bits per byte.
fn id3_syncsafe(u32 v) {
    return ((v >> 24) & 0x7F) << 21 | ((v >> 16) & 0x7F) << 14 | ((v >> 8) & 0x7F) << 7 | (v & 0x7F);
};

struct Id3Header {
    char identifier[3] [[comment("ID3")]];
    u8 version_major;
    u8 version_revision;
    Id3Flags flags;
    u32 size_syncsafe [[format("hex")]];
    auto tag_size = id3_syncsafe(size_syncsafe);
};

struct Id3Frame {
    if (id3_major == 2) {
        char id[3];
        u24 frame_size;
    } else {
        char id[4];
        u32 raw_size;
        auto frame_size = id3_major == 4 ? id3_syncsafe(raw_size) : raw_size;
        u16 flags [[format("hex")]];
    }
    if (id[0] == 'T' && id != "TXX" && id != "TXXX" && frame_size >= 1) {
        TextEncoding encoding;
        if (encoding == TextEncoding::UTF16 && frame_size >= 3) {
            if (std::mem::read_unsigned($, 2, "big") == 0xFFFE) {
                u16 byte_order_mark [[format("hex")]];
                le char16 text[(frame_size - 3) / 2];
            } else {
                u16 byte_order_mark [[format("hex")]];
                be char16 text[(frame_size - 3) / 2];
            }
        } else if (encoding == TextEncoding::UTF16BE) {
            be char16 text[(frame_size - 1) / 2];
        } else {
            char text[frame_size - 1];
        }
    } else {
        u8 data[frame_size];
    }
} [[format("mp3_frame_text")]];

fn mp3_frame_text(auto f) {
    return f.id;
};

struct Id3Tag {
    Id3Header header [[color("808080")]];
    id3_major = header.version_major;
    u64 tag_end = 10 + header.tag_size;
    if (header.flags.extended_header) {
        u32 extended_size_raw;
        u8 extended_header[(id3_major == 4 ? id3_syncsafe(extended_size_raw) : extended_size_raw + 4) - 4];
    }
    Id3Frame frames[while($ + 10 <= tag_end && std::mem::read_unsigned($, 1) != 0)];
    if ($ < tag_end) u8 padding_bytes[tag_end - $];
};

fn mpeg_bitrate(u8 version, u8 layer, u8 index) {
    if (index == 0 || index == 15) return 0;
    if (version == 3) {
        if (layer == 3) return index * 32;
        if (layer == 2) {
            match (index) {
                (1): return 32;  (2): return 48;  (3): return 56;  (4): return 64;
                (5): return 80;  (6): return 96;  (7): return 112; (8): return 128;
                (9): return 160; (10): return 192; (11): return 224; (12): return 256;
                (13): return 320; (14): return 384;
            }
        }
        match (index) {
            (1): return 32;  (2): return 40;  (3): return 48;  (4): return 56;
            (5): return 64;  (6): return 80;  (7): return 96;  (8): return 112;
            (9): return 128; (10): return 160; (11): return 192; (12): return 224;
            (13): return 256; (14): return 320;
        }
    }
    if (layer == 3) {
        match (index) {
            (1): return 32;  (2): return 48;  (3): return 56;  (4): return 64;
            (5): return 80;  (6): return 96;  (7): return 112; (8): return 128;
            (9): return 144; (10): return 160; (11): return 176; (12): return 192;
            (13): return 224; (14): return 256;
        }
    }
    if (index <= 8) return index * 8;
    return 64 + (index - 8) * 16;
};

fn mpeg_sample_rate(u8 version, u8 index) {
    u32 base = 0;
    match (index) {
        (0): base = 44100;
        (1): base = 48000;
        (2): base = 32000;
        (_): return 0;
    }
    if (version == 3) return base;
    if (version == 2) return base / 2;
    if (version == 0) return base / 4;
    return 0;
};

bitfield MpegHeader {
    frame_sync : 11 [[format("hex")]];
    MpegVersion version : 2;
    MpegLayer layer : 2;
    bool no_crc : 1 [[comment("1 = not protected by a CRC")]];
    bitrate_index : 4;
    sample_rate_index : 2;
    bool padded : 1;
    private_bit : 1;
    ChannelMode channel_mode : 2;
    mode_extension : 2;
    bool copyright : 1;
    bool original : 1;
    Emphasis emphasis : 2;
};

fn mpeg_frame_length(u8 version, u8 layer, u32 bitrate, u32 rate, u8 padded) {
    if (bitrate == 0 || rate == 0) return 0;
    if (layer == 3) return (12 * bitrate * 1000 / rate + padded) * 4;
    if (layer == 1 && version != 3) return 72 * bitrate * 1000 / rate + padded;
    return 144 * bitrate * 1000 / rate + padded;
};

fn mpeg_frame_at(u64 pos) {
    if (pos + 4 > audio_end) return false;
    u32 h = std::mem::read_unsigned(pos, 4, "big");
    if ((h & 0xFFE00000) != 0xFFE00000) return false;
    u8 version = (h >> 19) & 3;
    u8 layer = (h >> 17) & 3;
    u8 bitrate_index = (h >> 12) & 0xF;
    u8 rate_index = (h >> 10) & 3;
    if (version == 1 || layer == 0 || bitrate_index == 0 || bitrate_index == 15 || rate_index == 3)
        return false;
    return true;
};

struct MpegFrame {
    MpegHeader header [[color("E67E22")]];
    auto bitrate_kbps = mpeg_bitrate(header.version, header.layer, header.bitrate_index);
    auto sample_rate = mpeg_sample_rate(header.version, header.sample_rate_index);
    auto frame_length = mpeg_frame_length(header.version, header.layer, bitrate_kbps,
        sample_rate, header.padded);
    if (!header.no_crc) u16 crc [[format("hex")]];
    u64 used = $ - addressof(this);
    u64 available = audio_end - addressof(this);
    u64 length = frame_length < available ? frame_length : available;
    if (length > used) u8 audio_data[length - used];
} [[format("mp3_mpeg_text")]];

fn mp3_mpeg_text(auto f) {
    return std::format("{} kbit/s, {} Hz", f.bitrate_kbps, f.sample_rate);
};

struct Id3v1Tag {
    char identifier[3] [[comment("TAG")]];
    char title[30];
    char artist[30];
    char album[30];
    char year[4];
    if (std::mem::read_unsigned($ + 28, 1) == 0 && std::mem::read_unsigned($ + 29, 1) != 0) {
        char comment[28];
        u8 zero_byte;
        u8 track;
    } else {
        char comment[30];
    }
    u8 genre;
};

u8 id3_major = 0;
u64 audio_end = std::mem::size();
if (std::mem::size() >= 128 && std::mem::read_string(std::mem::size() - 128, 3) == "TAG")
    audio_end = std::mem::size() - 128;
if (std::mem::size() >= 10 && std::mem::read_string(0, 3) == "ID3") Id3Tag id3v2;
MpegFrame frames[while(mpeg_frame_at($))];
if ($ < audio_end) u8 unparsed[audio_end - $];
if (audio_end < std::mem::size()) Id3v1Tag id3v1 @ audio_end;
