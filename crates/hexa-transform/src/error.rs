//! Error type of the transformation engine.

/// Result alias used across `hexa-transform`.
pub type Result<T, E = TransformError> = std::result::Result<T, E>;

/// Every failure a transformation can report. Messages are meant to be shown
/// to the user as-is.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransformError {
    /// A step parameter is out of range or inconsistent.
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
    /// A cipher key has the wrong length.
    #[error("{algorithm} key must be {expected} bytes, got {actual}")]
    InvalidKeyLength {
        algorithm: &'static str,
        expected: &'static str,
        actual: usize,
    },
    /// A cipher IV or nonce has the wrong length.
    #[error("{algorithm} IV/nonce must be {expected} bytes, got {actual}")]
    InvalidIvLength {
        algorithm: &'static str,
        expected: &'static str,
        actual: usize,
    },
    /// Block cipher without padding got data that is not block aligned.
    #[error(
        "input length {len} is not a multiple of the {block}-byte block size \
         (choose PKCS#7 padding or select whole blocks)"
    )]
    NotBlockAligned { len: usize, block: usize },
    /// PKCS#7 padding is missing or malformed after decryption.
    #[error(
        "bad padding: decrypted data does not end with valid PKCS#7 padding (wrong key or IV?)"
    )]
    BadPadding,
    /// AEAD tag verification failed.
    #[error(
        "authentication failed: the GCM tag does not match \
         (wrong key, nonce or AAD, or the data is corrupted)"
    )]
    AuthenticationFailed,
    /// Text-to-binary decoding (Base64, hex, ...) failed.
    #[error("invalid {codec} input: {reason}")]
    InvalidEncoding { codec: &'static str, reason: String },
    /// Text-to-binary encoding is impossible for this input.
    #[error("cannot encode as {codec}: {reason}")]
    CannotEncode { codec: &'static str, reason: String },
    /// A compressed stream is corrupt, truncated or of the wrong format.
    #[error("corrupt or truncated {format} stream: {reason}")]
    CorruptStream {
        format: &'static str,
        reason: String,
    },
    /// Compression failed.
    #[error("{format} compression failed: {reason}")]
    CompressionFailed {
        format: &'static str,
        reason: String,
    },
    /// The result would exceed the in-memory output limit.
    #[error("output exceeds the limit of {limit} bytes")]
    OutputTooLarge { limit: usize },
    /// Integer division or remainder by zero.
    #[error("division by zero")]
    DivisionByZero,
    /// Text recoding failed (unknown encoding or unrepresentable character).
    #[error("text encoding error: {0}")]
    Text(String),
    /// The step (or recipe) cannot be inverted for write-back (XFRM-09).
    #[error("not reversible: {0}")]
    NotReversible(String),
    /// A recipe step failed; `index` is zero-based.
    #[error("step {} ({name}): {source}", index + 1)]
    Step {
        index: usize,
        name: &'static str,
        #[source]
        source: Box<TransformError>,
    },
    /// The node graph is malformed (XFRM-11).
    #[error("invalid graph: {0}")]
    Graph(String),
    /// A graph node failed during evaluation.
    #[error("node '{id}': {source}")]
    Node {
        id: String,
        #[source]
        source: Box<TransformError>,
    },
    /// Recipe or graph JSON could not be parsed.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl TransformError {
    pub(crate) fn param(msg: impl Into<String>) -> Self {
        Self::InvalidParameter(msg.into())
    }

    pub(crate) fn encoding(codec: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidEncoding {
            codec,
            reason: reason.into(),
        }
    }

    pub(crate) fn corrupt(format: &'static str, reason: impl std::fmt::Display) -> Self {
        Self::CorruptStream {
            format,
            reason: reason.to_string(),
        }
    }
}
