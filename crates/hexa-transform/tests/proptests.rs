//! Property tests: every reversible step round-trips, codecs and
//! compressors round-trip, previews are prefixes of the full output.

use hexa_transform::{
    AesMode, ArithOp, Base32Alphabet, Base64Alphabet, BitOp, BlockMode, Cipher, Codec, Compression,
    Direction, Endian, HexBytes, Padding, Recipe, Step, Width, apply, preview, reverse_apply,
};
use proptest::prelude::*;

fn width() -> impl Strategy<Value = Width> {
    prop_oneof![
        Just(Width::W8),
        Just(Width::W16),
        Just(Width::W32),
        Just(Width::W64)
    ]
}

fn endian() -> impl Strategy<Value = Endian> {
    prop_oneof![Just(Endian::Little), Just(Endian::Big)]
}

fn codec() -> impl Strategy<Value = Codec> {
    prop_oneof![
        any::<bool>().prop_map(|uppercase| Codec::Base16 { uppercase }),
        (any::<bool>(), any::<bool>()).prop_map(|(h, padding)| Codec::Base32 {
            alphabet: if h {
                Base32Alphabet::Hex
            } else {
                Base32Alphabet::Standard
            },
            padding
        }),
        Just(Codec::Base58),
        (any::<bool>(), any::<bool>()).prop_map(|(u, padding)| Codec::Base64 {
            alphabet: if u {
                Base64Alphabet::Url
            } else {
                Base64Alphabet::Standard
            },
            padding
        }),
        any::<bool>().prop_map(|delimiters| Codec::Ascii85 { delimiters }),
        any::<bool>().prop_map(|space_as_plus| Codec::Url { space_as_plus }),
        Just(Codec::QuotedPrintable),
        Just(Codec::Uuencode {
            file_name: "f".into()
        }),
        Just(Codec::Rot13),
    ]
}

fn compression() -> impl Strategy<Value = Compression> {
    prop_oneof![
        (0u32..=9).prop_map(|level| Compression::Deflate { level }),
        (0u32..=9).prop_map(|level| Compression::Zlib { level }),
        (0u32..=9).prop_map(|level| Compression::Gzip { level }),
        (1u32..=9).prop_map(|level| Compression::Bzip2 { level }),
        (0u32..=3).prop_map(|preset| Compression::Lzma { preset }),
        (0u32..=3).prop_map(|preset| Compression::Xz { preset }),
        Just(Compression::Lz4),
        (0u32..=3).prop_map(|level| Compression::Zstd { level }),
        (0u32..=6).prop_map(|quality| Compression::Brotli {
            quality,
            window: 18
        }),
        Just(Compression::Lzo),
        Just(Compression::Snappy),
        Just(Compression::SnappyFramed),
    ]
}

fn cipher() -> impl Strategy<Value = Cipher> {
    let pad = prop_oneof![Just(Padding::Pkcs7), Just(Padding::None)];
    let bmode = prop_oneof![
        Just(BlockMode::Ecb),
        Just(BlockMode::Cbc),
        Just(BlockMode::Ctr)
    ];
    prop_oneof![
        (
            prop_oneof![Just(16usize), Just(24), Just(32)],
            prop_oneof![
                Just(AesMode::Ecb),
                Just(AesMode::Cbc),
                Just(AesMode::Ctr),
                Just(AesMode::Gcm)
            ],
            any::<u8>(),
            pad.clone(),
            prop::collection::vec(any::<u8>(), 0..8),
        )
            .prop_map(|(klen, mode, seed, padding, aad)| Cipher::Aes {
                mode,
                key: HexBytes(vec![seed; klen]),
                iv: HexBytes(vec![
                    seed ^ 0x55;
                    if mode == AesMode::Gcm { 12 } else { 16 }
                ]),
                padding,
                aad: HexBytes(aad),
                tag: None,
            }),
        (bmode.clone(), pad.clone(), any::<u8>()).prop_map(|(mode, padding, s)| Cipher::Des {
            mode,
            key: HexBytes(vec![s; 8]),
            iv: HexBytes(vec![!s; 8]),
            padding
        }),
        (bmode.clone(), pad.clone(), any::<bool>()).prop_map(|(mode, padding, two)| {
            Cipher::TripleDes {
                mode,
                key: HexBytes((0..if two { 16 } else { 24 }).collect()),
                iv: HexBytes(vec![7; 8]),
                padding,
            }
        }),
        (bmode, pad, 4usize..=56).prop_map(|(mode, padding, k)| Cipher::Blowfish {
            mode,
            key: HexBytes((0..k as u8).collect()),
            iv: HexBytes(vec![3; 8]),
            padding
        }),
        prop::collection::vec(any::<u8>(), 1..=32).prop_map(|k| Cipher::Rc4 { key: HexBytes(k) }),
        any::<u32>().prop_map(|c| Cipher::ChaCha20 {
            key: HexBytes(vec![9; 32]),
            nonce: HexBytes(vec![1; 12]),
            counter: c % 1_000_000,
        }),
    ]
}

fn reversible_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (any::<u64>(), width(), endian()).prop_map(|(operand, width, endian)| Step::Bitwise {
            op: BitOp::Xor,
            operand,
            width,
            endian
        }),
        Just(Step::Not),
        (any::<bool>(), 0u32..130, width(), endian()).prop_map(|(l, amount, width, endian)| {
            Step::Rotate {
                direction: if l { Direction::Left } else { Direction::Right },
                amount,
                width,
                endian,
            }
        }),
        width().prop_map(|width| Step::ReverseBits { width }),
        (any::<bool>(), any::<u64>(), width(), endian()).prop_map(
            |(add, operand, width, endian)| {
                Step::Arithmetic {
                    op: if add { ArithOp::Add } else { ArithOp::Sub },
                    operand,
                    width,
                    endian,
                    signed: false,
                }
            }
        ),
        (any::<u64>(), width(), endian()).prop_map(|(operand, width, endian)| Step::Arithmetic {
            op: ArithOp::Mul,
            operand: operand | 1,
            width,
            endian,
            signed: false,
        }),
        prop::collection::vec(any::<u8>(), 1..16).prop_map(|k| Step::XorKey { key: HexBytes(k) }),
        width().prop_map(|width| Step::ByteSwap { width }),
        Just(Step::Reverse),
        (1usize..5, 1usize..5).prop_map(|(streams, width)| Step::Interleave { streams, width }),
        (1usize..5, 1usize..5).prop_map(|(streams, width)| Step::Deinterleave { streams, width }),
        codec().prop_map(|codec| Step::Encode { codec }),
        compression().prop_map(|codec| Step::Compress { codec }),
        cipher()
            .prop_filter("block aligned modes need padding", |c| !matches!(
                c,
                Cipher::Aes {
                    mode: AesMode::Ecb | AesMode::Cbc,
                    padding: Padding::None,
                    ..
                } | Cipher::Des {
                    mode: BlockMode::Ecb | BlockMode::Cbc,
                    padding: Padding::None,
                    ..
                } | Cipher::TripleDes {
                    mode: BlockMode::Ecb | BlockMode::Cbc,
                    padding: Padding::None,
                    ..
                } | Cipher::Blowfish {
                    mode: BlockMode::Ecb | BlockMode::Cbc,
                    padding: Padding::None,
                    ..
                }
            ))
            .prop_map(|cipher| Step::Encrypt { cipher }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn codecs_roundtrip(c in codec(), data in prop::collection::vec(any::<u8>(), 0..300)) {
        let e = hexa_transform::codec::encode(&c, &data).unwrap();
        prop_assert_eq!(hexa_transform::codec::decode(&c, &e).unwrap(), data);
    }

    #[test]
    fn z85_roundtrip(data in prop::collection::vec(any::<u32>(), 0..50)) {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_be_bytes()).collect();
        let e = hexa_transform::codec::encode(&Codec::Z85, &bytes).unwrap();
        prop_assert_eq!(hexa_transform::codec::decode(&Codec::Z85, &e).unwrap(), bytes);
    }

    #[test]
    fn compression_roundtrip(c in compression(), data in prop::collection::vec(0u8..8, 0..2000)) {
        let z = hexa_transform::compress::compress(&c, &data).unwrap();
        prop_assert_eq!(hexa_transform::compress::decompress(&c, &z).unwrap(), data);
    }

    #[test]
    fn cipher_roundtrip(c in cipher(), data in prop::collection::vec(any::<u8>(), 0..100)) {
        match hexa_transform::crypto::encrypt(&c, &data) {
            Ok(ct) => prop_assert_eq!(hexa_transform::crypto::decrypt(&c, &ct).unwrap(), data),
            Err(hexa_transform::TransformError::NotBlockAligned { .. }) => {}
            Err(e) => prop_assert!(false, "{e}"),
        }
    }

    #[test]
    fn decompressing_garbage_never_panics(c in compression(), data in prop::collection::vec(any::<u8>(), 0..200)) {
        let _ = hexa_transform::compress::decompress(&c, &data);
    }

    #[test]
    fn decoding_garbage_never_panics(c in codec(), data in prop::collection::vec(any::<u8>(), 0..200)) {
        let _ = hexa_transform::codec::decode(&c, &data);
        let _ = hexa_transform::codec::decode(&Codec::Z85, &data);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn recipes_write_back(
        steps in prop::collection::vec(reversible_step(), 0..4),
        data in prop::collection::vec(any::<u8>(), 0..400),
    ) {
        let r = Recipe::new(steps);
        prop_assert!(r.is_reversible());
        let out = apply(&r, &data).unwrap();
        prop_assert_eq!(reverse_apply(&r.steps, &out).unwrap(), data);
        let json = r.to_json().unwrap();
        prop_assert_eq!(Recipe::from_json(&json).unwrap(), r);
    }

    #[test]
    fn preview_is_prefix_of_apply(
        steps in prop::collection::vec(reversible_step(), 0..3),
        len in 0usize..300_000,
        max in 0usize..3000,
    ) {
        let data: Vec<u8> = (0..len).map(|i| (i * 7 + i / 300) as u8).collect();
        let r = Recipe::new(steps);
        let full = apply(&r, &data).unwrap();
        let p = preview(&r, &data, max).unwrap();
        prop_assert_eq!(&p.output[..], &full[..max.min(full.len())]);
        prop_assert_eq!(p.truncated, full.len() > max);
        if let Some(t) = p.total_len {
            prop_assert_eq!(t, full.len());
        }
    }
}
