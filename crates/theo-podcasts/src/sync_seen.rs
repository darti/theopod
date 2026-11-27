//! The identities one sync met, as a Bloom filter (ADR-0031).
//!
//! A false positive only keeps a tombstone one more sync: the salt changes
//! with every sync generation, so the same identity is not kept forever.
//! There are no false negatives, so a listed episode's tombstone is never
//! deleted.
use crate::catalogue::IdentityKey;

/// Filter size in bits (8 KiB).
const SEEN_BITS: usize = 1 << 16;
/// Bit positions set per identity.
const PROBES: usize = 4;
/// Rated load: about 4.4% false positives. Past it the filter keeps too
/// many tombstones to be worth a sweep (ADR-0031).
pub(crate) const SEEN_CAPACITY: u32 = 10_000;

/// A salted Bloom filter of [`IdentityKey`]s.
pub(crate) struct Seen {
    bits: [u64; SEEN_BITS / 64],
    salt: u64,
    /// Insertions since the last reset, duplicates included.
    inserted: u32,
}

impl Seen {
    /// An empty filter; [`Self::reset`] salts it.
    pub(crate) const fn new() -> Self {
        Self {
            bits: [0; SEEN_BITS / 64],
            salt: 0,
            inserted: 0,
        }
    }
    /// Forget every identity and salt the positions with `salt`.
    pub(crate) fn reset(&mut self, salt: u64) {
        self.bits.fill(0);
        self.salt = mix(salt);
        self.inserted = 0;
    }
    /// Record `key`.
    pub(crate) fn insert(&mut self, key: &IdentityKey) {
        for bit in positions(self.salt, key) {
            self.bits[bit / 64] |= 1 << (bit % 64);
        }
        self.inserted = self.inserted.saturating_add(1);
    }
    /// Whether `key` may have been inserted; false means it was not.
    pub(crate) fn contains(&self, key: &IdentityKey) -> bool {
        positions(self.salt, key).all(|bit| self.bits[bit / 64] & (1 << (bit % 64)) != 0)
    }
    /// Insertions since the last reset.
    pub(crate) fn inserted(&self) -> u32 {
        self.inserted
    }
}

/// Double hashing over the (already uniform) SHA-256, mixed with the salt
/// so that colliding identities differ from one generation to the next.
/// Every position is below [`SEEN_BITS`].
fn positions(salt: u64, &(kind, hash): &IdentityKey) -> impl Iterator<Item = usize> {
    let (low, rest) = hash.split_first_chunk::<8>().unwrap_or((&[0; 8], &[]));
    let high = rest.first_chunk::<8>().unwrap_or(&[0; 8]);
    let a = mix(u64::from_le_bytes(*low) ^ salt ^ u64::from(kind));
    // Odd, so the probes are distinct modulo the power-of-two size.
    let b = mix(u64::from_le_bytes(*high) ^ salt.rotate_left(32)) | 1;
    (0..PROBES as u64).map(move |i| {
        let bit = a.wrapping_add(i.wrapping_mul(b)) & (SEEN_BITS as u64 - 1);
        // Masked below 2^16, so the cast is lossless.
        bit as usize
    })
}

/// SplitMix64's finalizer: a bijective 64-bit mix.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
#[path = "sync_seen_tests.rs"]
mod tests;
