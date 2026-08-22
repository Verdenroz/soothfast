//! The exported Rust surface, as the walkers read it out of rustdoc JSON.
//!
//! Types carry ownership and nullability explicitly rather than leaving them
//! to a marshaling framework, so a backend with no framework at all (a C ABI
//! over opaque pointers) can lower this without guessing.

/// A type in an exported signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Unit,
    Bool,
    I8,
    I16,
    I32,
    I64,
    ISize,
    U8,
    U16,
    U32,
    U64,
    USize,
    F32,
    F64,
    /// `String`, `&str`, `Cow<str>`.
    Str,
    /// `Vec<u8>` and `&[u8]`, distinguished from `List(U8)` because every
    /// target language has a byte-string type that is not a list of numbers.
    Bytes,
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Optional(Box<Ty>),
    Tuple(Vec<Ty>),
    /// An exported struct or enum, by name.
    Class(String),
    /// A type with no known mapping. Always accompanied by a
    /// [`crate::gap::Gap`]; never emitted as a guess.
    Opaque(String),
}

impl Ty {
    /// Whether this type, or anything inside it, has no known mapping.
    pub fn has_opaque(&self) -> bool {
        match self {
            Ty::Opaque(_) => true,
            Ty::List(inner) | Ty::Optional(inner) => inner.has_opaque(),
            Ty::Map(key, value) => key.has_opaque() || value.has_opaque(),
            Ty::Tuple(items) => items.iter().any(Ty::has_opaque),
            _ => false,
        }
    }

    /// How the type reads in Rust, for gap reports.
    pub fn render(&self) -> String {
        match self {
            Ty::Unit => "()".into(),
            Ty::Bool => "bool".into(),
            Ty::I8 => "i8".into(),
            Ty::I16 => "i16".into(),
            Ty::I32 => "i32".into(),
            Ty::I64 => "i64".into(),
            Ty::ISize => "isize".into(),
            Ty::U8 => "u8".into(),
            Ty::U16 => "u16".into(),
            Ty::U32 => "u32".into(),
            Ty::U64 => "u64".into(),
            Ty::USize => "usize".into(),
            Ty::F32 => "f32".into(),
            Ty::F64 => "f64".into(),
            Ty::Str => "String".into(),
            Ty::Bytes => "Vec<u8>".into(),
            Ty::List(inner) => format!("Vec<{}>", inner.render()),
            Ty::Map(key, value) => format!("HashMap<{}, {}>", key.render(), value.render()),
            Ty::Optional(inner) => format!("Option<{}>", inner.render()),
            Ty::Tuple(items) => {
                let rendered: Vec<String> = items.iter().map(Ty::render).collect();
                format!("({})", rendered.join(", "))
            }
            Ty::Class(name) | Ty::Opaque(name) => name.clone(),
        }
    }

    /// Whether this is a scalar a binding layer can carry as-is.
    pub fn is_primitive(&self) -> bool {
        matches!(
            self,
            Ty::Bool
                | Ty::I8
                | Ty::I16
                | Ty::I32
                | Ty::I64
                | Ty::ISize
                | Ty::U8
                | Ty::U16
                | Ty::U32
                | Ty::U64
                | Ty::USize
                | Ty::F32
                | Ty::F64
        )
    }

    /// Every exported type named anywhere inside this one.
    pub fn classes(&self, out: &mut Vec<String>) {
        match self {
            Ty::Class(name) => out.push(name.clone()),
            Ty::List(inner) | Ty::Optional(inner) => inner.classes(out),
            Ty::Map(key, value) => {
                key.classes(out);
                value.classes(out);
            }
            Ty::Tuple(items) => items.iter().for_each(|t| t.classes(out)),
            _ => {}
        }
    }
}

/// How a parameter takes its argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Owned,
    Borrowed,
    BorrowedMut,
}

/// How a method takes its instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver {
    /// A free function or associated fn: no instance.
    None,
    /// `&self`.
    Shared,
    /// `&mut self`.
    Exclusive,
    /// `self`.
    Consuming,
}

/// One argument of an exported function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
    pub ownership: Ownership,
}

/// A named field of an exported struct, or of a struct-like enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    /// Private fields are read out so the shape is known to be complete, but
    /// no accessor can be generated for them.
    pub public: bool,
    pub doc: Option<String>,
}

/// The payload of one enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantFields {
    Unit,
    Tuple(Vec<Ty>),
    Named(Vec<Field>),
}

/// One variant of an exported enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub name: String,
    pub fields: VariantFields,
    pub doc: Option<String>,
}

/// Whether a variant list carries no payload anywhere.
///
/// Every target language has a plain enumeration to mirror this onto, so it
/// crosses by value; one carrying data has to stay an opaque handle.
pub fn is_plain(variants: &[Variant]) -> bool {
    variants.iter().all(|v| v.fields == VariantFields::Unit)
}

/// The shape of an exported type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeKind {
    Struct(Vec<Field>),
    Enum(Vec<Variant>),
}

/// What the registry knows about one exported item.
///
/// Identity, opt-outs, and the doc summary come from the annotation rather
/// than rustdoc: `#[soothfast::export]` is an attribute macro, so it is gone
/// by the time rustdoc sees the item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportRecord {
    pub id: String,
    /// `fn`, `method`, `struct`, or `enum`.
    pub kind: String,
    pub fingerprint: u64,
    pub skip: Vec<String>,
    pub owner: Option<String>,
    pub constructor: bool,
    pub summary: Option<String>,
}

/// A function, associated fn, or method annotated with `#[soothfast::export]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedFn {
    /// Registry id: `module_path`, then the owning type, then the fn name.
    pub id: String,
    /// Path a glue crate calls it by.
    pub rust_path: String,
    pub name: String,
    /// The type this belongs to, for an associated fn or method.
    pub owner: Option<String>,
    pub receiver: Receiver,
    pub params: Vec<Param>,
    pub ret: Ty,
    /// The `E` of a `Result<T, E>` return. `T` is in [`ExportedFn::ret`].
    pub throws: Option<Ty>,
    pub is_async: bool,
    /// Declared the type's constructor by `#[soothfast::export(constructor)]`.
    pub constructor: bool,
    pub doc: Option<String>,
    /// Languages this item opts out of. Empty binds every configured one.
    pub skip: Vec<String>,
}

/// A struct or enum annotated with `#[soothfast::export]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedType {
    pub id: String,
    pub rust_path: String,
    pub name: String,
    pub kind: TypeKind,
    /// Whether the Rust type is `Send`. A backend that binds across threads
    /// needs to know before it decides how to hold one.
    pub send: bool,
    /// Whether the Rust type is `Sync`, taken only from an auto-trait impl
    /// the document actually carries. A backend that hands `&T` to another
    /// thread cannot afford to assume this one.
    pub sync: bool,
    pub doc: Option<String>,
    pub skip: Vec<String>,
}

impl ExportedType {
    /// Whether the type mirrors onto a plain enumeration rather than a handle.
    pub fn is_plain_enum(&self) -> bool {
        matches!(&self.kind, TypeKind::Enum(variants) if is_plain(variants))
    }
}

/// Everything `#[soothfast::export]` declared in one package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Surface {
    pub fns: Vec<ExportedFn>,
    pub types: Vec<ExportedType>,
}

impl Surface {
    /// Contract fingerprint per exported item, keyed by registry id.
    ///
    /// Covers what a binding exposes and nothing else, so editing a function
    /// body leaves it alone while changing a parameter type does not.
    pub fn fingerprints(&self) -> std::collections::BTreeMap<String, String> {
        let fns = self.fns.iter().map(|f| (f.id.clone(), render_fn(f)));
        let types = self.types.iter().map(|t| (t.id.clone(), render_type(t)));
        fns.chain(types)
            .map(|(id, text)| {
                let fingerprint = soothfast_registry::fnv1a(text.as_bytes());
                (id, format!("{fingerprint:016x}"))
            })
            .collect()
    }
}

fn render_fn(f: &ExportedFn) -> String {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}:{:?}:{}", p.name, p.ownership, p.ty.render()))
        .collect();
    format!(
        "fn {} {:?}({}) -> {}{}{}{}",
        f.name,
        f.receiver,
        params.join(","),
        f.ret.render(),
        f.throws
            .as_ref()
            .map(|t| format!(" ! {}", t.render()))
            .unwrap_or_default(),
        if f.is_async { " async" } else { "" },
        format_args!(" skip[{}]", f.skip.join(",")),
    )
}

fn render_type(t: &ExportedType) -> String {
    let body = match &t.kind {
        TypeKind::Struct(fields) => fields
            .iter()
            .map(render_field)
            .collect::<Vec<_>>()
            .join(","),
        TypeKind::Enum(variants) => variants
            .iter()
            .map(|v| format!("{}{}", v.name, render_variant(&v.fields)))
            .collect::<Vec<_>>()
            .join(","),
    };
    format!("type {} {{{body}}} skip[{}]", t.name, t.skip.join(","))
}

fn render_field(field: &Field) -> String {
    let visibility = if field.public { "pub " } else { "" };
    format!("{visibility}{}:{}", field.name, field.ty.render())
}

fn render_variant(fields: &VariantFields) -> String {
    match fields {
        VariantFields::Unit => String::new(),
        VariantFields::Tuple(tys) => {
            let rendered: Vec<String> = tys.iter().map(Ty::render).collect();
            format!("({})", rendered.join(","))
        }
        VariantFields::Named(fields) => {
            let rendered: Vec<String> = fields.iter().map(render_field).collect();
            format!("{{{}}}", rendered.join(","))
        }
    }
}
