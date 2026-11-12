use super::*;

const CARD: u64 = 16 * 1024 * 1024; // 8 GiB, counted in 512-byte sectors.

#[test]
fn regions_are_aligned_disjoint_and_within_the_exact_card() {
    // Regression (t-116): raw DB placement must never overlap LittleFS or card bounds.
    let layout = CardLayout::new(CARD, 4096).unwrap();
    let db = layout.database();
    let media = layout.media();
    assert_eq!(db.start(), 16);
    assert_eq!(db.byte_len(), crate::page_store::file_len(4096));
    assert!(db.start() as u64 + db.sectors() as u64 <= media.start() as u64);
    assert!(media.start().is_multiple_of(8));
    assert!(media.sectors().is_multiple_of(8));
    assert!(media.start() as u64 + media.sectors() as u64 <= CARD);
    assert_eq!(db.translate(0).unwrap(), db.start());
    assert_eq!(
        db.translate(db.sectors() as u64 - 1).unwrap(),
        db.start() + db.sectors() - 1
    );
    assert_eq!(
        db.translate(db.sectors() as u64),
        Err(LayoutError::Geometry)
    );
    assert_eq!(db.translate(u64::MAX), Err(LayoutError::Geometry));
}

#[test]
fn legacy_media_is_only_a_mount_candidate_and_never_a_format_request() {
    // Regression (t-116): absent descriptors cannot authorize repartition or formatting.
    assert_eq!(CardLayout::select(&[0xff; 512], &[0; 512], CARD), Ok(None));
    assert_eq!(CardLayout::new(16, 4096), Err(LayoutError::Geometry));
    assert_eq!(CardLayout::new(u64::MAX, 4096), Err(LayoutError::Geometry));
    assert_eq!(CardLayout::new(CARD, 31), Err(LayoutError::Geometry));
    assert_eq!(CardLayout::new(CARD, 4097), Err(LayoutError::Geometry));
}

#[test]
fn descriptor_roundtrip_and_single_copy_recovery() {
    // Regression (t-116): one torn immutable descriptor must not hide a complete descriptor.
    for pages in [32, 256, 4096] {
        let layout = CardLayout::new(CARD, pages).unwrap();
        let header = layout.encode();
        assert_eq!(CardLayout::select(&header, &header, CARD), Ok(Some(layout)));
        for index in [0, 16, 56, 200, 508] {
            let mut torn = header;
            torn[index] ^= 0x80;
            assert_eq!(CardLayout::select(&torn, &header, CARD), Ok(Some(layout)));
            assert_eq!(CardLayout::select(&header, &torn, CARD), Ok(Some(layout)));
        }
    }
}

#[test]
fn incompatible_or_conflicting_descriptors_are_rejected_without_guessing() {
    // Regression (t-116): never mount a region selected from an incompatible/ambiguous layout.
    let layout = CardLayout::new(CARD, 4096).unwrap();
    let header = layout.encode();
    assert_eq!(
        CardLayout::select(&header, &header, CARD - 1),
        Err(LayoutError::Geometry)
    );
    let other = CardLayout::new(CARD, 256).unwrap().encode();
    assert_eq!(
        CardLayout::select(&header, &other, CARD),
        Err(LayoutError::Conflict)
    );
    let mut future = header;
    future[8..12].copy_from_slice(&3u32.to_le_bytes());
    checksum(&mut future);
    assert_eq!(
        CardLayout::select(&future, &header, CARD),
        Err(LayoutError::Version)
    );
    let mut overlap = header;
    overlap[40..48].copy_from_slice(&16u64.to_le_bytes());
    checksum(&mut overlap);
    assert_eq!(
        CardLayout::select(&overlap, &overlap, CARD),
        Err(LayoutError::Geometry)
    );
    let mut torn = header;
    torn[200] ^= 1;
    assert_eq!(
        CardLayout::select(&torn, &torn, CARD),
        Err(LayoutError::Checksum)
    );
}

// Regression (t-121): logs get their own region so log allocation never scans media.
#[test]
fn v2_layout_places_log_then_unit_aligned_media() {
    let card = 8 * 1024 * 1024 * 2; // 8 GiB in sectors
    let layout = CardLayout::new(card, 4096).unwrap();
    let db_end = layout.database().start() + layout.database().sectors();
    assert_eq!(layout.log().start(), db_end.next_multiple_of(8));
    assert_eq!(layout.log().sectors(), LOG_SECTORS);
    let log_end = layout.log().start() + LOG_SECTORS;
    assert_eq!(
        layout.media().start(),
        log_end.next_multiple_of(UNIT_SECTORS)
    );
    assert_eq!(layout.media().sectors() % UNIT_SECTORS, 0);
    assert_eq!(
        layout.media_units(),
        layout.media().sectors() / UNIT_SECTORS
    );
    assert!(layout.media().start() as u64 + layout.media().sectors() as u64 <= card);
}

// Regression (t-121): v1 descriptors are no longer mountable (none exist).
#[test]
fn v1_descriptor_is_rejected_as_version() {
    let card = 8 * 1024 * 1024 * 2;
    let mut v1 = CardLayout::new(card, 4096).unwrap().encode();
    v1[..8].copy_from_slice(b"THEOSD01");
    let crc = crate::crc::crc32(&v1[..508]);
    v1[508..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(
        CardLayout::select(&v1, &v1, card),
        Err(LayoutError::Version)
    );
}

#[test]
fn v2_round_trips_and_too_small_media_is_rejected() {
    let card = 8 * 1024 * 1024 * 2;
    let layout = CardLayout::new(card, 4096).unwrap();
    let bytes = layout.encode();
    assert_eq!(CardLayout::select(&bytes, &bytes, card), Ok(Some(layout)));
    let tight = layout.media().start() as u64 + 65_536 - 1;
    assert_eq!(CardLayout::new(tight, 4096), Err(LayoutError::Geometry));
}
