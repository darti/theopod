use super::*;
use std::vec::Vec;
type Parsed = (
    Vec<(std::string::String, std::string::String)>,
    Option<std::string::String>,
);
fn parse(body: &[u8]) -> Result<Parsed, FeedError> {
    let mut parser = RadioFranceFeed::new();
    let mut items = Vec::new();
    for (offset, &byte) in body.iter().enumerate() {
        let result = parser.push(byte);
        if result.is_err() {
            std::eprintln!(
                "offset {offset}, context {:?}",
                std::string::String::from_utf8_lossy(
                    &body[offset.saturating_sub(100)..(offset + 20).min(body.len())]
                )
            );
        }
        if let Some(item) = result? {
            items.push((item.guid.as_str().into(), item.title.as_str().into()));
        }
    }
    Ok((items, parser.finish()?.map(Into::into)))
}
#[test]
fn complete_real_history_streams_without_buffering_pages() {
    // Regression (t-108): ignoring rel=next or duplicate GUIDs loses history.
    let (first, next) = parse(include_bytes!("../fixtures/radio-france-page0.xml")).unwrap();
    let (last, end) = parse(include_bytes!("../fixtures/radio-france-page1.xml")).unwrap();
    assert_eq!(first.len(), 100);
    assert_eq!(last.len(), 34);
    assert_eq!(
        next.as_deref(),
        Some("https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b?page=1")
    );
    assert!(end.is_none());
    let unique: std::collections::BTreeSet<_> = first.iter().chain(&last).map(|x| &x.0).collect();
    assert_eq!(unique.len(), 129);
    assert_eq!(first[0].0, "e0c4bd07-bf68-4880-8c69-b28cefbc87bf");
    assert!(first[0].1.starts_with("Melita Norwood"));
}
fn document(item: &str) -> std::string::String {
    std::format!(
        "<rss version='2.0' xmlns:itunes='http://www.itunes.com/dtds/podcast-1.0.dtd'><channel><item>{item}</item></channel></rss>"
    )
}
#[test]
fn entities_cdata_and_optional_fields_survive_single_byte_input() {
    // Regression (t-108): chunk boundaries cannot corrupt UTF-8/entities/CDATA.
    let body = document(
        "<title>A &amp; B &#x1F600;</title><guid><![CDATA[x<y&z]]></guid><description><![CDATA[été <b>oui</b>]]></description><enclosure url='https://cdn.example/a?x=1&amp;y=2' length='42'/><pubDate>Sat, 27 Jun 2020 11:17:40 GMT</pubDate><itunes:duration>00:33:04</itunes:duration>",
    );
    let mut parser = RadioFranceFeed::new();
    let mut count = 0;
    for &b in body.as_bytes() {
        if let Some(item) = parser.push(b).unwrap() {
            count += 1;
            assert_eq!(item.title, "A & B 😀");
            assert_eq!(item.guid, "x<y&z");
            assert_eq!(item.description, "été <b>oui</b>");
            assert_eq!(item.enclosure, "https://cdn.example/a?x=1&y=2");
            assert_eq!(item.enclosure_bytes, Some(42));
            assert_eq!(item.published, Some(1593256660));
            assert_eq!(item.duration_seconds, Some(1984));
        }
    }
    assert_eq!(count, 1);
    parser.finish().unwrap();
}
#[test]
fn invalid_or_truncated_xml_never_finishes() {
    // Regression (t-108): partial/bad XML must not publish completed-sync validators.
    for input in [
        "",
        "<rss><channel>",
        "<rss><channel></rss>",
        "<rss><channel/></rss>junk",
        "<rss><channel/></rss><rss/>",
        "<!DOCTYPE rss [<!ENTITY x SYSTEM 'file:///secret'>]><rss><channel/></rss>",
        "<rss><channel><item><title>&external;</title></item></channel></rss>",
        "<rss><channel><item><title>x</title><enclosure url='a' url='b'/></item></channel></rss>",
        "<rss><channel><item><title>x</title><enclosure url='https://cdn.example/a'/><guid>&#0;</guid></item></channel></rss>",
    ] {
        assert!(parse(input.as_bytes()).is_err(), "{input}");
    }
}
#[test]
fn required_limits_fail_optional_long_description_is_omitted() {
    // Regression (t-108): oversized identities cannot alias; optional prose is droppable.
    for (tag, cap) in [("title", MAX_TITLE_BYTES), ("guid", MAX_GUID_BYTES)] {
        let body = document(&std::format!(
            "<title>ok</title><enclosure url='https://cdn.example/a'/><{tag}>{}</{tag}>",
            "x".repeat(cap + 1)
        ));
        assert!(parse(body.as_bytes()).is_err());
    }
    let body = document(&std::format!(
        "<title>ok</title><description>{}</description><pubDate>nonsense</pubDate><enclosure url='https://cdn.example/a'/>",
        "x".repeat(10000)
    ));
    let mut parser = RadioFranceFeed::new();
    let mut count = 0;
    for &byte in body.as_bytes() {
        if let Some(item) = parser.push(byte).unwrap() {
            count += 1;
            assert!(item.description.is_empty());
            assert!(item.guid.is_empty());
            assert_eq!(item.published, None);
        }
    }
    assert_eq!(count, 1);
    parser.finish().unwrap();
}
#[test]
fn source_boundary_and_pagination_cannot_escape_or_loop() {
    // Regression (t-108): a next link cannot switch shows/origins or replay a page.
    let root = "https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b";
    let feed = FeedUrl::parse(root).unwrap();
    assert_eq!(feed.page, 0);
    for bad in [
        "https://evil.example/rss/4a41823f-f1f7-4725-8380-e428893eb93b",
        "http://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b",
        "https://rss-rf.aerion.me@evil.example/rss/a",
        "https://rss-rf.aerion.me/rss/a",
        "https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b?page=-1",
        "https://media.radiofrance-podcast.net/a.m4a",
    ] {
        assert!(FeedUrl::parse(bad).is_err());
    }
    assert!(feed.next(&std::format!("{root}?page=1")).is_ok());
    assert!(feed.next(root).is_err());
    assert!(feed.next(&std::format!("{root}?page=2")).is_err());
    assert!(
        feed.next("https://rss-rf.aerion.me/rss/aaaaaaaa-f1f7-4725-8380-e428893eb93b?page=1")
            .is_err()
    );
}

#[test]
fn oversized_description_may_end_mid_unicode_scalar() {
    // Regression (t-108): omitted prose must not reject a valid whole document.
    let body = document(&std::format!(
        "<title>ok</title><description>{}é tail</description><enclosure url='https://cdn.example/a'/>",
        "a".repeat(MAX_DESCRIPTION_BYTES - 1)
    ));
    assert_eq!(parse(body.as_bytes()).unwrap().0.len(), 1);
}
#[test]
fn malformed_dates_are_absent_and_offsets_are_normalized() {
    // Regression (t-108): invalid dates cannot reorder pressure reclamation.
    for (date, want) in [
        ("Sat, 27 Jun 2020 11:17:40 GMT", Some(1593256660)),
        ("Sat, 27 Jun 2020 13:17:40 +0200", Some(1593256660)),
        ("Mon, 31 Feb 2020 11:17:40 GMT", None),
        ("Sat, 27 Jun 2020 24:17:40 GMT", None),
        ("Sat, 27 Jun 2020 11:17:40 +2460", None),
        ("bad", None),
    ] {
        assert_eq!(parse_date(date), want);
    }
}
#[test]
fn xml_cdata_terminator_and_declarations_are_checked() {
    // Regression (t-108): malformed raw CDATA terminators cannot certify valid XML.
    assert!(
        parse(
            document("<title>bad ]]&gt;</title><enclosure url='https://cdn.example/a'/>")
                .as_bytes()
        )
        .is_ok()
    );
    assert!(
        parse(
            document("<title>bad ]]></title><enclosure url='https://cdn.example/a'/>").as_bytes()
        )
        .is_err()
    );
    assert!(
        parse(
            document("<title>bad \u{fffe}</title><enclosure url='https://cdn.example/a'/>")
                .as_bytes()
        )
        .is_err()
    );
    assert!(parse(b"<?xml version='1.0' encoding='ISO-8859-1'?><rss><channel/></rss>").is_err());
}
#[test]
fn declarations_and_numeric_references_require_xml_grammar() {
    // Regression (t-108): malformed declarations/references must not certify a response.
    for prefix in [
        "<?xml garbage?>",
        "<?xml version='2.0'?>",
        "<?xml version='1.0'?><?xml version='1.0'?>",
        " <?xml version='1.0'?>",
        "<!--before--><?xml version='1.0'?>",
        "<?xml encoding='UTF-8' version='1.0'?>",
        "<?xml version='1.0' junk='x'?>",
    ] {
        let body = std::format!("{prefix}<rss><channel/></rss>");
        assert!(parse(body.as_bytes()).is_err(), "{prefix}");
    }
    for value in ["&#+65;", "&#x+41;", "&#;", "&#x;", "&#-1;"] {
        assert!(
            parse(
                document(&std::format!(
                    "<title>{value}</title><enclosure url='https://cdn.example/a'/>"
                ))
                .as_bytes()
            )
            .is_err(),
            "{value}"
        );
    }
}
#[test]
fn pagination_resolves_scoped_atom_namespace_aliases() {
    // Regression (t-108): XML prefix spelling must never silently drop historical pages.
    let url = "https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b?page=1";
    for link in [
        std::format!("<a:link xmlns:a='http://www.w3.org/2005/Atom' rel='next' href='{url}'/>"),
        std::format!("<link xmlns='http://www.w3.org/2005/Atom' rel='next' href='{url}'/>"),
    ] {
        let body = std::format!("<rss><channel>{link}</channel></rss>");
        assert_eq!(parse(body.as_bytes()).unwrap().1.as_deref(), Some(url));
    }
    for bad in [
        std::format!("<atom:link rel='next' href='{url}'/>"),
        std::format!("<atom:link xmlns:atom='wrong' rel='next' href='{url}'/>"),
    ] {
        assert!(parse(std::format!("<rss><channel>{bad}</channel></rss>").as_bytes()).is_err());
    }
    let body = std::format!(
        "<rss xmlns:a='wrong'><channel><a:link xmlns:a='http://www.w3.org/2005/Atom' rel='next' href='{url}'/><a:other/></channel></rss>"
    );
    assert_eq!(parse(body.as_bytes()).unwrap().1.as_deref(), Some(url));
}
