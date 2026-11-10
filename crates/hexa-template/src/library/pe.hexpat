#pragma description Windows PE / COFF executable (EXE, DLL, SYS, EFI)
#pragma endian little

enum MachineType : u16 {
    Unknown = 0x0,
    I386 = 0x14C,
    R4000 = 0x166,
    WCEMIPSv2 = 0x169,
    SH3 = 0x1A2,
    SH4 = 0x1A6,
    ARM = 0x1C0,
    Thumb = 0x1C2,
    ARMNT = 0x1C4,
    PowerPC = 0x1F0,
    IA64 = 0x200,
    MIPS16 = 0x266,
    EBC = 0xEBC,
    RISCV32 = 0x5032,
    RISCV64 = 0x5064,
    LoongArch64 = 0x6264,
    AMD64 = 0x8664,
    ARM64EC = 0xA641,
    ARM64 = 0xAA64
};

enum FileCharacteristics : u16 {
    RelocsStripped = 0x1,
    ExecutableImage = 0x2,
    LineNumsStripped = 0x4,
    LocalSymsStripped = 0x8,
    AggressiveWsTrim = 0x10,
    LargeAddressAware = 0x20,
    BytesReversedLo = 0x80,
    Machine32Bit = 0x100,
    DebugStripped = 0x200,
    RemovableRunFromSwap = 0x400,
    NetRunFromSwap = 0x800,
    System = 0x1000,
    DLL = 0x2000,
    UpSystemOnly = 0x4000,
    BytesReversedHi = 0x8000
} [[flags]];

enum OptionalMagic : u16 {
    ROM = 0x107,
    PE32 = 0x10B,
    PE32Plus = 0x20B
};

enum Subsystem : u16 {
    Unknown = 0,
    Native = 1,
    WindowsGUI = 2,
    WindowsCUI = 3,
    OS2CUI = 5,
    POSIXCUI = 7,
    NativeWindows = 8,
    WindowsCEGUI = 9,
    EFIApplication = 10,
    EFIBootServiceDriver = 11,
    EFIRuntimeDriver = 12,
    EFIROM = 13,
    Xbox = 14,
    WindowsBootApplication = 16
};

enum DllCharacteristics : u16 {
    HighEntropyVA = 0x20,
    DynamicBase = 0x40,
    ForceIntegrity = 0x80,
    NXCompat = 0x100,
    NoIsolation = 0x200,
    NoSEH = 0x400,
    NoBind = 0x800,
    AppContainer = 0x1000,
    WDMDriver = 0x2000,
    GuardCF = 0x4000,
    TerminalServerAware = 0x8000
} [[flags]];

enum SectionCharacteristics : u32 {
    TypeNoPad = 0x8,
    ContainsCode = 0x20,
    InitializedData = 0x40,
    UninitializedData = 0x80,
    LinkInfo = 0x200,
    LinkRemove = 0x800,
    LinkComdat = 0x1000,
    GPRelative = 0x8000,
    ExtendedRelocations = 0x1000000,
    Discardable = 0x2000000,
    NotCached = 0x4000000,
    NotPaged = 0x8000000,
    Shared = 0x10000000,
    Execute = 0x20000000,
    Read = 0x40000000,
    Write = 0x80000000
} [[flags]];

fn pe_end() {
    return std::mem::base_address() + std::mem::size();
};

fn pe_fits(u64 offset, u64 size) {
    return offset <= pe_end() && size <= pe_end() - offset;
};

fn pe_directory_name(u32 index) {
    match (index) {
        (0): return "Export";
        (1): return "Import";
        (2): return "Resource";
        (3): return "Exception";
        (4): return "Security";
        (5): return "BaseRelocation";
        (6): return "Debug";
        (7): return "Architecture";
        (8): return "GlobalPtr";
        (9): return "TLS";
        (10): return "LoadConfig";
        (11): return "BoundImport";
        (12): return "IAT";
        (13): return "DelayImport";
        (14): return "CLRRuntimeHeader";
        (15): return "Reserved";
    }
    return "";
};

// Converts a relative virtual address to a file offset using the section table.
fn pe_rva_to_offset(u64 rva) {
    for (u32 i = 0, i < pe_section_count, i += 1) {
        u64 va = sections[i].virtual_address;
        u64 span = sections[i].virtual_size;
        if (span < sections[i].size_of_raw_data) span = sections[i].size_of_raw_data;
        if (rva >= va && rva < va + span) return rva - va + sections[i].pointer_to_raw_data;
    }
    return rva;
};

fn pe_string_at_rva(u64 rva) {
    u64 at = pe_rva_to_offset(rva);
    if (rva == 0 || at >= pe_end()) return "";
    return ReadString(at, 256);
};

struct DosHeader {
    char magic[2] [[comment("MZ")]];
    u16 bytes_on_last_page;
    u16 pages_in_file;
    u16 relocations;
    u16 header_paragraphs;
    u16 min_extra_paragraphs;
    u16 max_extra_paragraphs;
    u16 initial_ss [[format("hex")]];
    u16 initial_sp [[format("hex")]];
    u16 checksum [[format("hex")]];
    u16 initial_ip [[format("hex")]];
    u16 initial_cs [[format("hex")]];
    u16 relocation_table_offset [[format("hex")]];
    u16 overlay_number;
    u16 reserved1[4];
    u16 oem_id;
    u16 oem_info;
    u16 reserved2[10];
    u32 new_header_offset [[format("hex"), comment("e_lfanew")]];
} [[color("808080")]];

struct FileHeader {
    MachineType machine;
    u16 number_of_sections;
    u32 time_date_stamp [[format("unix_time_text")]];
    u32 pointer_to_symbol_table [[format("hex")]];
    u32 number_of_symbols;
    u16 size_of_optional_header;
    FileCharacteristics characteristics;
};

struct DataDirectory {
    u32 virtual_address [[format("hex")]];
    u32 size;
    auto name = pe_directory_name(std::core::array_index());
} [[format("pe_directory_text")]];

fn pe_directory_text(auto d) {
    return std::format("{} (RVA 0x{:X}, {} bytes)", d.name, d.virtual_address, d.size);
};

struct OptionalHeader {
    OptionalMagic magic;
    u8 major_linker_version;
    u8 minor_linker_version;
    u32 size_of_code;
    u32 size_of_initialized_data;
    u32 size_of_uninitialized_data;
    u32 address_of_entry_point [[format("hex")]];
    u32 base_of_code [[format("hex")]];
    if (magic == OptionalMagic::PE32Plus) {
        u64 image_base [[format("hex")]];
    } else {
        u32 base_of_data [[format("hex")]];
        u32 image_base [[format("hex")]];
    }
    u32 section_alignment [[format("hex")]];
    u32 file_alignment [[format("hex")]];
    u16 major_os_version;
    u16 minor_os_version;
    u16 major_image_version;
    u16 minor_image_version;
    u16 major_subsystem_version;
    u16 minor_subsystem_version;
    u32 win32_version_value;
    u32 size_of_image [[format("hex")]];
    u32 size_of_headers [[format("hex")]];
    u32 checksum [[format("hex"), comment("PE image checksum (not a CRC)")]];
    Subsystem subsystem;
    DllCharacteristics dll_characteristics;
    if (magic == OptionalMagic::PE32Plus) {
        u64 size_of_stack_reserve [[format("hex")]];
        u64 size_of_stack_commit [[format("hex")]];
        u64 size_of_heap_reserve [[format("hex")]];
        u64 size_of_heap_commit [[format("hex")]];
    } else {
        u32 size_of_stack_reserve [[format("hex")]];
        u32 size_of_stack_commit [[format("hex")]];
        u32 size_of_heap_reserve [[format("hex")]];
        u32 size_of_heap_commit [[format("hex")]];
    }
    u32 loader_flags;
    u32 number_of_rva_and_sizes;
    DataDirectory data_directories[number_of_rva_and_sizes > 16 ? 16 : number_of_rva_and_sizes];
};

struct NtHeaders {
    char signature[4] [[comment("PE\\0\\0")]];
    FileHeader file_header;
    if (file_header.size_of_optional_header > 0)
        OptionalHeader optional_header [[size(file_header.size_of_optional_header)]];
} [[color("2E86C1")]];

struct SectionHeader {
    char name[8] [[color("E67E22")]];
    u32 virtual_size [[format("hex")]];
    u32 virtual_address [[format("hex")]];
    u32 size_of_raw_data [[format("hex")]];
    u32 pointer_to_raw_data [[format("hex")]];
    u32 pointer_to_relocations [[format("hex")]];
    u32 pointer_to_line_numbers [[format("hex")]];
    u16 number_of_relocations;
    u16 number_of_line_numbers;
    SectionCharacteristics characteristics;
    if (size_of_raw_data > 0 && pointer_to_raw_data > 0 && pe_fits(pointer_to_raw_data, size_of_raw_data))
        u8 data[size_of_raw_data] @ pointer_to_raw_data;
} [[format("pe_section_text")]];

fn pe_section_text(auto s) {
    return s.name;
};

struct ImportByName {
    u16 hint;
    string name;
};

struct Thunk {
    if (pe_is64) {
        u64 value [[format("hex")]];
        auto by_ordinal = (value >> 63) != 0;
    } else {
        u32 value [[format("hex")]];
        auto by_ordinal = (value >> 31) != 0;
    }
    if (by_ordinal) {
        auto ordinal = value & 0xFFFF;
    } else {
        u64 at = pe_rva_to_offset(value & 0x7FFFFFFF);
        if (at + 3 <= pe_end()) ImportByName import_by_name @ at;
    }
} [[format("pe_thunk_text")]];

fn pe_thunk_text(auto t) {
    if (t.by_ordinal) return std::format("ordinal {}", t.ordinal);
    if (std::core::has_member(t, "import_by_name")) return t.import_by_name.name;
    return "";
};

fn pe_thunk_present(u64 at) {
    u8 width = pe_is64 ? 8 : 4;
    return at + width <= pe_end() && std::mem::read_unsigned(at, width) != 0;
};

struct ImportDescriptor {
    u32 original_first_thunk [[format("hex"), comment("import lookup table RVA")]];
    u32 time_date_stamp;
    u32 forwarder_chain;
    u32 name_rva [[format("hex")]];
    u32 first_thunk [[format("hex"), comment("import address table RVA")]];
    auto dll_name = pe_string_at_rva(name_rva);
    u64 lookup = pe_rva_to_offset(original_first_thunk != 0 ? original_first_thunk : first_thunk);
    if (lookup < pe_end()) Thunk functions[while(pe_thunk_present($))] @ lookup;
} [[format("pe_import_text")]];

fn pe_import_text(auto d) {
    return d.dll_name;
};

fn pe_descriptor_present(u64 at) {
    return at + 20 <= pe_end() && (std::mem::read_unsigned(at, 8) != 0
        || std::mem::read_unsigned(at + 8, 8) != 0 || std::mem::read_unsigned(at + 16, 4) != 0);
};

struct ExportName {
    u32 name_rva [[format("hex")]];
    auto name = pe_string_at_rva(name_rva);
} [[format("pe_export_name_text")]];

fn pe_export_name_text(auto e) {
    return e.name;
};

struct ExportDirectory {
    u32 characteristics;
    u32 time_date_stamp [[format("unix_time_text")]];
    u16 major_version;
    u16 minor_version;
    u32 name_rva [[format("hex")]];
    u32 ordinal_base;
    u32 function_count;
    u32 name_count;
    u32 functions_rva [[format("hex")]];
    u32 names_rva [[format("hex")]];
    u32 name_ordinals_rva [[format("hex")]];
    auto dll_name = pe_string_at_rva(name_rva);
    u64 functions_at = pe_rva_to_offset(functions_rva);
    u64 names_at = pe_rva_to_offset(names_rva);
    u64 ordinals_at = pe_rva_to_offset(name_ordinals_rva);
    if (pe_fits(functions_at, function_count * 4)) u32 function_rvas[function_count] @ functions_at;
    if (pe_fits(names_at, name_count * 4)) ExportName names[name_count] @ names_at;
    if (pe_fits(ordinals_at, name_count * 2)) u16 name_ordinals[name_count] @ ordinals_at;
};

DosHeader dos_header @ 0;
assert(dos_header.magic == "MZ", "not a PE file: missing MZ signature");
u64 nt_at = dos_header.new_header_offset;
if (nt_at > 64 && nt_at <= pe_end()) u8 dos_stub[nt_at - 64] @ 64 [[comment("DOS stub program and Rich header")]];
assert(nt_at + 24 <= pe_end() && std::mem::read_unsigned(nt_at, 4, "big") == 0x50450000,
    "not a PE file: missing PE signature");
NtHeaders nt_headers @ nt_at;
bool pe_is64 = nt_headers.file_header.size_of_optional_header > 0
    && nt_headers.optional_header.magic == OptionalMagic::PE32Plus;
u64 section_table = nt_at + 24 + nt_headers.file_header.size_of_optional_header;
u32 pe_section_count = nt_headers.file_header.number_of_sections;
SectionHeader sections[pe_section_count] @ section_table;

if (nt_headers.file_header.size_of_optional_header > 0) {
    u32 directory_count = nt_headers.optional_header.data_directories.length;
    if (directory_count > 0 && nt_headers.optional_header.data_directories[0].virtual_address != 0) {
        u64 exports_at = pe_rva_to_offset(nt_headers.optional_header.data_directories[0].virtual_address);
        if (pe_fits(exports_at, 40)) ExportDirectory export_directory @ exports_at;
    }
    if (directory_count > 1 && nt_headers.optional_header.data_directories[1].virtual_address != 0) {
        u64 imports_at = pe_rva_to_offset(nt_headers.optional_header.data_directories[1].virtual_address);
        if (imports_at < pe_end())
            ImportDescriptor imports[while(pe_descriptor_present($))] @ imports_at;
    }
}
