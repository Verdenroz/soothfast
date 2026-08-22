//! The exported surface → one language-neutral wrapper model.
//!
//! Every question that is not about spelling is answered here: which types
//! become handle classes, which associated fn builds one, which fields get
//! accessors, what raises, and what cannot be bound at all. Emitters render
//! this; they never re-derive it, so two languages cannot disagree about
//! what the same Rust type is.

use std::collections::BTreeSet;
use std::iter::once;

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
            Ty::Opaque(_) => false,
            Ty::Class(name) => self.classes.iter().any(|c| c.name == *name && c.send),
            Ty::List(inner) | Ty::Optional(inner) => self.sendable(inner),
            Ty::Map(key, value) => self.sendable(key) && self.sendable(value),
            Ty::Tuple(items) => items.iter().all(|t| self.sendable(t)),
            _ => true,
        }
    }
}

/// How one parameter's data moves across the boundary.
///
/// The classification is language-neutral; what each language can *do* with
/// it is not. A borrowed buffer reaches Python and a C ABI as a pointer, is
/// pinned or copied through JNI depending on the call used, and is always
/// copied into wasm, which has its own address space. Naming the shape once
/// keeps every backend answering the same question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transfer {
    /// A single value. Copying it is free everywhere.
    Scalar,
    /// An exported type the caller keeps hold of.
    Handle {
        mirrored: bool,
        writable: bool,
    },
    Text {
        borrowed: bool,
    },
    /// A contiguous run of one primitive: the only shape a language can hope
    /// to hand over without copying.
    Buffer {
        element: Ty,
        borrowed: bool,
        writable: bool,
    },
    /// A collection the target has to walk element by element whatever
    /// happens.
    Collection,
}

impl Transfer {
    /// Classify one parameter.
    pub fn of(param: &Param, plan: &BindingPlan) -> Transfer {
        let writable = param.ownership == Ownership::BorrowedMut;
        let borrowed = param.ownership != Ownership::Owned;
        match &param.ty {
            Ty::Class(name) => Transfer::Handle {
                mirrored: plan.is_mirrored(name),
                writable,
            },
            Ty::Str => Transfer::Text { borrowed },
            Ty::Bytes => Transfer::Buffer {
                element: Ty::U8,
                borrowed,
                writable,
            },
            Ty::List(inner) if inner.is_primitive() => Transfer::Buffer {
                element: (**inner).clone(),
                borrowed,
                writable,
            },
            ty if ty.is_primitive() || *ty == Ty::Unit => Transfer::Scalar,
            _ => Transfer::Collection,
        }
    }
}

/// What a binding layer can do with a [`Transfer::Buffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSupport {
    /// A borrowed buffer arrives as a pointer into the caller's memory.
    ZeroCopy,
    /// Every buffer is copied whatever the signature says.
    AlwaysCopies,
}

/// Whether a call's work can run on a thread holding no interpreter lock.
///
/// Worth asking only where enough crosses to pay for the handover, and a
/// buffer parameter is that signal: a scalar call costs less than releasing
/// the lock would. Everything the call touches has to reach the other
/// thread, which for a method means the receiver as well as the arguments.
pub fn offloadable(f: &Function, owner: Option<&Class>, plan: &BindingPlan) -> bool {
    let carries_buffer = f
        .params
        .iter()
        .any(|p| matches!(Transfer::of(p, plan), Transfer::Buffer { .. }));
    if f.is_async || !carries_buffer {
        return false;
    }
    let receiver = match f.receiver {
        Receiver::None => true,
        Receiver::Shared => owner.is_some_and(|c| c.sync),
        Receiver::Exclusive => owner.is_some_and(|c| c.send),
        Receiver::Consuming => false,
    };
    receiver
        && f.params.iter().all(|p| leaves_alone(p, plan))
        && plan.sendable(&f.ret)
        && f.throws.as_ref().is_none_or(|e| plan.sendable(e))
}

/// Whether one parameter reaches another thread on its own.
///
/// A handle held by reference stays behind: it borrows a Python object, and
/// reading one is the thing the lock protects. A mirrored enum crossed by
/// value is just a number by the time it gets here.
fn leaves_alone(param: &Param, plan: &BindingPlan) -> bool {
    match Transfer::of(param, plan) {
        Transfer::Handle { mirrored, .. } => mirrored,
        _ => plan.sendable(&param.ty),
    }
}

/// Notes about shapes that cost more than the signature had to.
///
/// Derived from the same model that generates the code, so the advice cannot
/// drift from what the emitter actually does.
pub fn transfer_notes(plan: &BindingPlan, support: BufferSupport) -> Vec<String> {
    // A backend that copies every buffer whatever the signature says has
    // nothing to act on: taking a borrow saves it no copy, and writing into
    // the caller's buffer costs it one more.
    if support != BufferSupport::ZeroCopy {
        return Vec::new();
    }
    let mut notes: Vec<String> = plan
        .functions()
        .flat_map(|f| param_notes(f, plan).into_iter().chain(ret_note(f)))
        .collect();
    notes.sort();
    notes.dedup();
    notes
}

/// A parameter that costs a copy the signature did not have to ask for.
fn param_notes(f: &Function, plan: &BindingPlan) -> Vec<String> {
    f.params
        .iter()
        .filter_map(|p| match Transfer::of(p, plan) {
            Transfer::Buffer {
                element,
                borrowed: false,
                ..
            } => Some(format!(
                "{}: `{}` is taken by value, so every call copies it; `&[{}]` \
                 would arrive without a copy",
                f.name,
                p.name,
                element.render()
            )),
            _ => None,
        })
        .collect()
}

/// A returned sequence, which allocates one per call.
///
/// Worth saying only where the caller's own buffer arrives without a copy,
/// which is what makes writing through an `&mut [T]` cheaper than handing
/// back a fresh one. The gain shows up once the sequence outgrows the
/// allocator's fast path.
fn ret_note(f: &Function) -> Option<String> {
    let Ty::List(element) = &f.ret else {
        return None;
    };
    if !element.is_primitive() {
        return None;
    }
    Some(format!(
        "{}: returning `Vec<{}>` allocates a fresh sequence per call; an \
         `&mut [{}]` parameter would let the caller reuse one",
        f.name,
        element.render(),
        element.render()
    ))
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
    let fns: Vec<&ExportedFn> = surface
        .fns
        .iter()
        .filter(|f| !skips(&f.skip, lang))
        .filter(|f| bindable(f, kind, &mirrored, &mut plan.gaps))
        .collect();

    for ty in &types {
        let class = class_of(ty, &fns, opts, kind, &mirrored, &mut plan.gaps);
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
    gaps: &mut Vec<Gap>,
) -> Class {
    let owned: Vec<&&ExportedFn> = fns
        .iter()
        .filter(|f| f.owner.as_deref() == Some(ty.name.as_str()))
        .collect();

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
        TypeKind::Struct(fields) => (accessors_of(fields, &ty.name, kind, mirrored, gaps), None),
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
fn pick_ctor<'a>(owned: &'a [&&'a ExportedFn], name: &str) -> Option<&'a ExportedFn> {
    if let Some(f) = owned.iter().find(|f| f.constructor) {
        return Some(f);
    }
    owned
        .iter()
        .find(|f| f.name == "new" && f.receiver == Receiver::None && builds(&f.ret, name))
        .map(|f| **f)
}

fn builds(ret: &Ty, name: &str) -> bool {
    matches!(ret, Ty::Class(c) if c == name)
}

/// Public fields become properties. A private one is known to exist but has
/// no reader, so it contributes nothing to bind.
fn accessors_of(
    fields: &[Field],
    owner: &str,
    kind: BindKind,
    mirrored: &BTreeSet<String>,
    gaps: &mut Vec<Gap>,
) -> Vec<Accessor> {
    let mut out = Vec::new();
    for field in fields.iter().filter(|f| f.public) {
        let at = format!("{owner}.{}", field.name);
        if let Ty::Class(nested) = &field.ty
            && !mirrored.contains(nested)
        {
            gaps.push(Gap::HandleByValue {
                at,
                ty: nested.clone(),
            });
            continue;
        }
        if field.ty.has_opaque() || unsupported(kind, &field.ty).is_some() {
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
    if f.is_async && f.receiver == Receiver::Exclusive {
        return false;
    }
    if f.is_async && kind == BindKind::CAbi {
        record(
            gaps,
            Gap::UnsupportedByBackend {
                at: f.id.clone(),
                ty: "async fn".into(),
                lang: kind.name(),
                why: "C has nothing to await with; expose a blocking wrapper \
                      instead"
                    .into(),
            },
        );
        return false;
    }
    for param in &f.params {
        if let Ty::Class(name) = &param.ty
            && param.ownership == Ownership::Owned
            && !mirrored.contains(name)
        {
            record(
                gaps,
                Gap::HandleByValue {
                    at: f.id.clone(),
                    ty: name.clone(),
                },
            );
            return false;
        }
    }
    // An optional exported type crosses back as a pointer that may be null,
    // but nothing in the model says whether a parameter wants it borrowed or
    // owned, and the two need different C.
    if kind == BindKind::CAbi {
        for param in &f.params {
            if matches!(&param.ty, Ty::Optional(inner) if matches!(**inner, Ty::Class(_))) {
                record(
                    gaps,
                    Gap::UnsupportedByBackend {
                        at: f.id.clone(),
                        ty: param.ty.render(),
                        lang: kind.name(),
                        why: "C takes an optional exported type only as a return; \
                              take it by reference instead"
                            .into(),
                    },
                );
                return false;
            }
        }
    }
    // `throws` is deliberately absent: an error crosses as a message, so its
    // type never has to be one the target language can carry.
    let tys = f.params.iter().map(|p| &p.ty).chain(once(&f.ret));
    for ty in tys {
        if ty.has_opaque() {
            return false;
        }
        if let Some(why) = unsupported(kind, ty) {
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

/// Why a language cannot carry this type, if it cannot.
fn unsupported(kind: BindKind, ty: &Ty) -> Option<String> {
    if kind == BindKind::CAbi
        && let Some(why) = unsupported_by_c(ty)
    {
        return Some(why);
    }
    match ty {
        Ty::Map(..) if kind == BindKind::Wasm => Some(
            "wasm-bindgen carries no map type; return a list of pairs, or a \
             struct with named fields"
                .into(),
        ),
        Ty::Tuple(_) if kind == BindKind::Wasm => Some(
            "wasm-bindgen carries no tuple type; return a struct with named \
             fields"
                .into(),
        ),
        Ty::List(inner) | Ty::Optional(inner) => unsupported(kind, inner),
        Ty::Map(key, value) => unsupported(kind, key).or_else(|| unsupported(kind, value)),
        Ty::Tuple(items) => items.iter().find_map(|t| unsupported(kind, t)),
        _ => None,
    }
}

/// Why C cannot carry this type, if it cannot.
///
/// C has no generic container, so anything that is not a scalar, a string, a
/// handle or a contiguous run of one primitive would need a bespoke struct
/// and a bespoke way to release it. Those are reported rather than guessed.
fn unsupported_by_c(ty: &Ty) -> Option<String> {
    match ty {
        Ty::Map(..) => Some(
            "C has no map type; return a sequence of pairs, or an exported \
             type with accessors"
                .into(),
        ),
        Ty::Tuple(_) => {
            Some("C has no tuple type; return an exported type with named fields".into())
        }
        Ty::List(inner) if !inner.is_primitive() => Some(format!(
            "a sequence of `{}` has no C spelling that owns its elements; a \
             sequence of one primitive crosses as a pointer and a length",
            inner.render()
        )),
        Ty::Optional(inner) if !matches!(**inner, Ty::Class(_)) => Some(format!(
            "`Option<{}>` has no C spelling; only an optional exported type \
             does, as a pointer that may be null",
            inner.render()
        )),
        _ => None,
    }
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
