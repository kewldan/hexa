//! Symmetric encryption and decryption (XFRM-07), built on `RustCrypto`.
//!
//! ECB, CBC and CTR are implemented here on top of the raw block ciphers so
//! every block cipher gets every mode with identical semantics. CTR uses
//! the IV as the initial counter block, incremented big-endian over the
//! whole block (NIST SP 800-38A). AES-GCM appends the 16-byte tag to the
//! ciphertext unless a detached tag is supplied.

use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::aead::{Aead, Payload};
use cipher::{
    Block, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit, KeyIvInit, StreamCipher,
    StreamCipherSeek,
};
use serde::{Deserialize, Serialize};

use crate::error::{Result, TransformError};
use crate::types::HexBytes;

/// GCM tag length.
pub const GCM_TAG_LEN: usize = 16;

/// Block cipher mode for DES, 3DES and Blowfish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockMode {
    Ecb,
    #[default]
    Cbc,
    Ctr,
}

/// AES mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AesMode {
    Ecb,
    #[default]
    Cbc,
    Ctr,
    Gcm,
}

/// Padding for ECB and CBC (ignored by stream modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Padding {
    #[default]
    Pkcs7,
    None,
}

/// A cipher with its key material (all byte strings are hex in JSON).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Cipher {
    /// AES-128/192/256 (key length selects the variant). `iv` is the IV
    /// (CBC), initial counter block (CTR) or nonce (GCM, 12 or 16 bytes).
    Aes {
        mode: AesMode,
        key: HexBytes,
        #[serde(default)]
        iv: HexBytes,
        #[serde(default)]
        padding: Padding,
        /// GCM additional authenticated data.
        #[serde(default)]
        aad: HexBytes,
        /// GCM detached tag: decryption verifies against it instead of the
        /// last 16 input bytes and encryption omits the tag from the output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<HexBytes>,
    },
    /// Single DES, 8-byte key.
    Des {
        mode: BlockMode,
        key: HexBytes,
        #[serde(default)]
        iv: HexBytes,
        #[serde(default)]
        padding: Padding,
    },
    /// Triple DES EDE, 16-byte (K1 K2 K1) or 24-byte key.
    TripleDes {
        mode: BlockMode,
        key: HexBytes,
        #[serde(default)]
        iv: HexBytes,
        #[serde(default)]
        padding: Padding,
    },
    /// Blowfish, 4..=56-byte key, big-endian block convention.
    Blowfish {
        mode: BlockMode,
        key: HexBytes,
        #[serde(default)]
        iv: HexBytes,
        #[serde(default)]
        padding: Padding,
    },
    /// RC4, 1..=256-byte key.
    Rc4 { key: HexBytes },
    /// `ChaCha20` (RFC 8439): 32-byte key, 12-byte nonce, initial block
    /// counter.
    ChaCha20 {
        key: HexBytes,
        nonce: HexBytes,
        #[serde(default)]
        counter: u32,
    },
}

impl Cipher {
    /// Short display name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Aes { .. } => "AES",
            Self::Des { .. } => "DES",
            Self::TripleDes { .. } => "3DES",
            Self::Blowfish { .. } => "Blowfish",
            Self::Rc4 { .. } => "RC4",
            Self::ChaCha20 { .. } => "ChaCha20",
        }
    }

    /// Whether decrypt(encrypt(x)) == x and encrypt(decrypt(y)) == y with
    /// the same parameters (false only for GCM with a detached tag).
    pub fn is_reversible(&self) -> bool {
        !matches!(
            self,
            Self::Aes {
                mode: AesMode::Gcm,
                tag: Some(_),
                ..
            }
        )
    }

    /// Checks key, IV and tag lengths.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Aes {
                mode, key, iv, tag, ..
            } => {
                aes_block(key.as_slice())?;
                match mode {
                    AesMode::Ecb => {}
                    AesMode::Cbc | AesMode::Ctr => check_iv("AES", iv.as_slice(), 16)?,
                    AesMode::Gcm => {
                        if !matches!(iv.0.len(), 12 | 16) {
                            return Err(TransformError::InvalidIvLength {
                                algorithm: "AES-GCM",
                                expected: "12 or 16",
                                actual: iv.0.len(),
                            });
                        }
                        if let Some(t) = tag
                            && t.0.len() != GCM_TAG_LEN
                        {
                            return Err(TransformError::param(format!(
                                "GCM tag must be {GCM_TAG_LEN} bytes, got {}",
                                t.0.len()
                            )));
                        }
                    }
                }
                Ok(())
            }
            Self::Des { mode, key, iv, .. }
            | Self::TripleDes { mode, key, iv, .. }
            | Self::Blowfish { mode, key, iv, .. } => {
                let bc = self.block_cipher(key.as_slice())?;
                if *mode != BlockMode::Ecb {
                    check_iv(self.name(), iv.as_slice(), bc.size())?;
                }
                Ok(())
            }
            Self::Rc4 { key } => {
                if (1..=256).contains(&key.0.len()) {
                    Ok(())
                } else {
                    Err(TransformError::InvalidKeyLength {
                        algorithm: "RC4",
                        expected: "1..=256",
                        actual: key.0.len(),
                    })
                }
            }
            Self::ChaCha20 { key, nonce, .. } => {
                if key.0.len() != 32 {
                    return Err(TransformError::InvalidKeyLength {
                        algorithm: "ChaCha20",
                        expected: "32",
                        actual: key.0.len(),
                    });
                }
                check_iv("ChaCha20", nonce.as_slice(), 12)
            }
        }
    }

    fn block_cipher(&self, key: &[u8]) -> Result<Box<dyn BlockFn>> {
        match self {
            Self::Aes { .. } => aes_block(key),
            Self::Des { .. } => {
                if key.len() != 8 {
                    return Err(TransformError::InvalidKeyLength {
                        algorithm: "DES",
                        expected: "8",
                        actual: key.len(),
                    });
                }
                make::<des::Des>("DES", key)
            }
            Self::TripleDes { .. } => match key.len() {
                16 => make::<des::TdesEde2>("3DES", key),
                24 => make::<des::TdesEde3>("3DES", key),
                n => Err(TransformError::InvalidKeyLength {
                    algorithm: "3DES",
                    expected: "16 or 24",
                    actual: n,
                }),
            },
            Self::Blowfish { .. } => {
                if !(4..=56).contains(&key.len()) {
                    return Err(TransformError::InvalidKeyLength {
                        algorithm: "Blowfish",
                        expected: "4..=56",
                        actual: key.len(),
                    });
                }
                make::<blowfish::Blowfish>("Blowfish", key)
            }
            Self::Rc4 { .. } | Self::ChaCha20 { .. } => {
                Err(TransformError::param("not a block cipher"))
            }
        }
    }
}

fn check_iv(algorithm: &'static str, iv: &[u8], len: usize) -> Result<()> {
    if iv.len() == len {
        Ok(())
    } else {
        Err(TransformError::InvalidIvLength {
            algorithm,
            expected: match len {
                8 => "8",
                12 => "12",
                _ => "16",
            },
            actual: iv.len(),
        })
    }
}

/// Object-safe view of a block cipher.
trait BlockFn {
    fn size(&self) -> usize;
    fn enc(&self, block: &mut [u8]);
    fn dec(&self, block: &mut [u8]);
}

struct Bc<C>(C);

impl<C: BlockCipherEncrypt + BlockCipherDecrypt> BlockFn for Bc<C> {
    fn size(&self) -> usize {
        C::block_size()
    }

    fn enc(&self, block: &mut [u8]) {
        let mut b = Block::<C>::default();
        b.copy_from_slice(block);
        self.0.encrypt_block(&mut b);
        block.copy_from_slice(&b);
    }

    fn dec(&self, block: &mut [u8]) {
        let mut b = Block::<C>::default();
        b.copy_from_slice(block);
        self.0.decrypt_block(&mut b);
        block.copy_from_slice(&b);
    }
}

fn make<C>(algorithm: &'static str, key: &[u8]) -> Result<Box<dyn BlockFn>>
where
    C: BlockCipherEncrypt + BlockCipherDecrypt + KeyInit + 'static,
{
    let c = C::new_from_slice(key).map_err(|_| TransformError::InvalidKeyLength {
        algorithm,
        expected: "a supported length",
        actual: key.len(),
    })?;
    Ok(Box::new(Bc(c)))
}

fn aes_block(key: &[u8]) -> Result<Box<dyn BlockFn>> {
    match key.len() {
        16 => make::<aes::Aes128>("AES", key),
        24 => make::<aes::Aes192>("AES", key),
        32 => make::<aes::Aes256>("AES", key),
        n => Err(TransformError::InvalidKeyLength {
            algorithm: "AES",
            expected: "16, 24 or 32",
            actual: n,
        }),
    }
}

fn pkcs7_pad(data: &mut Vec<u8>, bs: usize) {
    let n = bs - data.len() % bs;
    data.extend(std::iter::repeat_n(n as u8, n));
}

fn pkcs7_unpad(data: &mut Vec<u8>, bs: usize) -> Result<()> {
    let n = usize::from(*data.last().ok_or(TransformError::BadPadding)?);
    if n == 0
        || n > bs
        || n > data.len()
        || !data[data.len() - n..].iter().all(|&b| usize::from(b) == n)
    {
        return Err(TransformError::BadPadding);
    }
    data.truncate(data.len() - n);
    Ok(())
}

fn block_crypt(
    bc: &dyn BlockFn,
    mode: BlockMode,
    iv: &[u8],
    padding: Padding,
    data: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>> {
    let bs = bc.size();
    let mut buf = data.to_vec();
    if mode == BlockMode::Ctr {
        let mut counter = iv.to_vec();
        let mut ks = vec![0u8; bs];
        for chunk in buf.chunks_mut(bs) {
            ks.copy_from_slice(&counter);
            bc.enc(&mut ks);
            for (d, k) in chunk.iter_mut().zip(&ks) {
                *d ^= k;
            }
            for byte in counter.iter_mut().rev() {
                *byte = byte.wrapping_add(1);
                if *byte != 0 {
                    break;
                }
            }
        }
        return Ok(buf);
    }
    if encrypt && padding == Padding::Pkcs7 {
        pkcs7_pad(&mut buf, bs);
    }
    if !buf.len().is_multiple_of(bs) {
        return Err(TransformError::NotBlockAligned {
            len: buf.len(),
            block: bs,
        });
    }
    let mut prev = iv.to_vec();
    let mut tmp = vec![0u8; bs];
    for chunk in buf.chunks_exact_mut(bs) {
        match (mode, encrypt) {
            (BlockMode::Ecb, true) => bc.enc(chunk),
            (BlockMode::Ecb, false) => bc.dec(chunk),
            (_, true) => {
                for (d, p) in chunk.iter_mut().zip(&prev) {
                    *d ^= p;
                }
                bc.enc(chunk);
                prev.copy_from_slice(chunk);
            }
            (_, false) => {
                tmp.copy_from_slice(chunk);
                bc.dec(chunk);
                for (d, p) in chunk.iter_mut().zip(&prev) {
                    *d ^= p;
                }
                prev.copy_from_slice(&tmp);
            }
        }
    }
    if !encrypt && padding == Padding::Pkcs7 {
        if buf.is_empty() {
            return Err(TransformError::BadPadding);
        }
        pkcs7_unpad(&mut buf, bs)?;
    }
    Ok(buf)
}

macro_rules! gcm_run {
    ($aes:ty, $n:ty, $key:expr, $nonce:expr, $payload:expr, $encrypt:expr) => {{
        let c = aes_gcm::AesGcm::<$aes, $n>::new_from_slice($key)
            .map_err(|_| TransformError::param("invalid AES key"))?;
        let nonce = aes_gcm::Nonce::<$n>::try_from($nonce)
            .map_err(|_| TransformError::param("invalid GCM nonce"))?;
        if $encrypt {
            c.encrypt(&nonce, $payload)
        } else {
            c.decrypt(&nonce, $payload)
        }
    }};
}

fn gcm(key: &[u8], nonce: &[u8], aad: &[u8], msg: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    let payload = Payload { msg, aad };
    let r = match (key.len(), nonce.len()) {
        (16, 12) => gcm_run!(aes::Aes128, U12, key, nonce, payload, encrypt),
        (24, 12) => gcm_run!(aes::Aes192, U12, key, nonce, payload, encrypt),
        (32, 12) => gcm_run!(aes::Aes256, U12, key, nonce, payload, encrypt),
        (16, 16) => gcm_run!(aes::Aes128, U16, key, nonce, payload, encrypt),
        (24, 16) => gcm_run!(aes::Aes192, U16, key, nonce, payload, encrypt),
        (32, 16) => gcm_run!(aes::Aes256, U16, key, nonce, payload, encrypt),
        _ => return Err(TransformError::param("invalid AES-GCM key or nonce length")),
    };
    r.map_err(|_| {
        if encrypt {
            TransformError::param("AES-GCM encryption failed (input too long)")
        } else {
            TransformError::AuthenticationFailed
        }
    })
}

fn run(c: &Cipher, data: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    c.validate()?;
    match c {
        Cipher::Aes {
            mode,
            key,
            iv,
            padding,
            aad,
            tag,
        } => {
            let block_mode = match mode {
                AesMode::Ecb => BlockMode::Ecb,
                AesMode::Cbc => BlockMode::Cbc,
                AesMode::Ctr => BlockMode::Ctr,
                AesMode::Gcm => {
                    return if encrypt {
                        let mut out = gcm(&key.0, &iv.0, &aad.0, data, true)?;
                        if tag.is_some() {
                            out.truncate(out.len() - GCM_TAG_LEN);
                        }
                        Ok(out)
                    } else if let Some(t) = tag {
                        let mut msg = data.to_vec();
                        msg.extend_from_slice(&t.0);
                        gcm(&key.0, &iv.0, &aad.0, &msg, false)
                    } else {
                        if data.len() < GCM_TAG_LEN {
                            return Err(TransformError::param(format!(
                                "AES-GCM input must end with the {GCM_TAG_LEN}-byte tag"
                            )));
                        }
                        gcm(&key.0, &iv.0, &aad.0, data, false)
                    };
                }
            };
            block_crypt(
                &*aes_block(&key.0)?,
                block_mode,
                &iv.0,
                *padding,
                data,
                encrypt,
            )
        }
        Cipher::Des {
            mode,
            key,
            iv,
            padding,
        }
        | Cipher::TripleDes {
            mode,
            key,
            iv,
            padding,
        }
        | Cipher::Blowfish {
            mode,
            key,
            iv,
            padding,
        } => block_crypt(
            &*c.block_cipher(&key.0)?,
            *mode,
            &iv.0,
            *padding,
            data,
            encrypt,
        ),
        Cipher::Rc4 { key } => {
            let mut s =
                rc4::Rc4::new_from_slice(&key.0).map_err(|_| TransformError::InvalidKeyLength {
                    algorithm: "RC4",
                    expected: "1..=256",
                    actual: key.0.len(),
                })?;
            let mut out = data.to_vec();
            s.apply_keystream(&mut out);
            Ok(out)
        }
        Cipher::ChaCha20 {
            key,
            nonce,
            counter,
        } => {
            let mut s = chacha20::ChaCha20::new_from_slices(&key.0, &nonce.0)
                .map_err(|_| TransformError::param("invalid ChaCha20 key or nonce"))?;
            s.try_seek(u64::from(*counter) * 64)
                .map_err(|_| TransformError::param("ChaCha20 counter out of range"))?;
            let mut out = data.to_vec();
            s.try_apply_keystream(&mut out).map_err(|_| {
                TransformError::param(
                    "ChaCha20 keystream exhausted (data too long for the counter)",
                )
            })?;
            Ok(out)
        }
    }
}

/// Encrypts `data`.
pub fn encrypt(c: &Cipher, data: &[u8]) -> Result<Vec<u8>> {
    run(c, data, true)
}

/// Decrypts `data`.
pub fn decrypt(c: &Cipher, data: &[u8]) -> Result<Vec<u8>> {
    run(c, data, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> HexBytes {
        HexBytes::parse(s).unwrap()
    }

    fn aes(mode: AesMode, key: &str, iv: &str, padding: Padding) -> Cipher {
        Cipher::Aes {
            mode,
            key: h(key),
            iv: h(iv),
            padding,
            aad: HexBytes::default(),
            tag: None,
        }
    }

    fn check(c: &Cipher, plain: &str, ct: &str) {
        assert_eq!(encrypt(c, &h(plain).0).unwrap(), h(ct).0, "{c:?}");
        assert_eq!(decrypt(c, &h(ct).0).unwrap(), h(plain).0, "{c:?}");
    }

    #[test]
    fn fips197_ecb() {
        let pt = "00112233445566778899aabbccddeeff";
        let keys = [
            (
                "000102030405060708090a0b0c0d0e0f",
                "69c4e0d86a7b0430d8cdb78070b4c55a",
            ),
            (
                "000102030405060708090a0b0c0d0e0f1011121314151617",
                "dda97ca4864cdfe06eaf70a0ec0d7191",
            ),
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "8ea2b7ca516745bfeafc49904b496089",
            ),
        ];
        for (k, ct) in keys {
            check(&aes(AesMode::Ecb, k, "", Padding::None), pt, ct);
        }
    }

    #[test]
    fn sp800_38a_cbc_ctr() {
        let key = "2b7e151628aed2a6abf7158809cf4f3c";
        let pt = "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51";
        check(
            &aes(
                AesMode::Cbc,
                key,
                "000102030405060708090a0b0c0d0e0f",
                Padding::None,
            ),
            pt,
            "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2",
        );
        check(
            &aes(
                AesMode::Ctr,
                key,
                "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
                Padding::None,
            ),
            pt,
            "874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff",
        );
        // CTR on a partial block is a plain prefix
        let c = aes(
            AesMode::Ctr,
            key,
            "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
            Padding::Pkcs7,
        );
        assert_eq!(encrypt(&c, &h("6bc1bee2").0).unwrap(), h("874d6191").0);
    }

    #[test]
    fn counter_wraps_across_whole_block() {
        let c = aes(
            AesMode::Ctr,
            "2b7e151628aed2a6abf7158809cf4f3c",
            "ffffffffffffffffffffffffffffffff",
            Padding::None,
        );
        let data = vec![0u8; 48];
        let ks = encrypt(&c, &data).unwrap();
        let ecb = aes(
            AesMode::Ecb,
            "2b7e151628aed2a6abf7158809cf4f3c",
            "",
            Padding::None,
        );
        let zero_ctr = encrypt(&ecb, &[0u8; 16]).unwrap();
        assert_eq!(&ks[16..32], &zero_ctr[..]);
    }

    #[test]
    fn openssl_cbc_vectors_with_pkcs7() {
        let pt = "68656c6c6f20776f726c642031323334"; // "hello world 1234"
        check(
            &aes(
                AesMode::Cbc,
                "000102030405060708090a0b0c0d0e0f1011121314151617",
                "000102030405060708090a0b0c0d0e0f",
                Padding::Pkcs7,
            ),
            pt,
            "d633fef4dc562f516df7b05f440c207941b052f1c30afb5e2c6a9a0a7ef398ff",
        );
        let bf = Cipher::Blowfish {
            mode: BlockMode::Cbc,
            key: h("0123456789abcdeff0e1d2c3b4a59687"),
            iv: h("fedcba9876543210"),
            padding: Padding::Pkcs7,
        };
        check(&bf, pt, "fe613138f0d69741d32dae62a6e3e5c23f30b39b363807da");
        let des = Cipher::Des {
            mode: BlockMode::Cbc,
            key: h("0123456789abcdef"),
            iv: h("1234567890abcdef"),
            padding: Padding::Pkcs7,
        };
        check(&des, pt, "129e6bb976efed49f3dd71e55549273ed9b5284deaa5dfd3");
        let tdes3 = Cipher::TripleDes {
            mode: BlockMode::Cbc,
            key: h("0123456789abcdef23456789abcdef01456789abcdef0123"),
            iv: h("1234567890abcdef"),
            padding: Padding::Pkcs7,
        };
        check(
            &tdes3,
            pt,
            "801545ad6cf2879bc6fcfb4e32ccc55fa27be5a4af3185fb",
        );
        let tdes2 = Cipher::TripleDes {
            mode: BlockMode::Cbc,
            key: h("0123456789abcdef23456789abcdef01"),
            iv: h("1234567890abcdef"),
            padding: Padding::Pkcs7,
        };
        check(
            &tdes2,
            pt,
            "6457d88469c9bd8e89580f2202f9f3f7803f1337049d339c",
        );
    }

    #[test]
    fn classic_block_vectors() {
        let des = Cipher::Des {
            mode: BlockMode::Ecb,
            key: h("133457799BBCDFF1"),
            iv: HexBytes::default(),
            padding: Padding::None,
        };
        check(&des, "0123456789ABCDEF", "85E813540F0AB405");
        let bf = |k: &str| Cipher::Blowfish {
            mode: BlockMode::Ecb,
            key: h(k),
            iv: HexBytes::default(),
            padding: Padding::None,
        };
        check(
            &bf("0000000000000000"),
            "0000000000000000",
            "4EF997456198DD78",
        );
        check(
            &bf("FFFFFFFFFFFFFFFF"),
            "FFFFFFFFFFFFFFFF",
            "51866FD5B85ECB8A",
        );
    }

    #[test]
    fn gcm_vectors() {
        let mut c = Cipher::Aes {
            mode: AesMode::Gcm,
            key: h("00000000000000000000000000000000"),
            iv: h("000000000000000000000000"),
            padding: Padding::None,
            aad: HexBytes::default(),
            tag: None,
        };
        // McGrew & Viega test cases 1 and 2
        assert_eq!(
            encrypt(&c, &[]).unwrap(),
            h("58e2fccefa7e3061367f1d57a4e7455a").0
        );
        check(
            &c,
            "00000000000000000000000000000000",
            "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf",
        );
        let mut bad = h("0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bdde").0;
        assert!(matches!(
            decrypt(&c, &bad),
            Err(TransformError::AuthenticationFailed)
        ));
        bad.truncate(4);
        assert!(decrypt(&c, &bad).is_err());
        // detached tag
        if let Cipher::Aes { tag, .. } = &mut c {
            *tag = Some(h("ab6e47d42cec13bdf53a67b21257bddf"));
        }
        assert!(!c.is_reversible());
        assert_eq!(
            decrypt(&c, &h("0388dace60b6a392f328c2b971b2fe78").0).unwrap(),
            vec![0u8; 16]
        );
        assert_eq!(
            encrypt(&c, &[0u8; 16]).unwrap(),
            h("0388dace60b6a392f328c2b971b2fe78").0
        );
        // AAD is authenticated
        let with_aad = Cipher::Aes {
            mode: AesMode::Gcm,
            key: h("feffe9928665731c6d6a8f9467308308"),
            iv: h("cafebabefacedbaddecaf888"),
            padding: Padding::None,
            aad: h("feedfacedeadbeeffeedfacedeadbeefabaddad2"),
            tag: None,
        };
        let ct = encrypt(&with_aad, b"secret").unwrap();
        assert_eq!(decrypt(&with_aad, &ct).unwrap(), b"secret");
        let other = Cipher::Aes {
            mode: AesMode::Gcm,
            key: h("feffe9928665731c6d6a8f9467308308"),
            iv: h("cafebabefacedbaddecaf888"),
            padding: Padding::None,
            aad: h("00"),
            tag: None,
        };
        assert!(decrypt(&other, &ct).is_err());
        // 16-byte nonce and AES-256
        let n16 = Cipher::Aes {
            mode: AesMode::Gcm,
            key: h(&"11".repeat(32)),
            iv: h(&"22".repeat(16)),
            padding: Padding::None,
            aad: HexBytes::default(),
            tag: None,
        };
        let ct = encrypt(&n16, b"abc").unwrap();
        assert_eq!(ct.len(), 3 + GCM_TAG_LEN);
        assert_eq!(decrypt(&n16, &ct).unwrap(), b"abc");
    }

    #[test]
    fn rc4_and_chacha20() {
        let rc4 = |k: &[u8]| Cipher::Rc4 {
            key: HexBytes(k.to_vec()),
        };
        assert_eq!(
            encrypt(&rc4(b"Key"), b"Plaintext").unwrap(),
            h("BBF316E8D940AF0AD3").0
        );
        assert_eq!(encrypt(&rc4(b"Wiki"), b"pedia").unwrap(), h("1021BF0420").0);
        assert_eq!(
            decrypt(&rc4(b"Wiki"), &h("1021BF0420").0).unwrap(),
            b"pedia"
        );
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let c = Cipher::ChaCha20 {
            key: h("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
            nonce: h("000000000000004a00000000"),
            counter: 1,
        };
        let ct = encrypt(&c, pt).unwrap();
        assert_eq!(
            ct,
            h("6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0bf91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d807ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab77937365af90bbf74a35be6b40b8eedf2785e42874d").0
        );
        assert_eq!(decrypt(&c, &ct).unwrap(), pt);
    }

    #[test]
    fn parameter_errors() {
        let e = encrypt(&aes(AesMode::Cbc, "0011", "", Padding::Pkcs7), b"x").unwrap_err();
        assert!(matches!(
            e,
            TransformError::InvalidKeyLength { actual: 2, .. }
        ));
        let e = encrypt(
            &aes(AesMode::Cbc, &"00".repeat(16), "00", Padding::Pkcs7),
            b"x",
        )
        .unwrap_err();
        assert!(matches!(e, TransformError::InvalidIvLength { .. }));
        let e = encrypt(
            &aes(AesMode::Ecb, &"00".repeat(16), "", Padding::None),
            b"x",
        )
        .unwrap_err();
        assert!(matches!(
            e,
            TransformError::NotBlockAligned { len: 1, block: 16 }
        ));
        // wrong key -> padding check fails (overwhelmingly likely)
        let good = aes(
            AesMode::Cbc,
            &"00".repeat(16),
            &"00".repeat(16),
            Padding::Pkcs7,
        );
        let ct = encrypt(&good, b"some secret text").unwrap();
        let wrong = aes(
            AesMode::Cbc,
            &"01".repeat(16),
            &"00".repeat(16),
            Padding::Pkcs7,
        );
        assert!(matches!(
            decrypt(&wrong, &ct),
            Err(TransformError::BadPadding)
        ));
        assert!(matches!(
            decrypt(&good, &[]),
            Err(TransformError::BadPadding)
        ));
        assert!(
            encrypt(
                &Cipher::Rc4 {
                    key: HexBytes::default()
                },
                b"x"
            )
            .is_err()
        );
        let bf = Cipher::Blowfish {
            mode: BlockMode::Ecb,
            key: h("010203"),
            iv: HexBytes::default(),
            padding: Padding::Pkcs7,
        };
        assert!(encrypt(&bf, b"x").is_err());
        let td = Cipher::TripleDes {
            mode: BlockMode::Ecb,
            key: h("0102030405060708"),
            iv: HexBytes::default(),
            padding: Padding::Pkcs7,
        };
        assert!(encrypt(&td, b"x").is_err());
    }

    #[test]
    fn padding_roundtrip_all_lengths() {
        let c = aes(
            AesMode::Cbc,
            &"0f".repeat(32),
            &"a5".repeat(16),
            Padding::Pkcs7,
        );
        for n in 0..40 {
            let d: Vec<u8> = (0..n).map(|i| i as u8).collect();
            let ct = encrypt(&c, &d).unwrap();
            assert_eq!(ct.len(), (n / 16 + 1) * 16);
            assert_eq!(decrypt(&c, &ct).unwrap(), d);
        }
    }

    #[test]
    fn serde_shape() {
        let c: Cipher = serde_json::from_str(
            r#"{"kind":"aes","mode":"gcm","key":"00112233445566778899aabbccddeeff","iv":"000000000000000000000000"}"#,
        )
        .unwrap();
        assert!(matches!(
            c,
            Cipher::Aes {
                mode: AesMode::Gcm,
                padding: Padding::Pkcs7,
                ..
            }
        ));
        let j = serde_json::to_string(&Cipher::ChaCha20 {
            key: h("00"),
            nonce: h("01"),
            counter: 0,
        })
        .unwrap();
        assert_eq!(
            j,
            r#"{"kind":"chaCha20","key":"00","nonce":"01","counter":0}"#
        );
    }
}
