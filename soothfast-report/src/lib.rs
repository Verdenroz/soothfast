//! Soothfast report engine: renders measurement baselines
//! and trend series into doc fragments — markdown/HTML perf tables,
//! hand-emitted SVG trend charts, changelog drafts, shields-endpoint badges,
//! and the agent-facing `llms.txt` digest. Everything here is *produced*
//! from recorded data, never written by hand.

pub mod badges;
pub mod changelog;
pub mod llms;
pub mod perf_table;
pub mod trend_chart;
