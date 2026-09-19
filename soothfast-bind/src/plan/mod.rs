//! The exported surface → one language-neutral wrapper model.
//!
//! Every question that is not about spelling is answered here: which types
//! become handle classes, which associated fn builds one, which fields get
//! accessors, what raises, and what cannot be bound at all. Emitters render
//! this; they never re-derive it, so two languages cannot disagree about
//! what the same Rust type is.

use std::collections::BTreeSet;
use std::iter::once;

mod errors;
mod transfer;
mod unsupported;

pub use errors::{ErrorClass, ErrorVariant, VariantShape};
pub use transfer::{BufferSupport, Transfer, detachable, offloadable, transfer_notes};
use unsupported::{optional_scalar_param_is_ready, unsupported};

use crate::gap::Gap;
use crate::model::{
    ExportedFn, ExportedType, Field, Ownership, Param, Receiver, Surface, Ty, TypeKind, Variant,
    is_plain,
};
use crate::{BindKind, BindOptions};

/// One bound callable, with every language-neutral decision already made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Stable name across every language, for a backend with no namespacing
    /// of its own to lean on.
    pub symbol: String,
    /// Path a glue crate calls it by.
    pub rust_path: String,
    pub name: String,
    pub receiver: Receiver,
    pub params: Vec<Param>,
    pub ret: Ty,
    /// The error a failing call raises. `None` means it cannot fail.
    pub throws: Option<Ty>,
    /// The Rust return is a reference the glue owns before handing across.
    pub ret_borrowed: bool,
    pub is_async: bool,
    pub doc: Option<String>,
}

/// A readable and writable property over one public field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accessor {
    pub field: String,
    pub ty: Ty,
    pub doc: Option<String>,
}

/// An exported type, as the handle class every language wraps it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    pub rust_path: String,
    pub name: String,
    pub doc: Option<String>,
    pub send: bool,
    /// Whether the Rust type is known `Sync`. Handing `&T` to a thread that
    /// holds no interpreter lock needs this, and an unproven guess would not
    /// compile.
    pub sync: bool,
    /// The associated fn that builds one, if any. Without it the class is
    /// only reachable as another call's return value.
    pub ctor: Option<Function>,
    pub accessors: Vec<Accessor>,
    /// Calls taking an instance.
    pub methods: Vec<Function>,
    /// Calls that do not, minus the constructor.
    pub statics: Vec<Function>,
    /// Present for an exported enum, whose variants are part of its shape.
    pub variants: Option<Vec<Variant>>,
}

impl Class {
    /// Whether the type is an enumeration with no payload anywhere.
    ///
    /// Every target language has a plain enumeration of its own to mirror
    /// this onto; one carrying data has to stay an opaque handle.
    pub fn is_plain_enum(&self) -> bool {
        self.variants.as_deref().is_some_and(is_plain)
    }
}

/// Everything one language emits, decided once for all of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingPlan {
    pub classes: Vec<Class>,
    /// Free functions, belonging to no class.
    pub functions: Vec<Function>,
    pub gaps: Vec<Gap>,
    /// What the target language can do with a borrowed buffer, decided once
    /// at [`lower`] time so [`offloadable`] and [`transfer_notes`] never have
    /// to be told which language they are answering for.
    pub buffer_support: BufferSupport,
    /// Every error a bound call throws, as the classes a language raises.
    pub errors: Vec<ErrorClass>,
}

impl BindingPlan {
    /// Every bound callable, whether it belongs to a class or not.
    pub fn functions(&self) -> impl Iterator<Item = &Function> {
        self.classes
            .iter()
            .flat_map(|c| {
                c.ctor
                    .iter()
                    .chain(c.methods.iter())
                    .chain(c.statics.iter())
            })
            .chain(self.functions.iter())
    }

    /// Whether a class name binds as a mirrored value rather than a handle.
    pub fn is_mirrored(&self, name: &str) -> bool {
        self.classes
            .iter()
            .any(|c| c.name == name && c.is_plain_enum())
    }

    /// Whether anything bound here is `async`. Both backends need an extra
    /// dependency to drive a future, and only when one is present.
    pub fn has_async(&self) -> bool {
        self.functions().any(|f| f.is_async)
    }

    /// Whether a value of this type can be handed to another thread.
    ///
    /// An exported type answers from its own auto-trait impls; everything
    /// else is built out of primitives, which always can.
    pub fn sendable(&self, ty: &Ty) -> bool {
        match ty {
            // A mapped type's Send-ness is not in the document either.
            Ty::Opaque(_) | Ty::Text(_) => false,
            Ty::Class(name) => self.classes.iter().any(|c| c.name == *name && c.send),
            Ty::List(inner) | Ty::Optional(inner) => self.sendable(inner),
            Ty::Map(key, value) => self.sendable(key) && self.sendable(value),
            Ty::Tuple(items) => items.iter().all(|t| self.sendable(t)),
            _ => true,
        }
    }
}

/// Lower the exported surface for one language.
///
/// `gaps` carries in whatever the walk already found; anything this pass
/// decides cannot be bound joins it, and the item is left out rather than
/// bound to a guess.
pub fn lower(
    surface: &Surface,
    gaps: Vec<Gap>,
    opts: &BindOptions,
    kind: BindKind,
) -> Result<BindingPlan, String> {
    let mut plan = BindingPlan {
        gaps,
        buffer_support: kind.buffer_support(),
        errors: errors::lower(&surface.errors),
        ..BindingPlan::default()
    };
    let lang = kind.name();

    let types: Vec<&ExportedType> = surface
        .types
        .iter()
        .filter(|t| !skips(&t.skip, lang))
        .collect();
    // A plain enumeration mirrors onto a value type, so crossing one by
    // value copies nothing the caller was holding.
    let mirrored: BTreeSet<String> = types
        .iter()
        .filter(|t| t.is_plain_enum())
        .map(|t| t.name.clone())
        .collect();
    let cloneable: BTreeSet<String> = types
        .iter()
        .filter(|t| t.clone)
        .map(|t| t.name.clone())
        .collect();
    let fns: Vec<&ExportedFn> = surface
        .fns
        .iter()
        .filter(|f| !skips(&f.skip, lang))
        .filter(|f| bindable(f, kind, &mirrored, &mut plan.gaps))
        .collect();

    for ty in &types {
        let class = class_of(ty, &fns, opts, kind, &mirrored, &cloneable, &mut plan.gaps);
        plan.classes.push(class);
    }
    plan.functions = fns
        .iter()
        .filter(|f| f.owner.is_none())
        .map(|f| function_of(f, opts, None))
        .collect();

    let known: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
    for name in referenced_classes(&plan) {
        if !known.contains(&name.as_str()) {
            plan.gaps.push(Gap::UnsupportedByBackend {
                at: name.clone(),
                ty: name.clone(),
                lang,
                why: "the type is not exported for this language, so nothing \
                      names it on the other side"
                    .into(),
            });
        }
    }

    Ok(plan)
}

fn class_of(
    ty: &ExportedType,
    fns: &[&ExportedFn],
    opts: &BindOptions,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    cloneable: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> Class {
    let owned: Vec<&&ExportedFn> = fns
        .iter()
        .filter(|f| f.owner.as_deref() == Some(ty.name.as_str()))
        .collect();

    // A plain enum mirrors onto a target language's own enumeration, which
    // has no constructor, methods or statics of its own to carry these on.
    if let TypeKind::Enum(variants) = &ty.kind
        && is_plain(variants)
    {
        for f in &owned {
            gaps.push(Gap::PlainEnumMember { at: f.id.clone() });
        }
        return Class {
            rust_path: ty.rust_path.clone(),
            name: ty.name.clone(),
            doc: ty.doc.clone(),
            send: ty.send,
            sync: ty.sync,
            ctor: None,
            accessors: Vec::new(),
            methods: Vec::new(),
            statics: Vec::new(),
            variants: Some(variants.clone()),
        };
    }

    let ctor = pick_ctor(&owned, &ty.name).map(|f| function_of(f, opts, Some(&ty.name)));
    let ctor_path = ctor.as_ref().map(|c| c.rust_path.clone());

    let mut methods = Vec::new();
    let mut statics = Vec::new();
    for f in &owned {
        if Some(&f.rust_path) == ctor_path.as_ref() {
            continue;
        }
        let lowered = function_of(f, opts, Some(&ty.name));
        if f.receiver == Receiver::None {
            statics.push(lowered);
        } else {
            methods.push(lowered);
        }
    }

    let (accessors, variants) = match &ty.kind {
        TypeKind::Struct(fields) => (
            accessors_of(fields, &ty.name, kind, mirrored, cloneable, gaps),
            None,
        ),
        TypeKind::Enum(v) => (Vec::new(), Some(v.clone())),
    };

    Class {
        rust_path: ty.rust_path.clone(),
        name: ty.name.clone(),
        doc: ty.doc.clone(),
        send: ty.send,
        sync: ty.sync,
        ctor,
        accessors,
        methods,
        statics,
        variants,
    }
}

/// The declared constructor, or an inherent `new` that builds the type.
/// No target language awaits inside construction, so an `async` one stays
/// a static factory the caller awaits.
fn pick_ctor<'a>(owned: &'a [&&'a ExportedFn], name: &str) -> Option<&'a ExportedFn> {
    if let Some(f) = owned.iter().find(|f| f.constructor && !f.is_async) {
        return Some(f);
    }
    owned
        .iter()
        .find(|f| {
            f.name == "new" && !f.is_async && f.receiver == Receiver::None && builds(&f.ret, name)
        })
        .map(|f| **f)
}

fn builds(ret: &Ty, name: &str) -> bool {
    matches!(ret, Ty::Class(c) if c == name)
}

/// Public fields become properties. A private one is known to exist but has
/// no reader, so it contributes nothing to bind.
///
/// A field holding an exported type is read out as a fresh handle, which
/// copies it: Python does so when the type is `Clone`, and reports it
/// otherwise. Every other backend still reports a bare or optional one.
fn accessors_of(
    fields: &[Field],
    owner: &str,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    cloneable: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> Vec<Accessor> {
    let mut out = Vec::new();
    for field in fields.iter().filter(|f| f.public) {
        let at = format!("{owner}.{}", field.name);
        let mut nested = Vec::new();
        field.ty.classes(&mut nested);
        nested.retain(|c| !mirrored.contains(c));
        let bare = matches!(&field.ty, Ty::Class(_))
            || matches!(&field.ty, Ty::Optional(inner) if matches!(**inner, Ty::Class(_)));
        let copied = match kind {
            BindKind::Python => nested.iter().find(|c| !cloneable.contains(*c)),
            _ if bare => nested.first(),
            _ => None,
        };
        if let Some(copied) = copied {
            gaps.push(Gap::HandleByValue {
                at,
                ty: copied.clone(),
            });
            continue;
        }
        if field.ty.has_opaque() || unsupported(kind, &field.ty, mirrored).is_some() {
            continue;
        }
        out.push(Accessor {
            field: field.name.clone(),
            ty: field.ty.clone(),
            doc: field.doc.clone(),
        });
    }
    out
}

fn function_of(f: &ExportedFn, opts: &BindOptions, owner: Option<&str>) -> Function {
    Function {
        symbol: symbol(&opts.package, owner, &f.name),
        rust_path: f.rust_path.clone(),
        name: f.name.clone(),
        receiver: f.receiver,
        params: f.params.clone(),
        ret: f.ret.clone(),
        throws: f.throws.clone(),
        ret_borrowed: f.ret_borrowed,
        is_async: f.is_async,
        doc: f.doc.clone(),
    }
}

/// A stable symbol a backend with no namespaces can export under.
fn symbol(package: &str, owner: Option<&str>, name: &str) -> String {
    let parts = [Some(package), owner, Some(name)];
    parts
        .into_iter()
        .flatten()
        .map(snake)
        .collect::<Vec<_>>()
        .join("_")
}

fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else if c.is_ascii_alphanumeric() {
            prev_lower = true;
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
            prev_lower = false;
        }
    }
    out.trim_matches('_').to_string()
}

/// Whether an item survives lowering, recording why when it does not.
fn bindable(
    f: &ExportedFn,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> bool {
    if f.receiver == Receiver::Consuming {
        return false;
    }
    if async_is_blocked(f, kind, gaps) {
        return false;
    }
    if owned_handle_param_is_blocked(f, mirrored, gaps) {
        return false;
    }
    if optional_handle_param_is_blocked(f, kind, mirrored, gaps) {
        return false;
    }
    if r_mutable_buffer_param_is_blocked(f, kind, gaps) {
        return false;
    }
    if r_optional_scalar_param_is_blocked(f, kind, mirrored, gaps) {
        return false;
    }
    if wasm_optional_handle_ref_param_is_blocked(f, kind, mirrored, gaps) {
        return false;
    }
    if borrowed_return_is_blocked(f, kind, mirrored, gaps) {
        return false;
    }
    every_type_is_carriable(f, kind, mirrored, gaps)
}

/// A borrowed return (`&str`, `&[f64]`, `Option<&str>`) is owned by the
/// Python glue before it crosses. A reference to an exported type would
/// need a clone the type is not known to carry, and no other backend owns
/// a borrowed return yet; both are reported.
fn borrowed_return_is_blocked(
    f: &ExportedFn,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> bool {
    if !f.ret_borrowed {
        return false;
    }
    let inner = match &f.ret {
        Ty::Optional(inner) => &**inner,
        ret => ret,
    };
    let why = match inner {
        Ty::Class(name) if !mirrored.contains(name) => {
            format!("returns a reference to the exported type `{name}`; return an owned value")
        }
        _ if kind != BindKind::Python => {
            "returns a borrowed value, which only the Python glue owns before \
             crossing; return an owned value"
                .into()
        }
        _ => return false,
    };
    record(
        gaps,
        Gap::UnsupportedByBackend {
            at: f.id.clone(),
            ty: format!("&{}", f.ret.render()),
            lang: kind.name(),
            why,
        },
    );
    true
}

/// A `None`/`NULL` parameter of any other optional shape has no hand-built
/// `Robj` conversion yet; the return and field directions are unrestricted
/// (see [`unsupported::optional_scalar_param_is_ready`]'s own doc).
fn r_optional_scalar_param_is_blocked(
    f: &ExportedFn,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> bool {
    if kind != BindKind::R {
        return false;
    }
    for param in &f.params {
        let Ty::Optional(inner) = &param.ty else {
            continue;
        };
        if optional_scalar_param_is_ready(inner, mirrored) {
            continue;
        }
        record(
            gaps,
            Gap::UnsupportedByBackend {
                at: f.id.clone(),
                ty: param.ty.render(),
                lang: kind.name(),
                why: format!(
                    "`Option<{}>` has no hand-built Robj conversion for a \
                     parameter; take the value directly, using an empty \
                     sequence or a sentinel for absent",
                    inner.render()
                ),
            },
        );
        return true;
    }
    false
}

/// wasm-bindgen implements `OptionFromWasmAbi` for an exported struct only
/// by value, never for a reference to one; the plan only ever carries the
/// reference form for a non-mirrored `Option<Class>` parameter (an owned one
/// is already a `Gap::HandleByValue` from `owned_handle_param_is_blocked`),
/// so every one that reaches here is a gap regardless of ownership.
fn wasm_optional_handle_ref_param_is_blocked(
    f: &ExportedFn,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> bool {
    if kind != BindKind::Wasm {
        return false;
    }
    for param in &f.params {
        let Ty::Optional(inner) = &param.ty else {
            continue;
        };
        let Ty::Class(name) = &**inner else {
            continue;
        };
        if mirrored.contains(name) {
            continue;
        }
        record(
            gaps,
            Gap::UnsupportedByBackend {
                at: f.id.clone(),
                ty: param.ty.render(),
                lang: kind.name(),
                why: "wasm-bindgen has no OptionFromWasmAbi for a reference \
                      to an exported type; return it instead, or accept a \
                      separate presence flag alongside the handle"
                    .into(),
            },
        );
        return true;
    }
    false
}

/// `async fn` needs a runtime story per backend, and never on an exclusive
/// receiver: driving a future from `&mut self` would need to hold the borrow
/// across every await point.
fn async_is_blocked(f: &ExportedFn, kind: BindKind, gaps: &mut Vec<Gap>) -> bool {
    if !f.is_async {
        return false;
    }
    if f.receiver == Receiver::Exclusive {
        return true;
    }
    if !matches!(
        kind,
        BindKind::CAbi
            | BindKind::Go
            | BindKind::Node
            | BindKind::Java
            | BindKind::Kotlin
            | BindKind::R
            | BindKind::Ruby
            | BindKind::Cpp
            | BindKind::Lua
            | BindKind::CSharp
    ) {
        return false;
    }
    record(
        gaps,
        Gap::UnsupportedByBackend {
            at: f.id.clone(),
            ty: "async fn".into(),
            lang: kind.name(),
            why: match kind {
                BindKind::Go => "no Go runtime story yet".into(),
                BindKind::Node => "no Node runtime story yet".into(),
                BindKind::Java => "no Java runtime story yet".into(),
                BindKind::Kotlin => "no Kotlin runtime story yet".into(),
                BindKind::R => "no R runtime story yet".into(),
                BindKind::Ruby => "no Ruby runtime story yet".into(),
                BindKind::Cpp => "no C++ runtime story yet".into(),
                BindKind::Lua => "no Lua runtime story yet".into(),
                BindKind::CSharp => "no .NET async story yet".into(),
                _ => "C has nothing to await with; expose a blocking wrapper \
                      instead"
                    .into(),
            },
        },
    );
    true
}

fn owned_handle_param_is_blocked(
    f: &ExportedFn,
    mirrored: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> bool {
    for param in &f.params {
        let owned_class = match &param.ty {
            Ty::Class(name) if param.ownership == Ownership::Owned => Some(name),
            Ty::Optional(inner) if param.inner_ownership == Ownership::Owned => match &**inner {
                Ty::Class(name) => Some(name),
                _ => None,
            },
            // A `Vec<Class>` is N owned handles; no backend has a pattern
            // for extracting a list of them any more than it does for one.
            Ty::List(inner) => match &**inner {
                Ty::Class(name) => Some(name),
                _ => None,
            },
            _ => None,
        };
        if let Some(name) = owned_class
            && !mirrored.contains(name)
        {
            record(
                gaps,
                Gap::HandleByValue {
                    at: f.id.clone(),
                    ty: name.clone(),
                },
            );
            return true;
        }
    }
    false
}

/// An optional exported type crosses back as a pointer that may be null, but
/// nothing in the model says whether a parameter wants it borrowed or owned,
/// and the two need different C. Go, C++, Lua and C# all call the same C
/// functions, so each inherits the restriction; Java and Kotlin have no way
/// to name a different constructor overload for it either; R's own handle is
/// an external pointer with the same borrowed-or-owned ambiguity. Ruby's own
/// handle is a `RefCell`, whose borrow guard cannot outlive the closure a
/// `None`/`Some` conversion would need to build one in. A mirrored enum has
/// none of these problems: it crosses by value, so R and Ruby both carry it,
/// though the rest of the list keeps blocking it either way.
fn optional_handle_param_is_blocked(
    f: &ExportedFn,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> bool {
    if !matches!(
        kind,
        BindKind::CAbi
            | BindKind::Go
            | BindKind::Java
            | BindKind::Kotlin
            | BindKind::R
            | BindKind::Ruby
            | BindKind::Cpp
            | BindKind::Lua
            | BindKind::CSharp
    ) {
        return false;
    }
    for param in &f.params {
        let Ty::Optional(inner) = &param.ty else {
            continue;
        };
        let Ty::Class(name) = &**inner else {
            continue;
        };
        if matches!(kind, BindKind::R | BindKind::Ruby) && mirrored.contains(name) {
            continue;
        }
        record(
            gaps,
            Gap::UnsupportedByBackend {
                at: f.id.clone(),
                ty: param.ty.render(),
                lang: kind.name(),
                why: "an optional exported type is taken only as a return; \
                      take it by reference instead"
                    .into(),
            },
        );
        return true;
    }
    false
}

/// R vectors are copy-on-write values, not caller-owned buffers: writing
/// through one in place is not something an R caller can safely observe,
/// since R gives no guarantee the vector it passed is not aliased elsewhere.
/// A mutable buffer parameter has no honest R spelling.
fn r_mutable_buffer_param_is_blocked(f: &ExportedFn, kind: BindKind, gaps: &mut Vec<Gap>) -> bool {
    if kind != BindKind::R {
        return false;
    }
    for param in &f.params {
        if param.ownership == Ownership::BorrowedMut && is_buffer_ty(&param.ty) {
            record(
                gaps,
                Gap::UnsupportedByBackend {
                    at: f.id.clone(),
                    ty: param.ty.render(),
                    lang: kind.name(),
                    why: "R vectors are values; an out-parameter cannot be \
                          written through, return the sequence instead"
                        .into(),
                },
            );
            return true;
        }
    }
    false
}

/// `throws` is deliberately absent: an error crosses as a message, so its
/// type never has to be one the target language can carry.
fn every_type_is_carriable(
    f: &ExportedFn,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> bool {
    let tys = f.params.iter().map(|p| &p.ty).chain(once(&f.ret));
    for ty in tys {
        if ty.has_opaque() {
            return false;
        }
        if let Some(why) = unsupported(kind, ty, mirrored) {
            record(
                gaps,
                Gap::UnsupportedByBackend {
                    at: f.id.clone(),
                    ty: ty.render(),
                    lang: kind.name(),
                    why,
                },
            );
            return false;
        }
    }
    true
}

fn record(gaps: &mut Vec<Gap>, gap: Gap) {
    if !gaps.contains(&gap) {
        gaps.push(gap);
    }
}

/// Whether this type is the one shape a contiguous buffer parameter takes: a
/// byte string, or a sequence of one primitive.
fn is_buffer_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::Bytes) || matches!(ty, Ty::List(inner) if inner.is_primitive())
}

fn skips(skip: &[String], lang: &str) -> bool {
    skip.iter().any(|s| s == "*" || s == lang)
}

/// Every class name the plan's signatures mention.
fn referenced_classes(plan: &BindingPlan) -> Vec<String> {
    let mut names = Vec::new();
    let functions = plan
        .classes
        .iter()
        .flat_map(|c| {
            c.ctor
                .iter()
                .chain(c.methods.iter())
                .chain(c.statics.iter())
        })
        .chain(plan.functions.iter());
    for f in functions {
        for param in &f.params {
            param.ty.classes(&mut names);
        }
        f.ret.classes(&mut names);
    }
    for class in &plan.classes {
        for accessor in &class.accessors {
            accessor.ty.classes(&mut names);
        }
    }
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ownership, Receiver};

    fn buffer_fn() -> Function {
        Function {
            symbol: "digest".into(),
            rust_path: "acme::digest".into(),
            name: "digest".into(),
            receiver: Receiver::None,
            params: vec![Param {
                name: "data".into(),
                ty: Ty::List(Box::new(Ty::F64)),
                ownership: Ownership::Borrowed,
                inner_ownership: Ownership::Owned,
            }],
            ret: Ty::Unit,
            throws: None,
            ret_borrowed: false,
            is_async: false,
            doc: None,
        }
    }

    fn plan_with(support: BufferSupport, functions: Vec<Function>) -> BindingPlan {
        BindingPlan {
            functions,
            buffer_support: support,
            ..BindingPlan::default()
        }
    }

    #[test]
    fn a_pinned_call_never_offloads_even_though_zero_copy_would() {
        let f = buffer_fn();
        let zero_copy = plan_with(BufferSupport::ZeroCopy, vec![f.clone()]);
        assert!(offloadable(&f, None, &zero_copy));

        let pinned = plan_with(BufferSupport::Pinned, vec![f]);
        assert!(!offloadable(&pinned.functions[0], None, &pinned));
    }

    #[test]
    fn a_pinned_package_gets_one_note_not_one_per_function() {
        let plan = plan_with(BufferSupport::Pinned, vec![buffer_fn(), buffer_fn()]);
        assert_eq!(transfer_notes(&plan).len(), 1);
    }

    #[test]
    fn a_pinned_package_with_no_buffer_gets_no_note() {
        let plan = plan_with(BufferSupport::Pinned, Vec::new());
        assert!(transfer_notes(&plan).is_empty());
    }

    #[test]
    fn optional_str_borrowedness_follows_the_inner_type_not_the_option() {
        let plan = BindingPlan::default();
        let optional_ref = Param {
            name: "label".into(),
            ty: Ty::Optional(Box::new(Ty::Str)),
            ownership: Ownership::Owned,
            inner_ownership: Ownership::Borrowed,
        };
        let optional_owned = Param {
            name: "label".into(),
            ty: Ty::Optional(Box::new(Ty::Str)),
            ownership: Ownership::Owned,
            inner_ownership: Ownership::Owned,
        };
        assert_eq!(
            Transfer::of(&optional_ref, &plan),
            Transfer::Text {
                borrowed: true,
                nullable: true,
            },
        );
        assert_eq!(
            Transfer::of(&optional_owned, &plan),
            Transfer::Text {
                borrowed: false,
                nullable: true,
            },
        );
    }
}
