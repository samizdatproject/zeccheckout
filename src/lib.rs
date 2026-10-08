//! ZecCheckout: a self-hosted shielded Zcash checkout service that holds only
//! a Unified Full Viewing Key.

pub mod db;
pub mod invoice;
pub mod keys;
pub mod scanner;
pub mod service;
pub mod webhook;
pub mod api;

#[cfg(test)]
mod e2e_tests;
