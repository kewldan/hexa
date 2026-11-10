#pragma description WebAssembly binary module (sections with LEB128 sizes)
#pragma endian little

enum SectionId : u8 {
    Custom = 0,
    Type = 1,
    Import = 2,
    Function = 3,
    Table = 4,
    Memory = 5,
    Global = 6,
    Export = 7,
    Start = 8,
    Element = 9,
    Code = 10,
    Data = 11,
    DataCount = 12,
    Tag = 13
};

enum ExportKind : u8 {
    Function = 0,
    Table = 1,
    Memory = 2,
    Global = 3,
    Tag = 4
};

// Value of the unsigned LEB128 number stored in `n` bytes at `at`.
fn wasm_leb_value(u64 at, u64 n) {
    u64 v = 0;
    u64 shift = 0;
    for (u64 i = 0, i < n, i += 1) {
        v |= (std::mem::read_unsigned(at + i, 1) & 0x7F) << shift;
        shift += 7;
    }
    return v;
};

struct VarUint {
    u8 bytes[until((_ & 0x80) == 0)] [[hidden]];
    auto value = wasm_leb_value(addressof(bytes), sizeof(bytes));
} [[format("wasm_leb_text")]];

fn wasm_leb_text(auto v) {
    return std::format("{}", v.value);
};

struct Name {
    VarUint length;
    char text[length.value];
} [[format("wasm_name_text")]];

fn wasm_name_text(auto n) {
    return n.text;
};

struct Export {
    Name name;
    ExportKind kind;
    VarUint index;
} [[format("wasm_export_text")]];

fn wasm_export_text(auto e) {
    return e.name.text;
};

struct Section {
    SectionId id [[color("2E86C1")]];
    VarUint size;
    u64 end = $ + size.value;
    assert(end <= std::mem::size(), "section extends past the end of the module");
    if (id == SectionId::Custom) {
        Name name;
    } else if (id == SectionId::Start) {
        VarUint function_index;
    } else if (id == SectionId::Export) {
        VarUint count;
        Export exports[count.value];
    } else {
        VarUint count [[comment("number of entries")]];
    }
    if ($ < end) u8 contents[end - $];
} [[format("wasm_section_text")]];

fn wasm_section_text(auto s) {
    if (s.id == SectionId::Custom) return std::format("custom \"{}\"", s.name.text);
    return std::format("{} ({} bytes)", s.id, s.size.value);
};

struct Module {
    char magic[4] [[comment("\\0asm")]];
    u32 version;
    Section sections[];
};

assert(std::mem::read_unsigned(0, 4, "big") == 0x0061736D, "not a WebAssembly module: bad magic");
Module module @ 0;
