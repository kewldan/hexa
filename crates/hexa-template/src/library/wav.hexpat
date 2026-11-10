#pragma description WAVE audio (RIFF)
#pragma endian little

enum AudioFormat : u16 {
    PCM = 0x0001,
    ADPCM = 0x0002,
    IEEEFloat = 0x0003,
    ALaw = 0x0006,
    MuLaw = 0x0007,
    IMAADPCM = 0x0011,
    GSM610 = 0x0031,
    MPEG = 0x0050,
    MPEGLayer3 = 0x0055,
    Extensible = 0xFFFE
};

enum SpeakerPositions : u32 {
    FrontLeft = 0x1,
    FrontRight = 0x2,
    FrontCenter = 0x4,
    LowFrequency = 0x8,
    BackLeft = 0x10,
    BackRight = 0x20,
    FrontLeftOfCenter = 0x40,
    FrontRightOfCenter = 0x80,
    BackCenter = 0x100,
    SideLeft = 0x200,
    SideRight = 0x400,
    TopCenter = 0x800
} [[flags]];

struct Guid {
    u32 data1;
    u16 data2;
    u16 data3;
    u8 data4[8];
} [[format("wav_guid_text")]];

fn wav_guid_text(auto g) {
    return std::format("{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3],
        g.data4[4], g.data4[5], g.data4[6], g.data4[7]);
};

fn wav_remaining() {
    return std::mem::base_address() + std::mem::size() - $;
};

fn wav_clamp(u64 n) {
    u64 left = wav_remaining();
    return n < left ? n : left;
};

struct FormatChunk {
    AudioFormat format;
    u16 channels;
    u32 sample_rate;
    u32 byte_rate;
    u16 block_align;
    u16 bits_per_sample;
    if (!std::mem::eof()) {
        u16 extension_size;
        if (format == AudioFormat::Extensible && extension_size >= 22) {
            u16 valid_bits_per_sample;
            SpeakerPositions channel_mask;
            Guid sub_format;
            if (extension_size > 22) u8 extra[extension_size - 22];
        } else {
            u8 extra[extension_size];
        }
    }
};

struct ListItem {
    char id[4];
    u32 size;
    char text[wav_clamp(size)];
    if (size % 2 == 1 && !std::mem::eof()) padding[1];
};

struct ListChunk {
    char list_type[4];
    if (list_type == "INFO") ListItem items[];
    else u8 data[];
};

struct Chunk {
    char id[4] [[color("E67E22")]];
    u32 size;
    match (id) {
        ("fmt "): FormatChunk format [[size(size)]];
        ("fact"): u32 sample_frames;
        ("LIST"): ListChunk list [[size(wav_clamp(size))]];
        ("data"): u8 samples[wav_clamp(size)];
        (_): u8 data[wav_clamp(size)];
    }
    if (size % 2 == 1 && !std::mem::eof()) padding[1];
} [[format("wav_chunk_text")]];

fn wav_chunk_text(auto c) {
    return std::format("{} ({} bytes)", c.id, c.size);
};

struct RiffHeader {
    char riff[4] [[comment("RIFF")]];
    u32 riff_size;
    char wave[4] [[comment("WAVE")]];
} [[color("808080")]];

RiffHeader header;
Chunk chunks[while(!std::mem::eof())];
