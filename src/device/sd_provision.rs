//! Explicit `sd-provision` builds: the one place the firmware erases a card.
//!
//! `THEO_SD_PROVISION` is parsed at compile time ([`REQUEST`]); a missing or
//! malformed value fails the build. A `v3` request erases only the card
//! whose exact size and CID identity it names, and only before that card has
//! a v3 layout (ADR-0021, ADR-0022).

use allocator_api2::alloc::AllocError;
use embassy_time::{Duration, Instant};
use littlefs_rust::Error;
use theo_podcasts::provision::{CardId, Target, parse_request, provision_v3};
use theo_storage::{card_layout::DESCRIPTOR_SECTORS, littlefs::SectorIo, scope::OWNER_MARGIN_MS};

pub(super) use theo_podcasts::provision::Request;

use super::{CardIo, IO_BUDGET, apart};

/// What this build was asked to do, checked at compile time.
pub(super) const REQUEST: Request = match option_env!("THEO_SD_PROVISION") {
    Some(spec) => match parse_request(spec) {
        Some(request) => request,
        None => panic!("THEO_SD_PROVISION must be `format` or `v3:<sectors>:<card id>`"),
    },
    None => panic!("the sd-provision feature needs THEO_SD_PROVISION"),
};

/// Runs a `v3` request before the owner serves anything.
///
/// Returns whether the card was written to; it must then stay offline.
pub(super) async fn run(card: &mut CardIo, card_id: Option<CardId>) -> bool {
    let Request::V3(target) = REQUEST else {
        return false;
    };
    match apart(|| provision(card, card_id, target)).await {
        Ok(wrote) => wrote,
        Err(AllocError) => {
            log::warn!("SD v3 provisioning not started: PSRAM allocation failed; card unchanged");
            false
        }
    }
}

async fn provision(card: &mut CardIo, card_id: Option<CardId>, target: Target) -> bool {
    let sectors = card.sectors;
    let mut paced = Paced(card);
    let mut first = [0; 512];
    let mut second = [0; 512];
    let read = paced
        .read_sector(DESCRIPTOR_SECTORS[0], &mut first)
        .and_then(|()| paced.read_sector(DESCRIPTOR_SECTORS[1], &mut second));
    if let Err(error) = read {
        log::warn!("SD v3 provisioning refused: descriptor read failed: {error:?}");
        return false;
    }
    if let Err(refusal) = target.authorizes(sectors, card_id, &first, &second) {
        log::warn!(
            "SD v3 provisioning refused: {refusal}; this build targets card {} with {} sectors",
            target.card,
            target.sectors
        );
        return false;
    }
    log::warn!(
        "SD v3 provisioning: erasing card {} ({sectors} sectors)",
        target.card
    );
    let started = Instant::now();
    match provision_v3(&mut paced, sectors, &esp_alloc::ExternalMemory) {
        Ok(layout) => log::info!(
            "SD v3 provisioning done in {} s: {} media units; flash the normal firmware",
            started.elapsed().as_secs(),
            layout.media_units()
        ),
        Err(error) => log::warn!(
            "SD v3 provisioning failed after {} s, card left blank: {error}",
            started.elapsed().as_secs()
        ),
    }
    true
}

/// Gives each sector operation its own `IO_BUDGET` deadline.
///
/// No request deadline covers the multi-minute run.
struct Paced<'a>(&'a mut CardIo);
impl Paced<'_> {
    fn arm(&mut self) {
        let deadline = Instant::now() + IO_BUDGET;
        self.0.deadline.set(deadline);
        self.0.soft.set(
            deadline
                .checked_sub(Duration::from_millis(OWNER_MARGIN_MS))
                .unwrap_or_else(Instant::now),
        );
    }
}
impl SectorIo for Paced<'_> {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        self.arm();
        self.0.read_sector(sector, out)
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        self.arm();
        self.0.write_sector(sector, data)
    }
    fn sync(&mut self) -> Result<(), Error> {
        self.arm();
        self.0.sync()
    }
    fn check(&mut self) -> Result<(), Error> {
        self.arm();
        self.0.check()
    }
    fn device_failed(&self) -> bool {
        self.0.device_failed()
    }
}
