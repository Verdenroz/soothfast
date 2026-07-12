//! End-to-end check that `#[soothfast::mock_seam]` actually expands, links
//! into `MOCKS`, and resolves through `soothfast::mock::activate` — the
//! registry's own unit tests only exercise `resolve_mock_seam` against
//! hand-built items, never the real macro expansion.

struct TestBackend(String);

impl soothfast::registry::MockSeam for TestBackend {
    fn base_url(&self) -> String {
        self.0.clone()
    }
}

#[soothfast::mock_seam]
fn zero_arg_seam() -> TestBackend {
    TestBackend("http://zero-arg".into())
}

#[soothfast::mock_seam]
fn one_arg_seam(arg: &str) -> TestBackend {
    TestBackend(format!("http://one-arg/{arg}"))
}

#[test]
fn zero_arg_seam_activates_by_name() {
    let mock = soothfast::mock::activate("zero_arg_seam", "");
    assert_eq!(mock.base_url(), "http://zero-arg");
}

#[test]
fn one_arg_seam_receives_the_activate_arg() {
    let mock = soothfast::mock::activate("one_arg_seam", "AAPL");
    assert_eq!(mock.base_url(), "http://one-arg/AAPL");
}

#[test]
#[should_panic(expected = "no mock seam named")]
fn activating_an_unknown_name_panics() {
    soothfast::mock::activate("does_not_exist", "");
}
