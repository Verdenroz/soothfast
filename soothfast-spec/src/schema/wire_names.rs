//! Which framework names one container's fields and variants on the wire.
//!
//! serde's attributes describe the wire only when serde produces it. A type
//! async-graphql serves is resolved by async-graphql, which emits its own
//! JSON and never consults serde — so for those, `#[graphql(...)]` replaces
//! `#[serde(...)]` for naming entirely rather than layering on top of it.

use serde_json::Value;

use super::graphql_attrs;
use super::serde_attrs::{self, ContainerAttrs};

/// The naming rules in force for one struct or enum.
pub(super) struct WireNames {
    /// serde's view. Always parsed: it carries the enum representation and
    /// the field-shape attributes, which have no async-graphql counterpart.
    pub(super) serde: ContainerAttrs,
    /// async-graphql's view, present only for the types it serves.
    pub(super) graphql: Option<graphql_attrs::ContainerAttrs>,
}

impl WireNames {
    /// The wire name of a field, or `None` when nothing puts it on the wire.
    pub(super) fn field(
        &self,
        rust_name: &str,
        raw: &[Value],
        serde: &serde_attrs::FieldAttrs,
    ) -> Option<String> {
        let Some(g) = &self.graphql else {
            return (!serde.skip)
                .then(|| serde_attrs::wire_name_field(rust_name, serde, &self.serde));
        };
        // serde does not produce this type's JSON, so only `#[graphql(skip)]`
        // can take the field off the wire — `#[serde(skip)]` never applies.
        let f = graphql_attrs::field(raw);
        (!f.skip).then(|| graphql_attrs::wire_name_field(rust_name, &f, g))
    }

    /// The wire name of an enum variant.
    pub(super) fn variant(&self, rust_name: &str, raw: &[Value]) -> String {
        match &self.graphql {
            Some(g) => graphql_attrs::wire_name_variant(rust_name, &graphql_attrs::variant(raw), g),
            None => {
                serde_attrs::wire_name_variant(rust_name, &serde_attrs::container(raw), &self.serde)
            }
        }
    }
}
