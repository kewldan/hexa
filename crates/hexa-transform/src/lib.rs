//! Data transformations for Hexa: bit ops, arithmetic, encodings,
//! compression, encryption, recipes and node graphs (XFRM-01..XFRM-12).
//!
//! Every transformation works on a materialised selection (`&[u8]`) and
//! returns a new buffer; nothing here does I/O. Operations are described by
//! the serialisable [`Step`] enum (internally tagged with `kind`, camelCase
//! fields) so the UI can build, save and load [`Recipe`]s and [`Graph`]s as
//! JSON.
//!
//! | Spec    | Module                                   |
//! |---------|------------------------------------------|
//! | XFRM-01 | [`bitwise`]                              |
//! | XFRM-02 | [`arith`]                                |
//! | XFRM-03 | [`xor`]                                  |
//! | XFRM-04 | [`reorder`]                              |
//! | XFRM-05 | [`codec`]                                |
//! | XFRM-06 | [`compress`]                             |
//! | XFRM-07 | [`crypto`]                               |
//! | XFRM-08 | [`recode`]                               |
//! | XFRM-09 | [`Step::invert`], [`reverse_apply`]      |
//! | XFRM-10 | [`Recipe`], [`apply`]                    |
//! | XFRM-11 | [`graph`]                                |
//! | XFRM-12 | [`preview`]                              |

pub mod arith;
pub mod bitwise;
pub mod codec;
pub mod compress;
pub mod crypto;
pub mod error;
pub mod graph;
pub mod recipe;
pub mod recode;
pub mod reorder;
pub mod types;
pub mod xor;

pub use arith::ArithOp;
pub use bitwise::{BitOp, Direction};
pub use codec::{Base32Alphabet, Base64Alphabet, Codec};
pub use compress::Compression;
pub use crypto::{AesMode, BlockMode, Cipher, Padding};
pub use error::{Result, TransformError};
pub use graph::{Edge, Graph, Node, NodeKind, evaluate};
pub use recipe::{Preview, Recipe, Step, apply, apply_steps, preview, reverse_apply};
pub use types::{Endian, HexBytes, MAX_OUTPUT_LEN, Width};
pub use xor::{
    GuessMethod, KeySizeScore, XorKeyCandidate, estimate_key_sizes, guess_repeating_xor,
    guess_single_byte_xor,
};
