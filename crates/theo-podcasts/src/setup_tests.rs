use super::*;
#[test]
fn setup_accepts_only_bounded_local_domain_commands_and_discards_overflow_line() {
    // Regression (t-110): oversized local setup input cannot execute a valid suffix or introduce generic feed support.
    assert!(matches!(
        parse("TIME 1793500000"),
        Ok(SetupCommand::Time(1793500000))
    ));
    assert!(parse("TIME 0").is_err());
    assert!(parse("FOLLOW https://example.com/rss").is_err());
    assert!(matches!(
        parse("READ 42"),
        Ok(SetupCommand::Read(crate::catalogue::EpisodeId(42)))
    ));
    let mut input = SetupInput::default();
    for _ in 0..600 {
        assert!(input.push(b'x').is_none());
    }
    for &byte in b"SYNC" {
        assert!(input.push(byte).is_none());
    }
    assert!(input.push(b'\n').unwrap().is_err());
    for &byte in b"SYNC" {
        assert!(input.push(byte).is_none());
    }
    assert!(matches!(input.push(b'\n').unwrap(), Ok(SetupCommand::Sync)));
}
