//! Which card an `sd-provision` firmware build may erase (ADR-0021).
//!
//! The build carries `THEO_SD_PROVISION`, parsed at compile time by
//! [`parse_request`] so a malformed value fails the build instead of booting
//! as a silent no-op. [`Target::authorizes`] is the whole run-time guard.
use core::fmt;
use theo_storage::{
    card_layout::{CardLayout, LayoutError},
    redb_backend::{LENGTH_SECTORS, region_is_corrupt},
};

/// The first sectors of a v3 card's database region: both length record
/// copies, then the start of the redb file.
pub type CatalogueHead = [[u8; 512]; LENGTH_SECTORS as usize + 1];

/// An SD card's identity: CID manufacturer ID and product serial number.
///
/// Displayed and parsed as ten hex digits, `MMSSSSSSSS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardId(u64);

impl CardId {
    /// The identity read from a card's CID register.
    pub const fn new(manufacturer: u8, serial: u32) -> Self {
        Self(((manufacturer as u64) << 32) | serial as u64)
    }
}

impl fmt::Display for CardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:010x}", self.0)
    }
}

/// What an `sd-provision` build was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// `format`: format a blank card as a whole-card LittleFS (legacy).
    Format,
    /// `v3:<sectors>:<card id>`: erase that one card and publish layout v3.
    V3(Target),
}

/// The one card a `v3` request may erase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Exact capacity in 512-byte sectors (boot log "microSD capacity").
    pub sectors: u64,
    /// Identity (boot log "microSD card id").
    pub card: CardId,
}

/// Why a [`Target`] refused a card; nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The card has another capacity.
    Size(u64),
    /// The card has another identity, or none could be read.
    Card(Option<CardId>),
    /// The card already has layout v3 and a catalogue that is not corrupt.
    AlreadyV3,
    /// A descriptor is unreadable as a v3 layout of this card.
    Descriptor(LayoutError),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Size(sectors) => write!(f, "the card has {sectors} sectors"),
            Self::Card(Some(card)) => write!(f, "the card id is {card}"),
            Self::Card(None) => f.write_str("the card id could not be read"),
            Self::AlreadyV3 => f.write_str("the card already has layout v3"),
            Self::Descriptor(error) => write!(f, "invalid descriptor ({error:?})"),
        }
    }
}

impl Target {
    /// Allows erasing only this exact card, and only before it has a usable
    /// v3 layout.
    ///
    /// `first` and `second` are the card's descriptor sectors. A blank card,
    /// a legacy whole-card LittleFS or an obsolete (v1/v2) layout passes; an
    /// unknown version or a damaged descriptor does not. A valid v3 layout
    /// passes only if `catalogue`, the head of its database region (`None`
    /// if unread), shows a corrupt catalogue: no valid length record, or a
    /// non-empty redb file without redb's magic. Nothing boot could open is
    /// erased.
    pub fn authorizes(
        &self,
        sectors: u64,
        card: Option<CardId>,
        first: &[u8; 512],
        second: &[u8; 512],
        catalogue: Option<&CatalogueHead>,
    ) -> Result<(), Refusal> {
        if sectors != self.sectors {
            return Err(Refusal::Size(sectors));
        }
        if card != Some(self.card) {
            return Err(Refusal::Card(card));
        }
        match CardLayout::select(first, second, sectors) {
            Ok(None) | Err(LayoutError::Obsolete) => Ok(()),
            Ok(Some(layout)) => match catalogue {
                Some(head) if region_is_corrupt(head, layout.database().sectors()) => Ok(()),
                _ => Err(Refusal::AlreadyV3),
            },
            Err(error) => Err(Refusal::Descriptor(error)),
        }
    }
}

/// Parses `format` or `v3:<decimal sectors>:<10 hex digit card id>`.
///
/// `const` so the firmware rejects a malformed value at compile time.
pub const fn parse_request(spec: &str) -> Option<Request> {
    let bytes = spec.as_bytes();
    if matches!(bytes, b"format") {
        return Some(Request::Format);
    }
    let [b'v', b'3', b':', rest @ ..] = bytes else {
        return None;
    };
    let mut sectors: u64 = 0;
    let mut index = 0;
    while index < rest.len() && rest[index] != b':' {
        let digit = rest[index];
        if !digit.is_ascii_digit() {
            return None;
        }
        sectors = match sectors.checked_mul(10) {
            Some(value) => match value.checked_add((digit - b'0') as u64) {
                Some(value) => value,
                None => return None,
            },
            None => return None,
        };
        index += 1;
    }
    // At least one digit, then ':' and exactly ten hex digits.
    if index == 0 || rest.len() != index + 11 {
        return None;
    }
    let mut card: u64 = 0;
    index += 1;
    while index < rest.len() {
        let nibble = match rest[index] {
            digit @ b'0'..=b'9' => digit - b'0',
            digit @ b'a'..=b'f' => digit - b'a' + 10,
            digit @ b'A'..=b'F' => digit - b'A' + 10,
            _ => return None,
        };
        card = (card << 4) | nibble as u64;
        index += 1;
    }
    Some(Request::V3(Target {
        sectors,
        card: CardId(card),
    }))
}

#[cfg(test)]
#[path = "provision_guard_tests.rs"]
mod tests;
