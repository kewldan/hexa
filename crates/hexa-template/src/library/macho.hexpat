#pragma description Mach-O executable / dylib / object (32/64-bit, fat/universal)

enum CpuType : u32 {
    Any = 0xFFFFFFFF,
    VAX = 1,
    MC680x0 = 6,
    X86 = 7,
    X86_64 = 0x01000007,
    MC98000 = 10,
    HPPA = 11,
    ARM = 12,
    ARM64 = 0x0100000C,
    ARM64_32 = 0x0200000C,
    MC88000 = 13,
    SPARC = 14,
    I860 = 15,
    PowerPC = 18,
    PowerPC64 = 0x01000012
};

enum FileType : u32 {
    Object = 0x1,
    Execute = 0x2,
    FixedVMLibrary = 0x3,
    Core = 0x4,
    Preload = 0x5,
    Dylib = 0x6,
    Dylinker = 0x7,
    Bundle = 0x8,
    DylibStub = 0x9,
    DSYM = 0xA,
    KextBundle = 0xB,
    FileSet = 0xC
};

enum HeaderFlags : u32 {
    NoUndefs = 0x1,
    IncrLink = 0x2,
    DyldLink = 0x4,
    BindAtLoad = 0x8,
    Prebound = 0x10,
    SplitSegs = 0x20,
    LazyInit = 0x40,
    TwoLevel = 0x80,
    ForceFlat = 0x100,
    NoMultiDefs = 0x200,
    NoFixPrebinding = 0x400,
    Prebindable = 0x800,
    AllModsBound = 0x1000,
    SubsectionsViaSymbols = 0x2000,
    Canonical = 0x4000,
    WeakDefines = 0x8000,
    BindsToWeak = 0x10000,
    AllowStackExecution = 0x20000,
    RootSafe = 0x40000,
    SetuidSafe = 0x80000,
    NoReexportedDylibs = 0x100000,
    PIE = 0x200000,
    DeadStrippableDylib = 0x400000,
    HasTLVDescriptors = 0x800000,
    NoHeapExecution = 0x1000000,
    AppExtensionSafe = 0x2000000,
    NListOutOfSyncWithDyldInfo = 0x4000000,
    SimSupport = 0x8000000,
    DylibInCache = 0x80000000
} [[flags]];

enum LoadCommandType : u32 {
    Segment = 0x1,
    SymTab = 0x2,
    SymSeg = 0x3,
    Thread = 0x4,
    UnixThread = 0x5,
    DySymTab = 0xB,
    LoadDylib = 0xC,
    IdDylib = 0xD,
    LoadDylinker = 0xE,
    IdDylinker = 0xF,
    PreboundDylib = 0x10,
    Routines = 0x11,
    SubFramework = 0x12,
    SubUmbrella = 0x13,
    SubClient = 0x14,
    SubLibrary = 0x15,
    TwoLevelHints = 0x16,
    PrebindChecksum = 0x17,
    LoadWeakDylib = 0x80000018,
    Segment64 = 0x19,
    Routines64 = 0x1A,
    UUID = 0x1B,
    RPath = 0x8000001C,
    CodeSignature = 0x1D,
    SegmentSplitInfo = 0x1E,
    ReexportDylib = 0x8000001F,
    LazyLoadDylib = 0x20,
    EncryptionInfo = 0x21,
    DyldInfo = 0x22,
    DyldInfoOnly = 0x80000022,
    LoadUpwardDylib = 0x80000023,
    VersionMinMacOSX = 0x24,
    VersionMinIPhoneOS = 0x25,
    FunctionStarts = 0x26,
    DyldEnvironment = 0x27,
    Main = 0x80000028,
    DataInCode = 0x29,
    SourceVersion = 0x2A,
    DylibCodeSignDRs = 0x2B,
    EncryptionInfo64 = 0x2C,
    LinkerOption = 0x2D,
    LinkerOptimizationHint = 0x2E,
    VersionMinTvOS = 0x2F,
    VersionMinWatchOS = 0x30,
    Note = 0x31,
    BuildVersion = 0x32,
    DyldExportsTrie = 0x80000033,
    DyldChainedFixups = 0x80000034,
    FileSetEntry = 0x80000035,
    AtomInfo = 0x36
};

enum VmProtection : u32 {
    Read = 0x1,
    Write = 0x2,
    Execute = 0x4
} [[flags]];

enum Platform : u32 {
    MacOS = 1,
    IOS = 2,
    TvOS = 3,
    WatchOS = 4,
    BridgeOS = 5,
    MacCatalyst = 6,
    IOSSimulator = 7,
    TvOSSimulator = 8,
    WatchOSSimulator = 9,
    DriverKit = 10,
    VisionOS = 11,
    VisionOSSimulator = 12
};

enum Tool : u32 {
    Clang = 1,
    Swift = 2,
    LD = 3,
    LLD = 4
};

fn macho_version_text(u32 v) {
    return std::format("{}.{}.{}", v >> 16, (v >> 8) & 0xFF, v & 0xFF);
};

fn macho_source_version_text(u64 v) {
    return std::format("{}.{}.{}.{}.{}", v >> 40, (v >> 30) & 0x3FF, (v >> 20) & 0x3FF,
        (v >> 10) & 0x3FF, v & 0x3FF);
};

struct Section32 {
    char section_name[16];
    char segment_name[16];
    u32 address [[format("hex")]];
    u32 size;
    u32 offset [[format("hex")]];
    u32 alignment [[comment("power of two")]];
    u32 relocation_offset;
    u32 relocation_count;
    u32 flags [[format("hex")]];
    u32 reserved1;
    u32 reserved2;
} [[format("macho_section_text")]];

struct Section64 {
    char section_name[16];
    char segment_name[16];
    u64 address [[format("hex")]];
    u64 size;
    u32 offset [[format("hex")]];
    u32 alignment [[comment("power of two")]];
    u32 relocation_offset;
    u32 relocation_count;
    u32 flags [[format("hex")]];
    u32 reserved1;
    u32 reserved2;
    u32 reserved3;
} [[format("macho_section_text")]];

fn macho_section_text(auto s) {
    return std::format("{},{}", s.segment_name, s.section_name);
};

struct BuildToolVersion {
    Tool tool;
    u32 version [[format("macho_version_text")]];
};

struct LoadCommand {
    LoadCommandType command [[color("E67E22")]];
    u32 command_size;
    match (command) {
        (LoadCommandType::Segment): {
            char segment_name[16];
            u32 vm_address [[format("hex")]];
            u32 vm_size [[format("hex")]];
            u32 file_offset [[format("hex")]];
            u32 file_size;
            VmProtection max_protection;
            VmProtection initial_protection;
            u32 section_count;
            u32 flags [[format("hex")]];
            Section32 sections[section_count];
        }
        (LoadCommandType::Segment64): {
            char segment_name[16];
            u64 vm_address [[format("hex")]];
            u64 vm_size [[format("hex")]];
            u64 file_offset [[format("hex")]];
            u64 file_size;
            VmProtection max_protection;
            VmProtection initial_protection;
            u32 section_count;
            u32 flags [[format("hex")]];
            Section64 sections[section_count];
        }
        (LoadCommandType::SymTab): {
            u32 symbol_offset [[format("hex")]];
            u32 symbol_count;
            u32 string_offset [[format("hex")]];
            u32 string_size;
        }
        (LoadCommandType::DySymTab): {
            u32 local_symbol_index;
            u32 local_symbol_count;
            u32 external_symbol_index;
            u32 external_symbol_count;
            u32 undefined_symbol_index;
            u32 undefined_symbol_count;
            u32 toc_offset;
            u32 toc_count;
            u32 module_table_offset;
            u32 module_count;
            u32 external_reference_offset;
            u32 external_reference_count;
            u32 indirect_symbol_offset;
            u32 indirect_symbol_count;
            u32 external_relocation_offset;
            u32 external_relocation_count;
            u32 local_relocation_offset;
            u32 local_relocation_count;
        }
        (LoadCommandType::LoadDylib | LoadCommandType::IdDylib | LoadCommandType::LoadWeakDylib
                | LoadCommandType::ReexportDylib | LoadCommandType::LazyLoadDylib
                | LoadCommandType::LoadUpwardDylib): {
            u32 name_offset;
            u32 timestamp;
            u32 current_version [[format("macho_version_text")]];
            u32 compatibility_version [[format("macho_version_text")]];
            if (name_offset < command_size)
                char name[command_size - name_offset] @ addressof(this) + name_offset;
        }
        (LoadCommandType::LoadDylinker | LoadCommandType::IdDylinker
                | LoadCommandType::DyldEnvironment | LoadCommandType::RPath
                | LoadCommandType::SubFramework | LoadCommandType::SubUmbrella
                | LoadCommandType::SubClient | LoadCommandType::SubLibrary): {
            u32 name_offset;
            if (name_offset < command_size)
                char name[command_size - name_offset] @ addressof(this) + name_offset;
        }
        (LoadCommandType::UUID): u8 uuid[16];
        (LoadCommandType::Main): {
            u64 entry_offset [[format("hex")]];
            u64 stack_size;
        }
        (LoadCommandType::SourceVersion): u64 version [[format("macho_source_version_text")]];
        (LoadCommandType::BuildVersion): {
            Platform platform;
            u32 minimum_os [[format("macho_version_text")]];
            u32 sdk [[format("macho_version_text")]];
            u32 tool_count;
            BuildToolVersion tools[tool_count];
        }
        (LoadCommandType::VersionMinMacOSX | LoadCommandType::VersionMinIPhoneOS
                | LoadCommandType::VersionMinTvOS | LoadCommandType::VersionMinWatchOS): {
            u32 version [[format("macho_version_text")]];
            u32 sdk [[format("macho_version_text")]];
        }
        (LoadCommandType::CodeSignature | LoadCommandType::SegmentSplitInfo
                | LoadCommandType::FunctionStarts | LoadCommandType::DataInCode
                | LoadCommandType::DylibCodeSignDRs | LoadCommandType::LinkerOptimizationHint
                | LoadCommandType::DyldExportsTrie | LoadCommandType::DyldChainedFixups
                | LoadCommandType::AtomInfo): {
            u32 data_offset [[format("hex")]];
            u32 data_size;
        }
        (LoadCommandType::DyldInfo | LoadCommandType::DyldInfoOnly): {
            u32 rebase_offset [[format("hex")]];
            u32 rebase_size;
            u32 bind_offset [[format("hex")]];
            u32 bind_size;
            u32 weak_bind_offset [[format("hex")]];
            u32 weak_bind_size;
            u32 lazy_bind_offset [[format("hex")]];
            u32 lazy_bind_size;
            u32 export_offset [[format("hex")]];
            u32 export_size;
        }
        (LoadCommandType::EncryptionInfo | LoadCommandType::EncryptionInfo64): {
            u32 crypt_offset [[format("hex")]];
            u32 crypt_size;
            u32 crypt_id;
        }
    }
    u64 command_end = addressof(this) + command_size;
    if ($ < command_end) u8 command_data[command_end - $];
    else if ($ > command_end) std::assert_warn(false, "load command is larger than command_size");
};

struct MachHeader {
    u32 magic [[format("hex"), color("808080")]];
    CpuType cpu_type;
    u32 cpu_subtype [[format("hex")]];
    FileType file_type;
    u32 command_count;
    u32 commands_size;
    HeaderFlags flags;
    if (magic == 0xFEEDFACF) u32 reserved;
} [[color("2E86C1")]];

struct MachO {
    MachHeader header;
    LoadCommand load_commands[header.command_count];
};

struct FatArch {
    CpuType cpu_type;
    u32 cpu_subtype [[format("hex")]];
    u32 offset [[format("hex")]];
    u32 size;
    u32 alignment [[comment("power of two")]];
    if (offset + 4 <= std::mem::size()) {
        u32 slice_magic = std::mem::read_unsigned(offset, 4, "big");
        if (slice_magic == 0xFEEDFACE || slice_magic == 0xFEEDFACF) be MachO image @ offset;
        else if (slice_magic == 0xCEFAEDFE || slice_magic == 0xCFFAEDFE) le MachO image @ offset;
    }
};

struct FatHeader {
    u32 magic [[format("hex"), comment("0xCAFEBABE")]];
    u32 arch_count;
    FatArch architectures[arch_count];
};

u32 macho_magic = std::mem::read_unsigned(0, 4, "big");
if (macho_magic == 0xCAFEBABE) be FatHeader fat @ 0;
else if (macho_magic == 0xFEEDFACE || macho_magic == 0xFEEDFACF) be MachO macho @ 0;
else if (macho_magic == 0xCEFAEDFE || macho_magic == 0xCFFAEDFE) le MachO macho @ 0;
else std::error("not a Mach-O file: unknown magic");
