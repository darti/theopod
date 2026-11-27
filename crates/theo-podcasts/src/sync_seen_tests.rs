use super::*;
use sha2::{Digest, Sha256};
use std::vec::Vec;

fn key(n: u32) -> IdentityKey {
    (1, Sha256::digest(n.to_le_bytes()).into())
}

fn filled(salt: u64, n: u32) -> std::boxed::Box<Seen> {
    let mut seen = std::boxed::Box::new(Seen::new());
    seen.reset(salt);
    for i in 0..n {
        seen.insert(&key(i));
    }
    seen
}

// Regression (t-188): no false negatives, so a listed episode's tombstone
// is never swept.
#[test]
fn seen_never_forgets_an_inserted_identity() {
    let seen = filled(7, 10_000);
    assert!((0..10_000).all(|i| seen.contains(&key(i))));
}

#[test]
fn seen_false_positives_stay_rare_at_ten_thousand_entries() {
    let seen = filled(7, 10_000);
    let false_positives = (10_000..20_000).filter(|&i| seen.contains(&key(i))).count();
    assert!(false_positives < 600, "{false_positives} false positives");
}

// Regression (t-188): the salt changes with the generation, so an identity
// kept by a false positive is not kept on every sync.
#[test]
fn seen_salt_changes_which_identities_collide() {
    let first = filled(1, 10_000);
    let second = filled(2, 10_000);
    let kept: Vec<u32> = (10_000..30_000)
        .filter(|&i| first.contains(&key(i)))
        .collect();
    assert!(!kept.is_empty());
    let kept_again = kept.iter().filter(|&&i| second.contains(&key(i))).count();
    assert!(
        kept_again * 5 < kept.len(),
        "{kept_again} of {}",
        kept.len()
    );
}

#[test]
fn seen_reset_forgets_every_identity() {
    let mut seen = filled(3, 100);
    seen.reset(4);
    assert!((0..100).all(|i| !seen.contains(&key(i))));
}

#[test]
fn seen_kind_is_part_of_the_identity() {
    let mut seen = Seen::new();
    seen.reset(5);
    let (_, hash) = key(1);
    seen.insert(&(1, hash));
    assert!(!seen.contains(&(2, hash)));
}
