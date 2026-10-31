//! Incremental, bounded parser for the Radio France RSS source.
use crate::catalogue::*;
use heapless::String;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedError {
    Xml,
    Limit,
    Source,
    Metadata,
    Incomplete,
}
#[derive(Debug, Default)]
pub struct OwnedEpisode {
    pub guid: String<MAX_GUID_BYTES>,
    pub title: String<MAX_TITLE_BYTES>,
    pub enclosure: String<MAX_URL_BYTES>,
    pub description: String<MAX_DESCRIPTION_BYTES>,
    pub published: Option<i64>,
    pub enclosure_bytes: Option<u64>,
    pub duration_seconds: Option<u32>,
}
impl OwnedEpisode {
    pub fn metadata(&self) -> EpisodeMetadata<'_> {
        EpisodeMetadata {
            guid: (!self.guid.is_empty()).then_some(self.guid.as_str()),
            title: &self.title,
            enclosure: &self.enclosure,
            description: (!self.description.is_empty()).then_some(self.description.as_str()),
            published: self.published,
            enclosure_bytes: self.enclosure_bytes,
            duration_seconds: self.duration_seconds,
        }
    }
}
use heapless::Vec;
#[derive(Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Text,
    Tag,
    Comment,
    Cdata,
    Pi,
}
/// One bounded item, a 4096-byte markup token and a 16-element XML stack.
/// Feed one byte at a time from a bounded transport chunk. On an emitted item,
/// await its catalogue upsert before reusing this parser. Any error is terminal.
#[derive(Default)]
pub struct RadioFranceFeed {
    item: OwnedEpisode,
    stack: Vec<String<64>, 16>,
    token: Vec<u8, 4096>,
    text: Vec<u8, MAX_URL_BYTES>,
    entity: Vec<u8, 16>,
    in_entity: bool,
    overflow: bool,
    mode: Mode,
    quote: u8,
    tail: usize,
    root: bool,
    channel: bool,
    done: bool,
    failed: bool,
    next: String<MAX_URL_BYTES>,
    utf8: Vec<u8, 4>,
    utf8_left: usize,
    text_brackets: usize,
    bytes_seen: u64,
    namespaces: Vec<Namespace, 16>,
}
const ATOM: &str = "http://www.w3.org/2005/Atom";
const ITUNES: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";
struct Namespace {
    prefix: String<64>,
    uri: String<128>,
    depth: usize,
}
impl RadioFranceFeed {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, byte: u8) -> Result<Option<&OwnedEpisode>, FeedError> {
        if self.failed {
            return Err(FeedError::Xml);
        }
        match self.step(byte) {
            Ok(emit) => Ok(emit.then_some(&self.item)),
            Err(e) => {
                self.failed = true;
                Err(e)
            }
        }
    }
    pub fn finish(&self) -> Result<Option<&str>, FeedError> {
        if self.failed
            || !self.done
            || self.mode != Mode::Text
            || self.in_entity
            || self.utf8_left != 0
            || !self.stack.is_empty()
        {
            return Err(FeedError::Incomplete);
        }
        Ok((!self.next.is_empty()).then_some(self.next.as_str()))
    }
    fn step(&mut self, b: u8) -> Result<bool, FeedError> {
        self.bytes_seen = self.bytes_seen.checked_add(1).ok_or(FeedError::Limit)?;
        // Validate even ignored text: no malformed UTF-8 or forbidden XML controls.
        if self.utf8_left != 0 {
            self.utf8.push(b).map_err(|_| FeedError::Xml)?;
            self.utf8_left -= 1;
            if self.utf8_left == 0 {
                let scalar = core::str::from_utf8(&self.utf8)
                    .map_err(|_| FeedError::Xml)?
                    .chars()
                    .next()
                    .ok_or(FeedError::Xml)?;
                if !xml_char(scalar) {
                    return Err(FeedError::Xml);
                }
                self.utf8.clear();
            }
        } else if b >= 128 {
            self.utf8_left = match b {
                0xc2..=0xdf => 1,
                0xe0..=0xef => 2,
                0xf0..=0xf4 => 3,
                _ => return Err(FeedError::Xml),
            };
            self.utf8.push(b).map_err(|_| FeedError::Xml)?;
        } else if b < 32 && !matches!(b, b'\t' | b'\n' | b'\r') {
            return Err(FeedError::Xml);
        }
        match self.mode {
            Mode::Text => {
                if b == b'<' {
                    if self.in_entity {
                        return Err(FeedError::Xml);
                    }
                    self.mode = Mode::Tag;
                    self.token.clear();
                    self.quote = 0;
                    self.text_brackets = 0;
                } else {
                    if b == b']' {
                        self.text_brackets = (self.text_brackets + 1).min(2);
                    } else {
                        if b == b'>' && self.text_brackets == 2 {
                            return Err(FeedError::Xml);
                        }
                        self.text_brackets = 0;
                    }
                    self.text_byte(b, false)?;
                }
            }
            Mode::Tag => {
                if self.quote == 0 && b == b'>' {
                    return self.tag();
                }
                self.token.push(b).map_err(|_| FeedError::Limit)?;
                if self.token == b"!--" {
                    self.mode = Mode::Comment;
                    self.tail = 0;
                } else if self.token == b"![CDATA[" {
                    if self.stack.is_empty() {
                        return Err(FeedError::Xml);
                    }
                    self.mode = Mode::Cdata;
                    self.tail = 0;
                } else if self.token == b"?" {
                    if self.bytes_seen != 2 {
                        return Err(FeedError::Xml);
                    }
                    self.mode = Mode::Pi;
                    self.tail = 0;
                } else if b == b'\'' || b == b'"' {
                    if self.quote == b {
                        self.quote = 0;
                    } else if self.quote == 0 {
                        self.quote = b;
                    }
                }
            }
            Mode::Comment => {
                if b == b'-' {
                    self.tail += 1;
                    if self.tail > 2 {
                        return Err(FeedError::Xml);
                    }
                } else if self.tail == 2 {
                    if b != b'>' {
                        return Err(FeedError::Xml);
                    }
                    self.mode = Mode::Text;
                    self.tail = 0;
                } else {
                    self.tail = 0;
                }
            }
            Mode::Cdata => {
                if b == b']' {
                    if self.tail == 2 {
                        self.text_byte(b']', true)?;
                    } else {
                        self.tail += 1;
                    }
                } else if b == b'>' && self.tail >= 2 {
                    for _ in 2..self.tail {
                        self.text_byte(b']', true)?;
                    }
                    self.tail = 0;
                    self.mode = Mode::Text;
                } else {
                    for _ in 0..self.tail {
                        self.text_byte(b']', true)?;
                    }
                    self.tail = 0;
                    self.text_byte(b, true)?;
                }
            }
            Mode::Pi => {
                // Only an XML declaration at the beginning is supported.
                if self.root {
                    return Err(FeedError::Xml);
                }
                self.token.push(b).map_err(|_| FeedError::Limit)?;
                if b == b'>' && self.tail == 1 {
                    declaration(core::str::from_utf8(&self.token).map_err(|_| FeedError::Xml)?)?;
                    self.mode = Mode::Text;
                }
                self.tail = usize::from(b == b'?');
            }
        }
        Ok(false)
    }
    fn text_byte(&mut self, b: u8, literal: bool) -> Result<(), FeedError> {
        if !literal && self.in_entity {
            if b == b';' {
                let value = entity_char(&self.entity)?;
                let mut bytes = [0; 4];
                for &c in value.encode_utf8(&mut bytes).as_bytes() {
                    self.append(c)?;
                }
                self.in_entity = false;
                self.entity.clear();
            } else {
                self.entity.push(b).map_err(|_| FeedError::Xml)?;
            }
        } else if !literal && b == b'&' {
            self.in_entity = true;
            self.entity.clear();
        } else {
            self.append(b)?;
        }
        Ok(())
    }
    fn namespace(&self, name: &str, attribute: bool) -> Result<&str, FeedError> {
        let prefix = match name.split_once(':') {
            Some(("xml", _)) => return Ok("http://www.w3.org/XML/1998/namespace"),
            Some(("xmlns", _)) => return Ok("http://www.w3.org/2000/xmlns/"),
            Some((prefix, _)) => prefix,
            None if attribute => return Ok(""),
            None => "",
        };
        match self.namespaces.iter().rev().find(|ns| ns.prefix == prefix) {
            Some(ns) => Ok(&ns.uri),
            None if prefix.is_empty() => Ok(""),
            None => Err(FeedError::Xml),
        }
    }
    fn field(&self) -> Option<&str> {
        if self.stack.len() != 4 || self.stack[2] != "item" {
            return None;
        }
        match self.stack[3].as_str() {
            "title" | "guid" | "description" | "pubDate"
                if self.namespace(&self.stack[3], false).ok() == Some("") =>
            {
                Some(self.stack[3].as_str())
            }
            s if local_name(s) == "duration" && self.namespace(s, false).ok() == Some(ITUNES) => {
                Some("duration")
            }
            _ => None,
        }
    }
    fn append(&mut self, b: u8) -> Result<(), FeedError> {
        let Some(field) = self.field() else {
            if self.stack.is_empty() && !b.is_ascii_whitespace() {
                return Err(FeedError::Xml);
            }
            return Ok(());
        };
        let limit = match field {
            "guid" => MAX_GUID_BYTES,
            "title" => MAX_TITLE_BYTES,
            "description" => MAX_DESCRIPTION_BYTES,
            _ => 64,
        };
        if self.text.len() >= limit {
            if field == "description" {
                self.overflow = true;
                return Ok(());
            }
            return Err(FeedError::Limit);
        }
        self.text.push(b).map_err(|_| FeedError::Limit)
    }
    fn tag(&mut self) -> Result<bool, FeedError> {
        self.mode = Mode::Text;
        // Move the fixed token temporarily so tag processing may mutate self.
        let token = core::mem::take(&mut self.token);
        let value = core::str::from_utf8(&token).map_err(|_| FeedError::Xml)?;
        let result = self.process_tag(value);
        self.token = token;
        self.token.clear();
        result
    }
    fn process_tag(&mut self, value: &str) -> Result<bool, FeedError> {
        if let Some(end) = value.strip_prefix('/') {
            let name = end.trim_end_matches(xml_space);
            if !valid_name(name) || self.stack.last().map(|n| n.as_str()) != Some(name) {
                return Err(FeedError::Xml);
            }
            return self.close();
        }
        let empty = value.ends_with('/');
        let value = if empty {
            &value[..value.len() - 1]
        } else {
            value
        };
        let n = value.find(xml_space).unwrap_or(value.len());
        let name = &value[..n];
        if !valid_name(name) || self.done {
            return Err(FeedError::Xml);
        }
        if self.field().is_some() {
            return Err(FeedError::Xml);
        }
        if self.stack.is_empty() {
            if self.root || name != "rss" {
                return Err(FeedError::Xml);
            }
            self.root = true;
        } else if self.stack.len() == 1 {
            if name != "channel" || self.channel {
                return Err(FeedError::Xml);
            }
            self.channel = true;
        } else if name == "item" {
            if self.stack.len() != 2 {
                return Err(FeedError::Xml);
            }
            self.item = OwnedEpisode::default();
        }
        let item_child = self.stack.len() == 3 && self.stack[2] == "item";
        let channel_child = self.stack.len() == 2;
        let mut href = String::<MAX_URL_BYTES>::new();
        let mut rel = String::<16>::new();
        attributes(&value[n..], |key, decoded| {
            if key == "xmlns" || key.starts_with("xmlns:") {
                let prefix = key.strip_prefix("xmlns:").unwrap_or("");
                if prefix == "xmlns"
                    || prefix == "xml"
                    || (!prefix.is_empty() && decoded.is_empty())
                {
                    return Err(FeedError::Xml);
                }
                self.namespaces
                    .push(Namespace {
                        prefix: String::try_from(prefix).map_err(|_| FeedError::Limit)?,
                        uri: String::try_from(decoded).map_err(|_| FeedError::Limit)?,
                        depth: self.stack.len() + 1,
                    })
                    .map_err(|_| FeedError::Limit)?;
            }
            if item_child && name == "enclosure" {
                match key {
                    "url" => {
                        if !self.item.enclosure.is_empty() {
                            return Err(FeedError::Metadata);
                        }
                        self.item
                            .enclosure
                            .push_str(decoded)
                            .map_err(|_| FeedError::Limit)?;
                    }
                    "length" => {
                        self.item.enclosure_bytes =
                            Some(decoded.parse().map_err(|_| FeedError::Metadata)?);
                    }
                    _ => (),
                }
            }
            if channel_child && local_name(name) == "link" {
                if key == "href" {
                    href.push_str(decoded).map_err(|_| FeedError::Limit)?;
                }
                if key == "rel" {
                    rel.push_str(decoded).map_err(|_| FeedError::Limit)?;
                }
            }
            Ok(())
        })?;
        let namespace = self.namespace(name, false)?;
        if matches!(name, "rss" | "channel" | "item") && !namespace.is_empty() {
            return Err(FeedError::Source);
        }
        attributes(&value[n..], |key, _| self.namespace(key, true).map(|_| ()))?;
        if rel == "next" {
            if namespace != ATOM {
                return Err(FeedError::Source);
            }
            if href.is_empty() || !self.next.is_empty() {
                return Err(FeedError::Xml);
            }
            self.next = href;
        }
        self.stack
            .push(String::try_from(name).map_err(|_| FeedError::Limit)?)
            .map_err(|_| FeedError::Limit)?;
        self.text.clear();
        self.overflow = false;
        if empty { self.close() } else { Ok(false) }
    }
    fn close(&mut self) -> Result<bool, FeedError> {
        if let Some(field) = self.field() {
            let text = if self.overflow {
                ""
            } else {
                core::str::from_utf8(&self.text)
                    .map_err(|_| FeedError::Xml)?
                    .trim()
            };
            match field {
                "title" => {
                    if !self.item.title.is_empty() {
                        return Err(FeedError::Metadata);
                    }
                    self.item
                        .title
                        .push_str(text)
                        .map_err(|_| FeedError::Limit)?;
                }
                "guid" => {
                    if !self.item.guid.is_empty() {
                        return Err(FeedError::Metadata);
                    }
                    self.item
                        .guid
                        .push_str(text)
                        .map_err(|_| FeedError::Limit)?;
                }
                "description" if !self.overflow => {
                    self.item.description.clear();
                    self.item
                        .description
                        .push_str(text)
                        .map_err(|_| FeedError::Limit)?;
                }
                "pubDate" => self.item.published = parse_date(text),
                "duration" => self.item.duration_seconds = parse_duration(text),
                _ => (),
            }
        }
        let name = self.stack.pop().ok_or(FeedError::Xml)?;
        while self
            .namespaces
            .last()
            .is_some_and(|ns| ns.depth > self.stack.len())
        {
            self.namespaces.pop();
        }
        self.text.clear();
        self.overflow = false;
        if name == "item" {
            if self.item.title.is_empty() || !https_url(&self.item.enclosure) {
                return Err(FeedError::Metadata);
            }
            return Ok(true);
        }
        if self.stack.is_empty() {
            if !self.channel {
                return Err(FeedError::Xml);
            }
            self.done = true;
        }
        Ok(false)
    }
}
fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}
fn xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}
fn declaration(input: &str) -> Result<(), FeedError> {
    let body = input
        .strip_prefix("?xml")
        .and_then(|s| s.strip_suffix("?>"))
        .ok_or(FeedError::Xml)?;
    if body.contains('&') {
        return Err(FeedError::Xml);
    }
    let mut state = 0;
    attributes(body, |key, value| {
        state = match (state, key, value) {
            (0, "version", "1.0") => 1,
            (1, "encoding", "UTF-8" | "utf-8") => 2,
            (1 | 2, "standalone", "yes" | "no") => 3,
            _ => return Err(FeedError::Xml),
        };
        Ok(())
    })?;
    if state == 0 {
        return Err(FeedError::Xml);
    }
    Ok(())
}
fn valid_name(s: &str) -> bool {
    !s.ends_with(':')
        && s.bytes().filter(|&b| b == b':').count() <= 1
        && !s.is_empty()
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'-' | b'.'))
}
fn xml_char(c: char) -> bool {
    matches!(c, '\t'|'\n'|'\r'|'\u{20}'..='\u{d7ff}'|'\u{e000}'..='\u{fffd}'|'\u{10000}'..='\u{10ffff}')
}
fn entity_char(bytes: &[u8]) -> Result<char, FeedError> {
    let s = core::str::from_utf8(bytes).map_err(|_| FeedError::Xml)?;
    let c = match s {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let number = if let Some(hex) = s.strip_prefix("#x") {
                if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(FeedError::Xml);
                }
                u32::from_str_radix(hex, 16).ok()
            } else {
                s.strip_prefix('#')
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|s| s.parse().ok())
            };
            number.and_then(char::from_u32).ok_or(FeedError::Xml)?
        }
    };
    if !xml_char(c) {
        return Err(FeedError::Xml);
    }
    Ok(c)
}
fn attributes(
    mut input: &str,
    mut emit: impl FnMut(&str, &str) -> Result<(), FeedError>,
) -> Result<(), FeedError> {
    let mut names = Vec::<&str, 32>::new();
    while !input.is_empty() {
        if !input.starts_with(xml_space) {
            return Err(FeedError::Xml);
        }
        input = input.trim_start_matches(xml_space);
        if input.is_empty() {
            break;
        }
        let end = input
            .find(|c: char| c == '=' || xml_space(c))
            .ok_or(FeedError::Xml)?;
        let name = &input[..end];
        if !valid_name(name) || names.contains(&name) {
            return Err(FeedError::Xml);
        }
        names.push(name).map_err(|_| FeedError::Limit)?;
        input = input[end..]
            .trim_start_matches(xml_space)
            .strip_prefix('=')
            .ok_or(FeedError::Xml)?
            .trim_start_matches(xml_space);
        let quote = input.as_bytes().first().copied().ok_or(FeedError::Xml)?;
        if !matches!(quote, b'\'' | b'"') {
            return Err(FeedError::Xml);
        }
        input = &input[1..];
        let end = input.find(char::from(quote)).ok_or(FeedError::Xml)?;
        let raw = &input[..end];
        if raw.contains('<') {
            return Err(FeedError::Xml);
        }
        let mut decoded = String::<MAX_URL_BYTES>::new();
        let mut rest = raw;
        while let Some(at) = rest.find('&') {
            decoded
                .push_str(&rest[..at])
                .map_err(|_| FeedError::Limit)?;
            rest = &rest[at + 1..];
            let end = rest.find(';').ok_or(FeedError::Xml)?;
            decoded
                .push(entity_char(&rest.as_bytes()[..end])?)
                .map_err(|_| FeedError::Limit)?;
            rest = &rest[end + 1..];
        }
        decoded.push_str(rest).map_err(|_| FeedError::Limit)?;
        emit(name, &decoded)?;
        input = &input[end + 1..];
    }
    Ok(())
}
/// Transport must additionally validate certificates, authorities and each redirect.
/// Audio CDN URLs are not subscription URLs.
fn https_url(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("");
    !host.is_empty()
        && !host.contains(['@', '?', '#', ':'])
        && !s.chars().any(|c| c.is_control() || c.is_whitespace())
}
fn parse_duration(s: &str) -> Option<u32> {
    let mut value = 0u32;
    let mut count = 0;
    for part in s.split(':') {
        let n = part.parse::<u32>().ok()?;
        if count > 0 && n >= 60 {
            return None;
        }
        value = value.checked_mul(60)?.checked_add(n)?;
        count += 1;
    }
    (count <= 3).then_some(value)
}
fn parse_date(s: &str) -> Option<i64> {
    let mut parts = s.split_ascii_whitespace();
    let weekday = parts.next()?;
    if !["Mon,", "Tue,", "Wed,", "Thu,", "Fri,", "Sat,", "Sun,"].contains(&weekday) {
        return None;
    }
    let day = parts.next()?.parse::<i64>().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let mut year = parts.next()?.parse::<i64>().ok()?;
    if !(1970..=9999).contains(&year) {
        return None;
    }
    let mut time = parts.next()?.split(':');
    let h = time.next()?.parse::<i64>().ok()?;
    let m = time.next()?.parse::<i64>().ok()?;
    let sec = time.next()?.parse::<i64>().ok()?;
    if time.next().is_some()
        || !(0..24).contains(&h)
        || !(0..60).contains(&m)
        || !(0..60).contains(&sec)
    {
        return None;
    }
    let zone = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let offset = if zone == "GMT" || zone == "UT" || zone == "+0000" {
        0
    } else {
        let bytes = zone.as_bytes();
        if bytes.len() != 5 || !matches!(bytes[0], b'+' | b'-') {
            return None;
        }
        if !bytes[1..].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let hh = zone[1..3].parse::<i64>().ok()?;
        let mm = zone[3..5].parse::<i64>().ok()?;
        if hh > 23 || mm > 59 {
            return None;
        }
        (hh * 3600 + mm * 60) * if bytes[0] == b'-' { -1 } else { 1 }
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > days {
        return None;
    }
    // Gregorian civil date to Unix days (400-year eras).
    year -= i64::from(month <= 2);
    let era = year / 400;
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + m * 60 + sec - offset)
}
#[cfg(test)]
#[path = "radio_france_tests.rs"]
mod tests;
/// Validated source route; history advances exactly one page in the same show.
#[derive(Debug)]
pub struct FeedUrl<'a> {
    pub show: &'a str,
    pub page: u32,
}
impl<'a> FeedUrl<'a> {
    pub fn parse(url: &'a str) -> Result<Self, FeedError> {
        let path = url
            .strip_prefix("https://rss-rf.aerion.me/rss/")
            .ok_or(FeedError::Source)?;
        let (show, page) = match path.split_once('?') {
            Some((show, query)) => {
                let n = query.strip_prefix("page=").ok_or(FeedError::Source)?;
                if n.is_empty()
                    || !n.bytes().all(|b| b.is_ascii_digit())
                    || (n.len() > 1 && n.starts_with('0'))
                {
                    return Err(FeedError::Source);
                }
                (show, n.parse::<u32>().map_err(|_| FeedError::Source)?)
            }
            None => (path, 0),
        };
        if show.len() != 36
            || !show.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit() && !b.is_ascii_uppercase()
                }
            })
        {
            return Err(FeedError::Source);
        }
        Ok(Self { show, page })
    }
    pub fn next(&self, url: &'a str) -> Result<Self, FeedError> {
        let next = Self::parse(url)?;
        if next.show != self.show || self.page.checked_add(1) != Some(next.page) {
            return Err(FeedError::Source);
        }
        Ok(next)
    }
}
