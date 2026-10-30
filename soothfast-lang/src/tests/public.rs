use super::*;
use crate::public_symbols;

fn surface(lang: Lang, source: &str) -> Vec<(String, bool)> {
    public_symbols(lang, source)
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.documented))
        .collect()
}

fn expect(pairs: &[(&str, bool)]) -> Vec<(String, bool)> {
    pairs.iter().map(|(n, d)| (n.to_string(), *d)).collect()
}

#[test]
fn kotlin_is_public_unless_restricted() {
    let src = r#"/** Doc. */
class Open {
    fun shown() {}
    private fun hidden() {}
    internal fun module() {}
    protected fun inherited() {}
    /** Doc. */
    public fun explicit() {}
}

internal class Hidden {
    fun inside() {}
}

private fun top() {}
"#;
    assert_eq!(
        surface(Lang::Kotlin, src),
        expect(&[
            ("Open", true),
            ("Open.shown", false),
            ("Open.explicit", true)
        ])
    );
}

#[test]
fn java_needs_public_except_in_interfaces() {
    let src = r#"/** Doc. */
public class Api {
    /** Doc. */
    public void run() {}
    void packagePrivate() {}
    private void hidden() {}
    protected void inherited() {}
}

class Local {
    public void inside() {}
}

public interface Shape {
    int area();
}
"#;
    assert_eq!(
        surface(Lang::Java, src),
        expect(&[
            ("Api", true),
            ("Api.run", true),
            ("Shape", false),
            ("Shape.area", false)
        ])
    );
}

#[test]
fn python_skips_underscore_names_and_reads_docstrings() {
    let src = r#"def shown():
    """Doc."""

def _hidden():
    pass

class Api:
    def run(self):
        pass

    def _helper(self):
        pass

class _Internal:
    def run(self):
        pass
"#;
    assert_eq!(
        surface(Lang::Python, src),
        expect(&[("shown", true), ("Api", false), ("Api.run", false)])
    );
}

#[test]
fn typescript_needs_export_and_hides_restricted_members() {
    let src = r#"/** Doc. */
export function add() {}
function local() {}
export const arrow = () => 1;
const hidden = () => 1;
export class Api {
    run() {}
    private secret() {}
    protected inherited() {}
    #own() {}
}
class Local {
    run() {}
}
"#;
    assert_eq!(
        surface(Lang::TypeScript, src),
        expect(&[
            ("add", true),
            ("arrow", false),
            ("Api", false),
            ("Api.run", false)
        ])
    );
}

#[test]
fn export_lists_and_default_export_make_local_names_public() {
    let src = "function a() {}\nfunction b() {}\nfunction c() {}\nfunction d() {}\nexport { a, b as renamed };\nexport default c;\nexport { d } from './other';\n";
    assert_eq!(
        surface(Lang::TypeScript, src),
        expect(&[("a", false), ("b", false), ("c", false)])
    );
    assert_eq!(
        surface(Lang::JavaScript, "const f = () => 1;\nexport { f };\n"),
        expect(&[("f", false)])
    );
}

#[test]
fn javascript_exports_are_public() {
    let src = "export function a() {}\nfunction b() {}\n";
    assert_eq!(surface(Lang::JavaScript, src), expect(&[("a", false)]));
}
