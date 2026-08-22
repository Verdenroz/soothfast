//! `#[export]` reaches the registry for every item shape it accepts.
//!
//! Binding generation reads identity, opt-outs, and the doc summary from the
//! registry and everything else from rustdoc, so the attribute-to-registry
//! path is the half no later stage can reconstruct.

use soothfast::export;
use soothfast::registry::{ExportItem, export_items};

/// Scale a series by a factor.
#[export]
pub fn normalize(input: Vec<f64>, factor: f64) -> Vec<f64> {
    input.into_iter().map(|v| v * factor).collect()
}

#[export(skip(wasm))]
pub fn read_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// A running total.
#[export]
pub struct Counter {
    pub value: i64,
}

#[export]
pub enum Mode {
    Fast,
    Precise,
}

#[export(skip(wasm))]
impl Counter {
    /// Start from zero.
    pub fn empty() -> Self {
        Counter { value: 0 }
    }

    #[export(constructor)]
    pub fn starting_at(start: i64) -> Self {
        Counter { value: start }
    }

    pub fn bump(&self, by: i64) -> i64 {
        self.value + by
    }

    #[export(skip)]
    pub fn debug_dump(&self) -> String {
        format!("{}", self.value)
    }

    fn internal(&self) -> i64 {
        self.value
    }
}

fn find(id: &str) -> &'static ExportItem {
    export_items()
        .iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("{id} not registered"))
}

fn registered(id: &str) -> bool {
    export_items().iter().any(|e| e.id == id)
}

#[test]
fn a_free_fn_registers_with_its_summary() {
    let item = find("export_registration::normalize");
    assert_eq!(item.kind, "fn");
    assert_eq!(item.summary, Some("Scale a series by a factor."));
    assert_eq!(item.skip, "");
    assert_eq!(item.owner, None);
    assert!(!item.constructor);
    assert_ne!(item.fingerprint, 0);
}

#[test]
fn types_register_under_their_own_kind() {
    assert_eq!(find("export_registration::Counter").kind, "struct");
    assert_eq!(
        find("export_registration::Counter").summary,
        Some("A running total.")
    );
    assert_eq!(find("export_registration::Mode").kind, "enum");
}

#[test]
fn a_named_language_is_the_only_one_opted_out() {
    assert_eq!(find("export_registration::read_env").skip, "wasm");
}

#[test]
fn impl_methods_register_against_their_owner() {
    let item = find("export_registration::Counter::bump");
    assert_eq!(item.kind, "method");
    assert_eq!(item.owner, Some("Counter"));
    assert!(!item.constructor);
}

#[test]
fn the_constructor_override_marks_only_that_fn() {
    assert!(find("export_registration::Counter::starting_at").constructor);
    assert!(!find("export_registration::Counter::empty").constructor);
}

#[test]
fn a_block_opt_out_applies_to_every_method_in_it() {
    assert_eq!(find("export_registration::Counter::bump").skip, "wasm");
    assert_eq!(find("export_registration::Counter::empty").skip, "wasm");
}

#[test]
fn bare_skip_and_private_methods_never_register() {
    assert!(!registered("export_registration::Counter::debug_dump"));
    assert!(!registered("export_registration::Counter::internal"));
}

#[test]
fn the_annotated_items_still_work() {
    assert_eq!(normalize(vec![1.0, 2.0], 3.0), vec![3.0, 6.0]);
    assert_eq!(Counter::starting_at(5).bump(2), 7);
    assert_eq!(Counter::empty().value, 0);
    assert_eq!(Counter::starting_at(5).internal(), 5);
    assert_eq!(Counter::empty().debug_dump(), "0");
    assert!(matches!(Mode::Fast, Mode::Fast));
}
