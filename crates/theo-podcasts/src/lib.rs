//! Bounded Radio France catalogue and persistent episode lifecycle.
#![no_std]
pub mod catalogue;
#[cfg(test)]
extern crate std;

pub mod radio_france;

pub mod sync;
#[cfg(test)]
mod test_support;
