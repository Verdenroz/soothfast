//! The language-neutral SDK model every emitter renders from.

/// A wire type, lowered from JSON Schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Str,
    Int,
    Float,
    Bool,
    /// An open schema — a gap, `serde_json::Value`, or anything else the
    /// source could not pin down.
    Any,
    /// No body at all.
    Unit,
    List(Box<Ty>),
    /// String-keyed map with uniform values.
    Map(Box<Ty>),
    Optional(Box<Ty>),
    /// A named component model.
    Named(String),
    /// `oneOf` arms, matched structurally when decoding.
    Union(Vec<Ty>),
    /// A closed set of string values.
    Literal(Vec<String>),
}

/// One field of a [`Model`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Attribute name in the target language.
    pub attr: String,
    /// Property name on the wire.
    pub wire: String,
    pub ty: Ty,
    pub required: bool,
    pub doc: Option<String>,
}

/// A named object schema, emitted as a class/interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub name: String,
    pub doc: Option<String>,
    pub fields: Vec<Field>,
}

/// Where a request parameter travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamLoc {
    Query,
    Path,
    Header,
}

/// One parameter of a [`Method`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Argument name in the target language.
    pub attr: String,
    /// Parameter name on the wire.
    pub wire: String,
    pub location: ParamLoc,
    pub ty: Ty,
    pub required: bool,
    pub doc: Option<String>,
}

/// One client method, lowered from an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    /// Method name in the target language.
    pub name: String,
    pub operation_id: String,
    pub http_method: String,
    /// Route template with `{placeholder}` segments.
    pub path: String,
    pub doc: Option<String>,
    pub params: Vec<Param>,
    /// Request body type, when the operation takes one.
    pub body: Option<Ty>,
    /// Success response type.
    pub ok: Ty,
    pub ok_status: u16,
    /// Whether the response switches to a page envelope when the
    /// pagination parameters are sent.
    pub paginated: bool,
}

/// The whole lowered SDK.
#[derive(Debug, Clone, Default)]
pub struct Sdk {
    /// Component models, sorted by name.
    pub models: Vec<Model>,
    /// Non-object components, as named aliases sorted by name.
    pub aliases: Vec<(String, Ty)>,
    /// Methods, sorted by operation id.
    pub methods: Vec<Method>,
    /// What lowering could not express precisely, deduplicated.
    pub notes: Vec<String>,
}
