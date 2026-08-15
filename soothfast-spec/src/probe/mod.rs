//! Live-response data quality: does the running server actually populate
//! what its spec declares?
//!
//! Spec reconciliation ([`crate::reconcile`]) proves the *surface* — every
//! operation exists on both sides. Nothing there notices an endpoint that
//! answers 200 with half its fields silently null, which is exactly how
//! serde key mismatches and hand-copied structs fail. This module closes
//! that gap with four checks over one probe pass:
//!
//! - **population** — which response fields carry data, diffed against a
//!   committed baseline so a field going null fails the gate
//!   ([`population`], [`baseline`])
//! - **coverage** — every field the spec declares, so a field that never
//!   populates at all is locked as visible debt instead of staying
//!   invisible to the learned baseline ([`coverage`])
//! - **shape** — the response validated structurally against the spec's
//!   own response schema ([`shape`])
//! - **assertions** — per-probe sanity bounds on values ([`assert`])
//!
//! The engine here is pure: it takes JSON in and produces findings. The
//! CLI half (manifest parsing, server launch, HTTP) lives in
//! `cargo-soothfast`.

pub mod assert;
pub mod baseline;
pub mod coverage;
pub mod population;
pub mod shape;
