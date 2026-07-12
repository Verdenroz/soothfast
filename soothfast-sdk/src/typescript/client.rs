//! Render `client.ts`: one options interface and one thin method per route.

use std::fmt::Write;

use crate::SdkOptions;
use crate::model::{Method, Param, ParamLoc, Sdk, Ty};
use crate::typescript::{HEADER, annotation, doc, member, naming};

pub(crate) fn render(sdk: &Sdk, opts: &SdkOptions, base_url: Option<&str>) -> String {
    let has_models = !sdk.models.is_empty() || !sdk.aliases.is_empty();
    let has_pager = sdk.methods.iter().any(|m| m.paginated);
    let has_path_params = sdk
        .methods
        .iter()
        .any(|m| m.params.iter().any(|p| p.location == ParamLoc::Path));
    let has_query = sdk
        .methods
        .iter()
        .any(|m| !query_params(m, opts).is_empty())
        || has_pager;

    let mut imports: Vec<String> = vec!["Transport".into(), "type ClientOptions".into()];
    if has_pager {
        imports.push("AsyncPager".into());
    }
    if has_path_params {
        imports.push("pathSeg".into());
    }
    if has_query {
        imports.push("queryOf".into());
    }
    if opts.embed.is_some() {
        imports.push("type BaseUrl".into());
    }
    imports.sort();

    let mut out = String::new();
    let _ = writeln!(out, "{HEADER}");
    let _ = writeln!(out, "/** Client for {}. */", opts.package);
    let _ = writeln!(out);
    if has_models {
        let _ = writeln!(out, "import type * as models from \"./models.js\";");
    }
    let _ = writeln!(
        out,
        "import {{ {} }} from \"./runtime.js\";",
        imports.join(", ")
    );
    if opts.embed.is_some() {
        let _ = writeln!(
            out,
            "import {{ embeddedBaseUrl, type EmbedConfig }} from \"./server.js\";"
        );
    }
    if let Some(url) = base_url {
        let _ = writeln!(out);
        doc(
            &mut out,
            "",
            &[if opts.embed.is_some() {
                "The hosted deployment. Pass it explicitly to talk to that \
                 instead of the bundled server."
            } else {
                "Where this client talks by default."
            }],
        );
        let _ = writeln!(
            out,
            "export const DEFAULT_BASE_URL = {};",
            naming::string_lit(url)
        );
    }
    if let Some(binary) = &opts.embed {
        render_embed_config(&mut out, opts, binary);
        render_server_env(&mut out, opts);
        render_embedded_options(&mut out);
    }

    for m in &sdk.methods {
        render_options(&mut out, m, opts);
    }

    let _ = writeln!(out);
    render_client(&mut out, sdk, opts, base_url);
    out
}

/// The bundled server's identity, handed to the generic launcher.
fn render_embed_config(out: &mut String, opts: &SdkOptions, binary: &str) {
    let prefix = crate::naming::env_prefix(&opts.package);
    let args: Vec<String> = opts
        .embed_args
        .iter()
        .map(|a| naming::string_lit(a))
        .collect();
    let _ = writeln!(out);
    doc(
        out,
        "",
        &[
            "The server this package bundles.",
            &format!(
                "Set `{prefix}_BASE_URL` to talk to an already-running \
                 instance instead of spawning one, or `{prefix}_SERVER_BIN` \
                 to point at a different binary."
            ),
        ],
    );
    let _ = writeln!(out, "export const EMBED: EmbedConfig = {{");
    let _ = writeln!(out, "  binary: {},", naming::string_lit(binary));
    let _ = writeln!(out, "  args: [{}],", args.join(", "));
    let _ = writeln!(
        out,
        "  baseUrlEnv: {},",
        naming::string_lit(&format!("{prefix}_BASE_URL"))
    );
    let _ = writeln!(
        out,
        "  binEnv: {},",
        naming::string_lit(&format!("{prefix}_SERVER_BIN"))
    );
    let _ = writeln!(
        out,
        "  packagePrefix: {},",
        naming::string_lit(&opts.package)
    );
    if !opts.embed_env.is_empty() {
        let _ = writeln!(out, "  env: {{");
        for (name, value) in &opts.embed_env {
            let _ = writeln!(
                out,
                "    {}: {},",
                naming::string_lit(name),
                naming::string_lit(value)
            );
        }
        let _ = writeln!(out, "  }},");
    }
    let _ = writeln!(out, "}};");
}

/// The knobs the bundled server documents, keyed by the very names it
/// reads — the client configures it the way a deployment does. The index
/// signature keeps anything undocumented reachable without an escape
/// hatch of its own.
fn render_server_env(out: &mut String, opts: &SdkOptions) {
    let _ = writeln!(out);
    doc(
        out,
        "",
        &[
            "Environment for the bundled server.",
            "Every key is optional, and the names are the server's own — the \
             same ones a deployment sets.",
        ],
    );
    let _ = writeln!(out, "export interface ServerEnv {{");
    for var in &opts.embed_env_vars {
        let mut lines: Vec<String> = var.doc.clone();
        match &var.default {
            Some(value) if !value.is_empty() => lines.push(format!("Defaults to `{value}`.")),
            _ => lines.push("Unset unless you set it.".into()),
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        doc(out, "  ", &refs);
        let _ = writeln!(out, "  {}?: string;", var.name);
    }
    if !opts.embed_env_vars.is_empty() {
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "  [key: string]: string | undefined;");
    let _ = writeln!(out, "}}");
}

/// Client options carry the server's environment too, so configuring the
/// bundled server is one object at the call site rather than a second
/// argument that means nothing to a hosted client.
fn render_embedded_options(out: &mut String) {
    let _ = writeln!(out);
    doc(
        out,
        "",
        &["Client options, plus the environment for the bundled server."],
    );
    let _ = writeln!(
        out,
        "export interface EmbeddedClientOptions extends ClientOptions {{"
    );
    doc(
        out,
        "  ",
        &[
            "Applied when this client starts the bundled server. Ignored \
             when a base URL is passed, or when the environment already \
             points at a running instance — there is no server of ours to \
             configure.",
        ],
    );
    let _ = writeln!(out, "  serverEnv?: ServerEnv;");
    let _ = writeln!(out, "}}");
}

fn render_client(out: &mut String, sdk: &Sdk, opts: &SdkOptions, base_url: Option<&str>) {
    doc(
        out,
        "",
        &[&format!("Generated client for {}.", opts.package)],
    );
    let _ = writeln!(out, "export class Client {{");
    let _ = writeln!(out, "  private readonly transport: Transport;");
    let _ = writeln!(out);
    if opts.embed.is_some() {
        let mut lines = vec![
            "Constructed with no base URL, the client starts the bundled \
             server on its first request and talks to that — nothing to \
             deploy, nothing to configure."
                .to_string(),
        ];
        if base_url.is_some() {
            lines.push("Pass `DEFAULT_BASE_URL` to use the hosted deployment instead.".to_string());
        }
        if !opts.embed_env_vars.is_empty() {
            lines.push(
                "`options.serverEnv` configures that server — see \
                 `ServerEnv` for the knobs it reads."
                    .to_string(),
            );
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        doc(out, "  ", &refs);
        let _ = writeln!(
            out,
            "  constructor(baseUrl?: BaseUrl, options: EmbeddedClientOptions = {{}}) {{"
        );
        let _ = writeln!(
            out,
            "    const target = baseUrl ?? embeddedBaseUrl(EMBED, options.serverEnv);"
        );
        let _ = writeln!(
            out,
            "    this.transport = options.transport ?? new Transport(target, options);"
        );
    } else {
        let _ = writeln!(
            out,
            "  constructor(baseUrl: string = DEFAULT_BASE_URL, options: ClientOptions = {{}}) {{"
        );
        let _ = writeln!(
            out,
            "    this.transport = options.transport ?? new Transport(baseUrl, options);"
        );
    }
    let _ = writeln!(out, "  }}");

    for m in &sdk.methods {
        let _ = writeln!(out);
        render_method(out, m, opts);
        if m.paginated {
            let _ = writeln!(out);
            render_iter_method(out, m, opts);
        }
    }
    let _ = writeln!(out, "}}");
}

/// The options interfaces a method needs: its query parameters, plus the
/// page-size variant when it paginates.
fn render_options(out: &mut String, m: &Method, opts: &SdkOptions) {
    let query = query_params(m, opts);
    if !query.is_empty() {
        let _ = writeln!(out);
        doc(
            out,
            "",
            &[&format!(
                "Query parameters for {{@link Client.{}}}.",
                m.name
            )],
        );
        let _ = writeln!(out, "export interface {} {{", options_name(m));
        for p in &query {
            if let Some(d) = &p.doc {
                doc(out, "  ", &[d]);
            }
            let _ = writeln!(out, "  {}", member(&p.wire, &p.ty, p.required, "models."));
        }
        let _ = writeln!(out, "}}");
    }
    if !m.paginated {
        return;
    }

    let _ = writeln!(out);
    doc(
        out,
        "",
        &[&format!(
            "Query parameters for {{@link Client.{}}}.",
            iter_name(m)
        )],
    );
    let extends = if query.is_empty() {
        String::new()
    } else {
        format!(" extends {}", options_name(m))
    };
    let _ = writeln!(out, "export interface {}{extends} {{", iter_options_name(m));
    doc(out, "  ", &["Items requested per page. Defaults to 50."]);
    let _ = writeln!(
        out,
        "  {}",
        member(&opts.limit_param, &limit_ty(m, opts), false, "models.")
    );
    let _ = writeln!(out, "}}");
}

fn render_method(out: &mut String, m: &Method, opts: &SdkOptions) {
    let query = query_params(m, opts);
    let ret = annotation(&m.ok, "models.");
    if let Some(d) = &m.doc {
        doc(out, "  ", &[d]);
    }
    let _ = writeln!(
        out,
        "  {}({}): Promise<{ret}> {{",
        m.name,
        args(m, &query, None).join(", ")
    );

    let mut init: Vec<String> = Vec::new();
    if !query.is_empty() {
        init.push("      query: queryOf(options),".into());
    }
    if m.body.is_some() {
        init.push("      body,".into());
    }
    let call = format!(
        "this.transport.request<{ret}>({}, {}",
        naming::string_lit(&m.http_method),
        path_expr(m)
    );
    if init.is_empty() {
        let _ = writeln!(out, "    return {call});");
    } else {
        let _ = writeln!(out, "    return {call}, {{");
        for line in init {
            let _ = writeln!(out, "{line}");
        }
        let _ = writeln!(out, "    }});");
    }
    let _ = writeln!(out, "  }}");
}

fn render_iter_method(out: &mut String, m: &Method, opts: &SdkOptions) {
    let query = query_params(m, opts);
    let item = annotation(&item_ty(&m.ok), "models.");
    let mut lines: Vec<&str> = Vec::new();
    if let Some(d) = &m.doc {
        lines.push(d);
    }
    lines.push("Auto-paginates; iterate items, or call `.pages()` for whole pages.");
    doc(out, "  ", &lines);

    let _ = writeln!(
        out,
        "  {}({}): AsyncPager<{item}> {{",
        iter_name(m),
        args(m, &query, Some(&iter_options_name(m))).join(", ")
    );
    let _ = writeln!(
        out,
        "    const {{ {}: pageLimit = 50, ...rest }} = options;",
        naming::prop_key(&opts.limit_param)
    );
    let _ = writeln!(out, "    return new AsyncPager<{item}>((cursor) =>");
    let _ = writeln!(
        out,
        "      this.transport.request({}, {}, {{",
        naming::string_lit(&m.http_method),
        path_expr(m)
    );
    let _ = writeln!(
        out,
        "        query: queryOf({{ ...rest, {}: pageLimit, {}: cursor }}),",
        naming::prop_key(&opts.limit_param),
        naming::prop_key(&opts.cursor_param),
    );
    let _ = writeln!(out, "      }}),");
    let _ = writeln!(out, "    );");
    let _ = writeln!(out, "  }}");
}

/// Path parameters positionally, then the body, then one options object
/// carrying every query parameter.
fn args(m: &Method, query: &[&Param], iter_options: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = path_params(m)
        .iter()
        .map(|p| {
            format!(
                "{}: {}",
                naming::ident(&p.attr),
                annotation(&p.ty, "models.")
            )
        })
        .collect();
    if let Some(body) = &m.body {
        out.push(format!("body: {}", annotation(body, "models.")));
    }
    match iter_options {
        // A defaulted page size does not make the object optional: one
        // required filter and `{}` stops being a legal value for it.
        Some(name) if query.iter().any(|p| p.required) => out.push(format!("options: {name}")),
        Some(name) => out.push(format!("options: {name} = {{}}")),
        None if query.is_empty() => {}
        None if query.iter().any(|p| p.required) => {
            out.push(format!("options: {}", options_name(m)))
        }
        None => out.push(format!("options: {} = {{}}", options_name(m))),
    }
    out
}

/// Query parameters a plain method exposes: everything except the
/// pagination pair, which only the `iter` variant may send.
fn query_params<'m>(m: &'m Method, opts: &SdkOptions) -> Vec<&'m Param> {
    m.params
        .iter()
        .filter(|p| p.location == ParamLoc::Query)
        .filter(|p| !m.paginated || (p.wire != opts.limit_param && p.wire != opts.cursor_param))
        .collect()
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

/// A quoted path, or a template literal when there are segments to fill.
fn path_expr(m: &Method) -> String {
    if placeholders(&m.path).is_empty() {
        return naming::string_lit(&m.path);
    }
    let mut expr = m.path.replace('`', "\\`").replace("${", "\\${");
    for p in m.params.iter().filter(|p| p.location == ParamLoc::Path) {
        expr = expr.replace(
            &format!("{{{}}}", p.wire),
            &format!("${{pathSeg({})}}", naming::ident(&p.attr)),
        );
    }
    format!("`{expr}`")
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

/// The declared type of the page-size parameter, when the route has one.
fn limit_ty(m: &Method, opts: &SdkOptions) -> Ty {
    m.params
        .iter()
        .find(|p| p.wire == opts.limit_param && p.location == ParamLoc::Query)
        .map(|p| p.ty.clone())
        .unwrap_or(Ty::Int)
}

fn options_name(m: &Method) -> String {
    format!("{}Options", naming::pascal(&m.name))
}

fn iter_options_name(m: &Method) -> String {
    format!("{}IterOptions", naming::pascal(&m.name))
}

/// `getQuotes` → `iterQuotes`, `listItems` → `iterListItems`.
fn iter_name(m: &Method) -> String {
    let stem = m
        .name
        .strip_prefix("get")
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
        .unwrap_or(&m.name);
    format!("iter{}", naming::pascal(stem))
}
