use crate::model::{Ownership, Param, Primitive, Receiver, Ty};

use super::{BindingPlan, Class, Function};

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
    Handle { mirrored: bool, writable: bool },
    Text {
        borrowed: bool,
        /// Whether `None` (a null pointer, at the FFI boundary) is a valid
        /// value: `Ty::Optional(Str)` rather than a plain `Ty::Str`.
        nullable: bool,
    },
    /// A contiguous run of one primitive: the only shape a language can hope
    /// to hand over without copying.
    Buffer {
        element: Primitive,
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
            Ty::Str => Transfer::Text {
                borrowed,
                nullable: false,
            },
            Ty::Optional(inner) if **inner == Ty::Str => Transfer::Text {
                borrowed: param.inner_ownership != Ownership::Owned,
                nullable: true,
            },
            Ty::Bytes => Transfer::Buffer {
                element: Primitive::U8,
                borrowed,
                writable,
            },
            Ty::List(inner) => match inner.primitive() {
                Some(element) => Transfer::Buffer {
                    element,
                    borrowed,
                    writable,
                },
                None => Transfer::Collection,
            },
            ty if ty.is_primitive() || *ty == Ty::Unit => Transfer::Scalar,
            _ => Transfer::Collection,
        }
    }
}

/// What a binding layer can do with a [`Transfer::Buffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BufferSupport {
    /// A borrowed buffer arrives as a pointer into the caller's memory.
    #[default]
    ZeroCopy,
    /// Every buffer is copied whatever the signature says.
    AlwaysCopies,
    /// A borrowed buffer arrives as a pointer into the runtime's heap, and
    /// the collector is held off until the call returns.
    Pinned,
}

/// Whether a call's work can run on a thread holding no interpreter lock.
///
/// Worth asking only where enough crosses to pay for the handover, and a
/// buffer parameter is that signal: a scalar call costs less than releasing
/// the lock would. Everything the call touches has to reach the other
/// thread, which for a method means the receiver as well as the arguments.
/// Under [`BufferSupport::Pinned`] the answer is always no: a pinned section
/// cannot leave its thread or re-enter the runtime.
pub fn offloadable(f: &Function, owner: Option<&Class>, plan: &BindingPlan) -> bool {
    if plan.buffer_support == BufferSupport::Pinned {
        return false;
    }
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
pub fn transfer_notes(plan: &BindingPlan) -> Vec<String> {
    match plan.buffer_support {
        // A backend that copies every buffer whatever the signature says has
        // nothing to act on: taking a borrow saves it no copy, and writing
        // into the caller's buffer costs it one more.
        BufferSupport::AlwaysCopies => Vec::new(),
        BufferSupport::Pinned => pinned_note(plan),
        BufferSupport::ZeroCopy => {
            let mut notes: Vec<String> = plan
                .functions()
                .flat_map(|f| param_notes(f, plan).into_iter().chain(ret_note(f)))
                .collect();
            notes.sort();
            notes.dedup();
            notes
        }
    }
}

/// One note for the whole package, not one per function: every pinned call
/// already arrives without a copy, so the only thing left to say is why it
/// cannot leave the calling thread.
fn pinned_note(plan: &BindingPlan) -> Vec<String> {
    let carries_buffer = plan
        .functions()
        .flat_map(|f| f.params.iter())
        .any(|p| matches!(Transfer::of(p, plan), Transfer::Buffer { .. }));
    if !carries_buffer {
        return Vec::new();
    }
    vec![
        "a pinned buffer blocks the collector for the call; none of its work \
         can move to another thread or call back into the runtime"
            .into(),
    ]
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
