//! Render `models.py`: frozen dataclasses plus named aliases.

use std::fmt::Write;

use crate::SdkOptions;
use crate::model::{Model, Sdk, Ty};
use crate::python::{HEADER, annotation, doc_escape};

pub(crate) fn render(sdk: &Sdk, opts: &SdkOptions) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{HEADER}");
    let _ = writeln!(out, "\"\"\"Wire models for {}.\"\"\"", opts.package);
    let _ = writeln!(out, "from __future__ import annotations");
    let _ = writeln!(out);
    let _ = writeln!(out, "import dataclasses");
    let _ = writeln!(out, "import typing");

    for model in &sdk.models {
        let _ = writeln!(out);
        let _ = writeln!(out);
        render_model(&mut out, model);
    }

    if !sdk.aliases.is_empty() {
        let _ = writeln!(out);
        for (name, ty) in &sdk.aliases {
            let _ = writeln!(out);
            let _ = writeln!(out, "{name} = {}", annotation(ty, ""));
        }
    }
    out
}

fn render_model(out: &mut String, model: &Model) {
    let _ = writeln!(out, "@dataclasses.dataclass(frozen=True)");
    let _ = writeln!(out, "class {}:", model.name);
    if let Some(doc) = &model.doc {
        let _ = writeln!(out, "    \"\"\"{}\"\"\"", doc_escape(doc));
        let _ = writeln!(out);
    }
    if model.fields.is_empty() {
        let _ = writeln!(out, "    pass");
        return;
    }

    let (required, optional): (Vec<_>, Vec<_>) = model.fields.iter().partition(|f| f.required);
    for f in &required {
        let _ = writeln!(out, "    {}: {}", f.attr, annotation(&f.ty, ""));
    }
    for f in &optional {
        let ann = match &f.ty {
            Ty::Optional(_) => annotation(&f.ty, ""),
            other => format!("{} | None", annotation(other, "")),
        };
        let _ = writeln!(out, "    {}: {ann} = None", f.attr);
    }

    let renamed: Vec<_> = model.fields.iter().filter(|f| f.attr != f.wire).collect();
    if !renamed.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "    _WIRE: typing.ClassVar[dict[str, str]] = {{");
        for f in renamed {
            let _ = writeln!(out, "        {:?}: {:?},", f.attr, f.wire);
        }
        let _ = writeln!(out, "    }}");
    }
}
