//! One reconciler for every provider: spec operations ↔ code routes,
//! matched by operation identity, cross-checked on method/path.

use std::collections::BTreeMap;

use crate::{DeclaredOp, RouteDecl};

/// Outcome of matching `#[route]` declarations against a spec's operations:
/// what's missing on each side, what disagrees, and what lined up.
#[derive(Debug, Default)]
pub struct Reconciliation {
    /// Spec declares it; no `#[route]` implements it.
    pub missing_handler: Vec<DeclaredOp>,
    /// Code declares it; the spec doesn't know it.
    pub missing_spec: Vec<RouteDecl>,
    /// Same operation, different method/path: (route, declared, what).
    pub mismatched: Vec<(RouteDecl, DeclaredOp, String)>,
    /// Operations that matched exactly on both sides.
    pub matched: usize,
}

impl Reconciliation {
    /// True when every operation matched: nothing missing on either side
    /// and no method/path disagreements.
    pub fn is_clean(&self) -> bool {
        self.missing_handler.is_empty()
            && self.missing_spec.is_empty()
            && self.mismatched.is_empty()
    }
}

/// Match routes to declared operations by operation id, then check that
/// method and path agree where the route states them.
pub fn reconcile(declared: &[DeclaredOp], routes: &[RouteDecl]) -> Reconciliation {
    let mut rec = Reconciliation::default();
    let by_op: BTreeMap<&str, &DeclaredOp> =
        declared.iter().map(|d| (d.operation.as_str(), d)).collect();

    for route in routes {
        match by_op.get(route.operation.as_str()) {
            None => rec.missing_spec.push(route.clone()),
            Some(decl) => {
                let mut problems = Vec::new();
                if !route.method.is_empty() && !route.method.eq_ignore_ascii_case(&decl.method) {
                    problems.push(format!("method {} != spec {}", route.method, decl.method));
                }
                if !route.path.is_empty() && route.path != decl.path {
                    problems.push(format!("path {} != spec {}", route.path, decl.path));
                }
                if problems.is_empty() {
                    rec.matched += 1;
                } else {
                    rec.mismatched
                        .push((route.clone(), (*decl).clone(), problems.join("; ")));
                }
            }
        }
    }

    let implemented: std::collections::BTreeSet<&str> =
        routes.iter().map(|r| r.operation.as_str()).collect();
    for decl in declared {
        if !implemented.contains(decl.operation.as_str()) {
            rec.missing_handler.push(decl.clone());
        }
    }
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(operation: &str, method: &str, path: &str) -> DeclaredOp {
        DeclaredOp {
            operation: operation.into(),
            method: method.into(),
            path: path.into(),
        }
    }
    fn route(operation: &str, method: &str, path: &str) -> RouteDecl {
        RouteDecl {
            id: format!("app::{operation}"),
            operation: operation.into(),
            method: method.into(),
            path: path.into(),
        }
    }

    #[test]
    fn clean_when_everything_matches() {
        let declared = vec![op("getItem", "GET", "/items/{id}")];
        let routes = vec![route("getItem", "GET", "/items/{id}")];
        let rec = reconcile(&declared, &routes);
        assert!(rec.is_clean());
        assert_eq!(rec.matched, 1);
    }

    #[test]
    fn catches_all_three_drift_kinds() {
        let declared = vec![
            op("getItem", "GET", "/items/{id}"),
            op("listItems", "GET", "/items"),
        ];
        let routes = vec![
            route("getItem", "POST", "/items/{id}"), // method mismatch
            route("ghost", "GET", "/ghost"),         // not in spec
        ];
        let rec = reconcile(&declared, &routes);
        assert_eq!(rec.mismatched.len(), 1);
        assert_eq!(rec.missing_spec.len(), 1);
        assert_eq!(rec.missing_handler.len(), 1); // listItems unimplemented
    }

    #[test]
    fn empty_route_fields_skip_cross_checks() {
        let declared = vec![op("item", "QUERY", "item")];
        let routes = vec![route("item", "", "")];
        assert!(reconcile(&declared, &routes).is_clean());
    }
}
