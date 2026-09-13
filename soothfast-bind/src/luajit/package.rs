//! The README section describing the LuaJIT module, appended to the C
//! backend's own README.

pub(crate) fn readme_section(module: &str) -> String {
    format!(
        "\n## Lua\n\n\
         This package also ships a LuaJIT module generated over the same \
         header and library. `require` it once the shared library the C \
         backend built is reachable through `ffi.load(\"{module}\")`'s \
         search (set `LD_LIBRARY_PATH`, or place it somewhere the \
         platform's loader already looks).\n\n\
         A plain Lua table crossing a buffer parameter is copied element by \
         element, boxed number by boxed number. A caller already holding a \
         matching FFI array, built with `ffi.new(\"<ctype>[?]\", n)`, passes \
         it through with no copy instead.\n"
    )
}
