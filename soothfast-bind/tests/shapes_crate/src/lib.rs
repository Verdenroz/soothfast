//! Real Rust definitions backing `tests/shape_matrix.rs`.
//!
//! One item per shape the matrix drives through every backend: the surface
//! built there names these exact paths, so a backend that gets the glue
//! wrong fails to compile against real signatures rather than an
//! approximation of them.

pub struct Handle {
    pub value: i64,
}

impl Handle {
    pub fn new(value: i64) -> Self {
        Handle { value }
    }
}

pub fn mutate_handle(h: &mut Handle) -> bool {
    h.value += 1;
    true
}

pub fn option_handle_param(h: Option<Handle>) -> bool {
    h.is_some()
}

pub fn handle_list_ret() -> Vec<Handle> {
    vec![Handle::new(1), Handle::new(2)]
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    A,
    B,
}

impl Flag {
    pub fn describe(&self) -> bool {
        matches!(self, Flag::A)
    }
}

pub fn option_flag_ret() -> Option<Flag> {
    Some(Flag::A)
}

pub fn option_flag_param(flag: Option<Flag>) -> bool {
    flag.is_some()
}

pub fn flag_ref_param(flag: &Flag) -> bool {
    matches!(flag, Flag::A)
}

pub fn flag_list_ret() -> Vec<Flag> {
    vec![Flag::A, Flag::B]
}

pub struct Bag {
    pub values: Vec<f64>,
}

impl Bag {
    pub fn new() -> Self {
        Bag { values: Vec::new() }
    }
}

impl Default for Bag {
    fn default() -> Self {
        Bag::new()
    }
}

/// The one place a nested handle field appears: its accessor is a gap, so
/// nothing here needs `Handle` to be `Clone`.
pub struct Wrap {
    pub inner: Option<Handle>,
}

impl Wrap {
    pub fn new() -> Self {
        Wrap { inner: None }
    }
}

impl Default for Wrap {
    fn default() -> Self {
        Wrap::new()
    }
}

pub fn usize_list_probe(v: Vec<usize>) -> Vec<usize> {
    v
}

pub fn option_list_probe(v: Option<Vec<f64>>) -> Option<Vec<f64>> {
    v
}

pub fn bool_list_probe(v: Vec<bool>) -> Vec<bool> {
    v
}

pub fn option_f64_param(v: Option<f64>) -> bool {
    v.is_some()
}

pub fn digest_mut(buf: &mut [u8]) -> bool {
    for b in buf.iter_mut() {
        *b = b.wrapping_add(1);
    }
    true
}
