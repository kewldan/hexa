#pragma description ZIP archive: local file headers, central directory, ZIP64 and end record
#pragma endian little

enum CompressionMethod : u16 {
    Stored = 0,
    Shrunk = 1,
    Reduced1 = 2,
    Reduced2 = 3,
    Reduced3 = 4,
    Reduced4 = 5,
    Imploded = 6,
    Deflated = 8,
    Deflate64 = 9,
    PKWareImplode = 10,
    BZIP2 = 12,
    LZMA = 14,
    IBMTerse = 18,
    LZ77 = 19,
    Zstandard = 93,
    MP3 = 94,
    XZ = 95,
    JPEG = 96,
    WavPack = 97,
    PPMd = 98,
    AES = 99
};

enum HostSystem : u8 {
    MSDOS = 0,
    Amiga = 1,
    OpenVMS = 2,
    Unix = 3,
    VMCMS = 4,
    AtariST = 5,
    OS2HPFS = 6,
    Macintosh = 7,
    ZSystem = 8,
    CPM = 9,
    NTFS = 10,
    MVS = 11,
    VSE = 12,
    AcornRisc = 13,
    VFAT = 14,
    AlternateMVS = 15,
    BeOS = 16,
    Tandem = 17,
    OS400 = 18,
    OSX = 19
};

enum ExtraId : u16 {
    Zip64 = 0x0001,
    AVInfo = 0x0007,
    OS2 = 0x0009,
    NTFS = 0x000A,
    OpenVMS = 0x000C,
    Unix = 0x000D,
    PatchDescriptor = 0x000F,
    StrongEncryption = 0x0017,
    ExtendedTimestamp = 0x5455,
    InfoZipUnix1 = 0x5855,
    InfoZipUnicodeComment = 0x6375,
    InfoZipUnicodePath = 0x7075,
    InfoZipUnix = 0x7875,
    JarMarker = 0xCAFE,
    AndroidAlignment = 0xD935,
    AES = 0x9901
};

bitfield GeneralPurposeFlags {
    bool encrypted : 1;
    compression_option : 2;
    bool data_descriptor : 1 [[comment("sizes and CRC follow the data")]];
    bool enhanced_deflation : 1;
    bool patched_data : 1;
    bool strong_encryption : 1;
    unused : 4;
    bool utf8_names : 1;
    reserved : 1;
    bool masked_local_header : 1;
    reserved2 : 2;
};

struct Version {
    u8 version [[comment("major * 10 + minor")]];
    HostSystem host;
} [[format("zip_version_text")]];

fn zip_version_text(auto v) {
    return std::format("{}.{}", v.version / 10, v.version % 10);
};

struct ExtraField {
    ExtraId id;
    u16 size;
    if (id == ExtraId::Zip64) {
        // Only the values whose 32-bit fields are 0xFFFFFFFF are present.
        u8 values[size] [[comment("ZIP64: uncompressed size, compressed size, header offset, disk")]];
    } else if (id == ExtraId::ExtendedTimestamp && size >= 5) {
        u8 timestamp_flags;
        u32 modification_time [[format("unix_time_text")]];
        if (size > 5) u8 more[size - 5];
    } else {
        u8 data[size];
    }
};

fn zip_end() {
    return std::mem::base_address() + std::mem::size();
};

// Scans the extra fields at `pos` for a ZIP64 record; returns the
// compressed size from it, or `fallback`.
fn zip64_compressed_size(u64 pos, u64 length, u32 uncompressed, u32 fallback) {
    u64 end = pos + length;
    while (pos + 4 <= end && pos + 4 <= zip_end()) {
        u16 id = std::mem::read_unsigned(pos, 2);
        u16 size = std::mem::read_unsigned(pos + 2, 2);
        if (id == 1) {
            u64 field = pos + 4;
            if (uncompressed == 0xFFFFFFFF) field += 8;
            if (fallback == 0xFFFFFFFF && field + 8 <= pos + 4 + size)
                return std::mem::read_unsigned(field, 8);
            return fallback;
        }
        pos += 4 + size;
    }
    return fallback;
};

// Offset of the next `PK\x07\x08` data descriptor at or after `from`, or -1.
fn zip_find_descriptor(u64 from) {
    s64 occurrence = 0;
    while (true) {
        s64 at = std::mem::find_sequence(occurrence, 'P', 'K', 0x07, 0x08);
        if (at < 0 || at >= from) return at;
        occurrence += 1;
    }
};

// Start of the file data behind the local header at `at`.
fn zip_local_data_start(u64 at) {
    if (at + 30 > zip_end()) return 0;
    return at + 30 + std::mem::read_unsigned(at + 26, 2) + std::mem::read_unsigned(at + 28, 2);
};

struct DataDescriptor {
    if (std::mem::read_unsigned($, 4) == 0x08074B50) u32 signature [[format("hex")]];
    u32 crc32 [[format("hex")]];
    if (zip64_descriptor) {
        u64 compressed_size;
        u64 uncompressed_size;
    } else {
        u32 compressed_size;
        u32 uncompressed_size;
    }
};

struct LocalFileHeader {
    u32 signature [[format("hex"), comment("PK\\x03\\x04")]];
    Version version_needed;
    GeneralPurposeFlags flags;
    CompressionMethod compression;
    u16 modification_time [[format("dos_time_text")]];
    u16 modification_date [[format("dos_date_text")]];
    u64 data_start = zip_local_data_start(addressof(this));
    u64 stored_size = std::mem::read_unsigned(addressof(this) + 18, 4);
    bool range_known = !flags.data_descriptor && data_start != 0 && stored_size != 0xFFFFFFFF
        && data_start + stored_size <= zip_end();
    if (range_known && compression == CompressionMethod::Stored)
        u32 crc32 [[format("hex"), checksum("crc32", data_start, data_start + stored_size)]];
    else if (range_known && compression == CompressionMethod::Deflated)
        u32 crc32 [[format("hex"), checksum("crc32_inflate", data_start, data_start + stored_size),
            comment("CRC-32 of the inflated data")]];
    else
        u32 crc32 [[format("hex")]];
    u32 compressed_size;
    u32 uncompressed_size;
    u16 file_name_length;
    u16 extra_field_length;
    char file_name[file_name_length] [[color("E67E22")]];
    ExtraField extra_fields[] [[size(extra_field_length)]];
    u64 data_size = zip64_compressed_size(addressof(extra_fields), extra_field_length,
        uncompressed_size, compressed_size);
    if (flags.data_descriptor && data_size == 0) {
        s64 descriptor = zip_find_descriptor($);
        data_size = descriptor < 0 ? 0 : descriptor - $;
    }
    zip64_descriptor = compressed_size == 0xFFFFFFFF || uncompressed_size == 0xFFFFFFFF;
    if (data_size > zip_end() - $) data_size = zip_end() - $;
    u8 data[data_size] [[color("27AE60")]];
    if (flags.data_descriptor) DataDescriptor data_descriptor;
} [[format("zip_entry_text")]];

fn zip_entry_text(auto e) {
    return e.file_name;
};

struct CentralDirectoryEntry {
    u32 signature [[format("hex"), comment("PK\\x01\\x02")]];
    Version version_made_by;
    Version version_needed;
    GeneralPurposeFlags flags;
    CompressionMethod compression;
    u16 modification_time [[format("dos_time_text")]];
    u16 modification_date [[format("dos_date_text")]];
    u64 data_start = zip_local_data_start(std::mem::read_unsigned(addressof(this) + 42, 4));
    u64 stored_size = std::mem::read_unsigned(addressof(this) + 20, 4);
    bool range_known = data_start != 0 && stored_size != 0xFFFFFFFF
        && data_start + stored_size <= zip_end();
    if (range_known && compression == CompressionMethod::Stored)
        u32 crc32 [[format("hex"), checksum("crc32", data_start, data_start + stored_size)]];
    else if (range_known && compression == CompressionMethod::Deflated)
        u32 crc32 [[format("hex"), checksum("crc32_inflate", data_start, data_start + stored_size),
            comment("CRC-32 of the inflated data")]];
    else
        u32 crc32 [[format("hex")]];
    u32 compressed_size;
    u32 uncompressed_size;
    u16 file_name_length;
    u16 extra_field_length;
    u16 file_comment_length;
    u16 disk_number_start;
    u16 internal_attributes [[format("hex"), comment("bit 0: text file")]];
    u32 external_attributes [[format("hex"), comment("Unix mode in the high 16 bits")]];
    u32 local_header_offset [[format("hex")]];
    char file_name[file_name_length] [[color("E67E22")]];
    ExtraField extra_fields[] [[size(extra_field_length)]];
    char file_comment[file_comment_length];
} [[format("zip_entry_text")]];

struct Zip64EndLocator {
    u32 signature [[format("hex"), comment("PK\\x06\\x07")]];
    u32 eocd_disk;
    u64 eocd_offset [[format("hex")]];
    u32 total_disks;
};

struct Zip64EndOfCentralDirectory {
    u32 signature [[format("hex"), comment("PK\\x06\\x06")]];
    u64 record_size;
    Version version_made_by;
    Version version_needed;
    u32 disk_number;
    u32 central_directory_disk;
    u64 disk_entries;
    u64 total_entries;
    u64 central_directory_size;
    u64 central_directory_offset [[format("hex")]];
    if (record_size > 44) u8 extensible_data[record_size - 44];
};

struct EndOfCentralDirectory {
    u32 signature [[format("hex"), comment("PK\\x05\\x06")]];
    u16 disk_number;
    u16 central_directory_disk;
    u16 disk_entries;
    u16 total_entries;
    u32 central_directory_size;
    u32 central_directory_offset [[format("hex")]];
    u16 comment_length;
    char comment[comment_length];
} [[color("2E86C1")]];

// Offset of the last end-of-central-directory signature, or -1.
fn zip_find_eocd() {
    s64 last = -1;
    s64 occurrence = 0;
    while (true) {
        s64 at = std::mem::find_sequence(occurrence, 'P', 'K', 0x05, 0x06);
        if (at < 0) return last;
        if (at + 22 <= zip_end()) last = at;
        occurrence += 1;
    }
};

bool zip64_descriptor = false;
LocalFileHeader files[while($ + 30 <= zip_end() && std::mem::read_unsigned($, 4) == 0x04034B50)];

s64 eocd_at = zip_find_eocd();
if (eocd_at >= 0) {
    EndOfCentralDirectory end_of_central_directory @ eocd_at;
    u64 entries = end_of_central_directory.total_entries;
    u64 directory_at = end_of_central_directory.central_directory_offset;
    if (eocd_at >= 20 && std::mem::read_unsigned(eocd_at - 20, 4) == 0x07064B50) {
        Zip64EndLocator zip64_locator @ eocd_at - 20;
        if (zip64_locator.eocd_offset + 56 <= zip_end()
                && std::mem::read_unsigned(zip64_locator.eocd_offset, 4) == 0x06064B50) {
            Zip64EndOfCentralDirectory zip64_end_of_central_directory @ zip64_locator.eocd_offset;
            entries = zip64_end_of_central_directory.total_entries;
            directory_at = zip64_end_of_central_directory.central_directory_offset;
        }
    }
    if (entries > 0 && directory_at + 46 <= zip_end()
            && std::mem::read_unsigned(directory_at, 4) == 0x02014B50)
        CentralDirectoryEntry central_directory[entries] @ directory_at;
}
