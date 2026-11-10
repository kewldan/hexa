#pragma description ELF executable / object / shared library (32/64-bit, both endians)

enum ElfClass : u8 {
    None = 0,
    ELF32 = 1,
    ELF64 = 2
};

enum ElfData : u8 {
    None = 0,
    LittleEndian = 1,
    BigEndian = 2
};

enum OsAbi : u8 {
    SystemV = 0,
    HPUX = 1,
    NetBSD = 2,
    Linux = 3,
    GNUHurd = 4,
    Solaris = 6,
    AIX = 7,
    IRIX = 8,
    FreeBSD = 9,
    Tru64 = 10,
    Modesto = 11,
    OpenBSD = 12,
    OpenVMS = 13,
    NonStopKernel = 14,
    AROS = 15,
    FenixOS = 16,
    CloudABI = 17,
    OpenVOS = 18,
    ARMEABI = 64,
    ARM = 97,
    Standalone = 255
};

enum ElfType : u16 {
    None = 0,
    Relocatable = 1,
    Executable = 2,
    SharedObject = 3,
    Core = 4
};

enum Machine : u16 {
    None = 0,
    M32 = 1,
    SPARC = 2,
    X86 = 3,
    M68K = 4,
    M88K = 5,
    IAMCU = 6,
    I860 = 7,
    MIPS = 8,
    S370 = 9,
    MIPS_RS3_LE = 10,
    PARISC = 15,
    SPARC32PLUS = 18,
    PPC = 20,
    PPC64 = 21,
    S390 = 22,
    ARM = 40,
    SuperH = 42,
    SPARCV9 = 43,
    IA64 = 50,
    X86_64 = 62,
    AVR = 83,
    MSP430 = 105,
    Xtensa = 94,
    AArch64 = 183,
    TileGX = 191,
    CUDA = 190,
    AMDGPU = 224,
    RISCV = 243,
    BPF = 247,
    CSKY = 252,
    LoongArch = 258
};

enum SegmentType : u32 {
    Null = 0,
    Load = 1,
    Dynamic = 2,
    Interp = 3,
    Note = 4,
    SharedLib = 5,
    ProgramHeader = 6,
    TLS = 7,
    GnuEhFrame = 0x6474E550,
    GnuStack = 0x6474E551,
    GnuRelro = 0x6474E552,
    GnuProperty = 0x6474E553,
    OsSpecific = 0x60000000 ... 0x6FFFFFFF,
    ArmExidx = 0x70000001,
    ProcessorSpecific = 0x70000000 ... 0x7FFFFFFF
};

enum SegmentFlags : u32 {
    Execute = 0x1,
    Write = 0x2,
    Read = 0x4
} [[flags]];

enum SectionType : u32 {
    Null = 0,
    ProgBits = 1,
    SymTab = 2,
    StrTab = 3,
    Rela = 4,
    Hash = 5,
    Dynamic = 6,
    Note = 7,
    NoBits = 8,
    Rel = 9,
    ShLib = 10,
    DynSym = 11,
    InitArray = 14,
    FiniArray = 15,
    PreinitArray = 16,
    Group = 17,
    SymTabShndx = 18,
    Relr = 19,
    GnuAttributes = 0x6FFFFFF5,
    GnuHash = 0x6FFFFFF6,
    GnuLibList = 0x6FFFFFF7,
    GnuVerDef = 0x6FFFFFFD,
    GnuVerNeed = 0x6FFFFFFE,
    GnuVerSym = 0x6FFFFFFF,
    ArmExidx = 0x70000001,
    ProcessorAttributes = 0x70000003
};

// `Word` is bound by the enclosing `SectionHeader<Word>` (u32 or u64).
enum SectionFlags : Word {
    Write = 0x1,
    Alloc = 0x2,
    ExecInstr = 0x4,
    Merge = 0x10,
    Strings = 0x20,
    InfoLink = 0x40,
    LinkOrder = 0x80,
    OsNonConforming = 0x100,
    Group = 0x200,
    TLS = 0x400,
    Compressed = 0x800,
    Exclude = 0x80000000
} [[flags]];

enum SymbolBinding : u8 {
    Local = 0,
    Global = 1,
    Weak = 2,
    GnuUnique = 10
};

enum SymbolType : u8 {
    NoType = 0,
    Object = 1,
    Func = 2,
    Section = 3,
    File = 4,
    Common = 5,
    TLS = 6,
    GnuIFunc = 10
};

enum SymbolVisibility : u8 {
    Default = 0,
    Internal = 1,
    Hidden = 2,
    Protected = 3
};

// `Word` is bound by the enclosing `Dynamic<Word>` (u32 or u64).
enum DynamicTag : Word {
    Null = 0, Needed = 1, PltRelSz = 2, PltGot = 3, Hash = 4, StrTab = 5, SymTab = 6,
    Rela = 7, RelaSz = 8, RelaEnt = 9, StrSz = 10, SymEnt = 11, Init = 12, Fini = 13,
    SoName = 14, RPath = 15, Symbolic = 16, Rel = 17, RelSz = 18, RelEnt = 19, PltRel = 20,
    Debug = 21, TextRel = 22, JmpRel = 23, BindNow = 24, InitArray = 25, FiniArray = 26,
    InitArraySz = 27, FiniArraySz = 28, RunPath = 29, Flags = 30, PreinitArray = 32,
    PreinitArraySz = 33, SymTabShndx = 34, RelrSz = 35, Relr = 36, RelrEnt = 37,
    GnuHash = 0x6FFFFEF5, VerSym = 0x6FFFFFF0, RelaCount = 0x6FFFFFF9, RelCount = 0x6FFFFFFA,
    Flags1 = 0x6FFFFFFB, VerDef = 0x6FFFFFFC, VerDefNum = 0x6FFFFFFD, VerNeed = 0x6FFFFFFE,
    VerNeedNum = 0x6FFFFFFF
};

bitfield SymbolInfo {
    SymbolType type : 4;
    SymbolBinding binding : 4;
};

struct Ident {
    u8 magic[4] [[comment("\\x7F E L F"), color("808080")]];
    ElfClass class;
    ElfData data;
    u8 version;
    OsAbi os_abi;
    u8 abi_version;
    padding[7];
};

fn elf_end() {
    return std::mem::base_address() + std::mem::size();
};

fn elf_fits(u64 offset, u64 size) {
    return offset <= elf_end() && size <= elf_end() - offset;
};

// NUL-terminated string from a string table (bounded read).
fn elf_string(u64 table, u64 index) {
    if (table == 0 || table + index >= elf_end()) return "";
    return ReadString(table + index, 256);
};

struct Symbol32 {
    u32 name_index;
    u32 value [[format("hex")]];
    u32 size;
    le SymbolInfo info;
    SymbolVisibility visibility;
    u16 section_index;
    auto name = elf_string(parent.linked_table_offset, name_index);
} [[format("elf_symbol_text")]];

struct Symbol64 {
    u32 name_index;
    le SymbolInfo info;
    SymbolVisibility visibility;
    u16 section_index;
    u64 value [[format("hex")]];
    u64 size;
    auto name = elf_string(parent.linked_table_offset, name_index);
} [[format("elf_symbol_text")]];

fn elf_symbol_text(auto s) {
    return s.name;
};

struct Dynamic<Word> {
    DynamicTag tag;
    Word value [[format("hex")]];
};

struct ProgramHeader32 {
    SegmentType type [[color("E67E22")]];
    u32 offset [[format("hex")]];
    u32 virtual_address [[format("hex")]];
    u32 physical_address [[format("hex")]];
    u32 file_size;
    u32 memory_size;
    SegmentFlags flags;
    u32 alignment;
    if (type == SegmentType::Interp && elf_fits(offset, file_size))
        char interpreter[file_size] @ offset;
};

struct ProgramHeader64 {
    SegmentType type [[color("E67E22")]];
    SegmentFlags flags;
    u64 offset [[format("hex")]];
    u64 virtual_address [[format("hex")]];
    u64 physical_address [[format("hex")]];
    u64 file_size;
    u64 memory_size;
    u64 alignment;
    if (type == SegmentType::Interp && elf_fits(offset, file_size))
        char interpreter[file_size] @ offset;
};

// Section header: `Word` is u32 (ELF32) or u64 (ELF64).
struct SectionHeader<Word> {
    u32 name_index;
    SectionType type [[color("E67E22")]];
    SectionFlags flags;
    Word address [[format("hex")]];
    Word offset [[format("hex")]];
    Word size;
    u32 link;
    u32 info;
    Word alignment;
    Word entry_size;
    auto name = elf_string(elf.section_names_offset, name_index);
    auto linked_table_offset = elf_section_offset(link);
    bool has_data = type != SectionType::NoBits && type != SectionType::Null && size != 0
        && elf_fits(offset, size);
    if (has_data && (type == SectionType::SymTab || type == SectionType::DynSym)) {
        if (sizeof(Word) == 8) Symbol64 symbols[size / 24] @ offset;
        else Symbol32 symbols[size / 16] @ offset;
    } else if (has_data && type == SectionType::Dynamic) {
        Dynamic<Word> entries[until(_.tag == 0 || $ >= offset + size)] @ offset;
    } else if (has_data && type == SectionType::StrTab) {
        char strings[size] @ offset;
    } else if (has_data) {
        u8 data[size] @ offset;
    }
} [[format("elf_section_text")]];

fn elf_section_text(auto s) {
    return s.name;
};

// File offset of section `index`, read straight from the section header table.
fn elf_section_offset(u32 index) {
    if (index == 0 || index >= elf_section_count) return 0;
    u64 at = elf_section_table + index * elf_section_entry_size;
    if (elf_is64) return std::mem::read_unsigned(at + 24, 8);
    return std::mem::read_unsigned(at + 16, 4);
};

struct ElfHeader<Word> {
    Ident ident;
    ElfType type;
    Machine machine;
    u32 version;
    Word entry [[format("hex")]];
    Word program_header_offset [[format("hex")]];
    Word section_header_offset [[format("hex")]];
    u32 flags [[format("hex")]];
    u16 header_size;
    u16 program_header_entry_size;
    u16 program_header_count;
    u16 section_header_entry_size;
    u16 section_header_count;
    u16 section_names_index;
};

struct ElfFile<Word> {
    ElfHeader<Word> header [[color("2E86C1")]];
    elf_section_table = header.section_header_offset;
    elf_section_count = header.section_header_count;
    elf_section_entry_size = header.section_header_entry_size;
    auto section_names_offset = elf_section_offset(header.section_names_index);
    u64 ph_bytes = header.program_header_count * header.program_header_entry_size;
    if (header.program_header_count > 0 && elf_fits(header.program_header_offset, ph_bytes)) {
        if (sizeof(Word) == 8)
            ProgramHeader64 program_headers[header.program_header_count]
                @ header.program_header_offset;
        else
            ProgramHeader32 program_headers[header.program_header_count]
                @ header.program_header_offset;
    }
    u64 sh_bytes = header.section_header_count * header.section_header_entry_size;
    if (header.section_header_count > 0 && elf_fits(header.section_header_offset, sh_bytes))
        SectionHeader<Word> section_headers[header.section_header_count]
            @ header.section_header_offset;
};

assert(std::mem::size() >= 16 && std::mem::read_unsigned(0, 4, "big") == 0x7F454C46,
    "not an ELF file: bad magic");
u64 elf_section_table = 0;
u64 elf_section_count = 0;
u64 elf_section_entry_size = 0;
bool elf_is64 = std::mem::read_unsigned(4, 1) == 2;
bool elf_big = std::mem::read_unsigned(5, 1) == 2;
if (elf_big) {
    if (elf_is64) be ElfFile<u64> elf @ 0;
    else be ElfFile<u32> elf @ 0;
} else {
    if (elf_is64) le ElfFile<u64> elf @ 0;
    else le ElfFile<u32> elf @ 0;
}
