#pragma description ext2/ext3/ext4 superblock, group descriptors and root inode
#pragma endian little

enum State : u16 {
    Clean = 0x1,
    Errors = 0x2,
    OrphansRecovering = 0x4
} [[flags]];

enum ErrorBehaviour : u16 {
    Continue = 1,
    RemountReadOnly = 2,
    Panic = 3
};

enum CreatorOs : u32 {
    Linux = 0,
    Hurd = 1,
    Masix = 2,
    FreeBSD = 3,
    Lites = 4
};

enum Revision : u32 {
    GoodOld = 0,
    Dynamic = 1
};

enum CompatFeatures : u32 {
    DirPrealloc = 0x1,
    ImagicInodes = 0x2,
    HasJournal = 0x4,
    ExtAttr = 0x8,
    ResizeInode = 0x10,
    DirIndex = 0x20,
    LazyBg = 0x40,
    ExcludeBitmap = 0x100,
    SparseSuper2 = 0x200,
    FastCommit = 0x400,
    StableInodes = 0x800,
    OrphanFile = 0x1000
} [[flags]];

enum IncompatFeatures : u32 {
    Compression = 0x1,
    FileType = 0x2,
    Recover = 0x4,
    JournalDev = 0x8,
    MetaBg = 0x10,
    Extents = 0x40,
    Bit64 = 0x80,
    Mmp = 0x100,
    FlexBg = 0x200,
    EaInode = 0x400,
    DirData = 0x1000,
    CsumSeed = 0x2000,
    LargeDir = 0x4000,
    InlineData = 0x8000,
    Encrypt = 0x10000,
    Casefold = 0x20000
} [[flags]];

enum RoCompatFeatures : u32 {
    SparseSuper = 0x1,
    LargeFile = 0x2,
    BtreeDir = 0x4,
    HugeFile = 0x8,
    GdtCsum = 0x10,
    DirNlink = 0x20,
    ExtraIsize = 0x40,
    HasSnapshot = 0x80,
    Quota = 0x100,
    BigAlloc = 0x200,
    MetadataCsum = 0x400,
    Replica = 0x800,
    ReadOnly = 0x1000,
    Project = 0x2000,
    SharedBlocks = 0x4000,
    Verity = 0x8000,
    OrphanPresent = 0x10000
} [[flags]];

enum HashVersion : u8 {
    Legacy = 0,
    HalfMD4 = 1,
    Tea = 2,
    LegacyUnsigned = 3,
    HalfMD4Unsigned = 4,
    TeaUnsigned = 5,
    SipHash = 6
};

enum GroupFlags : u16 {
    InodeUninit = 0x1,
    BlockUninit = 0x2,
    InodeTableZeroed = 0x4
} [[flags]];

enum InodeFlags : u32 {
    SecureRm = 0x1,
    Unrm = 0x2,
    Compressed = 0x4,
    Sync = 0x8,
    Immutable = 0x10,
    Append = 0x20,
    NoDump = 0x40,
    NoAtime = 0x80,
    Index = 0x1000,
    JournalData = 0x4000,
    NoTail = 0x8000,
    DirSync = 0x10000,
    TopDir = 0x20000,
    HugeFile = 0x40000,
    Extents = 0x80000,
    Verity = 0x100000,
    EaInode = 0x200000,
    InlineData = 0x10000000,
    ProjInherit = 0x20000000,
    Casefold = 0x40000000
} [[flags]];

struct Uuid {
    u8 bytes[16];
} [[format("ext4_uuid_text")]];

fn ext4_uuid_text(auto u) {
    return std::format("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u.bytes[0], u.bytes[1], u.bytes[2], u.bytes[3], u.bytes[4], u.bytes[5], u.bytes[6],
        u.bytes[7], u.bytes[8], u.bytes[9], u.bytes[10], u.bytes[11], u.bytes[12],
        u.bytes[13], u.bytes[14], u.bytes[15]);
};

struct Superblock {
    u32 inodes_count;
    u32 blocks_count_lo;
    u32 reserved_blocks_count_lo;
    u32 free_blocks_count_lo;
    u32 free_inodes_count;
    u32 first_data_block;
    u32 log_block_size [[comment("block size = 1024 << n")]];
    u32 log_cluster_size;
    u32 blocks_per_group;
    u32 clusters_per_group;
    u32 inodes_per_group;
    u32 mount_time [[format("unix_time_text")]];
    u32 write_time [[format("unix_time_text")]];
    u16 mount_count;
    s16 max_mount_count;
    u16 magic [[format("hex"), comment("0xEF53"), color("E67E22")]];
    State state;
    ErrorBehaviour errors;
    u16 minor_revision;
    u32 last_check [[format("unix_time_text")]];
    u32 check_interval;
    CreatorOs creator_os;
    Revision revision;
    u16 default_reserved_uid;
    u16 default_reserved_gid;
    // EXT2_DYNAMIC_REV fields
    u32 first_inode;
    u16 inode_size;
    u16 block_group_number;
    CompatFeatures feature_compat;
    IncompatFeatures feature_incompat;
    RoCompatFeatures feature_ro_compat;
    Uuid uuid;
    char volume_name[16];
    char last_mounted[64];
    u32 algorithm_usage_bitmap;
    u8 prealloc_blocks;
    u8 prealloc_dir_blocks;
    u16 reserved_gdt_blocks;
    Uuid journal_uuid;
    u32 journal_inode;
    u32 journal_device;
    u32 last_orphan;
    u32 hash_seed[4];
    HashVersion default_hash_version;
    u8 journal_backup_type;
    u16 group_descriptor_size;
    u32 default_mount_options [[format("hex")]];
    u32 first_meta_block_group;
    u32 mkfs_time [[format("unix_time_text")]];
    u32 journal_blocks[17];
    // 64-bit support
    u32 blocks_count_hi;
    u32 reserved_blocks_count_hi;
    u32 free_blocks_count_hi;
    u16 min_extra_inode_size;
    u16 want_extra_inode_size;
    u32 flags [[format("hex")]];
    u16 raid_stride;
    u16 mmp_interval;
    u64 mmp_block;
    u32 raid_stripe_width;
    u8 log_groups_per_flex;
    u8 checksum_type [[comment("1 = crc32c")]];
    u8 encryption_level;
    u8 reserved_pad;
    u64 kbytes_written;
    u32 snapshot_inode;
    u32 snapshot_id;
    u64 snapshot_reserved_blocks;
    u32 snapshot_list;
    u32 error_count;
    u32 first_error_time [[format("unix_time_text")]];
    u32 first_error_inode;
    u64 first_error_block;
    char first_error_function[32];
    u32 first_error_line;
    u32 last_error_time [[format("unix_time_text")]];
    u32 last_error_inode;
    u32 last_error_line;
    u64 last_error_block;
    char last_error_function[32];
    char mount_options[64];
    u32 user_quota_inode;
    u32 group_quota_inode;
    u32 overhead_clusters;
    u32 backup_block_groups[2];
    u8 encryption_algorithms[4];
    u8 encryption_password_salt[16];
    u32 lost_and_found_inode;
    u32 project_quota_inode;
    u32 checksum_seed;
    u8 write_time_hi;
    u8 mount_time_hi;
    u8 mkfs_time_hi;
    u8 last_check_hi;
    u8 first_error_time_hi;
    u8 last_error_time_hi;
    u8 first_error_code;
    u8 last_error_code;
    u16 filename_encoding;
    u16 filename_encoding_flags;
    u32 orphan_file_inode;
    u32 reserved[94];
    if ((feature_ro_compat & RoCompatFeatures::MetadataCsum) != 0)
        u32 checksum [[format("hex"), checksum("crc32c", addressof(this), addressof(checksum))]];
    else
        u32 checksum [[format("hex")]];
    auto block_size = 1024 << log_block_size;
    auto is_64bit = (feature_incompat & IncompatFeatures::Bit64) != 0;
    auto blocks_count = is_64bit ? blocks_count_hi << 32 | blocks_count_lo : blocks_count_lo;
    auto descriptor_size = is_64bit && group_descriptor_size >= 64 ? group_descriptor_size : 32;
    auto group_count = blocks_per_group == 0 ? 0
        : (blocks_count - first_data_block + blocks_per_group - 1) / blocks_per_group;
} [[color("2E86C1")]];

struct GroupDescriptor {
    u32 block_bitmap_lo;
    u32 inode_bitmap_lo;
    u32 inode_table_lo;
    u16 free_blocks_count_lo;
    u16 free_inodes_count_lo;
    u16 used_dirs_count_lo;
    GroupFlags flags;
    u32 exclude_bitmap_lo;
    u16 block_bitmap_checksum_lo [[format("hex")]];
    u16 inode_bitmap_checksum_lo [[format("hex")]];
    u16 inode_table_unused_lo;
    u16 checksum [[format("hex")]];
    if (superblock.descriptor_size >= 64) {
        u32 block_bitmap_hi;
        u32 inode_bitmap_hi;
        u32 inode_table_hi;
        u16 free_blocks_count_hi;
        u16 free_inodes_count_hi;
        u16 used_dirs_count_hi;
        u16 inode_table_unused_hi;
        u32 exclude_bitmap_hi;
        u16 block_bitmap_checksum_hi [[format("hex")]];
        u16 inode_bitmap_checksum_hi [[format("hex")]];
        u32 reserved;
        if (superblock.descriptor_size > 64) padding[superblock.descriptor_size - 64];
    }
};

fn ext4_mode_text(u16 mode) {
    str kind = "unknown";
    match (mode >> 12) {
        (0x1): kind = "fifo";
        (0x2): kind = "character device";
        (0x4): kind = "directory";
        (0x6): kind = "block device";
        (0x8): kind = "regular file";
        (0xA): kind = "symbolic link";
        (0xC): kind = "socket";
    }
    return std::format("{} {:04o}", kind, mode & 0xFFF);
};

struct ExtentHeader {
    u16 magic [[format("hex"), comment("0xF30A")]];
    u16 entries;
    u16 max_entries;
    u16 depth [[comment("0 = leaf extents follow")]];
    u32 generation;
};

struct Extent {
    u32 first_logical_block;
    u16 length [[comment("> 32768 = uninitialized")]];
    u16 start_hi;
    u32 start_lo;
    auto start_block = start_hi << 32 | start_lo;
};

struct ExtentIndex {
    u32 first_logical_block;
    u32 leaf_lo;
    u16 leaf_hi;
    u16 unused;
    auto leaf_block = leaf_hi << 32 | leaf_lo;
};

struct ExtentTree {
    ExtentHeader header;
    u16 count = header.entries > 4 ? 4 : header.entries;
    if (header.depth == 0) Extent extents[count];
    else ExtentIndex indexes[count];
};

struct Inode {
    u16 mode [[format("ext4_mode_text")]];
    u16 uid;
    u32 size_lo;
    u32 access_time [[format("unix_time_text")]];
    u32 change_time [[format("unix_time_text")]];
    u32 modification_time [[format("unix_time_text")]];
    u32 deletion_time [[format("unix_time_text")]];
    u16 gid;
    u16 links_count;
    u32 blocks_lo;
    InodeFlags flags;
    u32 osd1;
    if ((flags & InodeFlags::Extents) != 0) ExtentTree extent_tree [[size(60)]];
    else u32 block[15];
    u32 generation;
    u32 file_acl_lo;
    u32 size_hi;
    u32 obsolete_fragment_address;
    u8 osd2[12];
    if (superblock.inode_size > 128) {
        u16 extra_inode_size;
        u16 checksum_hi [[format("hex")]];
        u32 change_time_extra;
        u32 modification_time_extra;
        u32 access_time_extra;
        u32 creation_time [[format("unix_time_text")]];
        u32 creation_time_extra;
        u32 version_hi;
        u32 project_id;
    }
    auto size = size_hi << 32 | size_lo;
};

Superblock superblock @ 1024;
u64 block_size = superblock.block_size;
u64 gdt_at = (superblock.first_data_block + 1) * block_size;
u64 descriptors = superblock.group_count;
if (gdt_at < std::mem::size()) {
    u64 fit = (std::mem::size() - gdt_at) / superblock.descriptor_size;
    if (descriptors > fit) descriptors = fit;
    if (descriptors > 0) GroupDescriptor group_descriptors[descriptors] @ gdt_at;
}
if (descriptors > 0 && superblock.inode_size >= 128) {
    u64 table_block = group_descriptors[0].inode_table_lo;
    if (superblock.descriptor_size >= 64)
        table_block = table_block | group_descriptors[0].inode_table_hi << 32;
    u64 root_inode_at = table_block * block_size + superblock.inode_size;
    if (root_inode_at + superblock.inode_size <= std::mem::size())
        Inode root_inode @ root_inode_at [[size(superblock.inode_size), comment("inode 2: /")]];
}
