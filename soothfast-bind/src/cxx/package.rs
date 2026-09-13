//! The README section describing the C++ wrapper.

pub(crate) fn readme_section(namespace: &str, header: &str) -> String {
    format!(
        "\n## C++\n\n\
         This package also ships a header-only C++20 wrapper (`{header}.hpp`) \
         generated over the same header and library, in namespace \
         `{namespace}`. Build the library first, then \
         `#include \"{header}.hpp\"` and link against it the same way a C \
         consumer would.\n"
    )
}
