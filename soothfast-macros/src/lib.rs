//! Proc-macros for soothfast.
//!
//! `#[measured]`, `#[bench]`, `#[fixture]`, and `#[mock_seam]` register the
//! annotated function into the soothfast registry (see `soothfast-registry`)
//! without altering the function itself. `#[measured]`/`#[bench]` also
//! accept checked performance claims: `alloc = N`, `p99 = "1ms"`, and
//! (bench only) `complexity = "n log n"` with `setup_sized`/`sizes(...)`.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};

const COMPLEXITY_CLASSES: &[&str] = &["1", "log n", "n", "n log n", "n^2"];
const DEFAULT_SIZES: &[usize] = &[1024, 4096, 16384];

#[derive(Default)]
struct Attrs {
    group: Option<String>,
    setup: Option<syn::Path>,
    setup_sized: Option<syn::Path>,
    covers: String,
    max_allocs: Option<u64>,
    p99_ns: Option<u64>,
    complexity: Option<String>,
    sizes: Vec<usize>,
    tolerance_pct: Option<f64>,
}

fn parse_attrs(attr: TokenStream, allowed: &[&str]) -> Result<Attrs, syn::Error> {
    let mut out = Attrs::default();
    let parser = syn::meta::parser(|meta| {
        let key = meta
            .path
            .get_ident()
            .map(ToString::to_string)
            .unwrap_or_default();
        if !allowed.contains(&key.as_str()) {
            return Err(meta.error(format!(
                "unsupported key `{key}`; expected one of: {}",
                allowed.join(", ")
            )));
        }
        match key.as_str() {
            "group" => out.group = Some(meta.value()?.parse::<syn::LitStr>()?.value()),
            "setup" => out.setup = Some(meta.value()?.parse()?),
            "setup_sized" => out.setup_sized = Some(meta.value()?.parse()?),
            "covers" => out.covers = meta.value()?.parse::<syn::LitStr>()?.value(),
            "alloc" => out.max_allocs = Some(meta.value()?.parse::<syn::LitInt>()?.base10_parse()?),
            "tolerance" => {
                let lit = meta.value()?.parse::<syn::LitStr>()?;
                out.tolerance_pct =
                    Some(parse_percent(&lit.value()).map_err(|e| syn::Error::new(lit.span(), e))?);
            }
            "p99" => {
                let lit = meta.value()?.parse::<syn::LitStr>()?;
                out.p99_ns = Some(
                    parse_duration_ns(&lit.value()).map_err(|e| syn::Error::new(lit.span(), e))?,
                );
            }
            "complexity" => {
                let lit = meta.value()?.parse::<syn::LitStr>()?;
                let class = lit.value();
                if !COMPLEXITY_CLASSES.contains(&class.as_str()) {
                    return Err(syn::Error::new(
                        lit.span(),
                        format!("unknown complexity class; expected one of {COMPLEXITY_CLASSES:?}"),
                    ));
                }
                out.complexity = Some(class);
            }
            "sizes" => {
                let content;
                syn::parenthesized!(content in meta.input);
                let list: syn::punctuated::Punctuated<syn::LitInt, syn::Token![,]> = content
                    .parse_terminated(<syn::LitInt as syn::parse::Parse>::parse, syn::Token![,])?;
                out.sizes = list
                    .iter()
                    .map(syn::LitInt::base10_parse)
                    .collect::<Result<_, _>>()?;
            }
            _ => unreachable!(),
        }
        Ok(())
    });
    syn::parse::Parser::parse(parser, attr).map(|()| out)
}

/// Parse `"250ns" | "10us" | "1ms" | "2s"` into nanoseconds.
fn parse_duration_ns(s: &str) -> Result<u64, String> {
    let split = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or("missing unit (ns/us/ms/s)")?;
    let (num, unit) = s.split_at(split);
    let value: u64 = num.parse().map_err(|_| "invalid number".to_string())?;
    let factor = match unit {
        "ns" => 1,
        "us" | "µs" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        other => return Err(format!("unknown duration unit {other:?}")),
    };
    Ok(value * factor)
}

/// Parse `"8%"` into a percentage.
fn parse_percent(s: &str) -> Result<f64, String> {
    let num = s.strip_suffix('%').ok_or("missing `%`")?;
    let value: f64 = num
        .trim()
        .parse()
        .map_err(|_| "invalid number".to_string())?;
    if value <= 0.0 {
        return Err("tolerance must be positive".into());
    }
    Ok(value)
}

/// Register a zero-argument function as a directly measured item.
///
/// ```ignore
/// #[soothfast::measured(group = "checksums", alloc = 0, p99 = "100us")]
/// pub fn lcg_checksum() -> u64 { ... }
/// ```
///
/// Functions with parameters need `#[soothfast::bench(setup = ...)]` instead.
#[proc_macro_attribute]
pub fn measured(attr: TokenStream, item: TokenStream) -> TokenStream {
    // The raw attr tokens join the fingerprint: threshold/claim edits are
    // meaningful changes even though they sit outside the fn tokens.
    let attr_str = attr.to_string();
    let attrs = match parse_attrs(attr, &["group", "alloc", "p99", "covers"]) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let func = syn::parse_macro_input!(item as syn::ItemFn);
    if !func.sig.inputs.is_empty() {
        return syn::Error::new(
            func.sig.ident.span(),
            "#[measured] requires a zero-argument function; \
             use #[soothfast::bench(setup = ...)] for functions with inputs",
        )
        .to_compile_error()
        .into();
    }
    let name = &func.sig.ident;
    let glue = if func.sig.asyncness.is_some() {
        quote! { __b.iter_async(|| #name()); }
    } else {
        quote! { __b.iter(|| #name()); }
    };
    registration(&func, &attrs, &attr_str, glue, None)
}

/// Register a bench harness function, optionally fed by a setup fn.
///
/// ```ignore
/// #[soothfast::bench(group = "sort", setup_sized = values_n, sizes(1024, 4096, 16384),
///                  complexity = "n log n", alloc = 2, covers = "mylib::sorted")]
/// fn bench_sorted(input: &[f64]) { soothfast::keep(mylib::sorted(input)); }
/// ```
///
/// `setup = f` feeds `f() -> T` as `&T`; `setup_sized = f` feeds `f(n) -> T`
/// and enables `sizes(...)`/`complexity` sweeps (the plain measurement uses
/// the largest size). Setup always runs outside the measured region.
/// Route constant inputs through `soothfast::keep` or LLVM const-folds them.
#[proc_macro_attribute]
pub fn bench(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_str = attr.to_string();
    let mut attrs = match parse_attrs(
        attr,
        &[
            "group",
            "setup",
            "setup_sized",
            "covers",
            "alloc",
            "p99",
            "complexity",
            "sizes",
            "tolerance",
        ],
    ) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let func = syn::parse_macro_input!(item as syn::ItemFn);
    let name = &func.sig.ident;
    let span = name.span();

    if attrs.setup.is_some() && attrs.setup_sized.is_some() {
        return err(span, "#[bench] takes `setup` or `setup_sized`, not both");
    }
    if attrs.complexity.is_some() && attrs.setup_sized.is_none() {
        return err(span, "`complexity` needs `setup_sized = <fn(usize) -> T>`");
    }
    if attrs.sizes.is_empty() {
        attrs.sizes = DEFAULT_SIZES.to_vec();
    }
    // A sweep needs two points to have a slope; catch it here rather than
    // let the runner panic mid-measurement.
    if attrs.complexity.is_some() && attrs.sizes.len() < 2 {
        return err(
            span,
            "`complexity` needs `sizes(...)` with at least 2 sizes",
        );
    }

    // Async bench fns drive the counting executor via iter_async.
    let iter_fn = if func.sig.asyncness.is_some() {
        quote!(iter_async)
    } else {
        quote!(iter)
    };
    let arity = func.sig.inputs.len();
    match (&attrs.setup, &attrs.setup_sized, arity) {
        (Some(setup), None, 1) => {
            let glue = quote! {
                let __input = #setup();
                __b.#iter_fn(|| #name(&__input));
            };
            registration(&func, &attrs, &attr_str, glue, None)
        }
        (None, Some(setup_sized), 1) => {
            let largest = *attrs.sizes.iter().max().expect("sizes non-empty");
            let glue_sized_ident = format_ident!("__soothfast_glue_sized_{}", name);
            let sized = quote! {
                #[doc(hidden)]
                fn #glue_sized_ident(__b: &mut ::soothfast::__private::Bencher, __n: usize) {
                    let __input = #setup_sized(__n);
                    __b.#iter_fn(|| #name(&__input));
                }
            };
            let glue = quote! { #glue_sized_ident(__b, #largest); };
            registration(
                &func,
                &attrs,
                &attr_str,
                glue,
                Some((sized, glue_sized_ident)),
            )
        }
        (None, None, 0) => {
            let glue = quote! { __b.#iter_fn(|| #name()); };
            registration(&func, &attrs, &attr_str, glue, None)
        }
        (Some(_), None, n) => err(
            span,
            &format!("#[bench] with `setup` must take exactly one `&T`, found {n} args"),
        ),
        (None, Some(_), n) => err(
            span,
            &format!("#[bench] with `setup_sized` must take exactly one `&T`, found {n} args"),
        ),
        (None, None, _) => err(
            span,
            "#[bench] function has arguments but no `setup`; add `setup = <fn returning T>`",
        ),
        (Some(_), Some(_), _) => unreachable!("rejected above"),
    }
}

fn err(span: Span, msg: &str) -> TokenStream {
    syn::Error::new(span, msg).to_compile_error().into()
}

/// Declare the spec operation a handler implements — the code-side half of
/// `cargo soothfast spec check`. Stackable for handlers serving several
/// surfaces (REST + GraphQL + MCP).
///
/// Spec generation infers the request and response shapes from the handler's
/// own signature; `request`, `response`, `status`, `params` and `path_params`
/// override that for what inference provably cannot see — erased returns, and
/// detached markers whose empty signature says nothing at all.
///
/// ```ignore
/// #[soothfast::route(spec = "specs/openapi.yaml", operation = "getItem",
///                  method = "GET", path = "/items/{id}")]
/// pub fn get_item(id: u64) -> String { ... }
///
/// // `impl IntoResponse` erases the type; name it explicitly.
/// #[soothfast::route(spec = "specs/openapi.yaml", operation = "createItem",
///                  method = "POST", path = "/items",
///                  response = "Item", status = 201)]
/// pub async fn create_item(body: Json<NewItem>) -> impl IntoResponse { ... }
///
/// // A detached marker states its whole contract. `path_params` types the
/// // `{placeholder}`s that would otherwise be bare strings.
/// #[soothfast::route(spec = "specs/openapi.yaml", operation = "listBySector",
///                  method = "GET", path = "/sectors/{sector}",
///                  params = "SectorQuery", path_params = "sector: Sector",
///                  response = "[Item]")]
/// fn route_list_by_sector() {}
/// ```
#[proc_macro_attribute]
pub fn route(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut spec = String::new();
    let mut operation = String::new();
    let mut method = String::new();
    let mut path = String::new();
    let mut request: Option<String> = None;
    let mut response: Option<String> = None;
    let mut status: Option<u16> = None;
    let mut params: Option<String> = None;
    let mut path_params: Option<String> = None;
    let parser = syn::meta::parser(|meta| {
        let lit = |m: &syn::meta::ParseNestedMeta| -> syn::Result<String> {
            Ok(m.value()?.parse::<syn::LitStr>()?.value())
        };
        if meta.path.is_ident("spec") {
            spec = lit(&meta)?;
        } else if meta.path.is_ident("operation") {
            operation = lit(&meta)?;
        } else if meta.path.is_ident("method") {
            method = lit(&meta)?;
        } else if meta.path.is_ident("path") {
            path = lit(&meta)?;
        } else if meta.path.is_ident("request") {
            request = Some(lit(&meta)?);
        } else if meta.path.is_ident("response") {
            response = Some(lit(&meta)?);
        } else if meta.path.is_ident("status") {
            status = Some(
                meta.value()?
                    .parse::<syn::LitInt>()?
                    .base10_parse::<u16>()?,
            );
        } else if meta.path.is_ident("params") {
            params = Some(lit(&meta)?);
        } else if meta.path.is_ident("path_params") {
            path_params = Some(lit(&meta)?);
        } else {
            return Err(meta.error(
                "expected `spec`, `operation`, `method`, `path`, `request`, \
                 `response`, `status`, `params`, or `path_params`",
            ));
        }
        Ok(())
    });
    syn::parse_macro_input!(attr with parser);
    let func = syn::parse_macro_input!(item as syn::ItemFn);
    let name = &func.sig.ident;
    if spec.is_empty() || operation.is_empty() {
        return err(
            name.span(),
            "#[route] requires `spec = \"...\"` and `operation = \"...\"`",
        );
    }
    // Operation in the ident so stacked #[route] attrs don't collide.
    let op_san: String = operation
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let static_ident = format_ident!("__SOOTHFAST_ROUTE_{}_{}", name, op_san);
    let opt_str = |v: &Option<String>| match v {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    };
    let request_tok = opt_str(&request);
    let response_tok = opt_str(&response);
    let status_tok = match status {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    };
    let params_tok = opt_str(&params);
    let path_params_tok = opt_str(&path_params);
    let summary_tok = opt_str(&doc_summary(&func.attrs));
    quote! {
        #func

        #[cfg(not(target_arch = "wasm32"))]
        #[::soothfast::__private::linkme::distributed_slice(::soothfast::__private::ROUTES)]
        #[linkme(crate = ::soothfast::__private::linkme)]
        #[allow(non_upper_case_globals)]
        static #static_ident: ::soothfast::__private::RouteItem =
            ::soothfast::__private::RouteItem::new(
                concat!(module_path!(), "::", stringify!(#name)),
                #spec,
                #operation,
                #method,
                #path,
            )
            .with_shape(#request_tok, #response_tok, #status_tok)
            .with_params(#params_tok)
            .with_path_params(#path_params_tok)
            .with_summary(#summary_tok);
    }
    .into()
}

/// Languages an exported item opts out of.
#[derive(Clone, Default, PartialEq)]
enum Skip {
    #[default]
    None,
    All,
    Langs(Vec<String>),
}

impl Skip {
    /// Registry encoding: empty binds everything, `*` binds nothing.
    fn encode(&self) -> String {
        match self {
            Skip::None => String::new(),
            Skip::All => "*".into(),
            Skip::Langs(langs) => langs.join(","),
        }
    }

    /// An `impl` block's opt-outs apply to every method in it, on top of
    /// whatever the method names itself.
    fn merge(&self, other: &Skip) -> Skip {
        match (self, other) {
            (Skip::All, _) | (_, Skip::All) => Skip::All,
            (Skip::None, o) => o.clone(),
            (s, Skip::None) => s.clone(),
            (Skip::Langs(a), Skip::Langs(b)) => {
                let mut merged = a.clone();
                merged.extend(b.iter().cloned());
                merged.sort();
                merged.dedup();
                Skip::Langs(merged)
            }
        }
    }
}

#[derive(Default)]
struct ExportAttrs {
    skip: Skip,
    constructor: bool,
}

fn parse_export_attrs(attr: TokenStream) -> Result<ExportAttrs, syn::Error> {
    let mut out = ExportAttrs::default();
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("skip") {
            out.skip = if meta.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in meta.input);
                let list: syn::punctuated::Punctuated<syn::Ident, syn::Token![,]> = content
                    .parse_terminated(<syn::Ident as syn::parse::Parse>::parse, syn::Token![,])?;
                Skip::Langs(list.iter().map(ToString::to_string).collect())
            } else {
                Skip::All
            };
        } else if meta.path.is_ident("constructor") {
            out.constructor = true;
        } else {
            return Err(meta.error("expected `skip`, `skip(lang, ...)`, or `constructor`"));
        }
        Ok(())
    });
    syn::parse::Parser::parse(parser, attr).map(|()| out)
}

/// Bind a function, struct, enum, or inherent `impl` block into every
/// language the package configures under `[[bind]]`.
///
/// ```ignore
/// #[soothfast::export]
/// pub struct Counter { pub value: i64 }
///
/// #[soothfast::export]
/// impl Counter {
///     pub fn new(start: i64) -> Self { ... }
///     pub fn bump(&self, by: i64) -> Result<i64, CounterError> { ... }
///
///     #[soothfast::export(skip)]
///     pub fn debug_dump(&self) -> String { ... }
/// }
/// ```
///
/// `skip(wasm)` narrows to the languages named; bare `skip` binds none.
/// `constructor` picks an associated fn other than an inherent `new`.
#[proc_macro_attribute]
pub fn export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_str = attr.to_string();
    let attrs = match parse_export_attrs(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    match syn::parse_macro_input!(item as syn::Item) {
        syn::Item::Fn(func) => export_fn(&func, &attrs, &attr_str),
        syn::Item::Struct(item) => {
            let mut bare = item.clone();
            bare.attrs.retain(|a| !a.path().is_ident("doc"));
            export_type(
                quote!(#item),
                &item.ident,
                &item.attrs,
                "struct",
                &attrs,
                &fingerprint_tokens(attr_str.as_str(), quote!(#bare)),
            )
        }
        syn::Item::Enum(item) => {
            let mut bare = item.clone();
            bare.attrs.retain(|a| !a.path().is_ident("doc"));
            export_type(
                quote!(#item),
                &item.ident,
                &item.attrs,
                "enum",
                &attrs,
                &fingerprint_tokens(attr_str.as_str(), quote!(#bare)),
            )
        }
        syn::Item::Impl(block) => export_impl(block, &attrs, &attr_str),
        other => err(
            syn::spanned::Spanned::span(&other),
            "#[soothfast::export] applies to a fn, struct, enum, or inherent `impl` block",
        ),
    }
}

fn export_fn(func: &syn::ItemFn, attrs: &ExportAttrs, attr_str: &str) -> TokenStream {
    let name = &func.sig.ident;
    if attrs.constructor {
        return err(
            name.span(),
            "`constructor` applies to an associated fn inside an exported `impl` block",
        );
    }
    let entry = registration_entry(
        &format_ident!("__SOOTHFAST_EXPORT_FN_{}", name),
        quote!(concat!(module_path!(), "::", stringify!(#name))),
        "fn",
        &fingerprint_input(func, attr_str),
        &attrs.skip,
        None,
        false,
        doc_summary(&func.attrs),
    );
    quote! {
        #func

        #entry
    }
    .into()
}

fn export_type(
    item: proc_macro2::TokenStream,
    name: &syn::Ident,
    item_attrs: &[syn::Attribute],
    kind: &'static str,
    attrs: &ExportAttrs,
    fingerprint: &str,
) -> TokenStream {
    if attrs.constructor {
        return err(name.span(), "`constructor` applies to an associated fn");
    }
    let entry = registration_entry(
        &format_ident!("__SOOTHFAST_EXPORT_TY_{}", name),
        quote!(concat!(module_path!(), "::", stringify!(#name))),
        kind,
        fingerprint,
        &attrs.skip,
        None,
        false,
        doc_summary(item_attrs),
    );
    quote! {
        #item

        #entry
    }
    .into()
}

fn export_impl(mut block: syn::ItemImpl, attrs: &ExportAttrs, attr_str: &str) -> TokenStream {
    let span = syn::spanned::Spanned::span(&block.self_ty);
    if block.trait_.is_some() {
        return err(span, "#[soothfast::export] does not apply to trait impls");
    }
    if !block.generics.params.is_empty() {
        return err(span, "#[soothfast::export] does not apply to generic impls");
    }
    let Some(owner) = impl_type_name(&block.self_ty) else {
        return err(span, "cannot name the type this `impl` block is for");
    };

    let mut entries = Vec::new();
    for item in &mut block.items {
        let syn::ImplItem::Fn(func) = item else {
            continue;
        };
        let own = match take_export_attr(&mut func.attrs) {
            Ok(a) => a,
            Err(e) => return e.to_compile_error().into(),
        };
        if !matches!(func.vis, syn::Visibility::Public(_)) {
            continue;
        }
        let skip = attrs.skip.merge(&own.skip);
        if skip == Skip::All {
            continue;
        }
        let name = &func.sig.ident;
        let mut bare = func.clone();
        bare.attrs.retain(|a| !a.path().is_ident("doc"));
        entries.push(registration_entry(
            &format_ident!("__SOOTHFAST_EXPORT_M_{}_{}", owner, name),
            quote!(concat!(module_path!(), "::", #owner, "::", stringify!(#name))),
            "method",
            &fingerprint_tokens(attr_str, quote!(#bare)),
            &skip,
            Some(owner.as_str()),
            own.constructor,
            doc_summary(&func.attrs),
        ));
    }

    quote! {
        #block

        #(#entries)*
    }
    .into()
}

/// Read and remove a method's own `#[soothfast::export(...)]`. Left in place
/// it would expand a second time, nested inside the block this one emits.
fn take_export_attr(attrs: &mut Vec<syn::Attribute>) -> Result<ExportAttrs, syn::Error> {
    let Some(pos) = attrs.iter().position(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == "export")
    }) else {
        return Ok(ExportAttrs::default());
    };
    let attr = attrs.remove(pos);
    match &attr.meta {
        syn::Meta::Path(_) => Ok(ExportAttrs::default()),
        syn::Meta::List(list) => parse_export_attrs(list.tokens.clone().into()),
        syn::Meta::NameValue(nv) => Err(syn::Error::new_spanned(
            nv,
            "expected `skip`, `skip(lang, ...)`, or `constructor`",
        )),
    }
}

fn impl_type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    Some(path.path.segments.last()?.ident.to_string())
}

#[allow(clippy::too_many_arguments)]
fn registration_entry(
    static_ident: &syn::Ident,
    id: proc_macro2::TokenStream,
    kind: &'static str,
    fingerprint: &str,
    skip: &Skip,
    owner: Option<&str>,
    constructor: bool,
    summary: Option<String>,
) -> proc_macro2::TokenStream {
    let skip = skip.encode();
    let owner_tok = opt_str(&owner.map(ToString::to_string));
    let summary_tok = opt_str(&summary);
    let mut entry = quote! {
        ::soothfast::__private::ExportItem::new(
            #id,
            #kind,
            ::soothfast::__private::fnv1a(#fingerprint.as_bytes()),
        )
        .with_skip(#skip)
        .with_owner(#owner_tok)
        .with_summary(#summary_tok)
    };
    if constructor {
        entry = quote! { #entry.with_constructor() };
    }
    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #[::soothfast::__private::linkme::distributed_slice(::soothfast::__private::EXPORTS)]
        #[linkme(crate = ::soothfast::__private::linkme)]
        #[allow(non_upper_case_globals)]
        static #static_ident: ::soothfast::__private::ExportItem = #entry;
    }
}

/// Mark a deterministic input-builder. Registers metadata only; the function
/// is referenced by name from `#[bench(setup = ...)]` / `setup_sized`.
#[proc_macro_attribute]
pub fn fixture(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return err(Span::call_site(), "#[fixture] takes no arguments");
    }
    let func = syn::parse_macro_input!(item as syn::ItemFn);
    let name = &func.sig.ident;
    let static_ident = format_ident!("__SOOTHFAST_FIXTURE_{}", name);
    let normalized = fingerprint_input(&func, "");
    quote! {
        #func

        #[cfg(not(target_arch = "wasm32"))]
        #[::soothfast::__private::linkme::distributed_slice(::soothfast::__private::FIXTURES)]
        #[linkme(crate = ::soothfast::__private::linkme)]
        #[allow(non_upper_case_globals)]
        static #static_ident: ::soothfast::__private::FixtureItem =
            ::soothfast::__private::FixtureItem::new(
                concat!(module_path!(), "::", stringify!(#name)),
                ::soothfast::__private::fnv1a(#normalized.as_bytes()),
            );
    }
    .into()
}

/// Mark a mock-backend setup fn, resolved at runtime by name via
/// `soothfast::mock::activate` — referenced from a markdown `mock=NAME` /
/// `mock=NAME(ARG)` tag, which is plain text rather than a `syn::Path`, so
/// (unlike `#[fixture]`, wired at compile time through a macro attribute
/// argument) this can't be called from a literal call site the macro
/// controls; the registry stores a name-keyed fn pointer instead.
///
/// The annotated fn takes zero args or one `&str` (the tag's `(ARG)`, `""`
/// when omitted) and returns any `T: soothfast::registry::MockSeam`.
#[proc_macro_attribute]
pub fn mock_seam(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return err(Span::call_site(), "#[mock_seam] takes no arguments");
    }
    let func = syn::parse_macro_input!(item as syn::ItemFn);
    let name = &func.sig.ident;
    let span = name.span();
    let call = match func.sig.inputs.len() {
        0 => quote! { #name() },
        1 => quote! { #name(__arg) },
        n => {
            return err(
                span,
                &format!("#[mock_seam] function must take zero args or one `&str`, found {n} args"),
            );
        }
    };
    let static_ident = format_ident!("__SOOTHFAST_MOCK_{}", name);
    let glue_ident = format_ident!("__soothfast_mock_glue_{}", name);
    let normalized = fingerprint_input(&func, "");
    quote! {
        #func

        #[doc(hidden)]
        fn #glue_ident(__arg: &str) -> ::std::boxed::Box<dyn ::soothfast::__private::MockSeam> {
            ::std::boxed::Box::new(#call)
        }

        #[cfg(not(target_arch = "wasm32"))]
        #[::soothfast::__private::linkme::distributed_slice(::soothfast::__private::MOCKS)]
        #[linkme(crate = ::soothfast::__private::linkme)]
        #[allow(non_upper_case_globals)]
        static #static_ident: ::soothfast::__private::MockSeamItem =
            ::soothfast::__private::MockSeamItem::new(
                concat!(module_path!(), "::", stringify!(#name)),
                ::soothfast::__private::fnv1a(#normalized.as_bytes()),
                #glue_ident,
            );
    }
    .into()
}

fn opt_u64(v: Option<u64>) -> proc_macro2::TokenStream {
    match v {
        Some(x) => quote!(::core::option::Option::Some(#x)),
        None => quote!(::core::option::Option::None),
    }
}

fn opt_str(v: &Option<String>) -> proc_macro2::TokenStream {
    match v {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    }
}

/// Normalized token string for fingerprinting: `TokenStream::to_string()`
/// canonicalizes spacing (formatting-independent), and doc-comment attrs are
/// stripped first so a typo fix in prose never flags the code as changed.
fn fingerprint_input(func: &syn::ItemFn, attr_str: &str) -> String {
    let mut f = func.clone();
    f.attrs.retain(|a| !a.path().is_ident("doc"));
    fingerprint_tokens(attr_str, quote!(#f))
}

fn fingerprint_tokens(attr_str: &str, tokens: proc_macro2::TokenStream) -> String {
    format!("{attr_str}\u{1}{tokens}")
}

/// First non-empty `///` line. Captured at expansion because generated
/// artifacts (bench-target markers, binding glue) never reach the lib's
/// rustdoc JSON.
fn doc_summary(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| {
        if !a.path().is_ident("doc") {
            return None;
        }
        let syn::Meta::NameValue(nv) = &a.meta else {
            return None;
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = &nv.value
        else {
            return None;
        };
        let line = s.value().trim().to_string();
        (!line.is_empty()).then_some(line)
    })
}

/// Shared expansion: original fn + glue fn(s) + registry entry.
fn registration(
    func: &syn::ItemFn,
    attrs: &Attrs,
    attr_str: &str,
    iter_call: proc_macro2::TokenStream,
    sized: Option<(proc_macro2::TokenStream, syn::Ident)>,
) -> TokenStream {
    let name = &func.sig.ident;
    let group = attrs.group.clone().unwrap_or_else(|| "default".into());
    let covers = &attrs.covers;
    let normalized = fingerprint_input(func, attr_str);
    let glue_ident = format_ident!("__soothfast_glue_{}", name);
    let static_ident = format_ident!("__SOOTHFAST_MEASURED_{}", name);

    let mut entry = quote! {
        ::soothfast::__private::MeasuredItem::new(
            env!("CARGO_PKG_NAME"),
            module_path!(),
            stringify!(#name),
            #group,
            ::soothfast::__private::fnv1a(#normalized.as_bytes()),
            #covers,
            #glue_ident,
        )
    };
    // Record the setup fn name so the runner can fold the fixture's
    // fingerprint into this item's (input changes are workload changes).
    let setup_name = attrs
        .setup
        .as_ref()
        .or(attrs.setup_sized.as_ref())
        .and_then(|p| p.segments.last())
        .map(|s| s.ident.to_string());
    if let Some(s) = setup_name {
        entry = quote! { #entry.with_setup(#s) };
    }
    if attrs.max_allocs.is_some() || attrs.p99_ns.is_some() || attrs.complexity.is_some() {
        let (a, p, c) = (
            opt_u64(attrs.max_allocs),
            opt_u64(attrs.p99_ns),
            opt_str(&attrs.complexity),
        );
        entry = quote! {
            #entry.with_assertions(::soothfast::__private::Assertions::new(#a, #p, #c))
        };
    }
    if let Some(t) = attrs.tolerance_pct {
        entry = quote! { #entry.with_tolerance(#t) };
    }
    if func.sig.asyncness.is_some() {
        entry = quote! { #entry.with_async() };
    }
    let sized_defs = match &sized {
        Some((defs, sized_ident)) => {
            let sizes = &attrs.sizes;
            entry = quote! { #entry.with_sweep(&[#(#sizes),*], #sized_ident) };
            defs.clone()
        }
        None => quote!(),
    };

    quote! {
        #func

        #sized_defs

        #[doc(hidden)]
        fn #glue_ident(__b: &mut ::soothfast::__private::Bencher) {
            #iter_call
        }

        #[cfg(not(target_arch = "wasm32"))]
        #[::soothfast::__private::linkme::distributed_slice(::soothfast::__private::MEASURED)]
        #[linkme(crate = ::soothfast::__private::linkme)]
        #[allow(non_upper_case_globals)]
        static #static_ident: ::soothfast::__private::MeasuredItem = #entry;
    }
    .into()
}
