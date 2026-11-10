#pragma description ISO base media file (MP4, MOV, M4A, 3GP, HEIF)
#pragma endian big

// Fixed-point helpers.
fn mp4_fixed16_16(u32 v) {
    return std::format("{}.{:04}", v >> 16, (v & 0xFFFF) * 10000 / 65536);
};

fn mp4_fixed8_8(u16 v) {
    return std::format("{}.{:02}", v >> 8, (v & 0xFF) * 100 / 256);
};

fn mp4_language(u16 v) {
    return std::format("{:c}{:c}{:c}", ((v >> 10) & 0x1F) + 0x60, ((v >> 5) & 0x1F) + 0x60,
        (v & 0x1F) + 0x60);
};

fn mp4_remaining() {
    return std::mem::base_address() + std::mem::size() - $;
};

struct Brand {
    char name[4];
} [[format("mp4_brand_text")]];

fn mp4_brand_text(auto b) {
    return b.name;
};

struct FileType {
    char major_brand[4];
    u32 minor_version;
    Brand compatible_brands[];
};

struct FullBoxHeader {
    u8 version;
    u24 flags [[format("hex")]];
};

struct MovieHeader {
    FullBoxHeader full;
    if (full.version == 1) {
        u64 creation_time;
        u64 modification_time;
        u32 timescale;
        u64 duration;
    } else {
        u32 creation_time;
        u32 modification_time;
        u32 timescale;
        u32 duration;
    }
    u32 rate [[format("mp4_fixed16_16")]];
    u16 volume [[format("mp4_fixed8_8")]];
    padding[10];
    s32 matrix[9];
    u32 pre_defined[6];
    u32 next_track_id;
    auto duration_seconds = timescale == 0 ? 0 : duration / timescale;
};

struct TrackHeader {
    FullBoxHeader full;
    if (full.version == 1) {
        u64 creation_time;
        u64 modification_time;
        u32 track_id;
        u32 reserved;
        u64 duration;
    } else {
        u32 creation_time;
        u32 modification_time;
        u32 track_id;
        u32 reserved;
        u32 duration;
    }
    padding[8];
    s16 layer;
    s16 alternate_group;
    u16 volume [[format("mp4_fixed8_8")]];
    padding[2];
    s32 matrix[9];
    u32 width [[format("mp4_fixed16_16")]];
    u32 height [[format("mp4_fixed16_16")]];
};

struct MediaHeader {
    FullBoxHeader full;
    if (full.version == 1) {
        u64 creation_time;
        u64 modification_time;
        u32 timescale;
        u64 duration;
    } else {
        u32 creation_time;
        u32 modification_time;
        u32 timescale;
        u32 duration;
    }
    u16 language [[format("mp4_language")]];
    u16 quality;
};

struct HandlerReference {
    FullBoxHeader full;
    u32 pre_defined;
    char handler_type[4] [[comment("vide, soun, hint, meta, text, ...")]];
    u32 reserved[3];
    char name[];
};

struct TimeToSampleEntry {
    u32 sample_count;
    u32 sample_delta;
};

struct TimeToSample {
    FullBoxHeader full;
    u32 entry_count;
    TimeToSampleEntry entries[entry_count];
};

struct SampleToChunkEntry {
    u32 first_chunk;
    u32 samples_per_chunk;
    u32 sample_description_index;
};

struct SampleToChunk {
    FullBoxHeader full;
    u32 entry_count;
    SampleToChunkEntry entries[entry_count];
};

struct SampleSize {
    FullBoxHeader full;
    u32 sample_size;
    u32 sample_count;
    if (sample_size == 0) u32 entry_sizes[sample_count];
};

struct ChunkOffset {
    FullBoxHeader full;
    u32 entry_count;
    u32 chunk_offsets[entry_count];
};

struct ChunkOffset64 {
    FullBoxHeader full;
    u32 entry_count;
    u64 chunk_offsets[entry_count];
};

struct SyncSample {
    FullBoxHeader full;
    u32 entry_count;
    u32 sample_numbers[entry_count];
};

struct SampleDescription {
    FullBoxHeader full;
    u32 entry_count;
    Box entries[];
};

struct BoxList {
    Box boxes[];
};

struct Box {
    u32 size;
    char type[4] [[color("E67E22")]];
    if (size == 1) u64 large_size;
    if (type == "uuid") u8 user_type[16];
    u64 header_length = $ - addressof(this);
    u64 total = size == 1 ? large_size : (size == 0 ? mp4_remaining() + header_length : size);
    u64 body = total - header_length;
    match (type) {
        ("moov" | "trak" | "mdia" | "minf" | "stbl" | "edts" | "dinf" | "udta" | "mvex"
                | "moof" | "traf" | "mfra" | "ilst" | "sinf" | "schi" | "tref" | "gmhd"
                | "iprp" | "ipco"):
            BoxList children [[size(body)]];
        ("meta"): {
            // ISO meta is a full box; QuickTime meta is a plain container.
            if (std::mem::read_unsigned($ + 4, 4) == 'hdlr') {
                BoxList children [[size(body)]];
            } else {
                FullBoxHeader full;
                BoxList children [[size(body - 4)]];
            }
        }
        ("ftyp" | "styp"): FileType file_type [[size(body)]];
        ("mvhd"): MovieHeader movie_header [[size(body)]];
        ("tkhd"): TrackHeader track_header [[size(body)]];
        ("mdhd"): MediaHeader media_header [[size(body)]];
        ("hdlr"): HandlerReference handler [[size(body)]];
        ("stsd"): SampleDescription sample_description [[size(body)]];
        ("stts"): TimeToSample time_to_sample [[size(body)]];
        ("stsc"): SampleToChunk sample_to_chunk [[size(body)]];
        ("stsz"): SampleSize sample_size [[size(body)]];
        ("stco"): ChunkOffset chunk_offset [[size(body)]];
        ("co64"): ChunkOffset64 chunk_offset [[size(body)]];
        ("stss"): SyncSample sync_sample [[size(body)]];
        ("mdat"): u8 media_data[body];
        ("free" | "skip" | "wide"): u8 free_space[body];
        (_): u8 data[body];
    }
} [[format("mp4_box_text")]];

fn mp4_box_text(auto b) {
    if (b.size == 1) return std::format("{} ({} bytes)", b.type, b.large_size);
    return std::format("{} ({} bytes)", b.type, b.size);
};

Box boxes[];
