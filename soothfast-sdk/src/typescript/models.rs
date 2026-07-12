//! Render `models.ts`: one interface per component, aliases for the rest.

use std::fmt::Write;

use crate::SdkOptions;
use crate::model::{Model, Sdk};
use crate::typescript::{HEADER, annotation, doc, member, naming};

pub(crate) fn render(sdk: &Sdk, opts: &SdkOptions) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{HEADER}");
    let _ = writeln!(out, "/** Wire models for {}. */", opts.package);

    if sdk.models.is_empty() && sdk.aliases.is_empty() {
        // A file with no import and no export is a script, not a module.
        let _ = writeln!(out);
        let _ = writeln!(out, "export {{}};");
        return out;
    }

    for model in &sdk.models {
        let _ = writeln!(out);
        render_model(&mut out, model);
    }
    for (name, ty) in &sdk.aliases {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "export type {} = {};",
            naming::type_name(name),
            annotation(ty, "")
        );
    }
    out
}

fn render_model(out: &mut String, model: &Model) {
    if let Some(d) = &model.doc {
        doc(out, "", &[d]);
    }
    let _ = writeln!(
        out,
        "export interface {} {{",
        naming::type_name(&model.name)
    );
    for f in &model.fields {
        if let Some(d) = &f.doc {
            doc(out, "  ", &[d]);
        }
        let _ = writeln!(out, "  {}", member(&f.wire, &f.ty, f.required, ""));
    }
    let _ = writeln!(out, "}}");
}
