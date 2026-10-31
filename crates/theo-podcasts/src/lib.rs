//! Bounded Radio France catalogue and persistent episode lifecycle.
#![no_std]
pub mod catalogue;
#[cfg(test)]
extern crate std;

pub mod radio_france;

pub mod download;
pub mod media;
#[cfg(test)]
mod media_test_support;
pub mod reclaim;
pub mod recovery;
pub mod sync;
#[cfg(test)]
mod test_support;
