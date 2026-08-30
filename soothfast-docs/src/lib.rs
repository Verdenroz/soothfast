//! Soothfast docs engine.
//!
//! Four rot channels, four mechanisms:
//! - signatures can't rot: the API surface is read from rustdoc JSON
//!   ([`surface`], [`mod@reference`], [`diff`]);
//! - prose can't rot silently: bind blocks fingerprint the code they
//!   describe, locked in `soothfast.lock` ([`markdown`], [`lockfile`]);
//! - examples can't rot: every markdown `rust` block becomes a generated
//!   test, capture blocks become runnable examples ([`gentests`]);
//! - numbers can't rot: quantitative claims in prose evaluate against the
//!   latest measurement baseline ([`claims`]).

mod comments;

pub mod claims;
pub mod diff;
pub mod gentests;
pub mod lockfile;
pub mod markdown;
pub mod reference;
pub mod surface;
