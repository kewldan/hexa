//! Front-ends converting other structure formats into the template AST.
//!
//! - [`ksy`]: Kaitai Struct YAML (STRC-03).
//! - [`from_hexpat`]: `ImHex` pattern files, parsed natively (STRC-04).
//! - [`from_bt`]: 010 Editor binary templates (STRC-05).
//! - [`from_c_header`]: C headers with C layout rules (STRC-06).
//! - [`debuginfo`]: DWARF and PDB type information (STRC-07).

pub mod debuginfo;
pub mod ksy;

use crate::ast::Program;
use crate::diag::{Diagnostic, Result};
use crate::lexer::Resolver;
use crate::parser::{Dialect, ParseOptions, parse_strict, parse_with};

/// Parses an `ImHex` `.hexpat` pattern (STRC-04). `std`/`type` library
/// includes are built in; others go through `resolver`.
pub fn from_hexpat(src: &str, resolver: Option<Resolver<'_>>) -> Result<Program> {
    parse_strict(
        src,
        &ParseOptions {
            dialect: Dialect::Hexpat,
            resolver,
            ..ParseOptions::default()
        },
    )
}

/// Parses a 010 Editor binary template (STRC-05).
pub fn from_bt(src: &str, resolver: Option<Resolver<'_>>) -> Result<Program> {
    parse_strict(
        src,
        &ParseOptions {
            dialect: Dialect::Bt,
            resolver,
            ..ParseOptions::default()
        },
    )
}

/// Options for C header import (STRC-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CHeaderOptions {
    /// Size of `long` in bytes: 8 for LP64 (Linux, macOS), 4 for LLP64 (Windows).
    pub long_size: u8,
    /// Size of pointers in bytes.
    pub pointer_size: u8,
}

impl Default for CHeaderOptions {
    fn default() -> Self {
        Self {
            long_size: 8,
            pointer_size: 8,
        }
    }
}

/// Parses a C header (structs, unions, enums, typedefs, `<stdint.h>` types,
/// arrays, bit-fields, `#pragma pack`, `__attribute__((packed))`). The
/// result only defines types; apply one with `EvalOptions::root_type`.
pub fn from_c_header(
    src: &str,
    opts: CHeaderOptions,
    resolver: Option<Resolver<'_>>,
) -> Result<Program> {
    parse_strict(
        src,
        &ParseOptions {
            dialect: Dialect::C,
            resolver,
            c_long_size: opts.long_size,
            c_pointer_size: opts.pointer_size,
        },
    )
}

/// Like the functions above but keeps going after errors, returning the
/// partial program together with all diagnostics (for editors).
pub fn parse_dialect(src: &str, dialect: Dialect) -> (Program, Vec<Diagnostic>) {
    parse_with(
        src,
        &ParseOptions {
            dialect,
            ..ParseOptions::default()
        },
    )
}
