#pragma description GZIP compressed file (RFC 1952)
#pragma endian little

enum Method : u8 {
    Deflate = 8
};

enum ExtraFlags : u8 {
    None = 0,
    MaximumCompression = 2,
    FastestCompression = 4
};

enum OperatingSystem : u8 {
    FAT = 0,
    Amiga = 1,
    VMS = 2,
    Unix = 3,
    VMCMS = 4,
    AtariTOS = 5,
    HPFS = 6,
    Macintosh = 7,
    ZSystem = 8,
    CPM = 9,
    TOPS20 = 10,
    NTFS = 11,
    QDOS = 12,
    AcornRISCOS = 13,
    Unknown = 255
};

bitfield Flags {
    bool text : 1;
    bool header_crc : 1;
    bool extra : 1;
    bool name : 1;
    bool comment : 1;
    reserved : 3;
};

struct ExtraSubfield {
    char id[2];
    u16 length;
    u8 data[length];
};

struct Member {
    u8 magic[2] [[comment("1F 8B"), color("808080")]];
    Method method;
    Flags flags;
    u32 modification_time [[format("unix_time_text")]];
    ExtraFlags extra_flags;
    OperatingSystem os;
    if (flags.extra) {
        u16 extra_length;
        ExtraSubfield extra[] [[size(extra_length)]];
    }
    if (flags.name) string file_name;
    if (flags.comment) string comment;
    if (flags.header_crc)
        u16 header_crc16 [[format("hex"), comment("low 16 bits of the CRC-32 of the header")]];
    u64 trailer_at = std::mem::base_address() + std::mem::size() - 8;
    assert($ <= trailer_at, "truncated GZIP file: no room for the trailer");
    u8 compressed_data[trailer_at - $] [[comment("raw deflate stream"), color("27AE60")]];
    u32 crc32 [[format("hex"), color("E67E22"),
        checksum("crc32_inflate", addressof(compressed_data), trailer_at),
        comment("CRC-32 of the uncompressed data")]];
    u32 uncompressed_size [[comment("ISIZE: size modulo 2^32")]];
};

Member member @ 0;
