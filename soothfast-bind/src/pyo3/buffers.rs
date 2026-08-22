//! Sequences of one primitive, in both directions.
//!
//! Python reaches a contiguous run of numbers through the buffer protocol,
//! which is the difference between a binding that beats the host language and
//! one that trails it. A parameter takes a view over the caller's memory; a
//! return hands back an owned array exporting a view over Rust's.

use std::collections::BTreeMap;

use crate::model::Ty;
use crate::plan::{BindingPlan, Transfer};

/// The Rust spelling of an element type pyo3 can view through the buffer
/// protocol, or `None` for one it cannot.
pub(crate) fn buffered(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::U8 => Some("u8"),
        Ty::I8 => Some("i8"),
        Ty::U16 => Some("u16"),
        Ty::I16 => Some("i16"),
        Ty::U32 => Some("u32"),
        Ty::I32 => Some("i32"),
        Ty::U64 => Some("u64"),
        Ty::I64 => Some("i64"),
        Ty::F32 => Some("f32"),
        Ty::F64 => Some("f64"),
        _ => None,
    }
}

pub(crate) fn view_name(ty: &Ty, writable: bool) -> String {
    let element = buffered(ty).unwrap_or_default().to_uppercase();
    match writable {
        true => format!("BorrowedMut{element}"),
        false => format!("Borrowed{element}"),
    }
}

/// The buffer views the surface needs, one per element type and direction.
///
/// A setter is a parameter too, so a field of one of these types pulls in the
/// same readable view its methods use.
pub(crate) fn views(plan: &BindingPlan) -> Vec<String> {
    let mut wanted: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let params = plan.functions().flat_map(|f| f.params.iter());
    for param in params {
        let Transfer::Buffer {
            element, writable, ..
        } = Transfer::of(param, plan)
        else {
            continue;
        };
        let Some(rust) = buffered(&element) else {
            continue;
        };
        wanted.insert(view_name(&element, writable), (rust.to_string(), writable));
    }
    for element in accessor_elements(plan) {
        let Some(rust) = buffered(&element) else {
            continue;
        };
        wanted.insert(view_name(&element, false), (rust.to_string(), false));
    }
    wanted
        .into_iter()
        .map(|(name, (element, writable))| match writable {
            true => writable_view(&name, &element),
            false => readable_view(&name, &element),
        })
        .collect()
}

/// The element type of every accessor holding a sequence of one primitive.
fn accessor_elements(plan: &BindingPlan) -> Vec<Ty> {
    plan.classes
        .iter()
        .flat_map(|c| c.accessors.iter())
        .filter_map(|a| match &a.ty {
            Ty::List(inner) => Some((**inner).clone()),
            _ => None,
        })
        .collect()
}

fn readable_view(name: &str, element: &str) -> String {
    format!(
        "
/// An `{element}` sequence read through the buffer protocol when the caller
/// passes a buffer (`array.array`, `memoryview`, numpy), and unboxed element
/// by element only when it is some other sequence.
enum {name} {{
    Buffer(::pyo3::buffer::PyBuffer<{element}>),
    Owned(Vec<{element}>),
}}

impl<'py> ::pyo3::FromPyObject<'py> for {name} {{
    fn extract_bound(obj: &::pyo3::Bound<'py, ::pyo3::PyAny>) -> ::pyo3::PyResult<Self> {{
        if let Ok(buf) = ::pyo3::buffer::PyBuffer::<{element}>::get(obj)
            && buf.is_c_contiguous()
        {{
            return Ok({name}::Buffer(buf));
        }}
        Ok({name}::Owned(obj.extract()?))
    }}
}}

impl {name} {{
    fn as_slice(&self) -> &[{element}] {{
        match self {{
            // Item type and contiguity were both checked at extraction, so
            // the pointer addresses exactly `item_count` items.
            {name}::Buffer(b) => unsafe {{
                ::std::slice::from_raw_parts(b.buf_ptr() as *const {element}, b.item_count())
            }},
            {name}::Owned(v) => v,
        }}
    }}

    fn into_vec(self) -> Vec<{element}> {{
        match self {{
            {name}::Buffer(_) => self.as_slice().to_vec(),
            {name}::Owned(v) => v,
        }}
    }}
}}
"
    )
}

fn writable_view(name: &str, element: &str) -> String {
    format!(
        "
/// A borrowed mutable `{element}` sequence. Writing back in place needs a
/// real writable buffer, so unlike the read side there is no fallback.
struct {name}(::pyo3::buffer::PyBuffer<{element}>);

impl<'py> ::pyo3::FromPyObject<'py> for {name} {{
    fn extract_bound(obj: &::pyo3::Bound<'py, ::pyo3::PyAny>) -> ::pyo3::PyResult<Self> {{
        let buf = ::pyo3::buffer::PyBuffer::<{element}>::get(obj)?;
        if buf.readonly() {{
            return Err(::pyo3::exceptions::PyTypeError::new_err(
                \"expected a writable buffer\",
            ));
        }}
        if !buf.is_c_contiguous() {{
            return Err(::pyo3::exceptions::PyTypeError::new_err(
                \"expected a contiguous buffer\",
            ));
        }}
        Ok({name}(buf))
    }}
}}

impl {name} {{
    fn as_mut_slice(&mut self) -> &mut [{element}] {{
        // Writability and contiguity were both checked at extraction, so the
        // pointer addresses exactly `item_count` items and nothing aliases it.
        unsafe {{
            ::std::slice::from_raw_parts_mut(self.0.buf_ptr() as *mut {element}, self.0.item_count())
        }}
    }}
}}
"
    )
}

/// The array class a sequence of one primitive comes back as, or `None` for a
/// type with no such class.
///
/// `Vec<u8>` is deliberately absent: pyo3 already hands one back as `bytes`,
/// which is both the idiomatic Python type and already unboxed.
pub(crate) fn array_name(ty: &Ty) -> Option<String> {
    match ty {
        Ty::List(inner) => buffered(inner).map(|e| format!("{}Array", e.to_uppercase())),
        _ => None,
    }
}

/// The array classes the surface hands back, one per element type.
pub(crate) fn arrays(plan: &BindingPlan) -> Vec<(String, String)> {
    let returns = plan.functions().map(|f| &f.ret);
    let fields = plan
        .classes
        .iter()
        .flat_map(|c| c.accessors.iter())
        .map(|a| &a.ty);
    let mut wanted: BTreeMap<String, String> = BTreeMap::new();
    for ty in returns.chain(fields) {
        let sequence = match ty {
            Ty::Optional(inner) => &**inner,
            other => other,
        };
        let Ty::List(element) = sequence else {
            continue;
        };
        let (Some(name), Some(rust)) = (array_name(sequence), buffered(element)) else {
            continue;
        };
        wanted.insert(name, rust.to_string());
    }
    wanted.into_iter().collect()
}

/// The struct-module format code numpy reads an element type's dtype from.
fn format_code(element: &str) -> &'static str {
    match element {
        "u8" => "B",
        "i8" => "b",
        "u16" => "H",
        "i16" => "h",
        "u32" => "I",
        "i32" => "i",
        "u64" => "Q",
        "i64" => "q",
        "f32" => "f",
        _ => "d",
    }
}

/// One owned array class, exporting its contents through the buffer protocol.
///
/// `shape` and `strides` outlive the call that fills the view, so they are
/// boxed and handed back through `internal` rather than borrowed from a value
/// the exporting method no longer owns.
pub(crate) fn array(name: &str, element: &str) -> String {
    let code = format_code(element);
    format!(
        "
/// An owned `{element}` sequence. `memoryview(x)` and `numpy.asarray(x)` read
/// it without copying; `x.tolist()` copies it into a plain list.
#[pyclass(name = \"{name}\")]
pub struct {name}(Vec<{element}>);

impl {name} {{
    fn new(values: Vec<{element}>) -> Self {{
        {name}(values)
    }}
}}

#[pymethods]
impl {name} {{
    fn __len__(&self) -> usize {{
        self.0.len()
    }}

    fn __getitem__(&self, index: isize) -> ::pyo3::PyResult<{element}> {{
        let at = match index < 0 {{
            true => index + self.0.len() as isize,
            false => index,
        }};
        usize::try_from(at)
            .ok()
            .and_then(|at| self.0.get(at))
            .copied()
            .ok_or_else(|| ::pyo3::exceptions::PyIndexError::new_err(\"index out of range\"))
    }}

    fn __iter__<'py>(
        &self,
        py: ::pyo3::Python<'py>,
    ) -> ::pyo3::PyResult<::pyo3::Bound<'py, ::pyo3::PyAny>> {{
        use ::pyo3::types::PyAnyMethods;
        Ok(::pyo3::types::PyList::new(py, &self.0)?.try_iter()?.into_any())
    }}

    fn __repr__(&self) -> String {{
        format!(\"{name}({{:?}})\", self.0)
    }}

    /// The same values as a plain Python list.
    fn tolist(&self) -> Vec<{element}> {{
        self.0.clone()
    }}

    unsafe fn __getbuffer__(
        slf: ::pyo3::Bound<'_, Self>,
        view: *mut ::pyo3::ffi::Py_buffer,
        flags: ::std::os::raw::c_int,
    ) -> ::pyo3::PyResult<()> {{
        if view.is_null() {{
            return Err(::pyo3::exceptions::PyBufferError::new_err(\"view is null\"));
        }}
        if (flags & ::pyo3::ffi::PyBUF_WRITABLE) == ::pyo3::ffi::PyBUF_WRITABLE {{
            return Err(::pyo3::exceptions::PyBufferError::new_err(
                \"{name} is read-only\",
            ));
        }}
        let size = ::std::mem::size_of::<{element}>() as isize;
        let (buf, count) = {{
            let values = &slf.borrow().0;
            (values.as_ptr() as *mut ::std::ffi::c_void, values.len() as isize)
        }};
        let meta = Box::into_raw(Box::new([count, size]));
        unsafe {{
            (*view).obj = slf.into_ptr();
            (*view).buf = buf;
            (*view).len = count * size;
            (*view).readonly = 1;
            (*view).itemsize = size;
            (*view).ndim = 1;
            (*view).format = match (flags & ::pyo3::ffi::PyBUF_FORMAT) == ::pyo3::ffi::PyBUF_FORMAT
            {{
                true => c\"{code}\".as_ptr() as *mut ::std::os::raw::c_char,
                false => ::std::ptr::null_mut(),
            }};
            (*view).shape = match (flags & ::pyo3::ffi::PyBUF_ND) == ::pyo3::ffi::PyBUF_ND {{
                true => meta as *mut isize,
                false => ::std::ptr::null_mut(),
            }};
            (*view).strides = match (flags & ::pyo3::ffi::PyBUF_STRIDES)
                == ::pyo3::ffi::PyBUF_STRIDES
            {{
                true => (meta as *mut isize).add(1),
                false => ::std::ptr::null_mut(),
            }};
            (*view).suboffsets = ::std::ptr::null_mut();
            (*view).internal = meta as *mut ::std::ffi::c_void;
        }}
        Ok(())
    }}

    unsafe fn __releasebuffer__(&self, view: *mut ::pyo3::ffi::Py_buffer) {{
        // Paired with the `Box::into_raw` every filled view carries out.
        unsafe {{ drop(Box::from_raw((*view).internal as *mut [isize; 2])) }};
    }}
}}
"
    )
}
