//! SFTP over SSH provider (CORE-02 / FILE-16).
//!
//! The SSH client (`russh` + `russh-sftp`) runs on an internal Tokio runtime
//! owned by the provider, so the [`Provider`] surface stays synchronous and
//! can be called from any thread, including from inside another runtime.
//! Reads go through a paged LRU cache; a page is fetched with pipelined SFTP
//! `READ`s. When opened writable, `write_at` / `resize` / `flush` map to SFTP
//! `WRITE` / `FSETSTAT(size)` / `fsync@openssh.com`.

use std::fmt;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hexa_core::provider::{Capabilities, Provider};
use parking_lot::Mutex;
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use serde::{Deserialize, Serialize};

use crate::cache::{CacheOptions, PageCache};
use crate::error::{Error, Result};

/// How to authenticate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum SftpAuth {
    Password {
        password: String,
    },
    /// Private key text (OpenSSH `-----BEGIN OPENSSH PRIVATE KEY-----`,
    /// `PKCS#8` or `PuTTY` PPK), optionally encrypted with `passphrase`.
    PrivateKey {
        key: String,
        #[serde(default)]
        passphrase: Option<String>,
    },
}

impl fmt::Debug for SftpAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password { .. } => f.debug_struct("Password").finish_non_exhaustive(),
            Self::PrivateKey { .. } => f.debug_struct("PrivateKey").finish_non_exhaustive(),
        }
    }
}

fn default_port() -> u16 {
    22
}

fn default_timeout() -> u64 {
    20_000
}

/// Where the file is and how to reach it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpOptions {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub auth: SftpAuth,
    /// Remote path of the file.
    pub path: String,
    /// Open the file read-write.
    #[serde(default)]
    pub writable: bool,
    /// Timeout of the connection and of each request.
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub cache: CacheOptions,
}

/// The server key presented during the handshake, given to the host key
/// callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyInfo {
    pub host: String,
    pub port: u16,
    /// `ssh-ed25519`, `rsa-sha2-256`, …
    pub algorithm: String,
    /// OpenSSH style `SHA256:<base64>` fingerprint.
    pub fingerprint: String,
}

/// Host key callback: returns whether the key is trusted.
pub type HostKeyCheck = Arc<dyn Fn(&HostKeyInfo) -> bool + Send + Sync>;

/// A host key callback accepting exactly one fingerprint (`SHA256:…`).
pub fn pinned_fingerprint(fingerprint: impl Into<String>) -> HostKeyCheck {
    let expected = fingerprint.into();
    Arc::new(move |info: &HostKeyInfo| info.fingerprint == expected)
}

/// Positional file access of an SFTP session. [`SftpProvider`] adds caching
/// and bookkeeping on top; tests and alternative transports implement it.
pub trait SftpTransport: Send + Sync + fmt::Debug {
    /// Current file size.
    fn size(&self) -> io::Result<u64>;
    /// Fills `buf` from `offset`; returns fewer bytes only at end of file.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()>;
    fn set_len(&self, len: u64) -> io::Result<()>;
    /// Asks the server to persist written data (no-op when unsupported).
    fn sync(&self) -> io::Result<()>;
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// A file on an SSH server accessed through SFTP (CORE-02 / FILE-16).
#[derive(Debug)]
pub struct SftpProvider {
    transport: Box<dyn SftpTransport>,
    name: String,
    writable: bool,
    len: AtomicU64,
    cache: PageCache,
}

impl SftpProvider {
    /// Connects, verifies the host key with `check`, authenticates and opens
    /// `options.path`. A refused key fails with
    /// [`Error::HostKeyRejected`] carrying the fingerprint to show the user.
    pub fn connect(options: &SftpOptions, check: HostKeyCheck) -> Result<Self> {
        let transport = RusshTransport::connect(options, check)?;
        let name = options
            .path
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(&options.path)
            .to_owned();
        Ok(Self::with_transport(
            Box::new(transport),
            format!("{name} ({}@{})", options.user, options.host),
            options.writable,
            options.cache,
        )?)
    }

    /// Wraps an already open transport.
    pub fn with_transport(
        transport: Box<dyn SftpTransport>,
        name: impl Into<String>,
        writable: bool,
        cache: CacheOptions,
    ) -> io::Result<Self> {
        let len = transport.size()?;
        Ok(Self {
            transport,
            name: name.into(),
            writable,
            len: AtomicU64::new(len),
            cache: PageCache::new(cache),
        })
    }

    /// Re-reads the remote size; drops the cache when the file shrank.
    pub fn refresh_len(&self) -> io::Result<u64> {
        let len = self.transport.size()?;
        let old = self.len.swap(len, Ordering::SeqCst);
        if len < old {
            self.cache.clear();
        } else if len > old {
            // The last page may have been cached short.
            let ps = self.cache.page_size() as u64;
            self.cache.forget(old / ps * ps, 1);
        }
        Ok(len)
    }

    fn check_writable(&self) -> io::Result<()> {
        if self.writable {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only"))
        }
    }
}

impl Provider for SftpProvider {
    fn len(&self) -> u64 {
        self.len.load(Ordering::SeqCst)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.cache.read(self.len(), offset, buf, &|off, out| {
            let n = self.transport.read_at(off, out)?;
            if n < out.len() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "remote file is shorter than expected",
                ));
            }
            Ok(())
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            readable: true,
            writable: self.writable,
            resizable: self.writable,
            sector_size: None,
            live: false,
        }
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.check_writable()?;
        self.transport.write_at(offset, data)?;
        self.cache.forget(offset, data.len() as u64);
        let end = offset + data.len() as u64;
        let old = self.len.fetch_max(end, Ordering::SeqCst);
        if end > old {
            let ps = self.cache.page_size() as u64;
            self.cache.forget(old / ps * ps, 1);
        }
        Ok(())
    }

    fn resize(&self, len: u64) -> io::Result<()> {
        self.check_writable()?;
        self.transport.set_len(len)?;
        self.len.store(len, Ordering::SeqCst);
        self.cache.clear();
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        if self.writable {
            self.transport.sync()
        } else {
            Ok(())
        }
    }

    fn invalidate(&self) {
        self.cache.clear();
    }
}

// ---------------------------------------------------------------------------
// russh transport
// ---------------------------------------------------------------------------

/// Bytes per SFTP `READ` / `WRITE` when the server announces no limits
/// (the size every server must accept).
const DEFAULT_CHUNK: u64 = 32 * 1024;
/// Upper bound of pipelined chunk requests in flight.
const MAX_IN_FLIGHT: usize = 16;

struct ClientHandler {
    host: String,
    port: u16,
    check: HostKeyCheck,
    /// The key seen and whether it was refused.
    seen: Arc<Mutex<Option<(HostKeyInfo, bool)>>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> impl Future<Output = std::result::Result<bool, Self::Error>> + Send {
        let result = {
            let (algorithm, fingerprint) = match key {
                PublicKeyOrCertificate::PublicKey { key, .. } => (
                    key.algorithm().as_str().to_owned(),
                    key.fingerprint(HashAlg::Sha256).to_string(),
                ),
                PublicKeyOrCertificate::Certificate(cert) => (
                    cert.algorithm().as_str().to_owned(),
                    cert.public_key().fingerprint(HashAlg::Sha256).to_string(),
                ),
            };
            let info = HostKeyInfo {
                host: self.host.clone(),
                port: self.port,
                algorithm,
                fingerprint,
            };
            let ok = (self.check)(&info);
            *self.seen.lock() = Some((info, ok));
            Ok(ok)
        };
        std::future::ready(result)
    }
}

/// Live SFTP session plus the runtime driving it.
struct RusshTransport {
    rt: Option<tokio::runtime::Runtime>,
    session: Arc<client::Handle<ClientHandler>>,
    sftp: Arc<RawSftpSession>,
    handle: Arc<str>,
    timeout: Duration,
    read_chunk: u64,
    write_chunk: u64,
}

impl fmt::Debug for RusshTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RusshTransport")
            .field("handle", &self.handle)
            .field("timeout", &self.timeout)
            .field("read_chunk", &self.read_chunk)
            .finish_non_exhaustive()
    }
}

#[expect(clippy::needless_pass_by_value, reason = "map_err adapter")]
fn ssh_err(e: russh::Error) -> Error {
    Error::Ssh(e.to_string())
}

#[expect(clippy::needless_pass_by_value, reason = "map_err adapter")]
fn sftp_err(e: SftpError) -> Error {
    match &e {
        SftpError::Status(s) if s.status_code == StatusCode::NoSuchFile => {
            Error::Io(io::Error::new(io::ErrorKind::NotFound, e.to_string()))
        }
        SftpError::Status(s) if s.status_code == StatusCode::PermissionDenied => Error::Io(
            io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()),
        ),
        SftpError::Timeout => Error::Io(io::Error::new(io::ErrorKind::TimedOut, e.to_string())),
        _ => Error::Sftp(e.to_string()),
    }
}

fn is_status(e: &SftpError, code: StatusCode) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == code)
}

/// Runs `fut` on `rt` and waits for it from the calling (non-async) thread.
fn run_on<T, F>(rt: &tokio::runtime::Runtime, timeout: Duration, fut: F) -> Result<T>
where
    T: Send + 'static,
    F: Future<Output = Result<T>> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    rt.spawn(async move {
        let r = tokio::time::timeout(timeout, fut)
            .await
            .unwrap_or_else(|_| {
                Err(Error::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "SFTP request timed out",
                )))
            });
        let _ = tx.send(r);
    });
    rx.recv()
        .map_err(|_| Error::Sftp("SFTP runtime stopped".into()))?
}

/// Reads up to `len` bytes at `offset`, looping over short replies.
async fn read_exactish(
    sftp: Arc<RawSftpSession>,
    handle: Arc<str>,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(len as usize);
    while (out.len() as u64) < len {
        let want = (len - out.len() as u64) as u32;
        match sftp.read(&*handle, offset + out.len() as u64, want).await {
            Ok(data) if data.data.is_empty() => break,
            Ok(data) => out.extend_from_slice(&data.data[..data.data.len().min(want as usize)]),
            Err(e) if is_status(&e, StatusCode::Eof) => break,
            Err(e) => return Err(sftp_err(e)),
        }
    }
    Ok(out)
}

impl RusshTransport {
    fn connect(options: &SftpOptions, check: HostKeyCheck) -> Result<Self> {
        // Decode the key first so a typo fails before any network traffic.
        let key = match &options.auth {
            SftpAuth::PrivateKey { key, passphrase } => Some(Arc::new(
                russh::keys::decode_secret_key(key, passphrase.as_deref())
                    .map_err(|e| Error::InvalidKey(e.to_string()))?,
            )),
            SftpAuth::Password { .. } => None,
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("hexa-sftp")
            .enable_all()
            .build()?;
        let timeout = Duration::from_millis(options.timeout_ms.max(1));
        let seen = Arc::new(Mutex::new(None));
        let handler = ClientHandler {
            host: options.host.clone(),
            port: options.port,
            check,
            seen: seen.clone(),
        };
        let opts = options.clone();
        let connected = run_on(&rt, timeout, async move {
            let config = Arc::new(client::Config {
                keepalive_interval: Some(Duration::from_secs(30)),
                ..client::Config::default()
            });
            let mut session = client::connect(config, (opts.host.clone(), opts.port), handler)
                .await
                .map_err(|e| match seen.lock().clone() {
                    Some((info, false)) => Error::HostKeyRejected {
                        host: format!("{}:{}", info.host, info.port),
                        fingerprint: info.fingerprint,
                    },
                    _ => ssh_err(e),
                })?;
            let ok = match (&opts.auth, key) {
                (SftpAuth::Password { password }, _) => session
                    .authenticate_password(opts.user.clone(), password.clone())
                    .await
                    .map_err(ssh_err)?
                    .success(),
                (SftpAuth::PrivateKey { .. }, Some(key)) => {
                    let hash = if key.algorithm().is_rsa() {
                        session
                            .best_supported_rsa_hash()
                            .await
                            .map_err(ssh_err)?
                            .flatten()
                    } else {
                        None
                    };
                    session
                        .authenticate_publickey(
                            opts.user.clone(),
                            PrivateKeyWithHashAlg::new(key, hash),
                        )
                        .await
                        .map_err(ssh_err)?
                        .success()
                }
                (SftpAuth::PrivateKey { .. }, None) => unreachable!("key decoded above"),
            };
            if !ok {
                return Err(Error::AuthFailed { user: opts.user });
            }
            let channel = session.channel_open_session().await.map_err(ssh_err)?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(ssh_err)?;
            let mut sftp = RawSftpSession::new(channel.into_stream());
            sftp.set_timeout(opts.timeout_ms.div_ceil(1000).max(1));
            let version = sftp.init().await.map_err(sftp_err)?;
            let mut read_chunk = DEFAULT_CHUNK;
            let mut write_chunk = DEFAULT_CHUNK;
            if version.extensions.contains_key("limits@openssh.com")
                && let Ok(limits) = sftp.limits().await
            {
                if limits.max_read_len > 0 {
                    read_chunk = limits.max_read_len.min(1 << 20);
                }
                if limits.max_write_len > 0 {
                    write_chunk = limits.max_write_len.min(1 << 20);
                }
                sftp.set_limits(limits.into());
            }
            let mut flags = OpenFlags::READ;
            if opts.writable {
                flags |= OpenFlags::WRITE;
            }
            let handle = sftp
                .open(opts.path.clone(), flags, FileAttributes::default())
                .await
                .map_err(sftp_err)?
                .handle;
            Ok((session, sftp, handle, read_chunk, write_chunk))
        });
        let (session, sftp, handle, read_chunk, write_chunk) = match connected {
            Ok(v) => v,
            Err(e) => {
                rt.shutdown_background();
                return Err(e);
            }
        };
        Ok(Self {
            rt: Some(rt),
            session: Arc::new(session),
            sftp: Arc::new(sftp),
            handle: handle.into(),
            timeout,
            read_chunk,
            write_chunk,
        })
    }

    fn run<T, F>(&self, fut: F) -> io::Result<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let rt = self
            .rt
            .as_ref()
            .expect("runtime lives as long as the transport");
        run_on(rt, self.timeout, fut).map_err(io::Error::from)
    }
}

impl SftpTransport for RusshTransport {
    fn size(&self) -> io::Result<u64> {
        let (sftp, handle) = (self.sftp.clone(), self.handle.clone());
        self.run(async move {
            let attrs = sftp.fstat(&*handle).await.map_err(sftp_err)?;
            attrs
                .attrs
                .size
                .ok_or_else(|| Error::Sftp("server does not report the file size".into()))
        })
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let (sftp, handle) = (self.sftp.clone(), self.handle.clone());
        let (len, chunk) = (buf.len() as u64, self.read_chunk);
        let data = self.run(async move {
            // Pipeline the chunk requests, keeping a bounded window in flight.
            let mut out = Vec::with_capacity(len as usize);
            let mut next = 0u64;
            let mut pending = std::collections::VecDeque::new();
            loop {
                while next < len && pending.len() < MAX_IN_FLIGHT {
                    let n = chunk.min(len - next);
                    pending.push_back(tokio::spawn(read_exactish(
                        sftp.clone(),
                        handle.clone(),
                        offset + next,
                        n,
                    )));
                    next += n;
                }
                let Some(task) = pending.pop_front() else {
                    break;
                };
                let part = task
                    .await
                    .map_err(|e| Error::Sftp(format!("read task failed: {e}")))??;
                let short = (part.len() as u64) < chunk.min(len - out.len() as u64);
                out.extend_from_slice(&part);
                if short {
                    for t in pending {
                        t.abort();
                    }
                    break;
                }
            }
            Ok(out)
        })?;
        buf[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        let (sftp, handle) = (self.sftp.clone(), self.handle.clone());
        let chunk = self.write_chunk as usize;
        let data = data.to_vec();
        self.run(async move {
            for (i, part) in data.chunks(chunk).enumerate() {
                sftp.write(&*handle, offset + (i * chunk) as u64, part.to_vec())
                    .await
                    .map_err(sftp_err)?;
            }
            Ok(())
        })
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        let (sftp, handle) = (self.sftp.clone(), self.handle.clone());
        self.run(async move {
            let attrs = FileAttributes {
                size: Some(len),
                ..FileAttributes::empty()
            };
            sftp.fsetstat(&*handle, attrs).await.map_err(sftp_err)?;
            Ok(())
        })
    }

    fn sync(&self) -> io::Result<()> {
        let (sftp, handle) = (self.sftp.clone(), self.handle.clone());
        self.run(async move {
            match sftp.fsync(&*handle).await {
                Ok(_) => Ok(()),
                Err(e) if is_status(&e, StatusCode::OpUnsupported) => Ok(()),
                Err(e) => Err(sftp_err(e)),
            }
        })
    }
}

impl Drop for RusshTransport {
    fn drop(&mut self) {
        let Some(rt) = self.rt.take() else {
            return;
        };
        let (sftp, handle, session) =
            (self.sftp.clone(), self.handle.clone(), self.session.clone());
        // Best effort goodbye; never block longer than a moment.
        let _ = run_on(&rt, Duration::from_secs(2), async move {
            let _ = sftp.close(&*handle).await;
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            Ok(())
        });
        rt.shutdown_background();
    }
}

#[cfg(test)]
mod tests;
