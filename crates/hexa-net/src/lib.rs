//! Remote and container sources for Hexa (CORE-02, FILE-15, FILE-16).
//!
//! Every source implements [`hexa_core::provider::Provider`], so a document
//! can be opened on it exactly like on a local file:
//!
//! * [`http::HttpProvider`]: HTTP(S) object read through `Range` requests
//!   (CORE-02 / FILE-16).
//! * [`s3::S3Provider`]: object of an S3-compatible store, requests signed
//!   with AWS `SigV4` ([`sigv4`]) (FILE-16).
//! * [`sftp::SftpProvider`]: file on an SSH server read and written through
//!   SFTP at offsets (CORE-02 / FILE-16).
//! * [`stream::StreamProvider`]: live buffer filled by a UDP listener or a TCP
//!   client / server (CORE-02).
//! * [`archive::Archive`]: entries of zip, tar, tar.gz, gz and 7z archives,
//!   opened as providers and written back by repacking (FILE-15).
//!
//! Remote providers keep an LRU page cache ([`cache`]) and fetch on demand.

pub mod archive;
pub mod cache;
pub mod error;
pub mod http;
pub mod s3;
pub mod sftp;
pub mod sigv4;
pub mod stream;
mod time;

#[cfg(test)]
mod testutil;

pub use archive::{Archive, ArchiveEntry, ArchiveKind};
pub use cache::CacheOptions;
pub use error::{Error, Result};
pub use http::{HttpAuth, HttpOptions, HttpProvider, RemoteInfo, TransferOptions};
pub use s3::{S3Options, S3Provider};
pub use sftp::{HostKeyInfo, SftpAuth, SftpOptions, SftpProvider, SftpTransport};
pub use stream::{StreamOptions, StreamProvider, StreamSource, StreamState, StreamStatus};
