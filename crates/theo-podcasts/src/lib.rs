//! Bounded Radio France catalogue and persistent episode lifecycle.
//!
//! Audio bodies live in raw 1 MiB allocation units: `units` persists the
//! bitmap, counter and per-episode unit records in the catalogue, and
//! `unit_media` is the media store over them (ADR-0020).
#![no_std]
extern crate alloc;
pub mod catalogue;
#[cfg(test)]
extern crate std;

pub mod radio_france;

pub mod download;
pub mod media;
#[cfg(test)]
mod media_test_support;
pub mod network_policy;
pub mod reclaim;
pub mod recovery;
pub mod service;
pub mod sync;
#[cfg(test)]
mod test_support;

pub mod subscriptions;

pub mod unit_media;
pub mod units;

pub mod runtime;

pub mod setup;
pub mod worker;
