//! SFTP tests: the provider against a mocked transport, and the full SSH +
//! SFTP stack against an in-process `russh` server.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use parking_lot::Mutex;
use russh::keys::ssh_key::LineEnding;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{HashAlg, PrivateKey};
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{Attrs, Data, FileAttributes, Handle, OpenFlags, Status, StatusCode};

use super::*;

// ---------------------------------------------------------------------------
// Mocked transport
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct MockTransport {
    data: Mutex<Vec<u8>>,
    reads: AtomicUsize,
}

impl SftpTransport for Arc<MockTransport> {
    fn size(&self) -> io::Result<u64> {
        Ok(self.data.lock().len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let data = self.data.lock();
        let start = (offset as usize).min(data.len());
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn write_at(&self, offset: u64, src: &[u8]) -> io::Result<()> {
        let mut data = self.data.lock();
        let end = offset as usize + src.len();
        if end > data.len() {
            data.resize(end, 0);
        }
        data[offset as usize..end].copy_from_slice(src);
        Ok(())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.data.lock().resize(len as usize, 0);
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        Ok(())
    }
}

fn small_cache() -> CacheOptions {
    CacheOptions {
        page_size: 4096,
        budget: 64 * 4096,
    }
}

#[test]
fn provider_caches_and_writes_through() {
    let mock = Arc::new(MockTransport {
        data: Mutex::new((0..20_000u32).map(|i| i as u8).collect()),
        reads: AtomicUsize::new(0),
    });
    let p = SftpProvider::with_transport(Box::new(mock.clone()), "f", true, small_cache()).unwrap();
    let mut buf = [0u8; 100];
    p.read_at(4000, &mut buf).unwrap();
    p.read_at(4050, &mut buf).unwrap();
    assert_eq!(mock.reads.load(Ordering::SeqCst), 1);
    assert_eq!(buf[0], (4050u32 % 256) as u8);

    p.write_at(4060, b"XY").unwrap();
    p.read_at(4060, &mut buf[..2]).unwrap();
    assert_eq!(&buf[..2], b"XY", "written pages are refetched");

    p.write_at(20_000, b"tail").unwrap();
    assert_eq!(p.len(), 20_004);
    p.read_at(19_998, &mut buf[..6]).unwrap();
    assert_eq!(&buf[2..6], b"tail");

    p.resize(10).unwrap();
    assert_eq!(p.len(), 10);
    assert!(p.read_at(5, &mut buf[..10]).is_err());

    mock.data.lock().extend_from_slice(b"grown");
    assert_eq!(p.refresh_len().unwrap(), 15);
    p.read_at(10, &mut buf[..5]).unwrap();
    assert_eq!(&buf[..5], b"grown");
}

#[test]
fn read_only_provider_refuses_writes() {
    let mock = Arc::new(MockTransport::default());
    let p = SftpProvider::with_transport(Box::new(mock), "f", false, small_cache()).unwrap();
    assert!(!p.capabilities().writable);
    assert_eq!(
        p.write_at(0, b"x").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(p.resize(1).is_err());
}

// ---------------------------------------------------------------------------
// In-process SSH + SFTP server
// ---------------------------------------------------------------------------

const PATH: &str = "/srv/data.bin";

fn keypair(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

/// In-memory SFTP subsystem serving one file at [`PATH`]. Reads return at
/// most 1000 bytes to exercise short-read handling.
struct MemSftp {
    file: Arc<Mutex<Vec<u8>>>,
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

impl russh_sftp::server::Handler for MemSftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    fn open(
        &mut self,
        id: u32,
        filename: String,
        _pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> impl Future<Output = std::result::Result<Handle, Self::Error>> + Send {
        let result = {
            if filename == PATH {
                Ok(Handle {
                    id,
                    handle: "h".into(),
                })
            } else {
                Err(StatusCode::NoSuchFile)
            }
        };
        std::future::ready(result)
    }

    fn close(
        &mut self,
        id: u32,
        _handle: String,
    ) -> impl Future<Output = std::result::Result<Status, Self::Error>> + Send {
        let result = Ok(ok(id));
        std::future::ready(result)
    }

    fn read(
        &mut self,
        id: u32,
        _handle: String,
        offset: u64,
        len: u32,
    ) -> impl Future<Output = std::result::Result<Data, Self::Error>> + Send {
        let result = {
            let file = self.file.lock();
            let start = offset as usize;
            if start >= file.len() {
                Err(StatusCode::Eof)
            } else {
                let end = file.len().min(start + (len as usize).min(1000));
                Ok(Data {
                    id,
                    data: file[start..end].to_vec(),
                })
            }
        };
        std::future::ready(result)
    }

    fn write(
        &mut self,
        id: u32,
        _handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> impl Future<Output = std::result::Result<Status, Self::Error>> + Send {
        let result = {
            let mut file = self.file.lock();
            let end = offset as usize + data.len();
            if end > file.len() {
                file.resize(end, 0);
            }
            file[offset as usize..end].copy_from_slice(&data);
            Ok(ok(id))
        };
        std::future::ready(result)
    }

    fn fstat(
        &mut self,
        id: u32,
        _handle: String,
    ) -> impl Future<Output = std::result::Result<Attrs, Self::Error>> + Send {
        let result = {
            Ok(Attrs {
                id,
                attrs: FileAttributes {
                    size: Some(self.file.lock().len() as u64),
                    ..FileAttributes::empty()
                },
            })
        };
        std::future::ready(result)
    }

    fn fsetstat(
        &mut self,
        id: u32,
        _handle: String,
        attrs: FileAttributes,
    ) -> impl Future<Output = std::result::Result<Status, Self::Error>> + Send {
        let result = {
            if let Some(size) = attrs.size {
                self.file.lock().resize(size as usize, 0);
            }
            Ok(ok(id))
        };
        std::future::ready(result)
    }
}

struct SshSession {
    file: Arc<Mutex<Vec<u8>>>,
    client_key: russh::keys::PublicKey,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl russh::server::Handler for SshSession {
    type Error = russh::Error;

    fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> impl Future<Output = std::result::Result<Auth, Self::Error>> + Send {
        let result = {
            Ok(if user == "alice" && password == "s3cret" {
                Auth::Accept
            } else {
                Auth::reject()
            })
        };
        std::future::ready(result)
    }

    fn auth_publickey(
        &mut self,
        user: &str,
        key: russh::server::VerifiedPublicKey<'_>,
    ) -> impl Future<Output = std::result::Result<Auth, Self::Error>> + Send {
        let result = {
            Ok(
                if user == "alice" && key.public_key.key_data() == self.client_key.key_data() {
                    Auth::Accept
                } else {
                    Auth::reject()
                },
            )
        };
        std::future::ready(result)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> impl Future<Output = std::result::Result<(), Self::Error>> + Send {
        std::future::ready(session.close(channel))
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        match (name, self.channels.remove(&channel_id)) {
            ("sftp", Some(channel)) => {
                session.channel_success(channel_id)?;
                let handler = MemSftp {
                    file: self.file.clone(),
                };
                russh_sftp::server::run(channel.into_stream(), handler).await;
            }
            _ => session.channel_failure(channel_id)?,
        }
        Ok(())
    }
}

struct TestServer {
    addr: SocketAddr,
    file: Arc<Mutex<Vec<u8>>>,
    fingerprint: String,
    _rt: tokio::runtime::Runtime,
}

fn start_server(content: Vec<u8>, client_key: &PrivateKey) -> TestServer {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let host_key = keypair(7);
    let fingerprint = host_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..russh::server::Config::default()
    });
    let file = Arc::new(Mutex::new(content));
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    {
        let file = file.clone();
        let client_key = client_key.public_key().clone();
        rt.spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let handler = SshSession {
                    file: file.clone(),
                    client_key: client_key.clone(),
                    channels: HashMap::new(),
                };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(running) = russh::server::run_stream(config, socket, handler).await {
                        let _ = running.await;
                    }
                });
            }
        });
    }
    TestServer {
        addr,
        file,
        fingerprint,
        _rt: rt,
    }
}

fn options(server: &TestServer, auth: SftpAuth, writable: bool) -> SftpOptions {
    SftpOptions {
        host: server.addr.ip().to_string(),
        port: server.addr.port(),
        user: "alice".into(),
        auth,
        path: PATH.into(),
        writable,
        timeout_ms: 10_000,
        cache: small_cache(),
    }
}

#[test]
fn reads_and_writes_over_ssh_with_password() {
    let content: Vec<u8> = (0..50_000u32).map(|i| (i * 13 % 256) as u8).collect();
    let server = start_server(content.clone(), &keypair(9));
    let seen = Arc::new(Mutex::new(None));
    let check: HostKeyCheck = {
        let seen = seen.clone();
        let expected = server.fingerprint.clone();
        Arc::new(move |info: &HostKeyInfo| {
            *seen.lock() = Some(info.clone());
            info.fingerprint == expected
        })
    };
    let p = SftpProvider::connect(
        &options(
            &server,
            SftpAuth::Password {
                password: "s3cret".into(),
            },
            true,
        ),
        check,
    )
    .unwrap();
    let info = seen.lock().clone().unwrap();
    assert_eq!(info.algorithm, "ssh-ed25519");
    assert!(info.fingerprint.starts_with("SHA256:"));
    assert_eq!(p.len(), 50_000);
    assert_eq!(p.name(), format!("data.bin (alice@{})", server.addr.ip()));

    let mut buf = vec![0; 30_000];
    p.read_at(7, &mut buf).unwrap();
    assert_eq!(buf, content[7..30_007]);

    // Bulk read bypassing the cache, pipelined over many chunks.
    let mut all = vec![0; 50_000];
    p.read_at(0, &mut all).unwrap();
    assert_eq!(all, content);

    let patch = vec![0xAB; 70_000];
    p.write_at(45_000, &patch).unwrap();
    assert_eq!(p.len(), 115_000);
    let mut back = vec![0; 10];
    p.read_at(44_995, &mut back).unwrap();
    assert_eq!(&back[..5], &content[44_995..45_000]);
    assert_eq!(&back[5..], &[0xAB; 5]);
    assert_eq!(server.file.lock().len(), 115_000);

    p.resize(100).unwrap();
    p.flush().unwrap();
    assert_eq!(server.file.lock().len(), 100);
    assert_eq!(p.refresh_len().unwrap(), 100);
}

#[test]
fn authenticates_with_openssh_private_key() {
    let client = keypair(9);
    let server = start_server(b"hello sftp".to_vec(), &client);
    let pem = client.to_openssh(LineEnding::LF).unwrap().to_string();
    assert!(pem.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
    let p = SftpProvider::connect(
        &options(
            &server,
            SftpAuth::PrivateKey {
                key: pem,
                passphrase: None,
            },
            false,
        ),
        pinned_fingerprint(server.fingerprint.clone()),
    )
    .unwrap();
    let mut buf = [0; 10];
    p.read_at(0, &mut buf).unwrap();
    assert_eq!(&buf, b"hello sftp");
    assert!(!p.capabilities().writable);
}

#[test]
fn reports_rejected_host_key_and_bad_credentials() {
    let server = start_server(b"x".to_vec(), &keypair(9));
    let err = SftpProvider::connect(
        &options(
            &server,
            SftpAuth::Password {
                password: "s3cret".into(),
            },
            false,
        ),
        pinned_fingerprint("SHA256:not-the-key"),
    )
    .unwrap_err();
    match err {
        Error::HostKeyRejected { fingerprint, .. } => assert_eq!(fingerprint, server.fingerprint),
        other => panic!("unexpected {other}"),
    }

    let err = SftpProvider::connect(
        &options(
            &server,
            SftpAuth::Password {
                password: "nope".into(),
            },
            false,
        ),
        pinned_fingerprint(server.fingerprint.clone()),
    )
    .unwrap_err();
    assert!(matches!(err, Error::AuthFailed { .. }), "{err}");

    let mut opts = options(
        &server,
        SftpAuth::Password {
            password: "s3cret".into(),
        },
        false,
    );
    opts.path = "/missing".into();
    let err =
        SftpProvider::connect(&opts, pinned_fingerprint(server.fingerprint.clone())).unwrap_err();
    assert_eq!(io::Error::from(err).kind(), io::ErrorKind::NotFound);

    let err = SftpProvider::connect(
        &options(
            &server,
            SftpAuth::PrivateKey {
                key: "garbage".into(),
                passphrase: None,
            },
            false,
        ),
        pinned_fingerprint(server.fingerprint.clone()),
    )
    .unwrap_err();
    assert!(matches!(err, Error::InvalidKey(_)), "{err}");
    assert!(
        !format!(
            "{:?}",
            SftpAuth::Password {
                password: "s3cret".into()
            }
        )
        .contains("s3cret")
    );
}
