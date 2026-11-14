use super::*;
use std::string::ToString;

const SECTORS: u64 = 122_265_600;
const CARD: CardId = CardId::new(0x1b, 0x1234_abcd);
const TARGET: Target = Target {
    sectors: SECTORS,
    card: CARD,
};
const BLANK: [u8; 512] = [0; 512];

fn v3() -> [u8; 512] {
    CardLayout::new(SECTORS).unwrap().encode()
}

#[test]
fn parse_request_accepts_format_and_a_v3_target() {
    assert_eq!(parse_request("format"), Some(Request::Format));
    assert_eq!(
        parse_request("v3:122265600:1b1234ABcd"),
        Some(Request::V3(TARGET))
    );
}

#[test]
fn parse_request_malformed_values_return_none() {
    // Regression (t-133): the firmware turns `None` into a build error; a
    // malformed value used to boot as a silent no-op.
    for spec in [
        "",
        "v3",
        "v3:",
        "v3:122265600",
        "v3:122265600:",
        "v3::1b1234abcd",
        "v3:122,265,600:1b1234abcd",
        "v3:122265600:1b1234abc",
        "v3:122265600:1b1234abcde",
        "v3:122265600:1b1234abcg",
        "v3:99999999999999999999:1b1234abcd",
        "v2:122265600:1b1234abcd",
        "Format",
    ] {
        assert_eq!(parse_request(spec), None, "{spec:?}");
    }
}

#[test]
fn card_id_displays_as_ten_hex_digits() {
    assert_eq!(CARD.to_string(), "1b1234abcd");
    assert_eq!(CardId::new(0, 7).to_string(), "0000000007");
}

#[test]
fn authorizes_the_target_card_when_blank_or_legacy() {
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &BLANK, &BLANK, None),
        Ok(())
    );
    // A whole-card LittleFS superblock is not a descriptor.
    let mut legacy = [0xa5; 512];
    legacy[8..16].copy_from_slice(b"littlefs");
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &legacy, &legacy, None),
        Ok(())
    );
}

#[test]
fn authorizes_refuses_another_size_or_card() {
    // Regression (t-133): a same-size card swapped in must not be erased.
    assert_eq!(
        TARGET.authorizes(SECTORS - 1, Some(CARD), &BLANK, &BLANK, None),
        Err(Refusal::Size(SECTORS - 1))
    );
    let other = CardId::new(0x1b, 0x1234_abce);
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(other), &BLANK, &BLANK, None),
        Err(Refusal::Card(Some(other)))
    );
    assert_eq!(
        TARGET.authorizes(SECTORS, None, &BLANK, &BLANK, None),
        Err(Refusal::Card(None))
    );
}

#[test]
fn authorizes_refuses_a_card_with_any_descriptor() {
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &v3(), &v3(), None),
        Err(Refusal::AlreadyV3)
    );
    // One published copy is a complete layout (regions precede descriptors).
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &v3(), &BLANK, None),
        Err(Refusal::AlreadyV3)
    );
    let mut v1 = BLANK;
    v1[..8].copy_from_slice(b"THEOSD01");
    // A v1 card is refused, not upgraded (ADR-0021).
    assert!(matches!(
        TARGET.authorizes(SECTORS, Some(CARD), &v1, &BLANK, None),
        Err(Refusal::Descriptor(_))
    ));
    let mut corrupt = v3();
    corrupt[100] ^= 1;
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &corrupt, &BLANK, None),
        Err(Refusal::Descriptor(LayoutError::Checksum))
    );
}

/// `v3()` re-encoded as an older or newer descriptor.
fn versioned(magic: &[u8; 8], version: u32) -> [u8; 512] {
    let mut bytes = v3();
    bytes[..8].copy_from_slice(magic);
    bytes[8..12].copy_from_slice(&version.to_le_bytes());
    let crc = theo_storage::crc::crc32(&bytes[..508]);
    bytes[508..].copy_from_slice(&crc.to_le_bytes());
    bytes
}

#[test]
fn authorizes_a_v2_card_only_when_it_is_the_target() {
    // Regression (t-133): v2 cards must be re-provisionable to v3.
    let v2 = versioned(b"THEOSD02", 2);
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &v2, &v2, None),
        Ok(())
    );
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &v2, &BLANK, None),
        Ok(())
    );
    let other = CardId::new(0x1b, 0x1234_abce);
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(other), &v2, &v2, None),
        Err(Refusal::Card(Some(other)))
    );
    assert_eq!(
        TARGET.authorizes(SECTORS - 1, Some(CARD), &v2, &v2, None),
        Err(Refusal::Size(SECTORS - 1))
    );
}

#[test]
fn authorizes_refuses_an_unknown_version() {
    // Regression guard (t-133): only obsolete layouts may be replaced.
    let v4 = versioned(b"THEOSD03", 4);
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &v4, &v4, None),
        Err(Refusal::Descriptor(LayoutError::Version))
    );
    let v2 = versioned(b"THEOSD02", 2);
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &v2, &v3(), None),
        Err(Refusal::Descriptor(LayoutError::Conflict))
    );
}

#[test]
fn authorizes_refuses_an_unknown_theosd_descriptor() {
    // Regression (t-133): a `THEOSD04` descriptor used to read as "no
    // descriptor", so provisioning would have erased a newer layout.
    let future = versioned(b"THEOSD04", 4);
    for (first, second) in [(&future, &future), (&future, &BLANK), (&BLANK, &future)] {
        assert_eq!(
            TARGET.authorizes(SECTORS, Some(CARD), first, second, None),
            Err(Refusal::Descriptor(LayoutError::Version))
        );
    }
}

/// A v3 card's database region head: `format`ted, then `set_len(len)` and
/// `file` written at the start of the redb file.
fn catalogue(len: u64, file: [u8; 512]) -> CatalogueHead {
    use theo_storage::{littlefs::SectorIo, redb_backend::RegionBackend, test_disk::TestDisk};
    let sectors = theo_storage::card_layout::DATABASE_SECTORS;
    let mut disk = TestDisk::new(sectors);
    RegionBackend::format(&mut disk).unwrap();
    // SAFETY: single-threaded test.
    let backend = unsafe { RegionBackend::new(disk.clone(), sectors) }.unwrap();
    if len > 0 {
        redb::StorageBackend::set_len(&backend, len).unwrap();
        redb::StorageBackend::write(&backend, 0, &file).unwrap();
    }
    let mut head = [[0; 512]; 3];
    for (index, sector) in (0..).zip(head.iter_mut()) {
        disk.read_sector(index, sector).unwrap();
    }
    head
}
fn redb_file() -> [u8; 512] {
    let mut file = [0x5a; 512];
    file[..REDB_MAGIC.len()].copy_from_slice(&REDB_MAGIC);
    file
}
use theo_storage::redb_backend::REDB_MAGIC;

#[test]
fn authorizes_a_v3_card_whose_catalogue_is_corrupt() {
    // Regression (t-133): a v3 card whose catalogue is corrupt (first open
    // cut, or damaged records) was refused as `AlreadyV3` and stuck.
    let no_records = [[0xa5; 512]; 3];
    let no_magic = catalogue(4096, [0; 512]);
    for (name, head) in [("no records", &no_records), ("no magic", &no_magic)] {
        assert_eq!(
            TARGET.authorizes(SECTORS, Some(CARD), &v3(), &v3(), Some(head)),
            Ok(()),
            "{name}"
        );
        // Still only the exact target card.
        let other = CardId::new(0x1b, 0x1234_abce);
        assert_eq!(
            TARGET.authorizes(SECTORS, Some(other), &v3(), &v3(), Some(head)),
            Err(Refusal::Card(Some(other))),
            "{name}"
        );
        assert_eq!(
            TARGET.authorizes(SECTORS - 1, Some(CARD), &v3(), &v3(), Some(head)),
            Err(Refusal::Size(SECTORS - 1)),
            "{name}"
        );
    }
}

#[test]
fn authorizes_refuses_a_v3_card_whose_catalogue_is_healthy_or_unread() {
    // Regression guard (t-133): a usable catalogue is never erased.
    for (name, head) in [
        ("empty", Some(catalogue(0, [0; 512]))),
        ("created", Some(catalogue(4096, redb_file()))),
        ("unread", None),
    ] {
        assert_eq!(
            TARGET.authorizes(SECTORS, Some(CARD), &v3(), &v3(), head.as_ref()),
            Err(Refusal::AlreadyV3),
            "{name}"
        );
    }
}
