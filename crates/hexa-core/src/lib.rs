//! Core of the Hexa hex editor.
//!
//! `hexa-core` has no GUI dependencies so it can be reused by the desktop app,
//! a headless CLI and a WebAssembly build.

pub mod document;
pub mod error;
pub mod history;
pub mod piece;
pub mod provider;
pub mod save;
pub mod task;
pub mod tree;

pub use document::{ChangeKind, ChangeSpan, Document, EditMode, Fill, OpenOptions, ShareMode};
pub use error::{Error, Result};
pub use history::{Change, HistoryItem};
pub use save::{SaveOptions, SaveReport, SaveStrategy};
pub use task::Cancel;
pub use tree::Tree;
