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
        TARGET.authorizes(SECTORS, Some(CARD), &BLANK, &BLANK),
        Ok(())
    );
    // A whole-card LittleFS superblock is not a descriptor.
    let mut legacy = [0xa5; 512];
    legacy[8..16].copy_from_slice(b"littlefs");
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &legacy, &legacy),
        Ok(())
    );
}

#[test]
fn authorizes_refuses_another_size_or_card() {
    // Regression (t-133): a same-size card swapped in must not be erased.
    assert_eq!(
        TARGET.authorizes(SECTORS - 1, Some(CARD), &BLANK, &BLANK),
        Err(Refusal::Size(SECTORS - 1))
    );
    let other = CardId::new(0x1b, 0x1234_abce);
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(other), &BLANK, &BLANK),
        Err(Refusal::Card(Some(other)))
    );
    assert_eq!(
        TARGET.authorizes(SECTORS, None, &BLANK, &BLANK),
        Err(Refusal::Card(None))
    );
}

#[test]
fn authorizes_refuses_a_card_with_any_descriptor() {
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &v3(), &v3()),
        Err(Refusal::AlreadyV3)
    );
    // One published copy is a complete layout (regions precede descriptors).
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &v3(), &BLANK),
        Err(Refusal::AlreadyV3)
    );
    let mut v1 = BLANK;
    v1[..8].copy_from_slice(b"THEOSD01");
    // A v1 card is refused, not upgraded (ADR-0021).
    assert!(matches!(
        TARGET.authorizes(SECTORS, Some(CARD), &v1, &BLANK),
        Err(Refusal::Descriptor(_))
    ));
    let mut corrupt = v3();
    corrupt[100] ^= 1;
    assert_eq!(
        TARGET.authorizes(SECTORS, Some(CARD), &corrupt, &BLANK),
        Err(Refusal::Descriptor(LayoutError::Checksum))
    );
}
