#pragma description SQLite 3 database: header and b-tree page headers
#pragma endian big

enum FileFormat : u8 {
    Legacy = 1,
    WAL = 2
};

enum TextEncoding : u32 {
    UTF8 = 1,
    UTF16le = 2,
    UTF16be = 3
};

enum PageType : u8 {
    InteriorIndex = 0x02,
    InteriorTable = 0x05,
    LeafIndex = 0x0A,
    LeafTable = 0x0D
};

struct DatabaseHeader {
    char magic[16] [[comment("SQLite format 3\\0")]];
    u16 page_size_raw [[comment("1 means 65536")]];
    FileFormat write_version;
    FileFormat read_version;
    u8 reserved_space;
    u8 max_payload_fraction;
    u8 min_payload_fraction;
    u8 leaf_payload_fraction;
    u32 file_change_counter;
    u32 database_size_pages;
    u32 first_freelist_trunk_page;
    u32 freelist_page_count;
    u32 schema_cookie;
    u32 schema_format;
    u32 default_page_cache_size;
    u32 largest_root_btree_page [[comment("auto/incremental vacuum")]];
    TextEncoding text_encoding;
    u32 user_version;
    u32 incremental_vacuum;
    u32 application_id [[format("hex")]];
    padding[20];
    u32 version_valid_for;
    u32 sqlite_version_number;
    auto page_size = page_size_raw == 1 ? 65536 : page_size_raw;
} [[color("2E86C1")]];

fn sqlite_is_btree(u8 kind) {
    return kind == 0x02 || kind == 0x05 || kind == 0x0A || kind == 0x0D;
};

struct BTreePageHeader {
    PageType page_type [[color("E67E22")]];
    u16 first_freeblock;
    u16 cell_count;
    u16 cell_content_start [[comment("0 means 65536")]];
    u8 fragmented_free_bytes;
    if (page_type == PageType::InteriorIndex || page_type == PageType::InteriorTable)
        u32 right_most_pointer;
    u16 cell_pointers[cell_count];
};

struct Page {
    if (sqlite_is_btree(std::mem::read_unsigned($, 1))) BTreePageHeader btree;
    else u8 kind [[comment("not a b-tree page (overflow, freelist or pointer map)")]];
} [[size(header.page_size)]];

fn sqlite_page_count() {
    u64 by_size = std::mem::size() / header.page_size;
    if (header.database_size_pages != 0
            && header.version_valid_for == header.file_change_counter
            && header.database_size_pages < by_size)
        return header.database_size_pages;
    return by_size;
};

DatabaseHeader header @ 0;
BTreePageHeader first_page @ 100 [[comment("page 1: the schema table b-tree")]];
u64 page_count = sqlite_page_count();
if (page_count > 1) Page pages[page_count - 1] @ header.page_size;
