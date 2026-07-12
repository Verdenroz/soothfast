//! Render `client.py`: sync and async clients, one thin method per route.

use std::fmt::Write;

use crate::SdkOptions;
use crate::model::{Method, Param, ParamLoc, Sdk, Ty};
use crate::python::{HEADER, annotation, doc_escape, into_expr};

pub(crate) fn render(sdk: &Sdk, opts: &SdkOptions, base_url: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{HEADER}");
    let _ = writeln!(out, "# mypy: disable-error-code=\"no-any-return\"");
    let _ = writeln!(
        out,
        "\"\"\"Sync and async clients for {}.\"\"\"",
        opts.package
    );
    let _ = writeln!(out, "from __future__ import annotations");
    let _ = writeln!(out);
    let _ = writeln!(out, "import typing");
    let _ = writeln!(out);
    let _ = writeln!(out, "from . import models");
    let has_pager = sdk.methods.iter().any(|m| m.paginated);
    let has_body = sdk.methods.iter().any(|m| m.body.is_some());
    let mut runtime: Vec<&str> = vec!["AsyncTransport", "Transport"];
    if has_pager {
        runtime.extend(["AsyncPager", "SyncPager"]);
    }
    if has_body {
        runtime.push("to_wire");
    }
    runtime.push("path_seg");
    runtime.push("query_of");
    runtime.sort_unstable();
    let _ = writeln!(out, "from ._runtime import {}", runtime.join(", "));
    let _ = writeln!(out);
    let _ = writeln!(out, "DEFAULT_BASE_URL = {base_url:?}");

    for async_ in [false, true] {
        let _ = writeln!(out);
        let _ = writeln!(out);
        render_client(&mut out, sdk, opts, async_);
    }
    out
}

fn render_client(out: &mut String, sdk: &Sdk, opts: &SdkOptions, async_: bool) {
    let (class, transport) = if async_ {
        ("AsyncClient", "AsyncTransport")
    } else {
        ("Client", "Transport")
    };
    let _ = writeln!(out, "class {class}:");
    let _ = writeln!(
        out,
        "    \"\"\"{} client for {}.\"\"\"",
        if async_ {
            "Asynchronous"
        } else {
            "Synchronous"
        },
        opts.package,
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "    def __init__(");
    let _ = writeln!(out, "        self,");
    let _ = writeln!(out, "        base_url: str = DEFAULT_BASE_URL,");
    let _ = writeln!(out, "        *,");
    let _ = writeln!(out, "        timeout: float = 30.0,");
    let _ = writeln!(out, "        max_retries: int = 3,");
    let _ = writeln!(
        out,
        "        on_rate_limit: typing.Callable[[typing.Any], None] | None = None,"
    );
    let _ = writeln!(out, "        transport: {transport} | None = None,");
    let _ = writeln!(out, "    ) -> None:");
    let _ = writeln!(out, "        self._transport = transport or {transport}(");
    let _ = writeln!(
        out,
        "            base_url, timeout=timeout, max_retries=max_retries, \
         on_rate_limit=on_rate_limit"
    );
    let _ = writeln!(out, "        )");
    let _ = writeln!(out);
    if async_ {
        let _ = writeln!(out, "    async def aclose(self) -> None:");
        let _ = writeln!(out, "        await self._transport.aclose()");
        let _ = writeln!(out);
        let _ = writeln!(out, "    async def __aenter__(self) -> AsyncClient:");
        let _ = writeln!(out, "        return self");
        let _ = writeln!(out);
        let _ = writeln!(out, "    async def __aexit__(self, *exc: object) -> None:");
        let _ = writeln!(out, "        await self.aclose()");
    } else {
        let _ = writeln!(out, "    def close(self) -> None:");
        let _ = writeln!(out, "        self._transport.close()");
        let _ = writeln!(out);
        let _ = writeln!(out, "    def __enter__(self) -> Client:");
        let _ = writeln!(out, "        return self");
        let _ = writeln!(out);
        let _ = writeln!(out, "    def __exit__(self, *exc: object) -> None:");
        let _ = writeln!(out, "        self.close()");
    }

    for m in &sdk.methods {
        let _ = writeln!(out);
        render_method(out, m, opts, async_);
        if m.paginated {
            let _ = writeln!(out);
            render_iter_method(out, m, opts, async_);
        }
    }
}

fn render_method(out: &mut String, m: &Method, opts: &SdkOptions, async_: bool) {
    let query: Vec<&Param> = query_params(m, opts);
    let def = if async_ { "async def" } else { "def" };
    let ret = match &m.ok {
        Ty::Any => "typing.Any".to_string(),
        other => annotation(other, "models."),
    };

    let _ = writeln!(out, "    {def} {}(", m.name);
    let _ = writeln!(out, "        self,");
    write_args(out, m, &query, None);
    let _ = writeln!(out, "    ) -> {ret}:");
    write_doc(out, m, false);

    let call = request_call(m, &query, "        ", async_);
    let _ = writeln!(out, "        return {call}");
}

fn render_iter_method(out: &mut String, m: &Method, opts: &SdkOptions, async_: bool) {
    let query: Vec<&Param> = query_params(m, opts);
    let pager = if async_ { "AsyncPager" } else { "SyncPager" };
    let item = item_ty(&m.ok);
    let item_ann = match &item {
        Ty::Any => "typing.Any".to_string(),
        other => annotation(other, "models."),
    };

    let _ = writeln!(out, "    def iter_{}(", strip_get(&m.name));
    let _ = writeln!(out, "        self,");
    write_args(out, m, &query, Some(&opts.limit_param));
    let _ = writeln!(out, "    ) -> {pager}[{item_ann}]:");
    write_doc(out, m, true);

    let fetch_def = if async_ { "async def" } else { "def" };
    let _ = writeln!(
        out,
        "        {fetch_def} fetch(cursor: str | None) -> typing.Any:"
    );
    let mut fetch_query: Vec<String> = query
        .iter()
        .map(|p| format!("{:?}: {}", p.wire, p.attr))
        .collect();
    fetch_query.push(format!("{:?}: limit", opts.limit_param));
    fetch_query.push(format!("{:?}: cursor", opts.cursor_param));
    let await_ = if async_ { "await " } else { "" };
    let _ = writeln!(out, "            return {await_}self._transport.request(");
    let _ = writeln!(out, "                {:?},", m.http_method);
    let _ = writeln!(out, "                {},", path_expr(m));
    let _ = writeln!(
        out,
        "                query=query_of({{{}}}),",
        fetch_query.join(", ")
    );
    let _ = writeln!(out, "            )");
    let _ = writeln!(out);
    let _ = writeln!(out, "        return {pager}(fetch, {})", value_expr(&item));
}

/// Query parameters a plain method exposes: everything except the
/// pagination pair, which only the `iter_` variant may send.
fn query_params<'m>(m: &'m Method, opts: &SdkOptions) -> Vec<&'m Param> {
    m.params
        .iter()
        .filter(|p| p.location == ParamLoc::Query)
        .filter(|p| !m.paginated || (p.wire != opts.limit_param && p.wire != opts.cursor_param))
        .collect()
}

fn write_args(out: &mut String, m: &Method, query: &[&Param], limit_param: Option<&str>) {
    for p in path_params(m) {
        let _ = writeln!(out, "        {}: {},", p.attr, annotation(&p.ty, "models."));
    }
    if let Some(body) = &m.body {
        let _ = writeln!(out, "        body: {},", annotation(body, "models."));
    }
    if !query.is_empty() || limit_param.is_some() {
        let _ = writeln!(out, "        *,");
    }
    for p in query {
        let ann = annotation(&p.ty, "models.");
        if p.required {
            let _ = writeln!(out, "        {}: {ann},", p.attr);
        } else {
            let ann = match &p.ty {
                Ty::Optional(_) => ann,
                _ => format!("{ann} | None"),
            };
            let _ = writeln!(out, "        {}: {ann} = None,", p.attr);
        }
    }
    if limit_param.is_some() {
        let _ = writeln!(out, "        limit: int = 50,");
    }
}

fn write_doc(out: &mut String, m: &Method, iter: bool) {
    let mut lines = Vec::new();
    if let Some(doc) = &m.doc {
        lines.push(doc_escape(doc));
    }
    if iter {
        lines.push("Auto-paginates; iterate items or call `.pages()`.".into());
    }
    match lines.len() {
        0 => {}
        1 => {
            let _ = writeln!(out, "        \"\"\"{}\"\"\"", lines[0]);
        }
        _ => {
            let _ = writeln!(out, "        \"\"\"{}", lines[0]);
            let _ = writeln!(out);
            let _ = writeln!(out, "        {}", lines[1]);
            let _ = writeln!(out, "        \"\"\"");
        }
    }
}

fn request_call(m: &Method, query: &[&Param], indent: &str, async_: bool) -> String {
    let mut call = String::new();
    let await_ = if async_ { "await " } else { "" };
    let _ = writeln!(call, "{await_}self._transport.request(");
    let _ = writeln!(call, "{indent}    {:?},", m.http_method);
    let _ = writeln!(call, "{indent}    {},", path_expr(m));
    if !query.is_empty() {
        let pairs: Vec<String> = query
            .iter()
            .map(|p| format!("{:?}: {}", p.wire, p.attr))
            .collect();
        let _ = writeln!(
            call,
            "{indent}    query=query_of({{{}}}),",
            pairs.join(", ")
        );
    }
    if m.body.is_some() {
        let _ = writeln!(call, "{indent}    json_body=to_wire(body),");
    }
    let into = into_expr(&m.ok);
    if into != "None" {
        let _ = writeln!(call, "{indent}    into={into},");
    }
    let _ = write!(call, "{indent})");
    call
}

fn path_params(m: &Method) -> Vec<&Param> {
    let order = placeholders(&m.path);
    let mut params: Vec<&Param> = m
        .params
        .iter()
        .filter(|p| p.location == ParamLoc::Path)
        .collect();
    params.sort_by_key(|p| {
        order
            .iter()
            .position(|n| *n == p.wire)
            .unwrap_or(usize::MAX)
    });
    params
}

fn path_expr(m: &Method) -> String {
    let names = placeholders(&m.path);
    if names.is_empty() {
        return format!("{:?}", m.path);
    }
    let mut expr = m.path.clone();
    for p in m.params.iter().filter(|p| p.location == ParamLoc::Path) {
        expr = expr.replace(
            &format!("{{{}}}", p.wire),
            &format!("{{path_seg({})}}", p.attr),
        );
    }
    format!("f{expr:?}")
}

fn placeholders(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        out.push(rest[open + 1..open + close].to_string());
        rest = &rest[open + close + 1..];
    }
    out
}

fn item_ty(ok: &Ty) -> Ty {
    match ok {
        Ty::List(t) => (**t).clone(),
        _ => Ty::Any,
    }
}

fn value_expr(ty: &Ty) -> String {
    match ty {
        Ty::Any => "typing.Any".into(),
        other => annotation(other, "models."),
    }
}

fn strip_get(name: &str) -> &str {
    name.strip_prefix("get_").unwrap_or(name)
}
