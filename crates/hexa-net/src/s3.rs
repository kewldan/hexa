//! S3-compatible object provider (FILE-16).
//!
//! Works with AWS S3 and compatible stores (`MinIO`, Ceph RGW, R2, B2 …).
//! Requests are signed with AWS Signature Version 4 ([`crate::sigv4`]); the
//! object is read with ranged `GET`s through the same paged cache and retry
//! machinery as [`crate::http::HttpProvider`]. The source is read-only.

use std::fmt;
use std::io;
use std::time::SystemTime;

use hexa_core::provider::{Capabilities, Provider};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::http::{Authorizer, Ranged, RemoteInfo, TransferOptions, parse_url};
use crate::sigv4::{self, EMPTY_PAYLOAD_SHA256, SigningParams};

/// Where the object lives and how to authenticate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3Options {
    /// Service endpoint, e.g. `https://s3.eu-west-1.amazonaws.com` or
    /// `http://127.0.0.1:9000`.
    pub endpoint: String,
    /// Signing region (`us-east-1` for most compatible stores).
    pub region: String,
    pub bucket: String,
    /// Object key, unencoded (`dir/file.bin`).
    pub key: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Temporary credentials (`x-amz-security-token`).
    #[serde(default)]
    pub session_token: Option<String>,
    /// `endpoint/bucket/key` instead of `bucket.endpoint/key` (required by
    /// most self-hosted stores and by bucket names with dots).
    #[serde(default)]
    pub path_style: bool,
    #[serde(default)]
    pub transfer: TransferOptions,
}

impl fmt::Debug for S3Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Options")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("key", &self.key)
            .field("access_key_id", &self.access_key_id)
            .field("path_style", &self.path_style)
            .field("transfer", &self.transfer)
            .finish_non_exhaustive()
    }
}

impl S3Options {
    /// Object URL plus the `Host` value and canonical URI to sign.
    fn locate(&self) -> Result<(String, String, String)> {
        let endpoint = parse_url(self.endpoint.trim_end_matches('/'))?;
        if self.bucket.is_empty() || self.key.is_empty() {
            return Err(Error::InvalidUrl(format!(
                "s3://{}/{}",
                self.bucket, self.key
            )));
        }
        let scheme = endpoint.scheme_str().unwrap_or("https");
        let authority = endpoint
            .authority()
            .map(|a| a.as_str().rsplit('@').next().unwrap_or_default().to_owned())
            .ok_or_else(|| Error::InvalidUrl(self.endpoint.clone()))?;
        let prefix = endpoint.path().trim_end_matches('/');
        let key = sigv4::uri_encode(&self.key, true);
        let (host, path) = if self.path_style {
            (
                authority,
                format!("{prefix}/{}/{key}", sigv4::uri_encode(&self.bucket, false)),
            )
        } else {
            (
                format!("{}.{authority}", self.bucket),
                format!("{prefix}/{key}"),
            )
        };
        Ok((format!("{scheme}://{host}{path}"), host, path))
    }
}

/// Signs each request with `SigV4` at the time it is sent.
struct S3Signer {
    host: String,
    path: String,
    region: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

impl fmt::Debug for S3Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Signer")
            .field("host", &self.host)
            .field("path", &self.path)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl Authorizer for S3Signer {
    fn headers(&self, method: &str, extra: &[(String, String)]) -> Vec<(String, String)> {
        let amz_date = sigv4::amz_date(SystemTime::now());
        let mut headers = vec![
            ("Host".to_owned(), self.host.clone()),
            (
                "x-amz-content-sha256".to_owned(),
                EMPTY_PAYLOAD_SHA256.to_owned(),
            ),
            ("x-amz-date".to_owned(), amz_date.clone()),
        ];
        if let Some(token) = &self.session_token {
            headers.push(("x-amz-security-token".to_owned(), token.clone()));
        }
        headers.extend_from_slice(extra);
        let sig = sigv4::sign(
            method,
            &self.path,
            "",
            &headers,
            EMPTY_PAYLOAD_SHA256,
            &SigningParams {
                access_key_id: &self.access_key_id,
                secret_access_key: &self.secret_access_key,
                region: &self.region,
                service: "s3",
                amz_date: &amz_date,
            },
        );
        headers.push(("Authorization".to_owned(), sig.authorization));
        headers
    }
}

/// Read-only object of an S3-compatible store (FILE-16).
#[derive(Debug)]
pub struct S3Provider {
    inner: Ranged,
    name: String,
}

impl S3Provider {
    /// Stats the object (`HEAD`) and returns a provider reading it on demand.
    pub fn open(options: &S3Options) -> Result<Self> {
        let (url, host, path) = options.locate()?;
        let signer = S3Signer {
            host,
            path,
            region: options.region.clone(),
            access_key_id: options.access_key_id.clone(),
            secret_access_key: options.secret_access_key.clone(),
            session_token: options.session_token.clone(),
        };
        let inner = Ranged::open(&url, Box::new(signer), options.transfer)?;
        let name = options
            .key
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(&options.key)
            .to_owned();
        Ok(Self { inner, name })
    }

    /// Size and validators reported by the store.
    pub fn info(&self) -> &RemoteInfo {
        self.inner.info()
    }
}

impl Provider for S3Provider {
    fn len(&self) -> u64 {
        self.inner.info().len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn invalidate(&self) {
        self.inner.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheOptions;
    use crate::testutil::{HttpServer, Request, Response, serve_range};

    const ACCESS: &str = "AKIAIOSFODNN7EXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    /// Recomputes the signature the way S3 does and compares.
    fn verify(req: &Request) -> bool {
        let Some(auth) = req.header("authorization") else {
            return false;
        };
        let Some(signed) = auth
            .split("SignedHeaders=")
            .nth(1)
            .and_then(|s| s.split(',').next())
        else {
            return false;
        };
        let headers: Vec<(String, String)> = signed
            .split(';')
            .map(|name| (name.to_owned(), req.header(name).unwrap_or("").to_owned()))
            .collect();
        let date = req.header("x-amz-date").unwrap_or_default();
        let (path, query) = req.target.split_once('?').unwrap_or((&req.target, ""));
        let sig = sigv4::sign(
            &req.method,
            path,
            query,
            &headers,
            req.header("x-amz-content-sha256").unwrap_or_default(),
            &SigningParams {
                access_key_id: ACCESS,
                secret_access_key: SECRET,
                region: "us-east-1",
                service: "s3",
                amz_date: date,
            },
        );
        sig.authorization == auth && headers.iter().any(|(k, _)| k == "host")
    }

    #[test]
    fn signed_ranged_reads() {
        let body: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();
        let server = HttpServer::start({
            let body = body.clone();
            move |req: &Request| {
                if !verify(req) {
                    return Response::status(403);
                }
                if req.target != "/my-bucket/dir/a%20b.bin" {
                    return Response::status(404);
                }
                serve_range(req, &body, true).with_header("ETag", "\"abc\"")
            }
        });
        let mut opts = S3Options {
            endpoint: server.base(),
            region: "us-east-1".into(),
            bucket: "my-bucket".into(),
            key: "dir/a b.bin".into(),
            access_key_id: ACCESS.into(),
            secret_access_key: SECRET.into(),
            session_token: None,
            path_style: true,
            transfer: TransferOptions {
                retry_delay_ms: 1,
                cache: CacheOptions {
                    page_size: 8192,
                    budget: 1 << 20,
                },
                ..TransferOptions::default()
            },
        };
        let p = S3Provider::open(&opts).unwrap();
        assert_eq!(p.len(), body.len() as u64);
        assert_eq!(p.name(), "a b.bin");
        assert_eq!(p.info().etag.as_deref(), Some("\"abc\""));
        let mut buf = vec![0; 20_000];
        p.read_at(12_345, &mut buf).unwrap();
        assert_eq!(buf, body[12_345..32_345]);
        assert!(!format!("{opts:?}").contains(SECRET));

        opts.secret_access_key = "wrong".into();
        let err = S3Provider::open(&opts).unwrap_err();
        assert!(
            matches!(err, Error::HttpStatus { status: 403, .. }),
            "{err}"
        );
    }

    #[test]
    fn virtual_hosted_urls() {
        let opts = S3Options {
            endpoint: "https://s3.eu-west-1.amazonaws.com/".into(),
            region: "eu-west-1".into(),
            bucket: "b".into(),
            key: "x/y+z".into(),
            access_key_id: ACCESS.into(),
            secret_access_key: SECRET.into(),
            session_token: Some("tok".into()),
            path_style: false,
            transfer: TransferOptions::default(),
        };
        let (url, host, path) = opts.locate().unwrap();
        assert_eq!(url, "https://b.s3.eu-west-1.amazonaws.com/x/y%2Bz");
        assert_eq!(host, "b.s3.eu-west-1.amazonaws.com");
        assert_eq!(path, "/x/y%2Bz");
        let signer = S3Signer {
            host,
            path,
            region: opts.region.clone(),
            access_key_id: ACCESS.into(),
            secret_access_key: SECRET.into(),
            session_token: opts.session_token.clone(),
        };
        let headers = signer.headers("GET", &[("Range".into(), "bytes=0-9".into())]);
        let auth = &headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .unwrap()
            .1;
        assert!(auth.contains(
            "SignedHeaders=host;range;x-amz-content-sha256;x-amz-date;x-amz-security-token"
        ));
        assert!(auth.contains("/eu-west-1/s3/aws4_request"));
    }
}
