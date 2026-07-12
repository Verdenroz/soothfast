//! `#[route]` shape overrides reach the registry.
//!
//! Spec generation infers request and response shapes from the handler
//! signature; these attributes exist for what inference provably cannot see,
//! so the path from attribute to registry needs to be exact.

use soothfast::registry::{RouteItem, route_items};
use soothfast::route;

#[route(
    spec = "specs/openapi.yaml",
    operation = "getItem",
    method = "GET",
    path = "/items/{id}"
)]
pub fn get_item(id: u64) -> String {
    format!("item-{id}")
}

#[route(
    spec = "specs/openapi.yaml",
    operation = "createItem",
    method = "POST",
    path = "/items",
    request = "NewItem",
    response = "Item",
    status = 201
)]
pub fn create_item() -> String {
    String::new()
}

/// A handler whose real return type is erased — the case `response` exists
/// for. Only the override names the concrete shape.
#[route(
    spec = "specs/openapi.yaml",
    operation = "listItems",
    method = "GET",
    path = "/items",
    response = "ItemPage"
)]
pub fn list_items() -> impl std::fmt::Debug {
    Vec::<u8>::new()
}

/// Get one widget by key.
///
/// Detail paragraphs must not leak into the summary.
#[route(
    spec = "specs/openapi.yaml",
    operation = "getWidget",
    method = "GET",
    path = "/widgets/{key}",
    params = "WidgetQuery",
    response = "Widget"
)]
pub fn route_get_widget() {}

/// Stream widget updates.
#[route(
    spec = "specs/asyncapi.yaml",
    operation = "sendWidget",
    method = "PUBLISH",
    path = "/ws"
)]
#[route(
    spec = "specs/asyncapi.yaml",
    operation = "receiveWidget",
    method = "SUBSCRIBE",
    path = "/ws"
)]
pub fn route_widget_stream() {}

fn route_for(operation: &str) -> &'static RouteItem {
    route_items()
        .iter()
        .find(|r| r.operation == operation)
        .unwrap_or_else(|| panic!("route {operation} was not registered"))
}

#[test]
fn a_route_without_overrides_leaves_them_unset() {
    let r = route_for("getItem");
    assert_eq!(r.request, None);
    assert_eq!(r.response, None);
    assert_eq!(r.status, None);
    // Identity still registers exactly as before.
    assert_eq!(r.method, "GET");
    assert_eq!(r.path, "/items/{id}");
    assert!(r.id.ends_with("::get_item"), "got {}", r.id);
}

#[test]
fn all_three_overrides_reach_the_registry() {
    let r = route_for("createItem");
    assert_eq!(r.request, Some("NewItem"));
    assert_eq!(r.response, Some("Item"));
    assert_eq!(r.status, Some(201));
}

#[test]
fn overrides_are_independent_of_each_other() {
    let r = route_for("listItems");
    assert_eq!(r.response, Some("ItemPage"));
    assert_eq!(r.request, None, "an unset override stays unset");
    assert_eq!(r.status, None);
}

#[test]
fn the_annotated_functions_still_run_untouched() {
    // #[route] registers metadata; it must never alter the body.
    assert_eq!(get_item(42), "item-42");
    assert_eq!(create_item(), "");
}

#[test]
fn params_and_the_doc_summary_reach_the_registry() {
    let r = route_for("getWidget");
    assert_eq!(r.params, Some("WidgetQuery"));
    assert_eq!(r.summary, Some("Get one widget by key."));
}

#[test]
fn an_undocumented_route_has_no_summary() {
    let r = route_for("getItem");
    assert_eq!(r.params, None);
    assert_eq!(r.summary, None);
}

#[test]
fn stacked_attributes_each_carry_the_shared_doc_summary() {
    assert_eq!(
        route_for("sendWidget").summary,
        Some("Stream widget updates.")
    );
    assert_eq!(
        route_for("receiveWidget").summary,
        Some("Stream widget updates.")
    );
}
