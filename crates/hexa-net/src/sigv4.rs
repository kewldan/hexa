//! AWS Signature Version 4 (`SigV4`) request signing (FILE-16, S3-compatible sources).
//!
//! Implements the header-based variant of
//! <https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html>
//! with HMAC-SHA256.

use std::fmt::Write as _;
use std::time::SystemTime;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use crate::time::civil_from_unix;

/// Hex SHA-256 of the empty payload.
pub const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// Payload hash value telling S3 the body is not signed.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// Credentials and scope of one signature.
#[derive(Clone, Copy)]
pub struct SigningParams<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    /// `YYYYMMDD'T'HHMMSS'Z'`, see [`amz_date`].
    pub amz_date: &'a str,
}

impl std::fmt::Debug for SigningParams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningParams")
            .field("access_key_id", &self.access_key_id)
            .field("region", &self.region)
            .field("service", &self.service)
            .field("amz_date", &self.amz_date)
            .finish_non_exhaustive()
    }
}

/// A computed signature with its intermediate values (handy for debugging
/// `SignatureDoesNotMatch` answers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub canonical_request: String,
    pub string_to_sign: String,
    pub signed_headers: String,
    /// Lower-case hex signature.
    pub signature: String,
    /// Value of the `Authorization` header.
    pub authorization: String,
}

/// Formats `t` as the `x-amz-date` value.
pub fn amz_date(t: SystemTime) -> String {
    let c = civil_from_unix(crate::time::unix_seconds(t));
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

/// Hex SHA-256 of `data` (the `x-amz-content-sha256` value of a body).
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// URI-encodes per the `SigV4` rules: everything but `A-Z a-z 0-9 - _ . ~` is
/// percent-encoded with upper-case hex; `/` is kept when `keep_slash`.
pub fn uri_encode(input: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else if b == b'/' && keep_slash {
            out.push('/');
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
    fn nibble(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (nibble(bytes[i + 1]), nibble(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Canonical query string: parameters decoded, re-encoded, sorted by name
/// then value.
pub fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (
                uri_encode(&percent_decode(k), false),
                uri_encode(&percent_decode(v), false),
            )
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Canonical headers block and the signed header list. Names are lower-cased,
/// values trimmed with inner whitespace runs collapsed, duplicates joined
/// with `,`.
pub fn canonical_headers(headers: &[(String, String)]) -> (String, String) {
    let mut list: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.trim().to_ascii_lowercase(),
                v.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    let mut merged: Vec<(String, String)> = Vec::with_capacity(list.len());
    for (k, v) in list {
        match merged.last_mut() {
            Some((last, value)) if *last == k => {
                value.push(',');
                value.push_str(&v);
            }
            _ => merged.push((k, v)),
        }
    }
    let block = merged.iter().fold(String::new(), |mut s, (k, v)| {
        s.push_str(k);
        s.push(':');
        s.push_str(v);
        s.push('\n');
        s
    });
    let signed = merged
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (block, signed)
}

/// Derives the signing key `kSigning` for a date (`YYYYMMDD`).
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> [u8; 32] {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Signs a request.
///
/// `canonical_uri` is the already encoded absolute path (`/bucket/key`),
/// `query` the raw query string without `?`, `headers` every header to sign
/// (it must include `host` and `x-amz-date`), `payload_hash` the hex SHA-256
/// of the body or [`UNSIGNED_PAYLOAD`].
pub fn sign(
    method: &str,
    canonical_uri: &str,
    query: &str,
    headers: &[(String, String)],
    payload_hash: &str,
    params: &SigningParams<'_>,
) -> Signature {
    let (header_block, signed_headers) = canonical_headers(headers);
    let uri = if canonical_uri.is_empty() {
        "/"
    } else {
        canonical_uri
    };
    let canonical_request = format!(
        "{method}\n{uri}\n{}\n{header_block}\n{signed_headers}\n{payload_hash}",
        canonical_query(query)
    );
    let date = &params.amz_date[..8.min(params.amz_date.len())];
    let scope = format!("{date}/{}/{}/aws4_request", params.region, params.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        params.amz_date,
        sha256_hex(canonical_request.as_bytes())
    );
    let key = signing_key(
        params.secret_access_key,
        date,
        params.region,
        params.service,
    );
    let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        params.access_key_id
    );
    Signature {
        canonical_request,
        string_to_sign,
        signed_headers,
        signature,
        authorization,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn h(k: &str, v: &str) -> (String, String) {
        (k.to_owned(), v.to_owned())
    }

    /// IAM `ListUsers` example of the AWS General Reference
    /// ("Examples of the complete Signature Version 4 signing process").
    #[test]
    fn aws_iam_example() {
        let params = SigningParams {
            access_key_id: "AKIDEXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "iam",
            amz_date: "20150830T123600Z",
        };
        let sig = sign(
            "GET",
            "/",
            "Action=ListUsers&Version=2010-05-08",
            &[
                h(
                    "Content-Type",
                    "application/x-www-form-urlencoded; charset=utf-8",
                ),
                h("Host", "iam.amazonaws.com"),
                h("X-Amz-Date", "20150830T123600Z"),
            ],
            EMPTY_PAYLOAD_SHA256,
            &params,
        );
        assert_eq!(
            sig.canonical_request,
            "GET\n/\nAction=ListUsers&Version=2010-05-08\n\
             content-type:application/x-www-form-urlencoded; charset=utf-8\n\
             host:iam.amazonaws.com\nx-amz-date:20150830T123600Z\n\n\
             content-type;host;x-amz-date\n\
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(sig.canonical_request.as_bytes()),
            "f536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59"
        );
        assert_eq!(
            hex::encode(signing_key(
                params.secret_access_key,
                "20150830",
                "us-east-1",
                "iam"
            )),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
        assert_eq!(
            sig.signature,
            "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
        assert_eq!(
            sig.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }

    /// `GET Object` with a `Range` header, from the S3 API reference
    /// ("Signature Calculations for the Authorization Header: Transferring
    /// Payload in a Single Chunk").
    #[test]
    fn aws_s3_get_object_example() {
        let params = SigningParams {
            access_key_id: "AKIAIOSFODNN7EXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "s3",
            amz_date: "20130524T000000Z",
        };
        let sig = sign(
            "GET",
            "/test.txt",
            "",
            &[
                h("Host", "examplebucket.s3.amazonaws.com"),
                h("Range", "bytes=0-9"),
                h("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256),
                h("x-amz-date", "20130524T000000Z"),
            ],
            EMPTY_PAYLOAD_SHA256,
            &params,
        );
        assert_eq!(
            sha256_hex(sig.canonical_request.as_bytes()),
            "7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972"
        );
        assert_eq!(
            sig.string_to_sign,
            "AWS4-HMAC-SHA256\n20130524T000000Z\n20130524/us-east-1/s3/aws4_request\n\
             7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972"
        );
        assert_eq!(
            sig.signature,
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
        assert_eq!(
            sig.signed_headers,
            "host;range;x-amz-content-sha256;x-amz-date"
        );
    }

    #[test]
    fn encoding_and_dates() {
        assert_eq!(uri_encode("a b/c~d+é", true), "a%20b/c~d%2B%C3%A9");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(canonical_query("b=2&a=x%20y&a=1"), "a=1&a=x%20y&b=2");
        assert_eq!(
            amz_date(UNIX_EPOCH + Duration::from_hours(380_376)),
            "20130524T000000Z"
        );
    }
}
