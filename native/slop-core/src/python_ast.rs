//! Small traversal helpers over CPython's canonical AST objects.

use pyo3::prelude::*;
use pyo3::types::PyList;

pub(crate) fn type_name(value: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(value.get_type().name()?.to_string())
}

pub(crate) fn is_ast(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    value.hasattr("_fields")
}

pub(crate) fn ast_fields<'py>(
    node: &Bound<'py, PyAny>,
) -> PyResult<Vec<(String, Bound<'py, PyAny>)>> {
    let mut output = Vec::new();
    for field in node.getattr("_fields")?.try_iter()? {
        let field: String = field?.extract()?;
        output.push((field.clone(), node.getattr(field.as_str())?));
    }
    Ok(output)
}

pub(crate) fn ast_children<'py>(node: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let mut output = Vec::new();
    for (_, value) in ast_fields(node)? {
        if is_ast(&value)? {
            output.push(value);
        } else if let Ok(items) = value.cast::<PyList>() {
            for item in items.iter() {
                if is_ast(&item)? {
                    output.push(item);
                }
            }
        }
    }
    Ok(output)
}

pub(crate) fn descendants<'py>(root: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let mut output = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        let children = ast_children(&node)?;
        output.push(node);
        stack.extend(children.into_iter().rev());
    }
    Ok(output)
}

pub(crate) fn list_items<'py>(value: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    Ok(value.cast::<PyList>()?.iter().collect())
}

pub(crate) fn string_attr(node: &Bound<'_, PyAny>, name: &str) -> PyResult<String> {
    node.getattr(name)?.extract()
}
