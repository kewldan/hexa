#pragma description NTFS boot sector and the first MFT records
#pragma endian little

enum AttributeType : u32 {
    StandardInformation = 0x10,
    AttributeList = 0x20,
    FileName = 0x30,
    ObjectId = 0x40,
    SecurityDescriptor = 0x50,
    VolumeName = 0x60,
    VolumeInformation = 0x70,
    Data = 0x80,
    IndexRoot = 0x90,
    IndexAllocation = 0xA0,
    Bitmap = 0xB0,
    ReparsePoint = 0xC0,
    EAInformation = 0xD0,
    EA = 0xE0,
    LoggedUtilityStream = 0x100,
    End = 0xFFFFFFFF
};

enum RecordFlags : u16 {
    InUse = 0x1,
    Directory = 0x2,
    Extension = 0x4,
    SpecialIndex = 0x8
} [[flags]];

enum FileAttributes : u32 {
    ReadOnly = 0x1,
    Hidden = 0x2,
    System = 0x4,
    Directory = 0x10,
    Archive = 0x20,
    Device = 0x40,
    Normal = 0x80,
    Temporary = 0x100,
    SparseFile = 0x200,
    ReparsePoint = 0x400,
    Compressed = 0x800,
    Offline = 0x1000,
    NotContentIndexed = 0x2000,
    Encrypted = 0x4000,
    HasDirectoryIndex = 0x10000000,
    HasViewIndex = 0x20000000
} [[flags]];

enum FileNamespace : u8 {
    POSIX = 0,
    Win32 = 1,
    DOS = 2,
    Win32AndDOS = 3
};

// FILETIME: 100 ns intervals since 1601-01-01.
fn ntfs_time_text(u64 t) {
    if (t == 0) return "0 (not set)";
    return unix_time_text(t / 10000000 - 11644473600);
};

struct BootSector {
    u8 jump[3];
    char oem_id[8] [[comment("NTFS")]];
    u16 bytes_per_sector;
    u8 sectors_per_cluster;
    u16 reserved_sectors;
    u8 always_zero[3];
    u16 unused1;
    u8 media_descriptor [[format("hex")]];
    u16 unused2;
    u16 sectors_per_track;
    u16 head_count;
    u32 hidden_sectors;
    u32 unused3;
    u32 unused4 [[format("hex")]];
    u64 total_sectors;
    u64 mft_cluster;
    u64 mft_mirror_cluster;
    s8 clusters_per_file_record [[comment("negative: 2^-n bytes")]];
    padding[3];
    s8 clusters_per_index_buffer [[comment("negative: 2^-n bytes")]];
    padding[3];
    u64 volume_serial_number [[format("hex")]];
    u32 checksum;
    u8 bootstrap_code[426];
    u16 end_marker [[format("hex"), comment("0xAA55")]];
    auto cluster_size = bytes_per_sector * sectors_per_cluster;
    auto file_record_size = clusters_per_file_record < 0 ? 1 << -clusters_per_file_record
        : clusters_per_file_record * cluster_size;
    auto index_buffer_size = clusters_per_index_buffer < 0 ? 1 << -clusters_per_index_buffer
        : clusters_per_index_buffer * cluster_size;
    auto mft_offset = mft_cluster * cluster_size;
    auto mft_mirror_offset = mft_mirror_cluster * cluster_size;
} [[color("2E86C1")]];

struct StandardInformation {
    u64 creation_time [[format("ntfs_time_text")]];
    u64 modification_time [[format("ntfs_time_text")]];
    u64 mft_modification_time [[format("ntfs_time_text")]];
    u64 access_time [[format("ntfs_time_text")]];
    FileAttributes attributes;
    u32 max_versions;
    u32 version;
    u32 class_id;
};

struct FileNameAttribute {
    u64 parent_reference [[format("hex")]];
    u64 creation_time [[format("ntfs_time_text")]];
    u64 modification_time [[format("ntfs_time_text")]];
    u64 mft_modification_time [[format("ntfs_time_text")]];
    u64 access_time [[format("ntfs_time_text")]];
    u64 allocated_size;
    u64 real_size;
    FileAttributes attributes;
    u32 reparse_value;
    u8 name_length;
    FileNamespace name_space;
    char16 name[name_length];
};

struct Attribute {
    AttributeType type [[color("E67E22")]];
    u32 length;
    bool non_resident;
    u8 name_length;
    u16 name_offset;
    u16 flags [[format("hex"), comment("0x1 compressed, 0x4000 encrypted, 0x8000 sparse")]];
    u16 attribute_id;
    if (!non_resident) {
        u32 value_length;
        u16 value_offset;
        u8 indexed_flag;
        padding[1];
        u64 value_at = addressof(this) + value_offset;
        if (type == AttributeType::StandardInformation && value_length >= 48)
            StandardInformation standard_information @ value_at;
        else if (type == AttributeType::FileName && value_length >= 66)
            FileNameAttribute file_name @ value_at;
        else if (type == AttributeType::VolumeName)
            char16 volume_name[value_length / 2] @ value_at;
        else if (value_length > 0)
            u8 value[value_length] @ value_at;
    } else {
        u64 starting_vcn;
        u64 last_vcn;
        u16 data_runs_offset;
        u16 compression_unit;
        padding[4];
        u64 allocated_size;
        u64 real_size;
        u64 initialized_size;
    }
    if (name_length > 0) char16 name[name_length] @ addressof(this) + name_offset;
    if (addressof(this) + length > $) $ = addressof(this) + length;
};

struct MftRecord {
    char signature[4] [[comment("FILE (BAAD = damaged)")]];
    if (signature == "FILE") {
        u16 update_sequence_offset;
        u16 update_sequence_count;
        u64 logfile_sequence_number;
        u16 sequence_number;
        u16 hard_link_count;
        u16 first_attribute_offset;
        RecordFlags flags;
        u32 used_size;
        u32 allocated_size;
        u64 base_record [[format("hex")]];
        u16 next_attribute_id;
        u16 alignment;
        u32 record_number;
        u16 update_sequence[update_sequence_count] @ addressof(this) + update_sequence_offset;
        u64 record_end = addressof(this) + used_size;
        $ = addressof(this) + first_attribute_offset;
        Attribute attributes[while($ + 4 <= record_end
            && std::mem::read_unsigned($, 4) != 0xFFFFFFFF)];
        if ($ + 4 <= record_end) AttributeType end_marker;
    }
} [[size(boot.file_record_size)]];

BootSector boot @ 0;
u64 mft_records = 0;
if (boot.file_record_size > 0 && boot.mft_offset + boot.file_record_size <= std::mem::size()) {
    mft_records = (std::mem::size() - boot.mft_offset) / boot.file_record_size;
    if (mft_records > 16) mft_records = 16;
}
if (mft_records > 0) MftRecord mft[mft_records] @ boot.mft_offset [[comment("system files: $MFT, $MFTMirr, $LogFile, $Volume, ...")]];
