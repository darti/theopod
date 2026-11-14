use aes::cipher::{BlockCipherEncrypt, KeyInit};
use aes_gcm::{AeadInOut, Aes128Gcm};
use speculoos::prelude::*;

use crate::gcm::{GcmCipher, GcmError, ctr_in_pieces, open, seal};

/// Software AES-128 standing in for the hardware engine; counts its calls.
struct SoftAes {
    cipher: aes::Aes128,
    calls: usize,
}

impl SoftAes {
    fn new(key: &[u8; 16]) -> Self {
        Self {
            cipher: aes::Aes128::new(key.into()),
            calls: 0,
        }
    }
}

impl GcmCipher for SoftAes {
    type Error = ();

    fn encrypt_blocks(&mut self, blocks: &mut [[u8; 16]]) -> Result<(), ()> {
        self.calls += 1;
        for block in blocks {
            self.cipher.encrypt_block(block.into());
        }
        Ok(())
    }
}

/// Software AES with its own CTR mode, incrementing all 128 bits like the
/// ESP32-S3 peripheral; counts its ECB calls.
struct HardwareLikeCtr(SoftAes);

impl GcmCipher for HardwareLikeCtr {
    type Error = ();

    fn encrypt_blocks(&mut self, blocks: &mut [[u8; 16]]) -> Result<(), ()> {
        self.0.encrypt_blocks(blocks)
    }

    fn apply_ctr(&mut self, counter: &[u8; 16], buf: &mut [u8]) -> Result<(), ()> {
        let mut counter = u128::from_be_bytes(*counter);
        for chunk in buf.chunks_mut(16) {
            let mut block = counter.to_be_bytes();
            self.0.cipher.encrypt_block((&mut block).into());
            for (byte, key) in chunk.iter_mut().zip(block) {
                *byte ^= key;
            }
            counter = counter.wrapping_add(1);
        }
        Ok(())
    }
}

/// Like the ESP32-S3 AES DMA: whole blocks only, at most 64 bytes per
/// operation, and no counter carried between operations.
struct StatelessPieces {
    engine: HardwareLikeCtr,
    pieces: Vec<usize>,
}

impl GcmCipher for StatelessPieces {
    type Error = ();

    fn encrypt_blocks(&mut self, blocks: &mut [[u8; 16]]) -> Result<(), ()> {
        self.engine.encrypt_blocks(blocks)
    }

    fn apply_ctr(&mut self, counter: &[u8; 16], buf: &mut [u8]) -> Result<(), ()> {
        let (engine, pieces) = (&mut self.engine, &mut self.pieces);
        ctr_in_pieces(counter, buf, 64, |counter, piece| {
            assert!(piece.len().is_multiple_of(16) && piece.len() <= 64);
            pieces.push(piece.len());
            engine.apply_ctr(counter, piece)
        })
    }
}

/// An engine whose hardware is missing.
struct Broken;

impl GcmCipher for Broken {
    type Error = &'static str;

    fn encrypt_blocks(&mut self, _: &mut [[u8; 16]]) -> Result<(), &'static str> {
        Err("no engine")
    }
}

const KEY: [u8; 16] = *b"0123456789abcdef";
const NONCE: [u8; 12] = *b"nonce-012345";

fn message(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + 3) as u8).collect()
}

/// Reference seal by RustCrypto's `aes-gcm`.
fn reference(aad: &[u8], plain: &[u8]) -> (Vec<u8>, [u8; 16]) {
    let mut buf = plain.to_vec();
    let tag = Aes128Gcm::new(&KEY.into())
        .encrypt_inout_detached(&NONCE.into(), aad, buf.as_mut_slice().into())
        .unwrap();
    (buf, tag.into())
}

#[test]
fn seal_nist_test_case_4_matches_published_vector() {
    // NIST GCM spec (McGrew & Viega), test case 4: AES-128, 60-byte
    // plaintext, 20-byte AAD.
    let key = hex("feffe9928665731c6d6a8f9467308308");
    let nonce = hex("cafebabefacedbaddecaf888");
    let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
    let mut buf = hex(
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
         1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
    );
    let tag = seal(
        &mut SoftAes::new(&key.try_into().unwrap()),
        &nonce.try_into().unwrap(),
        &aad,
        &mut buf,
    )
    .unwrap();
    assert_that!(buf).is_equal_to(hex(
        "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e\
         21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091",
    ));
    assert_that!(tag.to_vec()).is_equal_to(hex("5bc94fbc3221a5db94fae95ae7121a47"));
}

#[test]
fn seal_any_length_matches_aes_gcm_crate() {
    for len in [0, 1, 15, 16, 17, 63, 64, 65, 1000, 16384 + 17] {
        for aad_len in [0, 5, 13, 16, 33] {
            let plain = message(len);
            let aad = message(aad_len);
            let (expected, expected_tag) = reference(&aad, &plain);
            let mut buf = plain.clone();
            let tag = seal(&mut SoftAes::new(&KEY), &NONCE, &aad, &mut buf).unwrap();
            assert_that!(buf).is_equal_to(expected);
            assert_that!(tag).is_equal_to(expected_tag);
        }
    }
}

#[test]
fn open_sealed_record_restores_plaintext() {
    let plain = message(16 * 1024 + 5);
    let (mut buf, tag) = reference(&[23, 3, 3, 64, 17], &plain);
    open(
        &mut SoftAes::new(&KEY),
        &NONCE,
        &[23, 3, 3, 64, 17],
        &mut buf,
        &tag,
    )
    .unwrap();
    assert_that!(buf).is_equal_to(plain);
}

#[test]
fn open_tampered_ciphertext_rejects_and_leaves_buffer() {
    let (mut buf, tag) = reference(b"aad", &message(100));
    buf[42] ^= 1;
    let tampered = buf.clone();
    let result = open(&mut SoftAes::new(&KEY), &NONCE, b"aad", &mut buf, &tag);
    assert_that!(result).is_equal_to(Err(GcmError::Tag));
    assert_that!(buf).is_equal_to(tampered);
}

#[test]
fn open_wrong_tag_or_aad_rejects() {
    let (buf, mut tag) = reference(b"aad", &message(100));
    let result = open(
        &mut SoftAes::new(&KEY),
        &NONCE,
        b"aaD",
        &mut buf.clone(),
        &tag,
    );
    assert_that!(result).is_equal_to(Err(GcmError::Tag));
    tag[15] ^= 0x80;
    let result = open(
        &mut SoftAes::new(&KEY),
        &NONCE,
        b"aad",
        &mut buf.clone(),
        &tag,
    );
    assert_that!(result).is_equal_to(Err(GcmError::Tag));
}

#[test]
fn seal_with_engine_ctr_matches_aes_gcm_crate() {
    for len in [0, 1, 17, 64, 16384 + 17] {
        let plain = message(len);
        let (expected, expected_tag) = reference(b"hdr", &plain);
        let mut engine = HardwareLikeCtr(SoftAes::new(&KEY));
        let mut buf = plain.clone();
        let tag = seal(&mut engine, &NONCE, b"hdr", &mut buf).unwrap();
        assert_that!(buf).is_equal_to(expected);
        assert_that!(tag).is_equal_to(expected_tag);
        // Only the hash key and tag mask go through ECB.
        assert_that!(engine.0.calls).is_equal_to(1);
        open(&mut engine, &NONCE, b"hdr", &mut buf, &tag).unwrap();
        assert_that!(buf).is_equal_to(plain);
    }
}

#[test]
fn seal_in_stateless_pieces_matches_aes_gcm_crate() {
    // Regression (t-154): esp-hal's AES DMA lost the counter between
    // operations, so records over one piece or with a partial last block
    // failed to decrypt on the board.
    for len in [0, 15, 16, 64, 100, 128, 4048, 16384 + 17] {
        let plain = message(len);
        let (expected, expected_tag) = reference(b"hdr", &plain);
        let mut engine = StatelessPieces {
            engine: HardwareLikeCtr(SoftAes::new(&KEY)),
            pieces: Vec::new(),
        };
        let mut buf = plain.clone();
        let tag = seal(&mut engine, &NONCE, b"hdr", &mut buf).unwrap();
        assert_that!(buf).is_equal_to(expected);
        assert_that!(tag).is_equal_to(expected_tag);
        assert_that!(engine.pieces.iter().sum::<usize>()).is_equal_to(len.div_ceil(16) * 16);
    }
}

#[test]
fn seal_large_record_batches_engine_calls() {
    let mut engine = SoftAes::new(&KEY);
    seal(&mut engine, &NONCE, &[], &mut message(16 * 1024)).unwrap();
    // 1024 keystream blocks plus H and E(J0): far fewer calls than blocks.
    assert_that!(engine.calls).is_less_than(1024 / 4 + 2);
}

#[test]
fn seal_engine_failure_is_reported() {
    let result = seal(&mut Broken, &NONCE, &[], &mut message(32));
    assert_that!(result).is_equal_to(Err("no engine"));
    let result = open(&mut Broken, &NONCE, &[], &mut message(32), &[0; 16]);
    assert_that!(result).is_equal_to(Err(GcmError::Cipher("no engine")));
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
