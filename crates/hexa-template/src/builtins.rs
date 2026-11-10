//! Catalogue of built-in functions, keywords, attributes and primitive
//! types, used by editor support (STRC-17). The evaluator implements every
//! function listed here (see `eval::Eval::builtin`).

/// `(name, signature, description)` of built-in functions.
pub const FUNCTIONS: &[(&str, &str, &str)] = &[
    (
        "std::mem::size",
        "std::mem::size() -> u64",
        "Size of the data (or selection) in bytes.",
    ),
    (
        "std::mem::base_address",
        "std::mem::base_address() -> u64",
        "Start offset of the data / selection.",
    ),
    (
        "std::mem::eof",
        "std::mem::eof() -> bool",
        "Whether `$` reached the end of the data.",
    ),
    (
        "std::mem::read_unsigned",
        "std::mem::read_unsigned(offset, size, endian = default) -> u128",
        "Reads an unsigned integer of `size` bytes without creating a field. `endian` is \"big\"/\"little\" or 1/0.",
    ),
    (
        "std::mem::read_signed",
        "std::mem::read_signed(offset, size, endian = default) -> s128",
        "Reads a signed integer of `size` bytes without creating a field.",
    ),
    (
        "std::mem::read_string",
        "std::mem::read_string(offset, length = -1) -> str",
        "Reads `length` bytes as text (NUL-terminated when `length` is negative).",
    ),
    (
        "std::mem::align_to",
        "std::mem::align_to(alignment, value) -> u64",
        "Rounds `value` up to a multiple of `alignment`.",
    ),
    (
        "std::mem::find_sequence",
        "std::mem::find_sequence(occurrence, bytes...) -> s64",
        "Offset of the n-th occurrence of a byte sequence, or -1.",
    ),
    (
        "std::core::array_index",
        "std::core::array_index() -> u64",
        "Index of the array element being read.",
    ),
    (
        "std::core::member_count",
        "std::core::member_count(value) -> u64",
        "Number of members of a structure.",
    ),
    (
        "std::core::has_member",
        "std::core::has_member(value, \"name\") -> bool",
        "Whether a structure has a member with that name.",
    ),
    (
        "std::string::length",
        "std::string::length(s) -> u64",
        "Length of a string in bytes.",
    ),
    (
        "std::string::at",
        "std::string::at(s, index) -> str",
        "Character at `index`.",
    ),
    (
        "std::string::substr",
        "std::string::substr(s, pos, len) -> str",
        "Substring.",
    ),
    (
        "std::string::to_string",
        "std::string::to_string(value) -> str",
        "Text of any value.",
    ),
    (
        "std::string::starts_with",
        "std::string::starts_with(s, prefix) -> bool",
        "Prefix test.",
    ),
    (
        "std::string::ends_with",
        "std::string::ends_with(s, suffix) -> bool",
        "Suffix test.",
    ),
    (
        "std::string::contains",
        "std::string::contains(s, part) -> bool",
        "Substring test.",
    ),
    (
        "std::string::parse_int",
        "std::string::parse_int(s, base = 10) -> s128",
        "Parses an integer.",
    ),
    (
        "std::print",
        "std::print(format, args...)",
        "Appends a line to the log (`{}` placeholders, `{:02X}` etc.).",
    ),
    (
        "std::format",
        "std::format(format, args...) -> str",
        "Formats text (`{}`, `{:x}`, `{:#x}`, `{:08b}`).",
    ),
    (
        "std::warning",
        "std::warning(message)",
        "Records a warning and continues.",
    ),
    (
        "std::error",
        "std::error(message)",
        "Stops evaluation with an error.",
    ),
    (
        "std::assert",
        "std::assert(condition, message)",
        "Stops evaluation when `condition` is false.",
    ),
    (
        "std::assert_warn",
        "std::assert_warn(condition, message)",
        "Records a warning when `condition` is false.",
    ),
    (
        "std::math::min",
        "std::math::min(a, b)",
        "Smaller of two integers.",
    ),
    (
        "std::math::max",
        "std::math::max(a, b)",
        "Larger of two integers.",
    ),
    ("std::math::abs", "std::math::abs(x)", "Absolute value."),
    (
        "std::math::floor",
        "std::math::floor(x) -> double",
        "Rounds down.",
    ),
    (
        "std::math::ceil",
        "std::math::ceil(x) -> double",
        "Rounds up.",
    ),
    ("std::math::pow", "std::math::pow(x, y) -> double", "Power."),
    (
        "align",
        "align(n)",
        "Moves `$` to the next multiple of `n`.",
    ),
    (
        "FTell",
        "FTell() -> int64",
        "010 Editor: current read position.",
    ),
    (
        "FSeek",
        "FSeek(pos) -> int",
        "010 Editor: moves the read position.",
    ),
    ("FSkip", "FSkip(n) -> int", "010 Editor: skips `n` bytes."),
    ("FEof", "FEof() -> int", "010 Editor: end of file reached."),
    (
        "FileSize",
        "FileSize() -> int64",
        "010 Editor: size of the data.",
    ),
    (
        "LittleEndian",
        "LittleEndian()",
        "010 Editor: following reads are little endian.",
    ),
    (
        "BigEndian",
        "BigEndian()",
        "010 Editor: following reads are big endian.",
    ),
    (
        "IsBigEndian",
        "IsBigEndian() -> int",
        "010 Editor: current byte order is big endian.",
    ),
    (
        "IsLittleEndian",
        "IsLittleEndian() -> int",
        "010 Editor: current byte order is little endian.",
    ),
    (
        "ReadByte",
        "ReadByte(pos = FTell()) -> char",
        "010 Editor: reads a signed byte.",
    ),
    (
        "ReadUByte",
        "ReadUByte(pos = FTell()) -> uchar",
        "010 Editor: reads an unsigned byte.",
    ),
    (
        "ReadShort",
        "ReadShort(pos = FTell()) -> short",
        "010 Editor: reads a 16-bit integer.",
    ),
    (
        "ReadUShort",
        "ReadUShort(pos = FTell()) -> ushort",
        "010 Editor: reads an unsigned 16-bit integer.",
    ),
    (
        "ReadInt",
        "ReadInt(pos = FTell()) -> int",
        "010 Editor: reads a 32-bit integer.",
    ),
    (
        "ReadUInt",
        "ReadUInt(pos = FTell()) -> uint",
        "010 Editor: reads an unsigned 32-bit integer.",
    ),
    (
        "ReadInt64",
        "ReadInt64(pos = FTell()) -> int64",
        "010 Editor: reads a 64-bit integer.",
    ),
    (
        "ReadUInt64",
        "ReadUInt64(pos = FTell()) -> uint64",
        "010 Editor: reads an unsigned 64-bit integer.",
    ),
    (
        "ReadQuad",
        "ReadQuad(pos = FTell()) -> int64",
        "010 Editor: reads a 64-bit integer.",
    ),
    (
        "ReadUQuad",
        "ReadUQuad(pos = FTell()) -> uint64",
        "010 Editor: reads an unsigned 64-bit integer.",
    ),
    (
        "ReadFloat",
        "ReadFloat(pos = FTell()) -> float",
        "010 Editor: reads a float.",
    ),
    (
        "ReadDouble",
        "ReadDouble(pos = FTell()) -> double",
        "010 Editor: reads a double.",
    ),
    (
        "ReadString",
        "ReadString(pos, maxLen = -1) -> string",
        "010 Editor: reads a NUL-terminated string.",
    ),
    (
        "Printf",
        "Printf(format, args...)",
        "010 Editor: prints to the log (printf syntax).",
    ),
    (
        "Warning",
        "Warning(format, args...)",
        "010 Editor: records a warning.",
    ),
    (
        "Str",
        "Str(format, args...) -> string",
        "010 Editor: formats a string.",
    ),
    (
        "SPrintf",
        "SPrintf(dest, format, args...)",
        "010 Editor: formats into `dest`.",
    ),
    ("Strlen", "Strlen(s) -> int", "010 Editor: string length."),
    (
        "SubStr",
        "SubStr(s, start, count) -> string",
        "010 Editor: substring.",
    ),
    ("Atoi", "Atoi(s) -> int", "010 Editor: parses an integer."),
    (
        "Exit",
        "Exit(code)",
        "010 Editor: stops the template without an error.",
    ),
    ("Min", "Min(a, b)", "010 Editor: minimum."),
    ("Max", "Max(a, b)", "010 Editor: maximum."),
    ("Abs", "Abs(x)", "010 Editor: absolute value."),
    (
        "exists",
        "exists(name) -> int",
        "010 Editor: whether a variable exists.",
    ),
];

/// Whether `name` is a built-in function (with or without the `std::`
/// prefix).
pub fn is_function(name: &str) -> bool {
    FUNCTIONS.iter().any(|(n, _, _)| {
        *n == name || n.strip_prefix("std::") == Some(name) || name.strip_prefix("std::") == Some(n)
    }) || matches!(
        name,
        "assert" | "print" | "format" | "min" | "max" | "abs" | "strlen"
    )
}

/// Looks up a built-in function.
pub fn function(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    FUNCTIONS
        .iter()
        .find(|(n, _, _)| *n == name || n.strip_prefix("std::") == Some(name))
        .copied()
}

/// Language keywords with a short description.
pub const KEYWORDS: &[(&str, &str)] = &[
    (
        "struct",
        "Declares a structure: fields are read one after another.",
    ),
    (
        "union",
        "Declares a union: every field starts at the same offset.",
    ),
    (
        "enum",
        "Declares an enumeration: `enum Name : u16 { A = 1, B, C = 0x10 }`.",
    ),
    (
        "bitfield",
        "Declares a bit-field group: `bitfield Flags { a : 3; b : 5; }`.",
    ),
    ("using", "Type alias: `using Word = be u16;`."),
    ("typedef", "C / 010 Editor type alias."),
    ("fn", "Function: `fn name(u32 a) { return a * 2; };`."),
    ("namespace", "Groups definitions: `namespace fmt { ... }`."),
    ("if", "Conditional fields."),
    ("else", "Alternative branch of `if`."),
    (
        "match",
        "Pattern match: `match (x) { (1 | 2): ...; (3 ... 5): ...; (_): ...; }`.",
    ),
    ("switch", "C-style switch, converted to `match`."),
    ("while", "Loop; inside arrays `T x[while(cond)]`."),
    ("for", "Loop: `for (u32 i = 0, i < n, i += 1) { ... }`."),
    ("do", "Loop with the condition at the end."),
    (
        "break",
        "Leaves a loop; inside a struct it ends the enclosing array.",
    ),
    ("continue", "Next loop iteration."),
    ("return", "Returns from a function (or ends a structure)."),
    (
        "auto",
        "Computed field shown in the tree: `auto x = expr;`.",
    ),
    ("let", "Computed field shown in the tree: `let x = expr;`."),
    ("local", "010 Editor local variable (not shown)."),
    ("const", "Constant local variable."),
    ("le", "Little-endian prefix: `le u32 x;`."),
    ("be", "Big-endian prefix: `be u32 x;`."),
    ("padding", "Skipped bytes: `padding[4];`."),
    ("parent", "The enclosing structure."),
    ("this", "The structure being read."),
    ("sizeof", "Size of a field or type in bytes."),
    ("addressof", "Offset of a field."),
    ("true", "Boolean true."),
    ("false", "Boolean false."),
];

/// Attributes understood by the evaluator.
pub const ATTRIBUTES: &[(&str, &str)] = &[
    ("name", "name(\"text\") — display name in the tree."),
    (
        "comment",
        "comment(\"text\") — comment shown next to the field.",
    ),
    (
        "color",
        "color(\"rrggbb\") — highlight colour (also on structs).",
    ),
    ("hidden", "hidden — hides the field in the tree."),
    (
        "format",
        "format(\"hex\"|\"dec\"|\"bin\"|\"oct\"|\"fn_name\") — value formatting.",
    ),
    (
        "flags",
        "flags — on an enum: values are bit flags (`A | C`).",
    ),
    (
        "checksum",
        "checksum(\"crc32\", start, end) — the field stores a checksum of start..end.",
    ),
    (
        "size",
        "size(n) — the field occupies exactly n bytes (sized sub-region).",
    ),
    ("align", "align(n) — aligns the field to a multiple of n."),
    (
        "pointer_base",
        "pointer_base(base | \"fn\") — base address added to a pointer.",
    ),
    (
        "type_name",
        "type_name(\"T\") — overrides the displayed type.",
    ),
    (
        "no_unique_address",
        "no_unique_address — the field does not advance `$`.",
    ),
];

/// Primitive type descriptions for completion and hover.
pub const TYPES: &[(&str, &str)] = &[
    ("u8", "unsigned 8-bit integer (1 byte)"),
    ("u16", "unsigned 16-bit integer (2 bytes)"),
    ("u24", "unsigned 24-bit integer (3 bytes)"),
    ("u32", "unsigned 32-bit integer (4 bytes)"),
    ("u48", "unsigned 48-bit integer (6 bytes)"),
    ("u64", "unsigned 64-bit integer (8 bytes)"),
    ("u96", "unsigned 96-bit integer (12 bytes)"),
    ("u128", "unsigned 128-bit integer (16 bytes)"),
    ("s8", "signed 8-bit integer (1 byte)"),
    ("s16", "signed 16-bit integer (2 bytes)"),
    ("s24", "signed 24-bit integer (3 bytes)"),
    ("s32", "signed 32-bit integer (4 bytes)"),
    ("s48", "signed 48-bit integer (6 bytes)"),
    ("s64", "signed 64-bit integer (8 bytes)"),
    ("s128", "signed 128-bit integer (16 bytes)"),
    ("float", "IEEE 754 single precision (4 bytes)"),
    ("double", "IEEE 754 double precision (8 bytes)"),
    ("f16", "IEEE 754 half precision (2 bytes)"),
    ("char", "8-bit character; `char s[n]` is a string"),
    ("char16", "UTF-16 code unit; `char16 s[n]` is a string"),
    ("bool", "boolean (1 byte)"),
    ("string", "NUL-terminated 8-bit string"),
    ("wstring", "NUL-terminated UTF-16 string"),
    ("padding", "skipped bytes: `padding[n];`"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert!(is_function("std::mem::size"));
        assert!(is_function("mem::size"));
        assert!(is_function("FSeek"));
        assert!(!is_function("nope"));
        assert_eq!(function("print").map(|f| f.0), Some("std::print"));
        for (n, _) in TYPES {
            assert!(crate::prim::Prim::from_name(n).is_some(), "{n}");
        }
    }
}
