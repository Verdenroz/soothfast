//! The committed population baseline: `probes.lock`.
//!
//! Live market data makes some fields legitimately intermittent
//! (pre-market prices outside pre-market hours, dividend fields on
//! non-payers), so the lock records a *class* per field, not a snapshot:
//!
//! - `always`: populated in every accepted run; going null fails the gate
//! - `sometimes`: observed both ways across accepted runs; never gated
//!
//! Accepting demotes a flapping `always` field to `sometimes` and keeps it
//! there: repeated accepts converge on the stable contract instead of
//! ratcheting on one lucky run. A manifest can pin a field's class when
//! the operator knows better than the observations.
//!
//! Fields the spec declares but no accepted run has ever populated go in
//! the probe's `uncovered` list: committed debt, visible in review, and
//! promoted to a real class automatically once data appears. A declared
//! field in neither place fails the gate: adding a schema field without
//! wiring data to it is a finding, not a silence.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

/// A field's population class across accepted runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Always,
    Sometimes,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Class::Always => "always",
            Class::Sometimes => "sometimes",
        }
    }
}

/// One probe's locked state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbeLock {
    pub fields: BTreeMap<String, Class>,
    /// Declared by the spec, never yet populated in an accepted run.
    pub uncovered: BTreeSet<String>,
}

/// The lock file: probe name → locked state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Baseline {
    pub probes: BTreeMap<String, ProbeLock>,
}

/// One probe's gate findings against the lock.
#[derive(Debug, Default)]
pub struct Findings {
    /// Locked `always`, now null or absent.
    pub regressed: Vec<String>,
    /// Populated now, unknown to the lock: the lock is stale.
    pub new_fields: Vec<String>,
    /// Declared by the spec, not populated, and not in the accepted
    /// `uncovered` list: a schema field nobody wired to data.
    pub uncovered: Vec<String>,
}

impl Findings {
    pub fn is_clean(&self) -> bool {
        self.regressed.is_empty() && self.new_fields.is_empty() && self.uncovered.is_empty()
    }
}

impl Baseline {
    pub fn parse(text: &str) -> Result<Baseline, String> {
        let doc: Value =
            serde_json::from_str(text).map_err(|e| format!("probes.lock is not JSON: {e}"))?;
        match doc.get("version").and_then(Value::as_u64) {
            Some(1) => {}
            other => return Err(format!("probes.lock version {other:?}, expected 1")),
        }
        let mut probes = BTreeMap::new();
        let Some(entries) = doc.get("probes").and_then(Value::as_object) else {
            return Err("probes.lock has no `probes` table".into());
        };
        for (name, entry) in entries {
            let Some(fields) = entry.get("fields").and_then(Value::as_object) else {
                return Err(format!("probe `{name}` has no `fields` table"));
            };
            let mut lock = ProbeLock::default();
            for (path, class) in fields {
                let class = match class.as_str() {
                    Some("always") => Class::Always,
                    Some("sometimes") => Class::Sometimes,
                    other => {
                        return Err(format!(
                            "probe `{name}` field `{path}`: bad class {other:?}"
                        ));
                    }
                };
                lock.fields.insert(path.clone(), class);
            }
            if let Some(uncovered) = entry.get("uncovered").and_then(Value::as_array) {
                lock.uncovered = uncovered
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect();
            }
            probes.insert(name.clone(), lock);
        }
        Ok(Baseline { probes })
    }

    pub fn render(&self) -> String {
        let probes: BTreeMap<&str, Value> = self
            .probes
            .iter()
            .map(|(name, lock)| {
                let fields: BTreeMap<&str, &str> = lock
                    .fields
                    .iter()
                    .map(|(path, class)| (path.as_str(), class.as_str()))
                    .collect();
                let entry = if lock.uncovered.is_empty() {
                    json!({ "fields": fields })
                } else {
                    json!({ "fields": fields, "uncovered": lock.uncovered })
                };
                (name.as_str(), entry)
            })
            .collect();
        let mut out = serde_json::to_string_pretty(&json!({
            "version": 1,
            "probes": probes,
        }))
        .expect("baseline is plain JSON");
        out.push('\n');
        out
    }

    /// Gate one probe's observed population against its locked classes.
    /// `pinned_sometimes` are manifest-declared intermittent fields; they
    /// gate like `sometimes` regardless of the locked class. `declared`
    /// is the spec's field enumeration when coverage applies.
    pub fn gate(
        &self,
        probe: &str,
        observed: &BTreeMap<String, bool>,
        pinned_sometimes: &[String],
        declared: Option<&BTreeSet<String>>,
    ) -> Findings {
        let mut findings = Findings::default();
        let locked = self.probes.get(probe).cloned().unwrap_or_default();
        for (path, class) in &locked.fields {
            if *class == Class::Always
                && !pinned(pinned_sometimes, path)
                && observed.get(path) != Some(&true)
            {
                findings.regressed.push(path.clone());
            }
        }
        for (path, populated) in observed {
            if *populated && !locked.fields.contains_key(path) {
                findings.new_fields.push(path.clone());
            }
        }
        if let Some(declared) = declared {
            for path in declared {
                if observed.get(path) != Some(&true)
                    && !locked.fields.contains_key(path)
                    && !locked.uncovered.contains(path)
                    && !pinned(pinned_sometimes, path)
                {
                    findings.uncovered.push(path.clone());
                }
            }
        }
        findings
    }

    /// Fold one probe's observed population into the lock. First sight of
    /// a populated field locks it `always`; any accepted run that sees a
    /// locked field empty demotes it to `sometimes`, permanently. The
    /// `uncovered` list is recomputed from `declared`: whatever still has
    /// no class after this run is the probe's accepted coverage debt.
    pub fn accept(
        &mut self,
        probe: &str,
        observed: &BTreeMap<String, bool>,
        declared: Option<&BTreeSet<String>>,
    ) {
        let lock = self.probes.entry(probe.to_string()).or_default();
        for (path, populated) in observed {
            match (lock.fields.get(path), populated) {
                (None, true) => {
                    lock.fields.insert(path.clone(), Class::Always);
                }
                (None, false) => {}
                (Some(Class::Always), false) => {
                    lock.fields.insert(path.clone(), Class::Sometimes);
                }
                _ => {}
            }
        }
        // A locked field the response no longer mentions at all is the
        // same evidence as seeing it empty.
        let absent: Vec<String> = lock
            .fields
            .iter()
            .filter(|(path, class)| **class == Class::Always && !observed.contains_key(*path))
            .map(|(path, _)| path.clone())
            .collect();
        for path in absent {
            lock.fields.insert(path, Class::Sometimes);
        }
        lock.uncovered = declared
            .map(|declared| {
                declared
                    .iter()
                    .filter(|path| !lock.fields.contains_key(*path))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
    }

    /// Drop locked probes the manifest no longer declares. Returns the
    /// dropped names so the gate can refuse them without `--allow-gone`.
    pub fn retain_probes(&mut self, declared: &[String]) -> Vec<String> {
        let gone: Vec<String> = self
            .probes
            .keys()
            .filter(|name| !declared.contains(name))
            .cloned()
            .collect();
        for name in &gone {
            self.probes.remove(name);
        }
        gone
    }
}

fn pinned(pins: &[String], path: &str) -> bool {
    pins.iter().any(|pin| {
        pin == path
            || pin
                .strip_suffix('*')
                .is_some_and(|prefix| path.starts_with(prefix))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
        pairs
            .iter()
            .map(|(path, populated)| (path.to_string(), *populated))
            .collect()
    }

    fn declared(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn lock_round_trips_through_render_and_parse() {
        let mut baseline = Baseline::default();
        baseline.accept(
            "quote",
            &observed(&[("price", true), ("pe", true)]),
            Some(&declared(&["price", "pe", "spark"])),
        );
        let reparsed = Baseline::parse(&baseline.render()).unwrap();
        assert_eq!(reparsed, baseline);
        assert!(baseline.probes["quote"].uncovered.contains("spark"));
    }

    #[test]
    fn an_always_field_going_null_regresses() {
        let mut baseline = Baseline::default();
        baseline.accept("quote", &observed(&[("pe", true)]), None);
        let findings = baseline.gate("quote", &observed(&[("pe", false)]), &[], None);
        assert_eq!(findings.regressed, ["pe"]);
    }

    #[test]
    fn accept_demotes_a_flapping_field_for_good() {
        let mut baseline = Baseline::default();
        baseline.accept("quote", &observed(&[("preMarket", true)]), None);
        baseline.accept("quote", &observed(&[("preMarket", false)]), None);
        baseline.accept("quote", &observed(&[("preMarket", true)]), None);
        assert_eq!(
            baseline.probes["quote"].fields["preMarket"],
            Class::Sometimes
        );
        let findings = baseline.gate("quote", &observed(&[("preMarket", false)]), &[], None);
        assert!(findings.is_clean());
    }

    #[test]
    fn a_field_vanishing_entirely_demotes_on_accept_and_regresses_on_gate() {
        let mut baseline = Baseline::default();
        baseline.accept("quote", &observed(&[("pe", true)]), None);
        let findings = baseline.gate("quote", &observed(&[]), &[], None);
        assert_eq!(findings.regressed, ["pe"]);
        baseline.accept("quote", &observed(&[]), None);
        assert_eq!(baseline.probes["quote"].fields["pe"], Class::Sometimes);
    }

    #[test]
    fn new_populated_fields_mark_the_lock_stale() {
        let baseline = Baseline::default();
        let findings = baseline.gate("quote", &observed(&[("spark", true)]), &[], None);
        assert_eq!(findings.new_fields, ["spark"]);
    }

    #[test]
    fn manifest_pins_silence_the_gate_with_and_without_wildcard() {
        let mut baseline = Baseline::default();
        baseline.accept(
            "quote",
            &observed(&[("preMarketPrice", true), ("preMarketTime", true)]),
            None,
        );
        let now = observed(&[("preMarketPrice", false), ("preMarketTime", false)]);
        let findings = baseline.gate("quote", &now, &["preMarket*".to_string()], None);
        assert!(findings.is_clean());
        let findings = baseline.gate("quote", &now, &["preMarketPrice".to_string()], None);
        assert_eq!(findings.regressed, ["preMarketTime"]);
    }

    #[test]
    fn gone_probes_are_reported_and_dropped() {
        let mut baseline = Baseline::default();
        baseline.accept("quote", &observed(&[("pe", true)]), None);
        baseline.accept("chart", &observed(&[("meta", true)]), None);
        let gone = baseline.retain_probes(&["quote".to_string()]);
        assert_eq!(gone, ["chart"]);
        assert!(!baseline.probes.contains_key("chart"));
    }

    #[test]
    fn a_declared_field_nobody_populates_fails_until_accepted() {
        let mut baseline = Baseline::default();
        let spec_fields = declared(&["price", "cvar95"]);
        let now = observed(&[("price", true)]);
        baseline.accept("risk", &now, Some(&spec_fields));
        let findings = baseline.gate("risk", &now, &[], Some(&spec_fields));
        assert!(findings.is_clean(), "accepted uncovered debt passes");

        let grown = declared(&["price", "cvar95", "omega"]);
        let findings = baseline.gate("risk", &now, &[], Some(&grown));
        assert_eq!(findings.uncovered, ["omega"]);
    }

    #[test]
    fn covered_fields_drop_off_the_uncovered_list_on_accept() {
        let mut baseline = Baseline::default();
        let spec_fields = declared(&["price", "cvar95"]);
        baseline.accept("risk", &observed(&[("price", true)]), Some(&spec_fields));
        assert!(baseline.probes["risk"].uncovered.contains("cvar95"));
        baseline.accept(
            "risk",
            &observed(&[("price", true), ("cvar95", true)]),
            Some(&spec_fields),
        );
        assert!(baseline.probes["risk"].uncovered.is_empty());
        assert_eq!(baseline.probes["risk"].fields["cvar95"], Class::Always);
    }

    #[test]
    fn pinned_sometimes_fields_are_not_coverage_findings() {
        let baseline = Baseline::default();
        let spec_fields = declared(&["preMarketPrice"]);
        let findings = baseline.gate(
            "quote",
            &observed(&[]),
            &["preMarket*".to_string()],
            Some(&spec_fields),
        );
        assert!(findings.is_clean());
    }
}
