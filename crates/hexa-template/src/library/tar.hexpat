#pragma description TAR archive (ustar / GNU / pax)

enum TypeFlag : u8 {
    RegularOld = 0,
    Regular = '0',
    HardLink = '1',
    SymbolicLink = '2',
    CharacterDevice = '3',
    BlockDevice = '4',
    Directory = '5',
    Fifo = '6',
    Contiguous = '7',
    PaxGlobalHeader = 'g',
    PaxExtendedHeader = 'x',
    GnuDumpDir = 'D',
    GnuLongLinkName = 'K',
    GnuLongName = 'L',
    GnuMultiVolume = 'M',
    GnuSparse = 'S',
    GnuVolumeHeader = 'V'
};

// Parses an octal number field (or GNU base-256 when the high bit is set).
fn tar_number(u64 pos, u64 length) {
    u8 first = std::mem::read_unsigned(pos, 1);
    u128 value = 0;
    if ((first & 0x80) != 0) {
        value = first & 0x7F;
        for (u64 i = 1, i < length, i += 1) value = value << 8 | std::mem::read_unsigned(pos + i, 1);
        return value;
    }
    for (u64 i = 0, i < length, i += 1) {
        u8 c = std::mem::read_unsigned(pos + i, 1);
        if (c >= '0' && c <= '7') value = value * 8 + (c - '0');
        else if (c != ' ' || value != 0) break;
    }
    return value;
};

// Header checksum: byte sum with the checksum field counted as spaces.
fn tar_header_sum(u64 pos) {
    u64 sum = 0;
    for (u64 i = 0, i < 512, i += 8) {
        u64 word = std::mem::read_unsigned(pos + i, 8);
        for (u8 j = 0, j < 8, j += 1) sum += (word >> (j * 8)) & 0xFF;
    }
    for (u64 k = 148, k < 156, k += 1) sum = sum - std::mem::read_unsigned(pos + k, 1) + ' ';
    return sum;
};

// Value of an octal text field (as shown in the tree).
fn tar_octal(str text) {
    u128 value = 0;
    for (u64 i = 0, i < std::string::length(text), i += 1) {
        u8 c = text[i];
        if (c >= '0' && c <= '7') value = value * 8 + (c - '0');
        else if (c != ' ' || value != 0) break;
    }
    return value;
};

fn tar_mode_text(str text) {
    return std::format("{:04o}", tar_octal(text) & 0xFFF);
};

fn tar_number_text(str text) {
    return tar_octal(text);
};

fn tar_time_text(str text) {
    return unix_time_text(tar_octal(text));
};

struct Header {
    char name[100] [[color("E67E22")]];
    char mode[8] [[format("tar_mode_text")]];
    char uid[8] [[format("tar_number_text")]];
    char gid[8] [[format("tar_number_text")]];
    char size[12] [[format("tar_number_text")]];
    char modification_time[12] [[format("tar_time_text")]];
    char checksum[8] [[format("tar_number_text")]];
    TypeFlag type_flag;
    char link_name[100];
    char magic[6] [[comment("ustar\\0 (POSIX) or ustar\\x20 (GNU)")]];
    char version[2];
    char user_name[32];
    char group_name[32];
    char device_major[8];
    char device_minor[8];
    char prefix[155];
    padding[12];
    auto file_size = tar_number(addressof(size), 12);
    auto checksum_valid = tar_header_sum(addressof(this)) == tar_number(addressof(checksum), 8);
} [[color("2E86C1")]];

struct Entry {
    Header header;
    u64 length = header.file_size;
    if (header.type_flag == TypeFlag::Directory || header.type_flag == TypeFlag::SymbolicLink
            || header.type_flag == TypeFlag::HardLink)
        length = 0;
    if (length > std::mem::size() - $) length = std::mem::size() - $;
    match (header.type_flag) {
        (TypeFlag::PaxGlobalHeader | TypeFlag::PaxExtendedHeader | TypeFlag::GnuLongName
                | TypeFlag::GnuLongLinkName):
            char text[length];
        (_): u8 data[length] [[color("27AE60")]];
    }
    u64 pad = (512 - length % 512) % 512;
    if (pad > std::mem::size() - $) pad = std::mem::size() - $;
    padding[pad];
} [[format("tar_entry_text")]];

fn tar_entry_text(auto e) {
    return e.header.name;
};

Entry entries[while($ + 512 <= std::mem::size() && std::mem::read_unsigned($, 1) != 0)];
if ($ < std::mem::size()) u8 end_of_archive[std::mem::size() - $] [[comment("zero blocks")]];
