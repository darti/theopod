//! Bounded Radio France catalogue and persistent episode lifecycle.
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

pub mod runtime;

pub mod littlefs_media;

pub mod setup;
pub mod worker;
