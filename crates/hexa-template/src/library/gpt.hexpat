#pragma description GUID partition table with protective MBR

struct Guid {
    u32 data1;
    u16 data2;
    u16 data3;
    u8 data4[8];
} [[format("gpt_guid_text")]];

fn gpt_guid_text(auto g) {
    return std::format("{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3],
        g.data4[4], g.data4[5], g.data4[6], g.data4[7]);
};

fn gpt_type_name(auto g) {
    match (gpt_guid_text(g)) {
        ("00000000-0000-0000-0000-000000000000"): return "unused";
        ("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"): return "EFI system partition";
        ("024DEE41-33E7-11D3-9D69-0008C781F39F"): return "MBR partition scheme";
        ("21686148-6449-6E6F-744E-656564454649"): return "BIOS boot partition";
        ("E3C9E316-0B5C-4DB8-817D-F92DF00215AE"): return "Microsoft reserved";
        ("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"): return "Microsoft basic data";
        ("DE94BBA4-06D1-4D40-A16A-BFD50179D6AC"): return "Windows recovery environment";
        ("5808C8AA-7E8F-42E0-85D2-E1E90434CFB3"): return "Windows LDM metadata";
        ("AF9B60A0-1431-4F62-BC68-3311714A69AD"): return "Windows LDM data";
        ("0FC63DAF-8483-4772-8E79-3D69D8477DE4"): return "Linux filesystem";
        ("4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709"): return "Linux root (x86-64)";
        ("B921B045-1DF0-41C3-AF44-4C6F280D3FAE"): return "Linux root (AArch64)";
        ("933AC7E1-2EB4-4F13-B844-0E14E2AEF915"): return "Linux /home";
        ("0657FD6D-A4AB-43C4-84E5-0933C84B4F4F"): return "Linux swap";
        ("E6D6D379-F507-44C2-A23C-238F2A3DF928"): return "Linux LVM";
        ("A19D880F-05FC-4D3B-A006-743F0F84911E"): return "Linux RAID";
        ("BC13C2FF-59E6-4262-A352-B275FD6F7172"): return "Linux extended boot";
        ("48465300-0000-11AA-AA11-00306543ECAC"): return "Apple HFS+";
        ("7C3457EF-0000-11AA-AA11-00306543ECAC"): return "Apple APFS";
        ("426F6F74-0000-11AA-AA11-00306543ECAC"): return "Apple boot";
        ("516E7CB4-6ECF-11D6-8FF8-00022D09712B"): return "FreeBSD data";
        ("83BD6B9D-7F41-11DC-BE0B-001560B84F0F"): return "FreeBSD boot";
        ("6A898CC3-1DD2-11B2-99A6-080020736631"): return "Solaris / ZFS";
        ("3B8F8425-20E0-4F3B-907F-1A25A76F98E8"): return "Linux /srv";
        ("CA7D7CCB-63ED-4C53-861C-1742536059CC"): return "LUKS";
    }
    return "unknown";
};

bitfield GptAttributes {
    bool required_partition : 1;
    bool no_block_io_protocol : 1;
    bool legacy_bios_bootable : 1;
    reserved : 45;
    type_specific : 16 [[format("hex")]];
};

struct GptHeader {
    char signature[8] [[comment("EFI PART")]];
    u32 revision [[format("hex")]];
    u32 header_size;
    u32 header_crc32 [[format("hex"),
        comment("CRC-32 of the first header_size bytes with this field zeroed")]];
    u32 reserved;
    u64 current_lba;
    u64 backup_lba;
    u64 first_usable_lba;
    u64 last_usable_lba;
    Guid disk_guid;
    u64 partition_entry_lba;
    u32 partition_entry_count;
    u32 partition_entry_size;
    auto entries_start = partition_entry_lba * gpt_sector_size;
    auto entries_end = entries_start + partition_entry_count * partition_entry_size;
    u32 partition_entries_crc32 [[format("hex"),
        checksum("crc32", entries_start, entries_end),
        comment("CRC-32 of the partition entry array")]];
} [[color("2E86C1")]];

struct GptEntry {
    Guid type_guid [[comment_fn("gpt_type_name")]];
    Guid unique_guid;
    u64 first_lba;
    u64 last_lba;
    GptAttributes attributes;
    char16 name[36];
    if (gpt.partition_entry_size > 128) padding[gpt.partition_entry_size - 128];
} [[format("gpt_entry_text")]];

fn gpt_entry_text(auto e) {
    if (e.first_lba == 0 && e.last_lba == 0) return "unused";
    return std::format("{} \"{}\" LBA {}..{}", gpt_type_name(e.type_guid), e.name, e.first_lba,
        e.last_lba);
};

fn gpt_detect_sector_size() {
    u64 size = std::mem::size();
    if (size >= 520 && std::mem::read_string(512, 8) == "EFI PART") return 512;
    if (size >= 4104 && std::mem::read_string(4096, 8) == "EFI PART") return 4096;
    return 512;
};

u64 gpt_sector_size = gpt_detect_sector_size();
MasterBootRecord protective_mbr @ 0;
GptHeader gpt @ gpt_sector_size;
if (gpt.partition_entry_size >= 128 && gpt.entries_end <= std::mem::size())
    GptEntry partitions[gpt.partition_entry_count] @ gpt.entries_start;
if (gpt.backup_lba > gpt.current_lba && (gpt.backup_lba + 1) * gpt_sector_size <= std::mem::size())
    GptHeader backup_gpt @ gpt.backup_lba * gpt_sector_size;
