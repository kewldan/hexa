#pragma description PDF document: header, cross-reference table, trailer, startxref

fn pdf_end() {
    return std::mem::base_address() + std::mem::size();
};

fn pdf_byte(u64 pos) {
    if (pos >= pdf_end()) return -1;
    return std::mem::read_unsigned(pos, 1);
};

fn pdf_is_eol(s16 c) {
    return c == '\r' || c == '\n';
};

fn pdf_is_space(s16 c) {
    return c == ' ' || c == '\t' || c == '\r' || c == '\n' || c == '\f' || c == 0;
};

fn pdf_is_digit(s16 c) {
    return c >= '0' && c <= '9';
};

// Length of the line starting at `pos` without its end-of-line marker.
fn pdf_line_length(u64 pos) {
    u64 n = 0;
    while (pos + n < pdf_end() && !pdf_is_eol(pdf_byte(pos + n))) n += 1;
    return n;
};

// Number of end-of-line bytes at `pos` (0, 1 or 2).
fn pdf_eol_length(u64 pos) {
    if (pdf_byte(pos) == '\r' && pdf_byte(pos + 1) == '\n') return 2;
    if (pdf_is_eol(pdf_byte(pos))) return 1;
    return 0;
};

fn pdf_space_length(u64 pos) {
    u64 n = 0;
    while (pdf_is_space(pdf_byte(pos + n))) n += 1;
    return n;
};

fn pdf_digit_count(u64 pos) {
    u64 n = 0;
    while (pdf_is_digit(pdf_byte(pos + n))) n += 1;
    return n;
};

fn pdf_number(u64 pos) {
    u64 v = 0;
    while (pdf_is_digit(pdf_byte(pos))) {
        v = v * 10 + pdf_byte(pos) - '0';
        pos += 1;
    }
    return v;
};

// Offset of the last `startxref` keyword, or -1.
fn pdf_find_startxref() {
    s64 last = -1;
    s64 occurrence = 0;
    while (true) {
        s64 at = std::mem::find_sequence(occurrence, 's', 't', 'a', 'r', 't', 'x', 'r', 'e', 'f');
        if (at < 0) return last;
        last = at;
        occurrence += 1;
    }
};

// Length of a `<< ... >>` dictionary starting at `pos` (nesting aware).
fn pdf_dictionary_length(u64 pos) {
    u64 n = 0;
    s32 depth = 0;
    while (pos + n + 1 < pdf_end()) {
        s16 a = pdf_byte(pos + n);
        s16 b = pdf_byte(pos + n + 1);
        if (a == '<' && b == '<') {
            depth += 1;
            n += 2;
        } else if (a == '>' && b == '>') {
            depth -= 1;
            n += 2;
            if (depth <= 0) return n;
        } else {
            n += 1;
        }
    }
    return pdf_end() - pos;
};

struct Line {
    char text[pdf_line_length($)];
    u8 end_of_line[pdf_eol_length($)];
} [[format("pdf_line_text")]];

fn pdf_line_text(auto l) {
    return l.text;
};

struct Header {
    char signature[5] [[comment("%PDF-")]];
    char version[pdf_line_length($)];
    u8 end_of_line[pdf_eol_length($)];
    if (pdf_byte($) == '%') Line binary_marker [[comment("marks the file as binary")]];
} [[color("808080")]];

struct XrefEntry {
    char offset_text[10];
    padding[1];
    char generation_text[5];
    padding[1];
    char kind [[comment("n = in use, f = free")]];
    u8 end_of_line[2];
    auto object_offset = pdf_number(addressof(offset_text));
    auto generation = pdf_number(addressof(generation_text));
    auto in_use = kind == 'n';
};

struct XrefSubsection {
    char first_object_text[pdf_digit_count($)];
    u8 separator[pdf_space_length($)];
    char count_text[pdf_digit_count($)];
    u8 end_of_line[pdf_space_length($)];
    auto first_object = pdf_number(addressof(first_object_text));
    auto count = pdf_number(addressof(count_text));
    XrefEntry entries[count];
};

struct Trailer {
    char keyword[7] [[comment("trailer")]];
    u8 whitespace[pdf_space_length($)];
    char dictionary[pdf_dictionary_length($)];
};

struct XrefTable {
    char keyword[4] [[comment("xref")]];
    u8 end_of_line[pdf_space_length($)];
    XrefSubsection subsections[while(pdf_is_digit(pdf_byte($)))];
    if (std::mem::read_string($, 7) == "trailer") Trailer trailer;
};

struct StartXref {
    char keyword[9] [[comment("startxref")]];
    u8 whitespace[pdf_space_length($)];
    char offset_text[pdf_digit_count($)];
    auto xref_offset = pdf_number(addressof(offset_text));
    u8 end_of_line[pdf_space_length($)];
    if ($ + 5 <= pdf_end() && std::mem::read_string($, 5) == "%%EOF") char eof_marker[5];
} [[color("27AE60")]];

Header header @ 0;
s64 startxref_at = pdf_find_startxref();
if (startxref_at >= 0) {
    StartXref startxref @ startxref_at;
    if (startxref.xref_offset + 4 <= pdf_end()) {
        if (std::mem::read_string(startxref.xref_offset, 4) == "xref")
            XrefTable xref @ startxref.xref_offset;
        else
            Line xref_stream_object @ startxref.xref_offset
                [[comment("cross-reference stream object (PDF 1.5+)")]];
    }
}
