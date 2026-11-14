//! AES-GCM built on an external block cipher (t-154).
//!
//! The ESP32-S3 AES peripheral runs ECB and CTR but has no GCM mode.
//! [`seal`] and [`open`] build GCM (NIST SP 800-38D, 96-bit nonce, 128-bit
//! tag) from any [`GcmCipher`]: the cipher supplies the hash key and the tag
//! mask (two blocks) and the counter keystream; GHASH runs in software
//! (RustCrypto `ghash`).

use ghash::{GHash, universal_hash::UniversalHash};

/// Keystream blocks requested from the cipher per call.
pub const BATCH: usize = 4;

/// A block cipher under a fixed key, used as GCM's forward cipher.
///
/// Implement this trait to run GCM on a cipher engine (e.g. a hardware
/// AES peripheral holding the key). Override [`apply_ctr`](Self::apply_ctr)
/// when the engine has a CTR mode of its own.
pub trait GcmCipher {
    /// Error reported when the cipher cannot run.
    type Error;

    /// Encrypts each 16-byte block of `blocks` in place.
    fn encrypt_blocks(&mut self, blocks: &mut [[u8; 16]]) -> Result<(), Self::Error>;

    /// XORs into `buf` the CTR keystream starting at counter block `counter`.
    ///
    /// GCM increments the low 32 bits only, a full 128-bit increment gives
    /// the same keystream: from GCM's first data counter (2), GCM's length
    /// limit keeps those bits from wrapping. The default encrypts counter
    /// blocks [`BATCH`] at a time with [`encrypt_blocks`](Self::encrypt_blocks).
    fn apply_ctr(&mut self, counter: &[u8; 16], buf: &mut [u8]) -> Result<(), Self::Error> {
        let mut nonce = [0; 12];
        nonce.copy_from_slice(&counter[..12]);
        let mut next = u32::from_be_bytes([counter[12], counter[13], counter[14], counter[15]]);
        for chunk in buf.chunks_mut(16 * BATCH) {
            let blocks = chunk.len().div_ceil(16);
            let mut keystream = [[0; 16]; BATCH];
            for (i, block) in keystream[..blocks].iter_mut().enumerate() {
                *block = counter_block(&nonce, next.wrapping_add(i as u32));
            }
            self.encrypt_blocks(&mut keystream[..blocks])?;
            next = next.wrapping_add(blocks as u32);
            for (byte, key) in chunk.iter_mut().zip(keystream.as_flattened()) {
                *byte ^= key;
            }
        }
        Ok(())
    }
}

/// XORs a CTR keystream into `buf` through an engine limited to whole
/// blocks, at most `max_piece` bytes per call.
///
/// `run(counter, piece)` must XOR into `piece` the keystream starting at
/// `counter` (128-bit big-endian increment). Each piece gets its own,
/// explicitly computed start counter: the engine keeps no state between
/// calls. A partial last block runs as one zero-padded block. `max_piece`
/// must be a non-zero multiple of 16.
pub fn ctr_in_pieces<E>(
    counter: &[u8; 16],
    buf: &mut [u8],
    max_piece: usize,
    mut run: impl FnMut(&[u8; 16], &mut [u8]) -> Result<(), E>,
) -> Result<(), E> {
    assert!(
        max_piece >= 16 && max_piece.is_multiple_of(16),
        "bad CTR piece size"
    );
    let start = u128::from_be_bytes(*counter);
    let whole = buf.len() / 16 * 16;
    let (body, tail) = buf.split_at_mut(whole);
    let mut done = 0u128;
    for piece in body.chunks_mut(max_piece) {
        run(&start.wrapping_add(done).to_be_bytes(), piece)?;
        done += (piece.len() / 16) as u128;
    }
    if !tail.is_empty() {
        let mut block = [0; 16];
        block[..tail.len()].copy_from_slice(tail);
        run(&start.wrapping_add(done).to_be_bytes(), &mut block)?;
        tail.copy_from_slice(&block[..tail.len()]);
    }
    Ok(())
}

/// GCM's plaintext limit (2^39 - 256 bits): from counter 2, inc32 never wraps.
const MAX_LEN: u64 = (1 << 36) - 32;

/// Why [`open`] rejected a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcmError<E> {
    /// The cipher failed.
    Cipher(E),
    /// The tag does not match: the record is forged or corrupt.
    Tag,
}

/// Encrypts `buf` in place, authenticating it and `aad`; returns the tag.
pub fn seal<C: GcmCipher>(
    cipher: &mut C,
    nonce: &[u8; 12],
    aad: &[u8],
    buf: &mut [u8],
) -> Result<[u8; 16], C::Error> {
    assert!(buf.len() as u64 <= MAX_LEN, "GCM message too long");
    let (hash_key, mask) = setup(cipher, nonce)?;
    cipher.apply_ctr(&counter_block(nonce, 2), buf)?;
    Ok(tag(&hash_key, aad, buf, &mask))
}

/// Verifies `expected` over `buf` and `aad`, then decrypts `buf` in place.
///
/// On a [`GcmError::Tag`] mismatch `buf` is left as it was.
pub fn open<C: GcmCipher>(
    cipher: &mut C,
    nonce: &[u8; 12],
    aad: &[u8],
    buf: &mut [u8],
    expected: &[u8; 16],
) -> Result<(), GcmError<C::Error>> {
    assert!(buf.len() as u64 <= MAX_LEN, "GCM message too long");
    let (hash_key, mask) = setup(cipher, nonce).map_err(GcmError::Cipher)?;
    let actual = tag(&hash_key, aad, buf, &mask);
    // Constant time: fold every byte difference before testing.
    let diff = actual
        .iter()
        .zip(expected)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if diff != 0 {
        return Err(GcmError::Tag);
    }
    cipher
        .apply_ctr(&counter_block(nonce, 2), buf)
        .map_err(GcmError::Cipher)
}

/// The counter block `nonce || counter` (big-endian 32-bit counter).
fn counter_block(nonce: &[u8; 12], counter: u32) -> [u8; 16] {
    let mut block = [0; 16];
    block[..12].copy_from_slice(nonce);
    block[12..].copy_from_slice(&counter.to_be_bytes());
    block
}

/// The hash key `H = E(0)` and the tag mask `E(J0)`, in one cipher call.
fn setup<C: GcmCipher>(cipher: &mut C, nonce: &[u8; 12]) -> Result<([u8; 16], [u8; 16]), C::Error> {
    let mut blocks = [[0; 16], counter_block(nonce, 1)];
    cipher.encrypt_blocks(&mut blocks)?;
    Ok((blocks[0], blocks[1]))
}

/// `GHASH_H(aad, ciphertext, lengths) XOR mask`.
fn tag(hash_key: &[u8; 16], aad: &[u8], ciphertext: &[u8], mask: &[u8; 16]) -> [u8; 16] {
    let mut ghash = GHash::new(hash_key.into());
    ghash.update_padded(aad);
    ghash.update_padded(ciphertext);
    let mut lengths = [0u8; 16];
    lengths[..8].copy_from_slice(&(aad.len() as u64 * 8).to_be_bytes());
    lengths[8..].copy_from_slice(&(ciphertext.len() as u64 * 8).to_be_bytes());
    ghash.update(&[lengths.into()]);
    let mut tag: [u8; 16] = ghash.finalize().into();
    for (byte, m) in tag.iter_mut().zip(mask) {
        *byte ^= m;
    }
    tag
}
