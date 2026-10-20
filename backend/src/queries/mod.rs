//! Database query functions.
//!
//! Each sub-module owns queries for one domain:
//!   * [`metrics`]   — insert metrics, fetch latest, fetch history by time range
//!   * [`computers`] — look up computer records needed by the ingest path
//!   * [`offices`]   — look up office records needed by the ingest path

pub mod computers;
pub mod metrics;
pub mod offices;
