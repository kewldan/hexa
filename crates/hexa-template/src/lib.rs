//! Hexa structure templates (spec section 8): a C-like template language
//! close to the `ImHex` pattern language, importers for other formats
//! (Kaitai `.ksy`, 010 Editor `.bt`, C headers, DWARF/PDB), the evaluator
//! producing the structure tree shown next to the hex grid, and the
//! tooling around it (editor support, debugger, built-in library,
//! exporters and code generators).

pub mod ast;
pub mod builtins;
pub mod codegen;
pub mod dap;
pub mod debugger;
pub mod diag;
pub mod editor;
pub mod eval;
pub mod export;
pub mod import;
pub mod lexer;
pub mod library;
pub mod lsp;
pub mod parser;
pub mod prim;
pub mod query;
pub mod repo;
pub mod result;
pub mod scan;
pub mod suggest;

pub use ast::{Endian, Program};
pub use diag::{Diagnostic, Error, Result, Severity};
pub use eval::{DebugState, EvalOptions, Limits, evaluate, evaluate_many};
pub use parser::{Dialect, ParseOptions, parse, parse_strict, parse_with};
pub use query::{ExportFormat, ExportOptions, FieldChange, diff, encode_value, export, filter};
pub use result::{
    ChecksumField, Color, Node, NodeHit, NodeId, NodeKind, TemplateResult, Value, checksum_fields,
    children, expand_element, highlights, node_at,
};
pub use scan::scan_valid;
pub use suggest::{StructBuilder, Suggestion, suggest};
