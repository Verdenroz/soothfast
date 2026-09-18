use soothfast_bind::BindKind;

use crate::fixture::lua_opts;
use crate::{check_goldens_with, emit_set_with};

#[test]
fn lua_goldens() {
    check_goldens_with(BindKind::Lua, "lua", &lua_opts());
}

#[test]
fn the_lua_module_lands_at_the_require_paths_own_directory() {
    let files = emit_set_with(BindKind::Lua, &lua_opts()).files;
    assert!(files.contains_key("acme/core.lua"), "{:?}", files.keys());
    assert!(files.contains_key("core.h"));
    assert!(files.contains_key("core.pc"));
}

#[test]
fn the_cdef_block_declares_the_same_symbols_the_header_would() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    for symbol in [
        "typedef struct core_counter core_counter;",
        "core_counter * core_counter_new(int64_t start);",
        "void core_counter_free(core_counter *handle);",
        "int64_t core_counter_bump(const core_counter *handle, int64_t by, char **error);",
    ] {
        assert!(lua.contains(symbol), "cdef omits {symbol}");
    }
    assert!(
        !lua.contains("#include"),
        "ffi.cdef cannot read a preprocessor directive"
    );
    assert!(
        !lua.contains("extern \"C\""),
        "ffi.cdef cannot read a C++ linkage block"
    );
}

#[test]
fn a_handle_is_freed_through_ffi_gc_and_an_idempotent_close() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("ffi.gc(ptr, lib.core_counter_free)"));
    assert!(lua.contains("function Counter:close()"));
    assert!(lua.contains("if self.ptr == nil then"));
    assert!(lua.contains("ffi.gc(self.ptr, nil)"));
    assert!(lua.contains("lib.core_counter_free(self.ptr)"));
}

#[test]
fn a_failing_call_raises_after_freeing_the_message() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("local err = ffi.new(\"char*[1]\")"));
    assert!(lua.contains("error(lua_string(err[0]))"));
    assert!(lua.contains("lib.core_string_free(s)"));
}

#[test]
fn a_lua_plain_enum_crosses_as_a_validated_string_not_an_ordinal() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("local LEVEL_TO_C = { low = 0, high = 1 }"));
    assert!(lua.contains("invalid Level: "));
    assert!(
        !lua.contains("local Level = {}"),
        "a plain enum gets no wrapper class"
    );
}

#[test]
fn a_buffer_parameter_accepts_a_matching_cdata_array_without_copying() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("ffi.istype(\"double[?]\", value)"));
    assert!(lua.contains("return value, ffi.sizeof(value) / ffi.sizeof(\"double\")"));
}

#[test]
fn a_writable_buffer_writes_back_into_a_plain_table_only() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("if type(out) == \"table\" then"));
    assert!(lua.contains("out[i] = out_ptr[i - 1]"));
}

#[test]
fn a_lua_buffer_call_copies_with_no_transfer_notes() {
    let notes = emit_set_with(BindKind::Lua, &lua_opts()).notes;
    assert!(
        notes
            .iter()
            .all(|n| !n.contains("would arrive without a copy")
                && !n.contains("allocates a fresh sequence")),
        "lua copies every buffer regardless of the signature unless the \
         caller already holds a matching FFI array: {notes:?}"
    );
}

#[test]
fn a_returned_array_is_a_metatyped_cdata_not_a_copied_table() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("ffi.metatype(\"core_f64_array\", {"));
    assert!(lua.contains("__len = function(self) return tonumber(self.len) end,"));
    assert!(lua.contains("if key == \"totable\" then"));
    assert!(lua.contains(
        "if key % 1 ~= 0 or key < 1 or key > tonumber(self.len) then\n\t\t\t\terror(\"core_f64_array index out of range: \" .. tostring(key))"
    ));
    assert!(lua.contains("return ffi.gc(ret, lib.core_f64_array_free)"));
    assert!(
        !lua.contains("_to_table"),
        "the array-to-table copier is gone: returns cross as cdata"
    );
}

#[test]
fn a_returned_array_can_be_closed_like_a_handle() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("local function core_f64_array_close(self)"));
    assert!(lua.contains("if self.data == nil then"));
    assert!(lua.contains("ffi.gc(self, nil)\n\tlib.core_f64_array_free(self)"));
    assert!(lua.contains("self.len = 0\n\tself.data = nil"));
    assert!(lua.contains("if key == \"close\" then\n\t\t\treturn core_f64_array_close"));
}

#[test]
fn a_buffer_argument_accepts_a_returned_array_with_no_copy() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("if ffi.istype(\"core_f64_array\", value) then"));
    assert!(lua.contains("return value.data, tonumber(value.len)"));
    assert!(
        !lua.contains("core_i64_array"),
        "i64 is only ever a buffer element here, never a return: naming an \
         undeclared array struct in ffi.istype would be a parse error"
    );
}

#[test]
fn a_parameter_named_after_luas_own_raise_builtin_is_escaped() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("function M.stamp(handle, error_, register)"));
}

#[test]
fn an_async_method_is_a_gap_with_no_lua_runtime_story() {
    let set = emit_set_with(BindKind::Lua, &lua_opts());
    assert!(!set.files["acme/core.lua"].contains("refresh"));
    assert!(
        set.gaps
            .iter()
            .any(|g| g.contains("no Lua runtime story yet"))
    );
}
