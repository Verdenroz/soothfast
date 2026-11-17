//! Lanes a pull request does not touch: the gate, import, check and docs
//! jobs whose inputs equal those of a pass the default branch stored.
//!
//! A lane's pass is stored under a key of its fingerprint, so a lookup needs
//! no build. [`store`] holds the passes.

pub mod store;
