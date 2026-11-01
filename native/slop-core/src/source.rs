//! One immutable source bound to both native ASTs. Multiple checks share parsing.
use crate::engine;
use ruff_python_ast::ModModule;
use tree_sitter::Tree;

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_NODES: usize = 65536;
pub const MAX_DEPTH: usize = 128;

pub struct PythonSource<'a> {
    source: &'a str,
    tree: Tree,
    module: ModModule,
}

pub(crate) fn validate_tree(source: &str, tree: &Tree) -> Result<(), String> {
    if source.len() > MAX_SOURCE_BYTES || source.contains('\0') {
        return Err("source exceeds size/NUL bound".into());
    }
    if tree.root_node().has_error() {
        return Err("source grammar reported a syntax error".into());
    }
    let mut stack = vec![(tree.root_node(), 0usize)];
    let mut count = 0;
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        if count > MAX_NODES || depth > MAX_DEPTH {
            return Err("AST exceeds node/depth bound".into());
        }
        stack.extend(engine::named(node).into_iter().map(|n| (n, depth + 1)));
    }
    Ok(())
}

impl<'a> PythonSource<'a> {
    pub fn new(source: &'a str) -> Result<Self, String> {
        if source.len() > MAX_SOURCE_BYTES || source.contains('\0') {
            return Err("Python source exceeds size/NUL bound".into());
        }
        let tree = engine::parse(source).ok_or("cannot initialize native Python parser")?;
        validate_tree(source, &tree)?;
        let module = ruff_python_parser::parse_module(source)
            .map_err(|e| format!("invalid Python: {e}"))?
            .into_syntax();
        Ok(Self {
            source,
            tree,
            module,
        })
    }

    pub fn text(&self) -> &str {
        self.source
    }
    pub(crate) fn tree(&self) -> &Tree {
        &self.tree
    }
    pub(crate) fn module(&self) -> &ModModule {
        &self.module
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detector_scan, hot_loop_scan, style_scan, test_value_scan, verbosity_scan};

    #[test]
    fn shared_ast_preserves_independent_scanner_findings() {
        let source = "def test_array(x: Tensor):\n    try:\n        for i in range(len(x)):\n            print(x[i].item())\n        assert True\n    except Exception:\n        pass\n";
        let parsed = PythonSource::new(source).unwrap();
        for path in ["models/a.py", "tests/test_array.py"] {
            assert_eq!(
                detector_scan::scan_source(path, source).unwrap(),
                detector_scan::scan_parsed(path, &parsed)
            );
            assert_eq!(
                style_scan::scan_source(path, source).unwrap(),
                style_scan::scan_parsed(path, &parsed)
            );
            assert_eq!(
                hot_loop_scan::scan_source(path, source).unwrap(),
                hot_loop_scan::scan_parsed(path, &parsed)
            );
            assert_eq!(
                test_value_scan::scan_source(path, source).unwrap(),
                test_value_scan::scan_parsed(path, &parsed)
            );
            assert_eq!(
                verbosity_scan::scan_source(path, source).unwrap(),
                verbosity_scan::scan_parsed(path, &parsed)
            );
        }
        assert!(!detector_scan::scan_parsed("a.py", &parsed).is_empty());
        assert!(!test_value_scan::scan_parsed("tests/test_array.py", &parsed).is_empty());
    }

    #[test]
    fn source_validation_rejects_invalid_targets_depth_and_size() {
        assert!(PythonSource::new("1.0 = x\n").is_err());
        assert!(PythonSource::new("def f(:").is_err());
        assert!(PythonSource::new(&"#".repeat(MAX_SOURCE_BYTES + 1)).is_err());
        let deep = format!(
            "x={}1{}",
            "(".repeat(MAX_DEPTH + 1),
            ")".repeat(MAX_DEPTH + 1)
        );
        assert!(PythonSource::new(&deep).is_err());
    }
}
