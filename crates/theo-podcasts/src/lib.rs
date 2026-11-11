//! Bounded Radio France catalogue and persistent episode lifecycle.
//!
//! Audio bodies live in raw 1 MiB allocation units (ADR-0020): the private
//! `units` module persists the bitmap, counter and per-episode extent records
//! in the catalogue, and `unit_media::UnitMedia` is the `media::MediaStore`
//! over them, failing with `unit_media::UnitError`. `provision::provision_v2`
//! initializes a v2 card (destructive; explicit provisioning only).
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
pub mod provision;
mod provision_guard;
pub mod reclaim;
pub mod recovery;
pub mod service;
pub mod sync;
#[cfg(test)]
mod test_support;

pub mod subscriptions;

pub mod unit_media;
pub(crate) mod units;

pub mod runtime;

pub mod setup;
pub mod worker;
