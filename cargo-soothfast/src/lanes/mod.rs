//! Lanes a pull request does not touch: the gate, import, check and docs
//! jobs whose inputs equal those of a pass the default branch stored.
//!
//! A lane's pass is stored under a key of its fingerprint, so a lookup needs
//! no build. [`store`] holds the passes, [`inputs`] says what a lane reads
//! and digests it, [`graph`] gives the packages a Rust lane reaches, and
//! [`tool`] names the soothfast build behind a pass.

pub mod graph;
pub mod inputs;
pub mod store;
pub mod tool;
