#pragma description FAT12/16/32 file system: boot sector (BPB), FSInfo, FAT and root directory
#pragma endian little

enum MediaDescriptor : u8 {
    Floppy1440K = 0xF0,
    FixedDisk = 0xF8,
    Floppy720K = 0xF9,
    Floppy320K = 0xFA,
    Floppy640K = 0xFB,
    Floppy180K = 0xFC,
    Floppy360K = 0xFD,
    Floppy160K = 0xFE,
    Floppy320K_DS = 0xFF
};

bitfield DirAttributes {
    bool read_only : 1;
    bool hidden : 1;
    bool system : 1;
    bool volume_label : 1;
    bool directory : 1;
    bool archive : 1;
    reserved : 2;
};

struct BootSector {
    u8 jump[3];
    char oem_name[8];
    u16 bytes_per_sector;
    u8 sectors_per_cluster;
    u16 reserved_sectors;
    u8 fat_count;
    u16 root_entry_count [[comment("0 on FAT32")]];
    u16 total_sectors_16;
    MediaDescriptor media;
    u16 sectors_per_fat_16 [[comment("0 on FAT32")]];
    u16 sectors_per_track;
    u16 head_count;
    u32 hidden_sectors;
    u32 total_sectors_32;
    if (sectors_per_fat_16 == 0) {
        // FAT32 extended BIOS parameter block
        u32 sectors_per_fat_32;
        u16 extended_flags [[format("hex")]];
        u16 fs_version [[format("hex")]];
        u32 root_cluster;
        u16 fs_info_sector;
        u16 backup_boot_sector;
        padding[12];
        u8 drive_number [[format("hex")]];
        u8 reserved1;
        u8 boot_signature [[format("hex"), comment("0x29: the next three fields are valid")]];
        u32 volume_id [[format("hex")]];
        char volume_label[11];
        char fs_type[8];
        u8 boot_code[420];
    } else {
        u8 drive_number [[format("hex")]];
        u8 reserved1;
        u8 boot_signature [[format("hex"), comment("0x29: the next three fields are valid")]];
        u32 volume_id [[format("hex")]];
        char volume_label[11];
        char fs_type[8];
        u8 boot_code[448];
    }
    u16 signature [[format("hex"), comment("0xAA55")]];
    auto total_sectors = total_sectors_16 != 0 ? total_sectors_16 : total_sectors_32;
    auto sectors_per_fat = sectors_per_fat_16 != 0 ? sectors_per_fat_16 : sectors_per_fat_32;
    auto root_dir_sectors = bytes_per_sector == 0 ? 0
        : (root_entry_count * 32 + bytes_per_sector - 1) / bytes_per_sector;
    auto first_data_sector = reserved_sectors + fat_count * sectors_per_fat + root_dir_sectors;
    auto cluster_count = sectors_per_cluster == 0 || total_sectors < first_data_sector ? 0
        : (total_sectors - first_data_sector) / sectors_per_cluster;
    auto fat_bits = sectors_per_fat_16 == 0 ? 32
        : (cluster_count < 4085 ? 12 : (cluster_count < 65525 ? 16 : 32));
} [[color("2E86C1")]];

struct FsInfo {
    u32 lead_signature [[format("hex"), comment("0x41615252")]];
    padding[480];
    u32 struct_signature [[format("hex"), comment("0x61417272")]];
    u32 free_cluster_count [[comment("0xFFFFFFFF = unknown")]];
    u32 next_free_cluster;
    padding[12];
    u32 trail_signature [[format("hex"), comment("0xAA550000")]];
};

struct DirEntry {
    if (std::mem::read_unsigned($ + 11, 1) == 0x0F) {
        // VFAT long file name fragment
        u8 sequence [[format("hex"), comment("0x40 marks the last fragment")]];
        char16 name_part1[5];
        u8 attributes [[format("hex")]];
        u8 entry_type;
        u8 short_name_checksum [[format("hex")]];
        char16 name_part2[6];
        u16 first_cluster;
        char16 name_part3[2];
    } else {
        char name[8];
        char extension[3];
        DirAttributes attributes;
        u8 nt_reserved;
        u8 creation_time_tenths;
        u16 creation_time [[format("dos_time_text")]];
        u16 creation_date [[format("dos_date_text")]];
        u16 access_date [[format("dos_date_text")]];
        u16 first_cluster_high;
        u16 write_time [[format("dos_time_text")]];
        u16 write_date [[format("dos_date_text")]];
        u16 first_cluster_low;
        u32 file_size;
        auto first_cluster = first_cluster_high << 16 | first_cluster_low;
        auto deleted = std::mem::read_unsigned(addressof(name), 1) == 0xE5;
    }
};

BootSector boot @ 0;
u64 data_size = std::mem::size();
u64 bps = boot.bytes_per_sector;
assert(bps != 0 && boot.sectors_per_cluster != 0, "not a FAT boot sector: zero geometry");

if (boot.fat_bits == 32 && boot.fs_info_sector != 0 && (boot.fs_info_sector + 1) * bps <= data_size)
    FsInfo fs_info @ boot.fs_info_sector * bps;

u64 fat_start = boot.reserved_sectors * bps;
u64 fat_bytes = boot.sectors_per_fat * bps;
if (fat_bytes != 0 && fat_start + fat_bytes <= data_size) {
    if (boot.fat_bits == 32) u32 fat[fat_bytes / 4] @ fat_start [[comment("first FAT, 28-bit entries")]];
    else if (boot.fat_bits == 16) u16 fat[fat_bytes / 2] @ fat_start [[comment("first FAT")]];
    else u8 fat[fat_bytes] @ fat_start [[comment("first FAT, packed 12-bit entries")]];
}

u64 root_start = (boot.reserved_sectors + boot.fat_count * boot.sectors_per_fat) * bps;
u64 root_end = root_start + boot.root_dir_sectors * bps;
if (boot.fat_bits == 32) {
    root_start = boot.first_data_sector * bps
        + (boot.root_cluster - 2) * boot.sectors_per_cluster * bps;
    root_end = root_start + boot.sectors_per_cluster * bps;
}
if (root_end > data_size) root_end = data_size;
if (root_start + 32 <= root_end)
    DirEntry root_directory[while($ + 32 <= root_end && std::mem::read_unsigned($, 1) != 0)]
        @ root_start;
