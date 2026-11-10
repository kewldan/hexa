#pragma description Java class file (JVM specification, chapter 4)
#pragma endian big

enum CpTag : u8 {
    Utf8 = 1,
    Integer = 3,
    Float = 4,
    Long = 5,
    Double = 6,
    Class = 7,
    String = 8,
    FieldRef = 9,
    MethodRef = 10,
    InterfaceMethodRef = 11,
    NameAndType = 12,
    MethodHandle = 15,
    MethodType = 16,
    Dynamic = 17,
    InvokeDynamic = 18,
    Module = 19,
    Package = 20
};

enum ReferenceKind : u8 {
    GetField = 1,
    GetStatic = 2,
    PutField = 3,
    PutStatic = 4,
    InvokeVirtual = 5,
    InvokeStatic = 6,
    InvokeSpecial = 7,
    NewInvokeSpecial = 8,
    InvokeInterface = 9
};

enum ClassAccess : u16 {
    Public = 0x0001,
    Final = 0x0010,
    Super = 0x0020,
    Interface = 0x0200,
    Abstract = 0x0400,
    Synthetic = 0x1000,
    Annotation = 0x2000,
    Enum = 0x4000,
    Module = 0x8000
} [[flags]];

enum MemberAccess : u16 {
    Public = 0x0001,
    Private = 0x0002,
    Protected = 0x0004,
    Static = 0x0008,
    Final = 0x0010,
    SynchronizedOrSuper = 0x0020,
    VolatileOrBridge = 0x0040,
    TransientOrVarargs = 0x0080,
    Native = 0x0100,
    Abstract = 0x0400,
    Strict = 0x0800,
    Synthetic = 0x1000,
    Enum = 0x4000
} [[flags]];

// Constant pool slot of the next entry; Long and Double take two slots.
u32 java_slot = 1;

struct CpInfo {
    auto index = java_slot;
    CpTag tag;
    match (tag) {
        (CpTag::Utf8): {
            u16 length;
            char bytes[length];
        }
        (CpTag::Integer): s32 value;
        (CpTag::Float): float value;
        (CpTag::Long): s64 value;
        (CpTag::Double): double value;
        (CpTag::Class | CpTag::Module | CpTag::Package): u16 name_index;
        (CpTag::String): u16 string_index;
        (CpTag::FieldRef | CpTag::MethodRef | CpTag::InterfaceMethodRef): {
            u16 class_index;
            u16 name_and_type_index;
        }
        (CpTag::NameAndType): {
            u16 name_index;
            u16 descriptor_index;
        }
        (CpTag::MethodHandle): {
            ReferenceKind reference_kind;
            u16 reference_index;
        }
        (CpTag::MethodType): u16 descriptor_index;
        (CpTag::Dynamic | CpTag::InvokeDynamic): {
            u16 bootstrap_method_attr_index;
            u16 name_and_type_index;
        }
        (_): std::error(std::format("unknown constant pool tag {} at slot {}", tag, index));
    }
    java_slot += (tag == CpTag::Long || tag == CpTag::Double) ? 2 : 1;
} [[format("java_cp_text")]];

fn java_cp_text(auto e) {
    if (e.tag == CpTag::Utf8) return std::format("#{} \"{}\"", e.index, e.bytes);
    return std::format("#{} {}", e.index, e.tag);
};

// Text of CONSTANT_Utf8 entry `index` (empty when absent).
fn java_utf8(u32 index) {
    u32 count = class_file.constant_pool.length;
    for (u32 i = 0, i < count, i += 1) {
        if (class_file.constant_pool[i].index == index) {
            if (class_file.constant_pool[i].tag == CpTag::Utf8)
                return class_file.constant_pool[i].bytes;
            return "";
        }
    }
    return "";
};

// Name of the class referenced by CONSTANT_Class entry `index`.
fn java_class_name(u32 index) {
    u32 count = class_file.constant_pool.length;
    for (u32 i = 0, i < count, i += 1) {
        if (class_file.constant_pool[i].index == index
                && class_file.constant_pool[i].tag == CpTag::Class)
            return java_utf8(class_file.constant_pool[i].name_index);
    }
    return "";
};

struct ExceptionEntry {
    u16 start_pc;
    u16 end_pc;
    u16 handler_pc;
    u16 catch_type;
};

struct AttributeInfo {
    u16 name_index;
    u32 length;
    auto name = java_utf8(name_index);
    u64 end = $ + length;
    if (name == "Code") {
        u16 max_stack;
        u16 max_locals;
        u32 code_length;
        u8 code[code_length] [[color("C0392B")]];
        u16 exception_table_length;
        ExceptionEntry exception_table[exception_table_length];
        u16 attributes_count;
        AttributeInfo attributes[attributes_count];
    } else if (name == "SourceFile") {
        u16 source_file_index;
        auto source_file = java_utf8(source_file_index);
    } else if (name == "ConstantValue") {
        u16 constant_value_index;
    } else if (length > 0) {
        u8 info[length];
    }
    if ($ < end) u8 unparsed[end - $];
} [[format("java_attribute_text")]];

fn java_attribute_text(auto a) {
    return a.name;
};

struct MemberInfo {
    MemberAccess access_flags;
    u16 name_index;
    u16 descriptor_index;
    auto name = java_utf8(name_index);
    auto descriptor = java_utf8(descriptor_index);
    u16 attributes_count;
    AttributeInfo attributes[attributes_count];
} [[format("java_member_text")]];

fn java_member_text(auto m) {
    return m.name + " " + m.descriptor;
};

struct ClassFile {
    u32 magic [[format("hex"), color("808080")]];
    u16 minor_version;
    u16 major_version [[comment("45 = Java 1.1 ... 52 = Java 8, 61 = Java 17")]];
    u16 constant_pool_count;
    CpInfo constant_pool[while(java_slot < constant_pool_count)] [[color("2E86C1")]];
    ClassAccess access_flags;
    u16 this_class;
    u16 super_class;
    auto class_name = java_class_name(this_class);
    auto super_class_name = java_class_name(super_class);
    u16 interfaces_count;
    u16 interfaces[interfaces_count];
    u16 fields_count;
    MemberInfo fields[fields_count];
    u16 methods_count;
    MemberInfo methods[methods_count];
    u16 attributes_count;
    AttributeInfo attributes[attributes_count];
};

assert(std::mem::read_unsigned(0, 4, "big") == 0xCAFEBABE, "not a Java class file: bad magic");
ClassFile class_file @ 0;
