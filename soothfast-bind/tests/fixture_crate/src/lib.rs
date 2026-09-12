//! A real crate matching `tests/fixture/mod.rs`'s synthetic rustdoc surface.
//!
//! The Java golden's `Cargo.toml` depends on `acme` at `..`, the layout
//! `bind gen` actually produces (the glue crate sits inside the bound
//! crate's own tree). `java_smoke.rs` copies this crate and the golden into
//! a scratch directory in that same shape, so it builds the golden's
//! cdylib for real rather than only comparing its text. Only the items the
//! plan actually binds need bodies; a gapped one (`with_time`, `merge`,
//! ...) is never called by generated code, so it is left out.

pub struct Counter {
    pub value: i64,
    #[allow(dead_code)]
    label: String,
}

pub enum Level {
    Low,
    High,
}

pub enum Mode {
    Fast,
    Precise(u32),
    Custom { level: u8 },
}

impl Counter {
    pub fn new(start: i64) -> Self {
        Counter {
            value: start,
            label: String::new(),
        }
    }

    pub fn bump(&self, by: i64) -> Result<i64, String> {
        self.value.checked_add(by).ok_or_else(|| "overflow".to_string())
    }

    pub fn bump_all(&self, by: Vec<i64>) -> i64 {
        self.value + by.iter().sum::<i64>()
    }

    pub fn at(&self, level: Level) -> i64 {
        match level {
            Level::Low => self.value,
            Level::High => self.value * 2,
        }
    }
}

pub fn digest(data: &[u8]) -> Vec<u8> {
    data.iter().map(|b| b.wrapping_add(1)).collect()
}

pub fn normalize(input: Vec<f64>, factor: f64) -> Vec<f64> {
    input.into_iter().map(|v| v * factor).collect()
}

pub fn stamp(handle: i64, error: f64, register: &[u8]) -> u64 {
    handle as u64 + error as u64 + register.len() as u64
}
