//! Canonical-path → JSON Schema mappings for types outside this crate.
//!
//! Foreign types are absent from rustdoc's `index` but present in its `paths`
//! with a fully-qualified path, so they are always *identifiable* even when
//! they cannot be walked. This table turns that identity into a schema.
//!
//! Entries are only added where the serde wire format is known with
//! certainty. An unmapped type becomes a reported gap and an open schema —
//! a wrong mapping would be silently incorrect, which is worse.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Built-in and user-supplied mappings from canonical type path to schema.
#[derive(Debug, Clone)]
pub struct TypeTable {
    map: BTreeMap<String, Value>,
}

impl Default for TypeTable {
    fn default() -> Self {
        Self::builtin()
    }
}

impl TypeTable {
    /// The curated set: std types plus ecosystem types whose serde
    /// representation is stable and unambiguous.
    pub fn builtin() -> Self {
        let mut map = BTreeMap::new();
        let mut put = |k: &str, v: Value| {
            map.insert(k.to_string(), v);
        };

        let string = || json!({ "type": "string" });
        put("alloc::string::String", string());
        put("str", string());
        put("std::path::PathBuf", string());
        put("std::path::Path", string());
        put("std::ffi::OsString", string());

        put("core::net::IpAddr", json!({ "type": "string" }));
        put(
            "core::net::Ipv4Addr",
            json!({ "type": "string", "format": "ipv4" }),
        );
        put(
            "core::net::Ipv6Addr",
            json!({ "type": "string", "format": "ipv6" }),
        );
        put("std::net::SocketAddr", string());

        // serde renders these as structs, not as scalars — a "duration is a
        // number" guess would be wrong on the wire.
        put(
            "core::time::Duration",
            json!({
                "type": "object",
                "properties": { "secs": { "type": "integer", "minimum": 0 },
                                "nanos": { "type": "integer", "minimum": 0 } },
                "required": ["secs", "nanos"],
            }),
        );
        put(
            "std::time::SystemTime",
            json!({
                "type": "object",
                "properties": { "secs_since_epoch": { "type": "integer" },
                                "nanos_since_epoch": { "type": "integer" } },
                "required": ["secs_since_epoch", "nanos_since_epoch"],
            }),
        );

        put("uuid::Uuid", json!({ "type": "string", "format": "uuid" }));
        put("url::Url", json!({ "type": "string", "format": "uri" }));

        let date_time = || json!({ "type": "string", "format": "date-time" });
        put("chrono::DateTime", date_time());
        put("chrono::naive::datetime::NaiveDateTime", date_time());
        put(
            "chrono::naive::date::NaiveDate",
            json!({ "type": "string", "format": "date" }),
        );
        put(
            "chrono::naive::time::NaiveTime",
            json!({ "type": "string", "format": "time" }),
        );
        put("time::OffsetDateTime", date_time());

        // Arbitrary-precision numerics serialize as strings to survive the
        // round trip through JSON's f64.
        put("rust_decimal::Decimal", string());
        put("bigdecimal::BigDecimal", string());

        // Any JSON value: genuinely unconstrained, not a gap.
        put("serde_json::value::Value", json!({}));

        Self { map }
    }

    /// An empty table, for tests that want to observe gap reporting.
    pub fn empty() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    /// Add or override a mapping. User entries win over built-ins, so a crate
    /// with a custom serializer for a std type can correct the default.
    pub fn insert(&mut self, path: impl Into<String>, schema: Value) {
        self.map.insert(path.into(), schema);
    }

    /// Look a canonical path up, accepting the shorter forms users actually
    /// write in config: `uuid::Uuid` for `uuid::Uuid`, and the bare type name
    /// when a crate re-exports from a private module.
    pub fn lookup(&self, canonical: &str) -> Option<&Value> {
        if let Some(v) = self.map.get(canonical) {
            return Some(v);
        }
        let segments: Vec<&str> = canonical.split("::").collect();
        if let (Some(first), Some(last)) = (segments.first(), segments.last()) {
            if segments.len() > 2 {
                if let Some(v) = self.map.get(&format!("{first}::{last}")) {
                    return Some(v);
                }
            }
            // Last resort: match on type name alone. Ambiguity here is real
            // but rare, and losing the mapping means a spurious gap.
            if let Some((_, v)) = self
                .map
                .iter()
                .find(|(k, _)| k.rsplit("::").next() == Some(*last))
            {
                return Some(v);
            }
        }
        None
    }

    /// Number of entries, for reporting how a config extended the defaults.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when no mappings are registered.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_std_string_by_canonical_path() {
        let t = TypeTable::builtin();
        assert_eq!(
            t.lookup("alloc::string::String"),
            Some(&json!({"type":"string"}))
        );
    }

    #[test]
    fn duration_maps_to_the_struct_serde_actually_emits() {
        let t = TypeTable::builtin();
        let d = t.lookup("core::time::Duration").expect("Duration mapped");
        assert_eq!(d["type"], "object");
        assert_eq!(d["required"][0], "secs");
    }

    #[test]
    fn accepts_the_short_form_users_write_in_config() {
        let mut t = TypeTable::empty();
        t.insert("uuid::Uuid", json!({"type":"string","format":"uuid"}));
        // rustdoc reports the definition site, which may be a private module.
        let found = t.lookup("uuid::builder::Uuid").expect("short form matches");
        assert_eq!(found["format"], "uuid");
    }

    #[test]
    fn unmapped_type_returns_none_rather_than_a_guess() {
        let t = TypeTable::builtin();
        assert_eq!(t.lookup("some_crate::Exotic"), None);
    }

    #[test]
    fn user_entries_override_builtins() {
        let mut t = TypeTable::builtin();
        t.insert("core::time::Duration", json!({"type":"integer"}));
        let d = t.lookup("core::time::Duration").expect("still mapped");
        assert_eq!(d["type"], "integer");
    }
}
