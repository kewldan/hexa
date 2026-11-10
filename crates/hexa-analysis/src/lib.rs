//! Analysis for Hexa: hashes, entropy, statistics, strings, classification,
//! visualisation, previews and firmware / dump formats.
//!
//! Everything that scans a file takes a [`hexa_core::Tree`] snapshot and
//! streams it in chunks of at most 4 MiB, honouring a [`hexa_core::Cancel`]
//! flag and reporting progress through [`hexa_core::task::Progress`].
//!
//! | Spec | Module |
//! |------|--------|
//! | ANL-03 | [`hash`] |
//! | ANL-04 | [`fuzzy`] |
//! | ANL-05 | [`blocks`] |
//! | ANL-07, ANL-08 | [`stats`] |
//! | ANL-09 | [`classify`] |
//! | ANL-10 | [`visual`] |
//! | ANL-11 | [`strings`] |
//! | ANL-15 | [`bitmap`] |
//! | ANL-16 | [`preview`], [`embedded`] |
//! | ANL-17 | [`plot`] |
//! | IO-01, IO-02 | [`firmware`] |
//! | IO-03, IO-04 | [`dump`] |

pub mod bitmap;
pub mod blocks;
pub mod classify;
pub mod dump;
pub mod embedded;
pub mod firmware;
pub mod fuzzy;
pub mod hash;
pub mod plot;
pub mod preview;
pub mod stats;
pub mod strings;
pub mod visual;

mod util;

#[cfg(test)]
mod test_util;
