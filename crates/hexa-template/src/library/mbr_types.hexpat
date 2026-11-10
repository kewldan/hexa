// Master boot record types shared by the `mbr` and `gpt` templates.
#pragma endian little

enum PartitionType : u8 {
    Empty = 0x00,
    FAT12 = 0x01,
    XenixRoot = 0x02,
    FAT16Small = 0x04,
    Extended = 0x05,
    FAT16 = 0x06,
    NTFS_exFAT_HPFS = 0x07,
    FAT32 = 0x0B,
    FAT32_LBA = 0x0C,
    FAT16_LBA = 0x0E,
    Extended_LBA = 0x0F,
    HiddenNTFS = 0x17,
    WindowsRecovery = 0x27,
    Plan9 = 0x39,
    Minix = 0x81,
    LinuxSwap = 0x82,
    Linux = 0x83,
    LinuxExtended = 0x85,
    LinuxLVM = 0x8E,
    FreeBSD = 0xA5,
    OpenBSD = 0xA6,
    NeXTSTEP = 0xA7,
    DarwinUFS = 0xA8,
    NetBSD = 0xA9,
    DarwinBoot = 0xAB,
    HFS = 0xAF,
    Solaris = 0xBF,
    GPTProtective = 0xEE,
    EFISystem = 0xEF,
    LinuxRAID = 0xFD
};

enum BootIndicator : u8 {
    Inactive = 0x00,
    Active = 0x80
};

struct Chs {
    u8 head;
    u8 sector_cylinder_high;
    u8 cylinder_low;
    auto sector = sector_cylinder_high & 0x3F;
    auto cylinder = (sector_cylinder_high & 0xC0) << 2 | cylinder_low;
} [[format("mbr_chs_text")]];

fn mbr_chs_text(auto c) {
    return std::format("C/H/S {}/{}/{}", c.cylinder, c.head, c.sector);
};

struct PartitionEntry {
    BootIndicator status;
    Chs first_chs;
    PartitionType type [[color("E67E22")]];
    Chs last_chs;
    u32 first_lba;
    u32 sector_count;
    auto first_byte = first_lba * 512;
    auto size_bytes = sector_count * 512;
} [[format("mbr_partition_text")]];

fn mbr_partition_text(auto p) {
    if (p.type == 0) return "unused";
    return std::format("type 0x{:02X} at LBA {}, {} sectors", p.type, p.first_lba, p.sector_count);
};

struct MasterBootRecord {
    u8 bootstrap_code[440];
    u32 disk_signature [[format("hex")]];
    u16 copy_protected [[comment("0x5A5A = copy protected")]];
    PartitionEntry partitions[4];
    u16 boot_signature [[format("hex"), comment("0xAA55")]];
};

// Extended boot record: one logical partition plus a link to the next EBR.
struct ExtendedBootRecord<auto extended_start> {
    u8 unused[446];
    PartitionEntry logical_partition [[comment("first LBA relative to this EBR")]];
    PartitionEntry next_ebr [[comment("first LBA relative to the extended partition")]];
    padding[32];
    u16 boot_signature [[format("hex")]];
    u64 next_at = (extended_start + next_ebr.first_lba) * 512;
    if ((next_ebr.type == PartitionType::Extended || next_ebr.type == PartitionType::Extended_LBA)
            && next_ebr.first_lba != 0 && next_at + 512 <= std::mem::size())
        ExtendedBootRecord<extended_start> next @ next_at;
};
