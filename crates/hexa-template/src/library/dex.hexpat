#pragma description Android DEX (Dalvik executable) header, string table and map list
#pragma endian little

enum MapType : u16 {
    HeaderItem = 0x0000,
    StringIdItem = 0x0001,
    TypeIdItem = 0x0002,
    ProtoIdItem = 0x0003,
    FieldIdItem = 0x0004,
    MethodIdItem = 0x0005,
    ClassDefItem = 0x0006,
    CallSiteIdItem = 0x0007,
    MethodHandleItem = 0x0008,
    MapList = 0x1000,
    TypeList = 0x1001,
    AnnotationSetRefList = 0x1002,
    AnnotationSetItem = 0x1003,
    ClassDataItem = 0x2000,
    CodeItem = 0x2001,
    StringDataItem = 0x2002,
    DebugInfoItem = 0x2003,
    AnnotationItem = 0x2004,
    EncodedArrayItem = 0x2005,
    AnnotationsDirectoryItem = 0x2006,
    HiddenapiClassDataItem = 0xF000
};

enum EndianTag : u32 {
    Little = 0x12345678,
    Reverse = 0x78563412
};

// Value of the unsigned LEB128 number stored in `n` bytes at `at`.
fn dex_uleb_value(u64 at, u64 n) {
    u64 v = 0;
    u64 shift = 0;
    for (u64 i = 0, i < n, i += 1) {
        v |= (std::mem::read_unsigned(at + i, 1) & 0x7F) << shift;
        shift += 7;
    }
    return v;
};

struct Uleb128 {
    u8 bytes[until((_ & 0x80) == 0)] [[hidden]];
    auto value = dex_uleb_value(addressof(bytes), sizeof(bytes));
} [[format("dex_uleb_text")]];

fn dex_uleb_text(auto u) {
    return std::format("{}", u.value);
};

struct StringData {
    Uleb128 utf16_size;
    string data [[comment("MUTF-8")]];
};

struct StringId {
    u32 string_data_off [[format("hex")]];
    if (string_data_off > 0 && string_data_off < std::mem::size())
        StringData string_data @ string_data_off;
} [[format("dex_string_text")]];

fn dex_string_text(auto s) {
    if (std::core::has_member(s, "string_data")) return s.string_data.data;
    return "";
};

struct MapItem {
    MapType type;
    u16 unused;
    u32 size;
    u32 offset [[format("hex")]];
};

struct MapList {
    u32 size;
    MapItem list[size];
};

struct Header {
    char magic[8] [[comment("dex\\n + version + \\0")]];
    u32 checksum [[format("hex"), checksum("adler32", 12, std::mem::read_unsigned(32, 4))]];
    u8 signature[20] [[checksum("sha1", 32, std::mem::read_unsigned(32, 4))]];
    u32 file_size;
    u32 header_size;
    EndianTag endian_tag;
    u32 link_size;
    u32 link_off [[format("hex")]];
    u32 map_off [[format("hex")]];
    u32 string_ids_size;
    u32 string_ids_off [[format("hex")]];
    u32 type_ids_size;
    u32 type_ids_off [[format("hex")]];
    u32 proto_ids_size;
    u32 proto_ids_off [[format("hex")]];
    u32 field_ids_size;
    u32 field_ids_off [[format("hex")]];
    u32 method_ids_size;
    u32 method_ids_off [[format("hex")]];
    u32 class_defs_size;
    u32 class_defs_off [[format("hex")]];
    u32 data_size;
    u32 data_off [[format("hex")]];
    auto version = std::string::substr(magic, 4, 3);
} [[color("2E86C1")]];

fn dex_fits(u64 off, u64 size) {
    return off > 0 && off + size <= std::mem::size();
};

assert(std::mem::size() >= 0x70 && std::mem::read_string(0, 4) == "dex\n",
    "not a DEX file: bad magic");
Header header @ 0;
if (dex_fits(header.string_ids_off, header.string_ids_size * 4))
    StringId string_ids[header.string_ids_size] @ header.string_ids_off;
if (dex_fits(header.type_ids_off, header.type_ids_size * 4))
    u32 type_ids[header.type_ids_size] @ header.type_ids_off [[comment("string_ids indices")]];
if (dex_fits(header.map_off, 4))
    MapList map @ header.map_off;
