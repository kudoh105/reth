//! Epoch logic.
//!
//! All logic assumes at least 3 heights per epoch.

pub(crate) mod manager;
mod scheme_provider;

pub(crate) use scheme_provider::SchemeProvider;
