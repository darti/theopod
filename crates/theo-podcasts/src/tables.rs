//! redb tables of the podcast catalogue (ADR-0022). Values keep the byte
//! layouts of schema 2; keys are typed, so ranges keep their order.
use redb::TableDefinition;

pub(crate) const SCHEMA: u64 = 3;

/// Episode id → episode record.
pub(crate) const EPISODES: TableDefinition<u64, &[u8]> = TableDefinition::new("episodes");
/// (subscription, 1 = guid / 2 = enclosure, SHA-256) → episode id.
pub(crate) const EPISODE_IDENTITY: TableDefinition<(u64, u8, [u8; 32]), u64> =
    TableDefinition::new("episode_identity");
/// Episode id → published audio length (bytes, non-zero).
pub(crate) const AUDIO: TableDefinition<u64, u64> = TableDefinition::new("audio");
/// Subscription id → canonical feed URL.
pub(crate) const SUBSCRIPTIONS: TableDefinition<u64, &str> = TableDefinition::new("subscriptions");
/// Radio France show id → subscription id.
pub(crate) const SHOWS: TableDefinition<[u8; 36], u64> = TableDefinition::new("shows");
/// Subscription id → last reconciled sync checkpoint.
pub(crate) const SYNC: TableDefinition<u64, &[u8]> = TableDefinition::new("sync");
/// Bitmap page → 4 KiB of unit allocation bits.
pub(crate) const UNIT_PAGES: TableDefinition<u16, &[u8]> = TableDefinition::new("unit_pages");
/// Episode id → unit record (state, length, extents).
pub(crate) const UNIT_EXTENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("unit_extents");
/// Eviction index: one entry per read episode with `Available` audio, keyed
/// in the reclaim policy's order (no publication date last, oldest date,
/// import order = id); written in the same transaction as the episode.
pub(crate) const EVICTABLE: TableDefinition<(u8, i64, u64), ()> = TableDefinition::new("evictable");
/// Named `u64` counters, keyed by the constants below.
pub(crate) const COUNTERS: TableDefinition<&str, u64> = TableDefinition::new("counters");

pub(crate) const NEXT_EPISODE: &str = "next_episode";
pub(crate) const NEXT_SUBSCRIPTION: &str = "next_subscription";
pub(crate) const UNITS_USED: &str = "units_used";
pub(crate) const SCHEMA_KEY: &str = "schema";
