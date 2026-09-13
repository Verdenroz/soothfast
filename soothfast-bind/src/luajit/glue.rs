//! `<name>.lua`: the LuaJIT wrapper over the C backend's own ABI.
//!
//! Every exported item becomes ordinary Lua over the flat symbols the C
//! backend already declared: a handle is a table with a `ptr` field, freed
//! through its `*_free` and registered with `ffi.gc` as a backstop, and a
//! failing call raises through Lua's own `error`.

use std::collections::{BTreeMap, BTreeSet};

use crate::GENERATED_LUA;
use crate::cabi::glue::{arrays, returns_text};
use crate::cabi::header;
use crate::cabi::types as c;
use crate::model::{Param, Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};

use super::types::lua_ident;

/// Render the package's one Lua module.
pub(crate) fn render(plan: &BindingPlan, module: &str) -> String {
    let mut out = format!("{GENERATED_LUA}local ffi = require(\"ffi\")\n\nffi.cdef[[\n");
    out.push_str(&header::declarations(plan, module));
    out.push_str(&format!("]]\n\nlocal lib = ffi.load(\"{module}\")\n\n"));

    if returns_text(plan) {
        out.push_str(&string_helper(module));
    }
    let array_elements: BTreeSet<String> = arrays(plan).into_iter().map(|(_, e)| e).collect();
    for element in buffer_elements(plan) {
        out.push_str(&buffer_helper(&element, module, &array_elements));
    }
    for (ty, element) in arrays(plan) {
        out.push_str(&array_helper(&ty, &element, module));
    }

    // Every class and enum is declared before any method body is rendered:
    // a local declared later in the file is invisible to a function literal
    // that appears earlier, however later it is actually called, so a
    // method on one class could not otherwise call another class's helper
    // if the plan happened to order that class first.
    out.push_str("local M = {}\n\n");
    for class in &plan.classes {
        out.push_str(&class_decl(class, module));
    }
    for class in &plan.classes {
        out.push_str(&class_members(class, plan, module));
    }
    for function in &plan.functions {
        out.push_str(&function_block(function, None, plan, module));
    }
    out.push_str("return M\n");
    out
}

/// Every scalar a buffer parameter is built from, each needing its own
/// `<elem>_buf` conversion helper.
fn buffer_elements(plan: &BindingPlan) -> Vec<Ty> {
    let mut wanted: BTreeMap<String, Ty> = BTreeMap::new();
    for f in plan.functions() {
        for param in &f.params {
            if let Transfer::Buffer { element, .. } = Transfer::of(param, plan) {
                wanted.insert(element.render(), element);
            }
        }
    }
    wanted.into_values().collect()
}

/// Converts a buffer argument into a pointer and a length. A caller already
/// holding a matching FFI array (built with `ffi.new("<ctype>[?]", n)`) or a
/// returned array struct of the same element passes it through unboxed; a
/// plain Lua table is copied into one, boxed number by boxed number, since a
/// table has no contiguous memory to hand over.
///
/// The array-struct check is only emitted for an element that actually has
/// one declared in this module's `ffi.cdef` block: naming one that was never
/// declared (an element used only as a buffer, never as a return) is a
/// parse error the moment `ffi.istype` sees the string.
fn buffer_helper(element: &Ty, module: &str, array_elements: &BTreeSet<String>) -> String {
    let spelling = c::scalar(element).expect("checked buffer element");
    let name = format!("{}_buf", spelling.rust);
    let array_check = match array_elements.contains(&spelling.rust) {
        true => format!(
            "\tif ffi.istype(\"{module}_{}_array\", value) then\n\
             \t\treturn value.data, tonumber(value.len)\n\
             \tend\n",
            spelling.rust,
        ),
        false => String::new(),
    };
    format!(
        "local function {name}(value)\n\
         \t-- a VLA cdata array carries no `#`; its instance size does.\n\
         \tif ffi.istype(\"{c}[?]\", value) then\n\
         \t\treturn value, ffi.sizeof(value) / ffi.sizeof(\"{c}\")\n\
         \tend\n\
         {array_check}\
         \tlocal n = #value\n\
         \tlocal arr = ffi.new(\"{c}[?]\", n)\n\
         \tfor i = 1, n do\n\
         \t\tarr[i - 1] = value[i]\n\
         \tend\n\
         \treturn arr, n\n\
         end\n\n",
        c = spelling.c,
    )
}

/// Copies a string this library returned and releases it. Doubles as the
/// error path: a fallible call's message is released the same way.
fn string_helper(module: &str) -> String {
    format!(
        "local function lua_string(s)\n\
         \tlocal text = ffi.string(s)\n\
         \tlib.{module}_string_free(s)\n\
         \treturn text\n\
         end\n\n"
    )
}

/// Registers the array struct as its own cdata type: `#arr` and `arr[i]`
/// read straight through the returned buffer, and `arr:totable()` copies it
/// into a plain table for code that wants one. `data`/`len` are real struct
/// fields, so they resolve before `__index` ever sees them.
fn array_helper(ty: &Ty, element: &str, module: &str) -> String {
    let c_arr = c::array_c(ty, module);
    let totable = format!("{c_arr}_totable");
    format!(
        "-- Copies a `{element}` array into a plain table.\n\
         local function {totable}(self)\n\
         \tlocal n = tonumber(self.len)\n\
         \tlocal out = {{}}\n\
         \tfor i = 1, n do\n\
         \t\tout[i] = self.data[i - 1]\n\
         \tend\n\
         \treturn out\n\
         end\n\n\
         ffi.metatype(\"{c_arr}\", {{\n\
         \t__len = function(self) return tonumber(self.len) end,\n\
         \t__index = function(self, key)\n\
         \t\tif key == \"totable\" then\n\
         \t\t\treturn {totable}\n\
         \t\tend\n\
         \t\tif type(key) == \"number\" then\n\
         \t\t\tif key < 1 or key > tonumber(self.len) then\n\
         \t\t\t\terror(\"{c_arr} index out of range: \" .. tostring(key))\n\
         \t\t\tend\n\
         \t\t\treturn self.data[key - 1]\n\
         \t\tend\n\
         \tend,\n\
         }})\n\n"
    )
}

/// The class table, its metatable, its constructor/destructor pair, and its
/// registration in `M`: everything another class's method might need to call
/// before any method body is rendered.
fn class_decl(class: &Class, module: &str) -> String {
    if class.is_plain_enum() {
        return enum_block(class);
    }
    let name = &class.name;
    let lower = c::snake(name);
    let handle = c::handle_c(name, module);
    format!(
        "local {name} = {{}}\n{name}.__index = {name}\nM.{name} = {name}\n\n\
         local function wrap_{lower}(ptr)\n\
         \treturn setmetatable({{ ptr = ffi.gc(ptr, lib.{handle}_free) }}, {name})\n\
         end\n\n\
         function {name}:close()\n\
         \tif self.ptr == nil then\n\
         \t\treturn\n\
         \tend\n\
         \tffi.gc(self.ptr, nil)\n\
         \tlib.{handle}_free(self.ptr)\n\
         \tself.ptr = nil\n\
         end\n\n"
    )
}

fn class_members(class: &Class, plan: &BindingPlan, module: &str) -> String {
    if class.is_plain_enum() {
        return String::new();
    }
    let mut out = String::new();
    if let Some(ctor) = &class.ctor {
        out.push_str(&function_block(ctor, Some(class), plan, module));
    }
    for accessor in &class.accessors {
        out.push_str(&accessor_block(accessor, class, plan, module));
    }
    for function in class.methods.iter().chain(class.statics.iter()) {
        out.push_str(&function_block(function, Some(class), plan, module));
    }
    out
}

/// A payload-free enum crosses as a validated string rather than a wrapped
/// class, checked against the variant names on the way in and looked back up
/// on the way out.
fn enum_block(class: &Class) -> String {
    let lower = c::snake(&class.name);
    let upper = lower.to_uppercase();
    let variants: Vec<String> = class
        .variants
        .iter()
        .flatten()
        .map(|v| c::snake(&v.name))
        .collect();
    let to_c: String = variants
        .iter()
        .enumerate()
        .map(|(i, v)| format!("{v} = {i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let from_c: String = variants
        .iter()
        .enumerate()
        .map(|(i, v)| format!("[{i}] = \"{v}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "local {upper}_TO_C = {{ {to_c} }}\n\
         local {upper}_FROM_C = {{ {from_c} }}\n\n\
         local function {lower}_to_c(name)\n\
         \tlocal v = {upper}_TO_C[name]\n\
         \tif v == nil then\n\
         \t\terror(\"invalid {class}: \" .. tostring(name))\n\
         \tend\n\
         \treturn v\n\
         end\n\n\
         local function {lower}_from_c(value)\n\
         \treturn {upper}_FROM_C[tonumber(value)]\n\
         end\n\n",
        class = class.name,
    )
}

fn accessor_block(accessor: &Accessor, class: &Class, plan: &BindingPlan, module: &str) -> String {
    let name = lua_ident(&accessor.field);
    let symbol = format!(
        "{}_{}",
        c::handle_c(&class.name, module),
        c::snake(&accessor.field)
    );
    let call = format!("lib.{symbol}(self.ptr)");
    format!(
        "function {}:{name}()\n\treturn {}\nend\n\n",
        class.name,
        returned(&call, &accessor.ty, plan, module),
    )
}

fn function_block(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    module: &str,
) -> String {
    let name = lua_ident(&function.name);
    let params: String = function
        .params
        .iter()
        .map(|p| lua_ident(&p.name))
        .collect::<Vec<_>>()
        .join(", ");
    let header = match (owner, function.receiver) {
        (Some(class), Receiver::None) => format!("function {}.{name}({params})", class.name),
        (Some(class), _) => format!("function {}:{name}({params})", class.name),
        (None, _) => format!("function M.{name}({params})"),
    };
    format!(
        "{header}\n{}end\n\n",
        function_body(function, owner, plan, module)
    )
}

fn function_body(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    module: &str,
) -> String {
    let mut lines: Vec<String> = Vec::new();

    for param in &function.params {
        if let Transfer::Buffer { element, .. } = Transfer::of(param, plan) {
            let helper = format!("{}_buf", c::scalar(&element).expect("checked").rust);
            let name = lua_ident(&param.name);
            lines.push(format!("\tlocal {name}_ptr, {name}_len = {helper}({name})"));
        }
    }
    if function.throws.is_some() {
        lines.push("\tlocal err = ffi.new(\"char*[1]\")".to_string());
    }

    let call = call_expr(function, owner, plan);
    let needs_ret = function.throws.is_some() || function.ret != Ty::Unit;
    match needs_ret {
        true => lines.push(format!("\tlocal ret = {call}")),
        false => lines.push(format!("\t{call}")),
    }

    if function.throws.is_some() {
        lines.push("\tif err[0] ~= nil then".to_string());
        lines.push("\t\terror(lua_string(err[0]))".to_string());
        lines.push("\tend".to_string());
    }

    for param in &function.params {
        if let Transfer::Buffer { writable: true, .. } = Transfer::of(param, plan) {
            let name = lua_ident(&param.name);
            lines.push(format!("\tif type({name}) == \"table\" then"));
            lines.push(format!("\t\tfor i = 1, {name}_len do"));
            lines.push(format!("\t\t\t{name}[i] = {name}_ptr[i - 1]"));
            lines.push("\t\tend".to_string());
            lines.push("\tend".to_string());
        }
    }

    if function.ret != Ty::Unit {
        lines.push(format!(
            "\treturn {}",
            returned("ret", &function.ret, plan, module)
        ));
    }

    lines.join("\n") + "\n"
}

fn call_expr(function: &Function, owner: Option<&Class>, plan: &BindingPlan) -> String {
    let mut args: Vec<String> = Vec::new();
    if let (Some(_), receiver) = (owner, function.receiver)
        && receiver != Receiver::None
    {
        args.push("self.ptr".to_string());
    }
    for param in &function.params {
        args.push(call_arg(param, plan));
    }
    if function.throws.is_some() {
        args.push("err".to_string());
    }
    format!("lib.{}({})", function.symbol, args.join(", "))
}

fn call_arg(param: &Param, plan: &BindingPlan) -> String {
    let name = lua_ident(&param.name);
    match Transfer::of(param, plan) {
        Transfer::Buffer { .. } => format!("{name}_ptr, {name}_len"),
        Transfer::Text { .. } => name,
        Transfer::Handle { mirrored: true, .. } => {
            format!("{}_to_c({name})", c::snake(class_name(&param.ty)))
        }
        Transfer::Handle { .. } => format!("{name}.ptr"),
        _ => name,
    }
}

fn class_name(ty: &Ty) -> &str {
    match ty {
        Ty::Class(name) => name,
        Ty::Optional(inner) => class_name(inner),
        _ => "",
    }
}

/// A value coming back from the library, converted into the shape the
/// signature promised.
fn returned(expr: &str, ty: &Ty, plan: &BindingPlan, module: &str) -> String {
    match ty {
        Ty::Unit => expr.to_string(),
        Ty::Str => format!("lua_string({expr})"),
        Ty::Class(name) if plan.is_mirrored(name) => format!("{}_from_c({expr})", c::snake(name)),
        Ty::Class(name) => format!("wrap_{}({expr})", c::snake(name)),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => format!(
                "(function() local p = {expr}; if p == nil then return nil end; \
                 return wrap_{}(p) end)()",
                c::snake(name)
            ),
            Ty::Str => format!(
                "(function() local p = {expr}; if p == nil then return nil end; \
                 return lua_string(p) end)()"
            ),
            _ => expr.to_string(),
        },
        ty if c::element(ty).is_some() => {
            format!("ffi.gc({expr}, lib.{}_free)", c::array_c(ty, module))
        }
        _ => expr.to_string(),
    }
}
