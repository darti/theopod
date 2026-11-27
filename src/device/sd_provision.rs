//! Explicit `sd-provision` builds: the one place the firmware erases a card.
//!
//! `THEO_SD_PROVISION` is parsed at compile time ([`REQUEST`]); a missing or
//! malformed value fails the build. A `v3` request erases only the card
//! whose exact size and CID identity it names, and only before that card has
//! a usable v3 layout: a v3 card whose catalogue is corrupt, or whose
//! catalogue open is refused as `Schema` (unknown schema, missing table),
//! may be erased (ADR-0021, ADR-0022).

use alloc::rc::Rc;
use allocator_api2::alloc::AllocError;
use core::cell::RefCell;
use embassy_time::{Duration, Instant};
use littlefs_rust::Error;
use theo_podcasts::{
    catalogue::{Catalogue, Error as CatalogueError},
    provision::{CardId, CatalogueHead, Target, parse_request, provision_v3, should_probe},
};
use theo_storage::{
    card_layout::{CardLayout, DESCRIPTOR_SECTORS},
    redb_backend::RegionBackend,
    scope::OWNER_MARGIN_MS,
    sector::SectorIo,
};

pub(super) use theo_podcasts::provision::Request;

use super::{CardIo, IO_BUDGET, in_psram};

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
pub(super) async fn run(card: &mut Option<CardIo>, card_id: Option<CardId>) -> bool {
    let Request::V3(target) = REQUEST else {
        return false;
    };
    match in_psram(|| provision(card, card_id, target)).await {
        Ok(wrote) => wrote,
        Err(AllocError) => {
            log::warn!("SD v3 provisioning not started: PSRAM allocation failed; card unchanged");
            false
        }
    }
}

async fn provision(slot: &mut Option<CardIo>, card_id: Option<CardId>, target: Target) -> bool {
    let Some(card) = slot.as_mut() else {
        return false;
    };
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
    let layout = CardLayout::select(&first, &second, sectors).ok().flatten();
    let catalogue = layout.and_then(|layout| read_catalogue_head(&mut paced, layout));
    // Only a card that is the target and holds a database is opened: the
    // guard then learns whether boot would refuse it as `Schema` (t-152).
    // Everything else is decided from the raw sectors.
    let opened = match (layout, &catalogue) {
        (Some(layout), Some(head))
            if sectors == target.sectors
                && card_id == Some(target.card)
                && should_probe(head, layout.database().sectors()) =>
        {
            probe(slot, layout)
        }
        _ => None,
    };
    let Some(card) = slot.as_mut() else {
        log::warn!("SD v3 provisioning refused: the card was lost opening its catalogue");
        return false;
    };
    let mut paced = Paced(card);
    if let Err(refusal) = target.authorizes(
        sectors,
        card_id,
        &first,
        &second,
        catalogue.as_ref(),
        opened,
    ) {
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

/// The head of a v3 card's database region, for the guard; `None` (logged)
/// if unreadable, which keeps the card refused.
fn read_catalogue_head(card: &mut Paced<'_>, layout: CardLayout) -> Option<CatalogueHead> {
    let mut head: CatalogueHead = [[0; 512]; 3];
    let start = layout.database().start();
    for (offset, sector) in (0..).zip(head.iter_mut()) {
        if let Err(error) = card.read_sector(start + offset, sector) {
            log::warn!("SD v3 provisioning: catalogue head read failed: {error:?}");
            return None;
        }
    }
    Some(head)
}

/// Opens the catalogue of a v3 card as boot does (redb may rewrite its own
/// header) and returns why that failed, if it did. The card returns to
/// `slot`.
#[inline(never)]
fn probe(slot: &mut Option<CardIo>, layout: CardLayout) -> Option<CatalogueError> {
    let card = slot.take()?;
    let shared = Rc::new(RefCell::new(card));
    let window = Window {
        card: shared.clone(),
        start: layout.database().start(),
    };
    // SAFETY: the backend and the catalogue are created, used and dropped
    // inside this function, on this task only.
    let outcome = match unsafe { RegionBackend::new(window, layout.database().sectors()) } {
        Ok(backend) => Catalogue::open(backend).err(),
        Err(error) => {
            log::warn!("SD v3 provisioning: database region not opened: {error:?}");
            None
        }
    };
    // The catalogue and its backend are gone: this is the last reference.
    *slot = Rc::into_inner(shared).map(RefCell::into_inner);
    outcome
}

/// The card's database region, for the catalogue probe.
struct Window {
    card: Rc<RefCell<CardIo>>,
    start: u32,
}
impl SectorIo for Window {
    fn read_sector(&mut self, sector: u32, out: &mut [u8; 512]) -> Result<(), Error> {
        let mut card = self.card.borrow_mut();
        arm(&card);
        card.read_sector(self.start + sector, out)
    }
    fn write_sector(&mut self, sector: u32, data: &[u8; 512]) -> Result<(), Error> {
        let mut card = self.card.borrow_mut();
        arm(&card);
        card.write_sector(self.start + sector, data)
    }
    fn sync(&mut self) -> Result<(), Error> {
        let mut card = self.card.borrow_mut();
        arm(&card);
        card.sync()
    }
    fn check(&mut self) -> Result<(), Error> {
        let mut card = self.card.borrow_mut();
        arm(&card);
        card.check()
    }
    fn device_failed(&self) -> bool {
        self.card.borrow().device_failed()
    }
}

/// Gives the next sector operation its own `IO_BUDGET` deadline.
fn arm(card: &CardIo) {
    let deadline = Instant::now() + IO_BUDGET;
    card.deadline.set(deadline);
    card.soft.set(
        deadline
            .checked_sub(Duration::from_millis(OWNER_MARGIN_MS))
            .unwrap_or_else(Instant::now),
    );
}

/// Gives each sector operation its own `IO_BUDGET` deadline.
///
/// No request deadline covers the multi-minute run.
struct Paced<'a>(&'a mut CardIo);
impl Paced<'_> {
    fn arm(&mut self) {
        arm(self.0);
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
