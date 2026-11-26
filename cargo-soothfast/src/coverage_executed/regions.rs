//! Executed source regions from `llvm-cov export` JSON, mapped onto the
//! spans of a package's public items.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use soothfast_docs::surface::{Span, Surface};

/// Region kind 0 in the export format: counted code, as opposed to
/// expansion, skipped, gap and branch regions.
const CODE_REGION: u64 = 0;

/// The line ranges of every region with a nonzero count, by absolute file.
#[derive(Debug, Default)]
pub struct Regions {
    by_file: BTreeMap<PathBuf, BTreeSet<(u32, u32)>>,
    /// The run that produced them failed, so more code may have run than
    /// they show.
    pub partial: bool,
}

impl Regions {
    /// Fold the regions of one `llvm-cov export` document into `self`. A
    /// region is `[line_start, col_start, line_end, col_end, count, file_id,
    /// expanded_file_id, kind]`, its file named by the function's `filenames`.
    pub fn add_export(&mut self, export: &Value) {
        let functions = export["data"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|data| data["functions"].as_array().into_iter().flatten());
        let mut named: HashMap<&str, PathBuf> = HashMap::new();
        for function in functions {
            for region in function["regions"].as_array().into_iter().flatten() {
                let field = |i: usize| region[i].as_u64().unwrap_or(0);
                let (begin, end, count, file, kind) =
                    (field(0), field(2), field(4), field(5), field(7));
                if kind != CODE_REGION || count == 0 {
                    continue;
                }
                if let Some(name) = function["filenames"][file as usize].as_str() {
                    let path = named
                        .entry(name)
                        .or_insert_with(|| resolved(Path::new(name)));
                    self.by_file
                        .entry(path.clone())
                        .or_default()
                        .insert((begin as u32, end as u32));
                }
            }
        }
    }

    /// The regions as `{file: [[begin, end], ...]}`.
    pub fn to_json(&self) -> Value {
        Value::Object(
            self.by_file
                .iter()
                .map(|(file, ranges)| (file.to_string_lossy().into_owned(), json!(ranges)))
                .collect(),
        )
    }

    /// Regions written by [`Regions::to_json`].
    pub fn from_json(doc: &Value) -> Option<Regions> {
        let by_file = doc
            .as_object()?
            .iter()
            .map(|(file, ranges)| {
                let ranges: BTreeSet<(u32, u32)> = serde_json::from_value(ranges.clone()).ok()?;
                Some((PathBuf::from(file), ranges))
            })
            .collect::<Option<_>>()?;
        Some(Regions {
            by_file,
            partial: false,
        })
    }

    /// Whether any executed region shares a line with one of `spans`, which
    /// are relative to `root`, the directory rustdoc ran in.
    pub fn reaches(&self, root: &Path, spans: &[Span]) -> bool {
        spans.iter().any(|span| {
            self.by_file
                .get(&resolved(&root.join(&span.file)))
                .is_some_and(|regions| {
                    regions
                        .iter()
                        .any(|&(begin, end)| begin <= span.end && end >= span.begin)
                })
        })
    }
}

/// `path` with symlinks and `..` resolved, so a symlinked checkout names a
/// file the way the coverage mapping does. A path that does not exist stays
/// as written.
fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// One public item counted in the denominator.
#[derive(Debug, PartialEq, Eq)]
pub struct Declared<'a> {
    pub path: String,
    pub kind: &'a str,
    pub spans: &'a [Span],
}

/// The public functions, types and inherent methods of a surface: the items
/// `Surface::counted` yields that can run.
pub fn declared(surface: &Surface) -> Vec<Declared<'_>> {
    const KINDS: [&str; 5] = ["function", "struct", "enum", "union", "trait"];
    surface
        .counted()
        .into_iter()
        .filter(|c| KINDS.contains(&c.info.kind.as_str()) && !c.info.spans.is_empty())
        .map(|c| Declared {
            path: c.path,
            kind: &c.info.kind,
            spans: &c.info.spans,
        })
        .collect()
}

/// The workspace members other than `pkg` whose source files hold a span of
/// `declared`: the items `pkg` re-exports from them. `members` pairs each
/// package name with its directory; the deepest directory holding a file owns
/// it, since the root package's directory holds every member's.
pub fn foreign_members(
    declared: &[Declared<'_>],
    root: &Path,
    pkg: &str,
    members: &[(String, PathBuf)],
) -> BTreeSet<String> {
    let dirs: Vec<(&str, PathBuf)> = members
        .iter()
        .map(|(name, dir)| (name.as_str(), resolved(dir)))
        .collect();
    declared
        .iter()
        .flat_map(|item| item.spans)
        .filter_map(|span| {
            let file = resolved(&root.join(&span.file));
            dirs.iter()
                .filter(|(_, dir)| file.starts_with(dir))
                .max_by_key(|(_, dir)| dir.components().count())
                .map(|(name, _)| *name)
        })
        .filter(|name| *name != pkg)
        .map(str::to_string)
        .collect()
}

/// The declared items any counted region reaches.
pub fn executed<'d>(
    declared: &'d [Declared<'_>],
    root: &Path,
    regions: &Regions,
) -> BTreeSet<&'d str> {
    declared
        .iter()
        .filter(|item| regions.reaches(root, item.spans))
        .map(|item| item.path.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use soothfast_docs::surface::ItemInfo;

    use super::*;

    fn span(begin: u32, end: u32) -> Span {
        Span {
            file: "src/lib.rs".into(),
            begin,
            end,
        }
    }

    fn info(kind: &str, spans: Vec<Span>) -> ItemInfo {
        ItemInfo {
            kind: kind.into(),
            fingerprint: 1,
            signature: String::new(),
            has_docs: false,
            example: false,
            spans,
        }
    }

    fn export(regions: Value) -> Value {
        json!({
            "type": "llvm.coverage.json.export",
            "version": "2.0.1",
            "data": [{
                "functions": [{
                    "name": "demo::f",
                    "count": 1,
                    "filenames": ["/ws/src/lib.rs", "/ws/src/other.rs"],
                    "regions": regions,
                }],
            }],
        })
    }

    fn regions(export: Value) -> Regions {
        let mut regions = Regions::default();
        regions.add_export(&export);
        regions
    }

    #[test]
    fn a_counted_region_on_an_item_line_reaches_it() {
        let r = regions(export(json!([[10, 1, 12, 2, 3, 0, 0, 0]])));
        assert!(r.reaches(Path::new("/ws"), &[span(8, 10)]));
        assert!(r.reaches(Path::new("/ws"), &[span(12, 20)]));
        assert!(!r.reaches(Path::new("/ws"), &[span(13, 20)]));
        assert!(!r.reaches(Path::new("/ws"), &[span(1, 9)]));
    }

    #[test]
    fn a_span_in_another_member_names_that_member() {
        let members = [
            ("app".to_string(), PathBuf::from("/ws")),
            ("types".to_string(), PathBuf::from("/ws/types")),
            ("derive".to_string(), PathBuf::from("/ws/derive")),
        ];
        let spans = [
            [Span {
                file: "src/lib.rs".into(),
                begin: 1,
                end: 2,
            }],
            [Span {
                file: "types/src/ma.rs".into(),
                begin: 1,
                end: 2,
            }],
        ];
        let declared: Vec<_> = spans
            .iter()
            .enumerate()
            .map(|(i, spans)| Declared {
                path: format!("app::f{i}"),
                kind: "function",
                spans,
            })
            .collect();
        let found = foreign_members(&declared, Path::new("/ws"), "app", &members);
        assert_eq!(found, BTreeSet::from(["types".to_string()]));
    }

    #[test]
    fn a_region_with_a_zero_count_reaches_nothing() {
        let r = regions(export(json!([[10, 1, 12, 2, 0, 0, 0, 0]])));
        assert!(!r.reaches(Path::new("/ws"), &[span(10, 12)]));
    }

    #[test]
    fn only_code_regions_count() {
        let r = regions(export(json!([
            [10, 1, 12, 2, 5, 0, 0, 1],
            [10, 1, 12, 2, 5, 0, 0, 2],
            [10, 1, 12, 2, 5, 0, 0, 3],
            [10, 1, 12, 2, 5, 0, 0, 4],
        ])));
        assert!(!r.reaches(Path::new("/ws"), &[span(10, 12)]));
    }

    #[test]
    fn a_region_in_another_file_reaches_nothing() {
        let r = regions(export(json!([[10, 1, 12, 2, 1, 1, 0, 0]])));
        assert!(!r.reaches(Path::new("/ws"), &[span(10, 12)]));
        let other = Span {
            file: "src/other.rs".into(),
            ..span(10, 12)
        };
        assert!(r.reaches(Path::new("/ws"), &[other]));
    }

    #[test]
    fn a_member_crate_span_joins_the_workspace_root() {
        let r = regions(json!({
            "data": [{ "functions": [{
                "filenames": ["/ws/pkg/src/lib.rs"],
                "regions": [[4, 1, 6, 2, 1, 0, 0, 0]],
            }] }],
        }));
        let member = Span {
            file: "pkg/src/lib.rs".into(),
            ..span(5, 5)
        };
        assert!(r.reaches(Path::new("/ws"), std::slice::from_ref(&member)));
        assert!(!r.reaches(Path::new("/ws/pkg"), &[member]));
    }

    #[test]
    fn a_symlinked_root_names_the_same_file() {
        let dir = std::env::temp_dir().join(format!("soothfast-regions-{}", std::process::id()));
        let real = dir.join("real");
        std::fs::create_dir_all(real.join("src")).unwrap();
        std::fs::write(real.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        std::os::unix::fs::symlink(&real, dir.join("link")).unwrap();
        let name = real.join("src/lib.rs");
        let r = regions(json!({
            "data": [{ "functions": [{
                "filenames": [name],
                "regions": [[1, 1, 1, 14, 1, 0, 0, 0]],
            }] }],
        }));
        assert!(r.reaches(&dir.join("link"), &[span(1, 1)]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn regions_survive_a_json_round_trip() {
        let mut r = regions(export(json!([[1, 1, 2, 2, 1, 0, 0, 0]])));
        r.add_export(&export(json!([[1, 1, 2, 2, 4, 1, 0, 0]])));
        let back = Regions::from_json(&r.to_json()).unwrap();
        assert_eq!(back.to_json(), r.to_json());
        assert!(back.reaches(Path::new("/ws"), &[span(2, 2)]));
    }

    #[test]
    fn exports_fold_together() {
        let mut r = regions(export(json!([[1, 1, 2, 2, 1, 0, 0, 0]])));
        r.add_export(&export(json!([[30, 1, 31, 2, 1, 0, 0, 0]])));
        assert!(r.reaches(Path::new("/ws"), &[span(1, 2)]));
        assert!(r.reaches(Path::new("/ws"), &[span(31, 40)]));
    }

    #[test]
    fn an_item_is_executed_through_any_of_its_spans() {
        let mut surface = Surface::default();
        surface.items.insert(
            "demo::Meter".into(),
            info("struct", vec![span(5, 5), span(8, 10), span(14, 16)]),
        );
        surface
            .items
            .insert("demo::idle".into(), info("function", vec![span(30, 32)]));
        let r = regions(export(json!([[15, 5, 15, 20, 2, 0, 0, 0]])));
        let declared = declared(&surface);
        let hit = executed(&declared, Path::new("/ws"), &r);
        assert_eq!(hit.into_iter().collect::<Vec<_>>(), ["demo::Meter"]);
    }

    #[test]
    fn re_export_spellings_collapse_to_the_shortest_path() {
        let mut surface = Surface::default();
        let f = info("function", vec![span(1, 3)]);
        surface.items.insert("demo::deep::run".into(), f.clone());
        surface.items.insert("demo::run".into(), f);
        let paths: Vec<String> = declared(&surface).into_iter().map(|d| d.path).collect();
        assert_eq!(paths, ["demo::run"]);
    }

    #[test]
    fn modules_constants_and_spanless_items_are_not_declared() {
        let mut surface = Surface::default();
        surface
            .items
            .insert("demo::m".into(), info("module", vec![span(1, 9)]));
        surface
            .items
            .insert("demo::C".into(), info("constant", vec![span(2, 2)]));
        surface
            .items
            .insert("demo::f".into(), info("function", Vec::new()));
        surface
            .items
            .insert("demo::T".into(), info("struct", vec![span(3, 3)]));
        surface
            .members
            .insert("demo::T::go".into(), info("function", vec![span(4, 6)]));
        let paths: Vec<String> = declared(&surface).into_iter().map(|d| d.path).collect();
        assert_eq!(paths, ["demo::T", "demo::T::go"]);
    }
}
