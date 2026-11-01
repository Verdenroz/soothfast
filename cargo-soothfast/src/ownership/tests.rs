use super::*;
use crate::invoke::ItemMetrics;

#[test]
fn a_stored_pkg_gives_way_to_the_fresh_listing() {
    let mut run = Run::default();
    let stale = ItemMetrics {
        pkg: Some("stale".into()),
        ..ItemMetrics::default()
    };
    run.items.insert("core::parse".into(), stale.clone());
    run.items.insert("app::decode".into(), stale);
    let listing = [
        ("core::parse".to_string(), Some("core".to_string())),
        ("app::decode".to_string(), None),
    ];
    let stamped = stamped(&run, &listing);
    assert_eq!(stamped.items["core::parse"].pkg.as_deref(), Some("core"));
    assert_eq!(stamped.items["app::decode"].pkg, None);
}

#[test]
fn with_no_listing_no_item_names_a_package() {
    let mut run = Run::default();
    let cached = ItemMetrics {
        pkg: Some("core".into()),
        ..ItemMetrics::default()
    };
    run.items.insert("core::parse".into(), cached);
    assert_eq!(stamped(&run, &[]).items["core::parse"].pkg, None);
}
