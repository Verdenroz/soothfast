//! Whether each registered route or tool is in the committed spec and
//! exercised by a probe.

use crate::reconcile::Reconciliation;
use crate::{DeclaredOp, RouteDecl};

/// One registered route or tool and where it is accounted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specified {
    /// Spec file the route is registered against.
    pub spec: String,
    /// Operation identity: operationId, GraphQL field, MCP tool name or channel.
    pub operation: String,
    /// Verb the code serves, or `TOOL`/`QUERY`/... for non-HTTP dialects.
    pub method: String,
    /// Path or template the code serves.
    pub path: String,
    /// The handler's public path, as the registry records it.
    pub handler: String,
    /// The committed spec declares this operation with the method and path
    /// the code serves.
    pub in_spec: bool,
    /// How the spec's declaration of this operation disagrees with the code.
    pub mismatch: Option<String>,
    /// Some probe exercises this method and path. `None` for a `TOOL`, which
    /// no HTTP probe can reach.
    pub probed: Option<bool>,
}

impl Specified {
    /// In the spec and, where a probe can reach it, probed.
    pub fn is_specified(&self) -> bool {
        self.in_spec && self.probed.unwrap_or(true)
    }
}

/// One entry per route, in registry order. `rec` is the reconciliation of
/// `routes` against the spec file's declared operations, and `covers`
/// decides whether a probe exercises a route's operation.
pub fn specified(
    spec: &str,
    routes: &[RouteDecl],
    rec: &Reconciliation,
    covers: impl Fn(&DeclaredOp) -> bool,
) -> Vec<Specified> {
    routes
        .iter()
        .map(|route| {
            let op = DeclaredOp {
                operation: route.operation.clone(),
                method: route.method.clone(),
                path: route.path.clone(),
            };
            let mismatch = rec
                .mismatched
                .iter()
                .find(|(m, _, _)| m.id == route.id && m.operation == route.operation)
                .map(|(_, _, what)| what.clone());
            let absent = rec
                .missing_spec
                .iter()
                .any(|m| m.id == route.id && m.operation == route.operation);
            Specified {
                spec: spec.to_string(),
                in_spec: !absent && mismatch.is_none(),
                mismatch,
                probed: (route.method != "TOOL").then(|| covers(&op)),
                operation: route.operation.clone(),
                method: route.method.clone(),
                path: route.path.clone(),
                handler: route.id.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::reconcile;

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
    fn reports_spec_and_probe_membership_per_route() {
        let declared = vec![op("getItem", "GET", "/items/{id}")];
        let routes = vec![
            route("getItem", "GET", "/items/{id}"),
            route("ghost", "GET", "/ghost"),
        ];
        let rec = reconcile(&declared, &routes);
        let items = specified("api.yaml", &routes, &rec, |op| op.operation == "getItem");
        assert_eq!(items.len(), 2);
        assert!(items[0].in_spec && items[0].probed == Some(true) && items[0].is_specified());
        assert!(!items[1].in_spec && items[1].probed == Some(false) && !items[1].is_specified());
        assert_eq!(items[0].handler, "app::getItem");
        assert_eq!(items[0].spec, "api.yaml");
    }

    #[test]
    fn an_mcp_tool_is_specified_by_being_in_the_spec_alone() {
        let declared = vec![op("search", "TOOL", "search")];
        let routes = vec![route("search", "TOOL", "search")];
        let rec = reconcile(&declared, &routes);
        let items = specified("tools.json", &routes, &rec, |_| false);
        assert!(items[0].in_spec);
        assert_eq!(items[0].probed, None);
        assert!(items[0].is_specified());
    }

    #[test]
    fn a_mismatched_route_is_not_in_the_spec() {
        let declared = vec![op("getItem", "GET", "/items/{id}")];
        let routes = vec![route("getItem", "POST", "/items/{id}")];
        let rec = reconcile(&declared, &routes);
        let items = specified("api.yaml", &routes, &rec, |_| true);
        assert!(!items[0].in_spec);
        assert_eq!(items[0].probed, Some(true));
        assert_eq!(
            items[0].mismatch.as_deref(),
            Some("method POST != spec GET")
        );
        assert!(!items[0].is_specified());
    }
}
