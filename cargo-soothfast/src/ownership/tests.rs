use super::*;
use crate::invoke::ItemMetrics;

#[test]
fn a_stored_pkg_gives_way_to_the_fresh_listing() {
    let mut run = Run::default();
    let stale = ItemMetrics {
        pkg: Some("stale".into()),
        ..ItemMetrics::default()
    };
    run.items.insert("core::parse".into(), stale);
    let listing = [("core::parse".to_string(), Some("core".to_string()))];
    let stamped = stamped(&run, &listing);
    assert_eq!(stamped.items["core::parse"].pkg.as_deref(), Some("core"));
}

#[test]
fn an_item_the_listing_does_not_name_keeps_its_run_s_pkg() {
    let mut run = Run::default();
    let measured = ItemMetrics {
        pkg: Some("core".into()),
        ..ItemMetrics::default()
    };
    run.items.insert("core::parse".into(), measured.clone());
    run.items
        .insert("app::decode".into(), ItemMetrics::default());
    let listing = [("core::parse".to_string(), None)];
    let named = stamped(&run, &listing);
    assert_eq!(named.items["core::parse"].pkg.as_deref(), Some("core"));
    assert_eq!(named.items["app::decode"].pkg, None);
    assert_eq!(
        stamped(&run, &[]).items["core::parse"].pkg.as_deref(),
        Some("core")
    );
}
