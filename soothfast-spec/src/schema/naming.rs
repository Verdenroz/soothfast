//! One component name per type, decided before any of them is visited.
//!
//! A component name has to be a property of the *type*, not of the walk that
//! reached it. Dialects merge the components of every operation into one
//! document, so a name chosen by whichever operation happened to arrive first
//! would both churn between runs and collide across operations — two distinct
//! types named `Region` claiming one schema, or the same type named two ways.
//!
//! So the whole assignment is computed up front from the documents alone:
//! every canonical path the documents can walk, in sorted order, in or out of
//! the operations that end up referencing it. The result is a pure function
//! of the inputs, identical for every resolver built from the same documents.

use std::collections::{BTreeMap, BTreeSet};

/// Assign a component-name stem to every walkable canonical path.
///
/// A type keeps its bare Rust name while no other type wants it. Where
/// several do, each takes the shortest trailing run of its module path that
/// tells it apart — `finance_query::constants::Region` stays `Region` and
/// `finance_query::constants::indices::Region` becomes `indices_Region`.
/// Unambiguous names are settled first, so qualifying one type can never
/// take a bare name another type was entitled to.
pub(super) fn stems(paths: &BTreeSet<String>) -> BTreeMap<String, String> {
    let mut claimants: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in paths {
        claimants.entry(bare(path)).or_default().push(path);
    }

    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut assigned: BTreeMap<String, String> = BTreeMap::new();
    for (name, paths) in claimants.iter().filter(|(_, p)| p.len() == 1) {
        taken.insert((*name).to_string());
        assigned.insert(paths[0].to_string(), (*name).to_string());
    }
    // `claimants` is keyed by a BTreeMap and each list came out of a
    // BTreeSet, so both loops run in sorted order — the assignment does not
    // depend on the order anything was encountered in.
    for (_, paths) in claimants.iter().filter(|(_, p)| p.len() > 1) {
        for path in paths {
            let chosen = pick(path, &taken);
            taken.insert(chosen.clone());
            assigned.insert((*path).to_string(), chosen);
        }
    }
    assigned
}

/// The shortest name for `path` that nothing else has taken.
fn pick(path: &str, taken: &BTreeSet<String>) -> String {
    let segs: Vec<&str> = path.split("::").collect();
    for n in 1..=segs.len() {
        let candidate = segs[segs.len() - n..].join("_");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    // The whole path is spoken for, which takes two types of identical path.
    // Number it rather than let one stand in for the other.
    let stem = segs.join("_");
    (2..)
        .map(|n| format!("{stem}_{n}"))
        .find(|c| !taken.contains(c))
        .unwrap_or(stem)
}

fn bare(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn an_unambiguous_name_stays_bare() {
        let s = stems(&set(&["app::model::Item", "app::model::Order"]));
        assert_eq!(s["app::model::Item"], "Item");
        assert_eq!(s["app::model::Order"], "Order");
    }

    #[test]
    fn two_same_named_types_in_one_crate_are_told_apart_by_module() {
        // The real case: finance-query has a `Region` in `constants` and
        // another in `constants::indices`, and operations reference both.
        let s = stems(&set(&[
            "finance_query::constants::Region",
            "finance_query::constants::indices::Region",
        ]));
        assert_eq!(s["finance_query::constants::Region"], "Region");
        assert_eq!(
            s["finance_query::constants::indices::Region"],
            "indices_Region"
        );
    }

    #[test]
    fn same_named_types_in_different_crates_are_told_apart_too() {
        let s = stems(&set(&["server::Meta", "lib_crate::Meta"]));
        assert_eq!(s["lib_crate::Meta"], "Meta", "sorts first, keeps the name");
        assert_eq!(s["server::Meta"], "server_Meta");
    }

    #[test]
    fn the_assignment_does_not_depend_on_input_order() {
        let forward = stems(&set(&[
            "a::x::Region",
            "b::Region",
            "a::y::Region",
            "a::Item",
        ]));
        let backward = stems(&set(&[
            "a::Item",
            "a::y::Region",
            "b::Region",
            "a::x::Region",
        ]));
        assert_eq!(forward, backward);
        // And every type still got a name of its own.
        let names: BTreeSet<&String> = forward.values().collect();
        assert_eq!(names.len(), forward.len());
    }

    #[test]
    fn qualifying_never_steals_a_name_another_type_owns() {
        // `indices_Region` exists as a type in its own right, so the
        // ambiguous `indices::Region` has to reach further back.
        let s = stems(&set(&[
            "app::Region",
            "app::indices::Region",
            "app::indices_Region",
        ]));
        assert_eq!(s["app::indices_Region"], "indices_Region");
        assert_eq!(s["app::Region"], "Region");
        assert_eq!(s["app::indices::Region"], "app_indices_Region");
    }

    #[test]
    fn three_claimants_all_get_distinct_names() {
        let s = stems(&set(&["a::R", "b::R", "c::R"]));
        let names: BTreeSet<&String> = s.values().collect();
        assert_eq!(names.len(), 3, "got {s:?}");
        assert_eq!(s["a::R"], "R", "the smallest path keeps the bare name");
    }

    #[test]
    fn a_single_segment_path_is_handled() {
        let s = stems(&set(&["Region", "app::Region"]));
        assert_eq!(s["Region"], "Region");
        assert_eq!(s["app::Region"], "app_Region");
    }
}
