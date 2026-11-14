//! Source-specific HTTPS authority, redirect and trusted-setup policy.
use crate::{catalogue::MAX_URL_BYTES, radio_france::FeedUrl};
use heapless::{String, Vec};
use sha2::{Digest, Sha256};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Feed,
    Audio,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    GtsR4,
    IsrgX1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Url,
    Authority,
    Redirect,
    Limit,
}
#[derive(Debug)]
pub struct Endpoint<'a> {
    pub host: &'a str,
    pub path: &'a str,
    pub root: Root,
}
pub fn endpoint(url: &str, purpose: Purpose) -> Result<Endpoint<'_>, Error> {
    if url.len() > MAX_URL_BYTES {
        return Err(Error::Limit);
    }
    if url.bytes().any(|b| !b.is_ascii() || b <= 32 || b == 127) || url.contains('#') {
        return Err(Error::Url);
    }
    let tail = url.strip_prefix("https://").ok_or(Error::Url)?;
    let split = tail.find('/').ok_or(Error::Url)?;
    let host = &tail[..split];
    let path = &tail[split..];
    if host.is_empty() || host.len() > 64 || host.contains(['@', ':', '?', '\\']) {
        return Err(Error::Authority);
    }
    let root = match (purpose, host) {
        (Purpose::Feed, "rss-rf.aerion.me") => {
            FeedUrl::parse(url).map_err(|_| Error::Url)?;
            Root::GtsR4
        }
        (Purpose::Audio, "rss-rf.aerion.me") if path.starts_with("/audio/") && path.len() > 7 => {
            Root::GtsR4
        }
        (Purpose::Audio, "media.radiofrance-podcast.net") => Root::IsrgX1,
        _ => return Err(Error::Authority),
    };
    Ok(Endpoint { host, path, root })
}
/// At most five hops; no origin-specific conditional header is forwarded after
/// any redirect. Caller still performs hostname/chain/date/signature checks.
pub struct Redirects {
    current: String<MAX_URL_BYTES>,
    purpose: Purpose,
    show: Option<String<36>>,
    page: u32,
    seen: Vec<[u8; 32], 6>,
}
impl Redirects {
    pub fn new(url: &str, purpose: Purpose) -> Result<Self, Error> {
        endpoint(url, purpose)?;
        let (show, page) = if purpose == Purpose::Feed {
            let feed = FeedUrl::parse(url).map_err(|_| Error::Url)?;
            (
                Some(String::try_from(feed.show).map_err(|_| Error::Limit)?),
                feed.page,
            )
        } else {
            (None, 0)
        };
        let mut seen = Vec::new();
        seen.push(Sha256::digest(url.as_bytes()).into())
            .map_err(|_| Error::Limit)?;
        Ok(Self {
            current: String::try_from(url).map_err(|_| Error::Limit)?,
            purpose,
            show,
            page,
            seen,
        })
    }
    pub fn accepts_not_modified(&self, has_validator: bool) -> bool {
        has_validator && self.forward_conditionals()
    }
    pub fn forward_conditionals(&self) -> bool {
        self.seen.len() == 1
    }
    pub fn follow(&mut self, location: &str) -> Result<String<MAX_URL_BYTES>, Error> {
        if self.seen.len() >= 6 {
            return Err(Error::Limit);
        }
        let mut next = String::new();
        if location.starts_with("https://") {
            next.push_str(location).map_err(|_| Error::Limit)?;
        } else {
            let current = endpoint(&self.current, self.purpose)?;
            next.push_str("https://").map_err(|_| Error::Limit)?;
            next.push_str(current.host).map_err(|_| Error::Limit)?;
            if location.starts_with('/') && !location.starts_with("//") {
                next.push_str(location).map_err(|_| Error::Limit)?;
            } else if location.starts_with('?') {
                next.push_str(current.path.split('?').next().ok_or(Error::Url)?)
                    .map_err(|_| Error::Limit)?;
                next.push_str(location).map_err(|_| Error::Limit)?;
            } else {
                return Err(Error::Url);
            }
        }
        endpoint(&next, self.purpose)?;
        if let Some(show) = &self.show {
            let feed = FeedUrl::parse(&next).map_err(|_| Error::Url)?;
            if feed.show != show.as_str() || feed.page != self.page {
                return Err(Error::Redirect);
            }
        }
        let fingerprint = Sha256::digest(next.as_bytes()).into();
        if self.seen.contains(&fingerprint) {
            return Err(Error::Redirect);
        }
        self.seen.push(fingerprint).map_err(|_| Error::Limit)?;
        self.current = next.clone();
        Ok(next)
    }
}
#[cfg(test)]
#[path = "network_policy_tests.rs"]
mod tests;
/// Owned header facts passed to the storage core, never borrowed HTTP buffers.
#[derive(Debug, Default)]
pub struct ResponseHeaders {
    pub length: Option<u64>,
    pub chunked: bool,
    /// A satisfied `Content-Range` (`bytes first-last/total`).
    pub range: Option<ContentRange>,
    pub location: String<MAX_URL_BYTES>,
    pub etag: String<512>,
    pub modified: String<128>,
    pub retry_after: Option<u32>,
}
impl ResponseHeaders {
    pub fn parse<'a>(
        headers: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> Result<Self, Error> {
        let mut out = Self::default();
        for (name, bytes) in headers {
            if name.is_empty() {
                continue;
            }
            let value = core::str::from_utf8(bytes).map_err(|_| Error::Url)?;
            if value.bytes().any(|b| b < 32 || b == 127) {
                return Err(Error::Url);
            }
            if name.eq_ignore_ascii_case("content-length") {
                if out.length.is_some() {
                    return Err(Error::Url);
                }
                out.length = Some(value.parse().map_err(|_| Error::Url)?);
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                if out.chunked || !value.eq_ignore_ascii_case("chunked") {
                    return Err(Error::Url);
                }
                out.chunked = true;
            } else if name.eq_ignore_ascii_case("content-encoding")
                && !value.eq_ignore_ascii_case("identity")
            {
                return Err(Error::Url);
            } else if name.eq_ignore_ascii_case("content-range") {
                if out.range.is_some() {
                    return Err(Error::Url);
                }
                out.range = Some(ContentRange::parse(value).ok_or(Error::Url)?);
            } else if name.eq_ignore_ascii_case("location") {
                if !out.location.is_empty() {
                    return Err(Error::Url);
                }
                out.location.push_str(value).map_err(|_| Error::Limit)?;
            } else if name.eq_ignore_ascii_case("etag") {
                if !out.etag.is_empty() {
                    return Err(Error::Url);
                }
                out.etag.push_str(value).map_err(|_| Error::Limit)?;
            } else if name.eq_ignore_ascii_case("last-modified") {
                out.modified.push_str(value).map_err(|_| Error::Limit)?;
            } else if name.eq_ignore_ascii_case("retry-after") {
                out.retry_after = value.parse::<u32>().ok().map(|n| n.min(86400));
            }
        }
        if out.length.is_some() && out.chunked {
            return Err(Error::Url);
        }
        Ok(out)
    }
}
/// A satisfied byte range of a `206` response, `last` inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentRange {
    pub first: u64,
    pub last: u64,
    pub total: u64,
}
impl ContentRange {
    /// Parse `bytes first-last/total`; `*` forms and `last >= total` are refused.
    fn parse(value: &str) -> Option<Self> {
        let (span, total) = value.strip_prefix("bytes ")?.split_once('/')?;
        let (first, last) = span.split_once('-')?;
        let number = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        };
        let range = Self {
            first: number(first)?,
            last: number(last)?,
            total: number(total)?,
        };
        (range.first <= range.last && range.last < range.total).then_some(range)
    }
}
/// The `If-Range` validator of a response: its strong ETag, else (no ETag at
/// all) its `Last-Modified`. A weak ETag gives none (RFC 9110 13.1.5).
pub fn range_validator<'a>(etag: &'a str, modified: &'a str) -> Option<&'a str> {
    if !etag.is_empty() {
        return (!etag.starts_with("W/")).then_some(etag);
    }
    (!modified.is_empty()).then_some(modified)
}
