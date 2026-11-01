use super::*;
use crate::invoke::ItemMetrics;

fn run_with_pkg(pkg: Option<&str>) -> Run {
    let mut run = Run::default();
    let item = ItemMetrics {
        pkg: pkg.map(str::to_string),
        ..ItemMetrics::default()
    };
    run.items.insert("core::parse".into(), item);
    run
}

#[test]
fn a_stored_pkg_gives_way_to_the_fresh_listing() {
    let listing = [("core::parse".to_string(), Some("core".to_string()))];
    let stamped = stamped(&run_with_pkg(Some("stale")), &listing);
    assert_eq!(stamped.items["core::parse"].pkg.as_deref(), Some("core"));
}

#[test]
fn with_no_listing_or_no_package_named_the_pkg_is_unknown() {
    let run = run_with_pkg(Some("spliced"));
    assert_eq!(stamped(&run, &[]).items["core::parse"].pkg, None);
    let unnamed = [("core::parse".to_string(), None)];
    assert_eq!(stamped(&run, &unnamed).items["core::parse"].pkg, None);
}
