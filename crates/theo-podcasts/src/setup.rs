//! Local USB setup grammar; never used for remotely supplied input.
use crate::{catalogue::EpisodeId, radio_france::FeedUrl};
// Keep local setup parsing allocation-free and explicitly bounded.
#[allow(clippy::large_enum_variant)]
pub enum SetupCommand {
    Time(u32),
    Follow(heapless::String<256>),
    Read(EpisodeId),
    Sync,
    Retry,
    Status,
}
#[derive(Debug)]
pub struct InvalidSetup;
pub fn parse(line: &str) -> Result<SetupCommand, InvalidSetup> {
    let line = line.trim();
    Ok(if let Some(time) = line.strip_prefix("TIME ") {
        let time = time.parse().map_err(|_| InvalidSetup)?;
        if !(1704067200..=4102444800).contains(&time) {
            return Err(InvalidSetup);
        }
        SetupCommand::Time(time)
    } else if let Some(feed) = line.strip_prefix("FOLLOW ") {
        if FeedUrl::parse(feed).map_err(|_| InvalidSetup)?.page != 0 {
            return Err(InvalidSetup);
        }
        SetupCommand::Follow(feed.try_into().map_err(|_| InvalidSetup)?)
    } else if let Some(id) = line.strip_prefix("READ ") {
        let id = id.parse().map_err(|_| InvalidSetup)?;
        if id == 0 {
            return Err(InvalidSetup);
        }
        SetupCommand::Read(EpisodeId(id))
    } else {
        match line {
            "SYNC" => SetupCommand::Sync,
            "RETRY" => SetupCommand::Retry,
            "STATUS" => SetupCommand::Status,
            _ => return Err(InvalidSetup),
        }
    })
}
#[derive(Default)]
pub struct SetupInput {
    line: heapless::Vec<u8, 512>,
    overflow: bool,
}
impl SetupInput {
    pub fn push(&mut self, byte: u8) -> Option<Result<SetupCommand, InvalidSetup>> {
        if byte == b'\r' {
            return None;
        }
        if byte == b'\n' {
            let result = if self.overflow {
                Err(InvalidSetup)
            } else {
                core::str::from_utf8(&self.line)
                    .map_err(|_| InvalidSetup)
                    .and_then(parse)
            };
            self.line.clear();
            self.overflow = false;
            Some(result)
        } else {
            if self.line.push(byte).is_err() {
                self.overflow = true;
            }
            None
        }
    }
}
#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
