//! Fixed-layout episode records: the bytes stored per episode (ADR-0022).
use super::*;

struct Writer<'a> {
    bytes: &'a mut [u8],
    pos: usize,
}
impl Writer<'_> {
    fn put(&mut self, value: &[u8]) -> Option<()> {
        let end = self.pos.checked_add(value.len())?;
        self.bytes.get_mut(self.pos..end)?.copy_from_slice(value);
        self.pos = end;
        Some(())
    }
    fn text(&mut self, value: &str) -> Option<()> {
        self.put(&u16::try_from(value.len()).ok()?.to_le_bytes())?;
        self.put(value.as_bytes())
    }
}
pub(super) fn encode(episode: Episode<'_>, scratch: &mut [u8]) -> Option<usize> {
    let m = episode.metadata;
    let flags = u8::from(m.guid.is_some())
        | u8::from(m.description.is_some()) << 1
        | u8::from(m.published.is_some()) << 2
        | u8::from(m.enclosure_bytes.is_some()) << 3
        | u8::from(m.duration_seconds.is_some()) << 4;
    let mut w = Writer {
        bytes: scratch.get_mut(..MAX_RECORD_BYTES)?,
        pos: 0,
    };
    w.put(&[1, 1])?;
    w.put(&episode.subscription.0.to_le_bytes())?;
    w.put(&episode.id.0.to_le_bytes())?;
    w.put(&[
        episode.state.audio as u8,
        u8::from(episode.state.read),
        flags,
    ])?;
    if let Some(v) = m.published {
        w.put(&v.to_le_bytes())?;
    }
    if let Some(v) = m.enclosure_bytes {
        w.put(&v.to_le_bytes())?;
    }
    if let Some(v) = m.duration_seconds {
        w.put(&v.to_le_bytes())?;
    }
    if let Some(v) = m.guid {
        w.text(v)?;
    }
    w.text(m.title)?;
    w.text(m.enclosure)?;
    if let Some(v) = m.description {
        w.text(v)?;
    }
    Some(w.pos)
}
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn n<const N: usize>(&mut self) -> Option<[u8; N]> {
        let value = self.take(N)?;
        value.try_into().ok()
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let value = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(value)
    }
    fn text(&mut self, max: usize) -> Option<&'a str> {
        let n = u16::from_le_bytes(self.n()?) as usize;
        if n > max {
            return None;
        }
        core::str::from_utf8(self.take(n)?).ok()
    }
}
pub(super) fn decode(bytes: &[u8]) -> Option<Episode<'_>> {
    let mut r = Reader { bytes, pos: 0 };
    if r.n::<2>()? != [1, 1] {
        return None;
    }
    let subscription = SubscriptionId(u64::from_le_bytes(r.n()?));
    let id = EpisodeId(u64::from_le_bytes(r.n()?));
    let audio = match r.n::<1>()?[0] {
        0 => AudioState::Missing,
        1 => AudioState::Queued,
        2 => AudioState::Downloading,
        3 => AudioState::Available,
        4 => AudioState::Deleting,
        5 => AudioState::Reclaimed,
        _ => return None,
    };
    let read = match r.n::<1>()?[0] {
        0 => false,
        1 => true,
        _ => return None,
    };
    let flags = r.n::<1>()?[0];
    if flags & !31 != 0 {
        return None;
    }
    let published = if flags & 4 != 0 {
        Some(i64::from_le_bytes(r.n()?))
    } else {
        None
    };
    let enclosure_bytes = if flags & 8 != 0 {
        Some(u64::from_le_bytes(r.n()?))
    } else {
        None
    };
    let duration_seconds = if flags & 16 != 0 {
        Some(u32::from_le_bytes(r.n()?))
    } else {
        None
    };
    let guid = if flags & 1 != 0 {
        Some(r.text(MAX_GUID_BYTES)?)
    } else {
        None
    };
    let title = r.text(MAX_TITLE_BYTES)?;
    let enclosure = r.text(MAX_URL_BYTES)?;
    let description = if flags & 2 != 0 {
        Some(r.text(MAX_DESCRIPTION_BYTES)?)
    } else {
        None
    };
    if r.pos != bytes.len()
        || subscription.0 == 0
        || id.0 == 0
        || title.is_empty()
        || enclosure.is_empty()
        || guid.is_some_and(str::is_empty)
        || (!read && matches!(audio, AudioState::Deleting | AudioState::Reclaimed))
    {
        return None;
    }
    Some(Episode {
        id,
        subscription,
        source: Source::RadioFrance,
        state: EpisodeState { read, audio },
        metadata: EpisodeMetadata {
            guid,
            title,
            enclosure,
            description,
            published,
            enclosure_bytes,
            duration_seconds,
        },
    })
}
