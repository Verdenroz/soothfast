//! Soothfast report engine: renders measurement baselines
//! and trend series into doc fragments — markdown/HTML perf tables,
//! hand-emitted SVG trend charts, changelog drafts, and shields-endpoint
//! badges. Everything here is *produced* from recorded data, never written by
//! hand.

pub mod badges;
pub mod changelog;
pub mod perf_table;
pub mod surface;
pub mod trend_chart;
