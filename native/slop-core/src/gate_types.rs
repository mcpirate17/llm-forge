//! Native summaries shared by analysis, reporting and backlog tools.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A unit to ablate: `(qualname, def line, driver nodeids)`.
pub type Unit = (String, usize, Vec<String>);
/// A unit no driver fit the timeout of: `(qualname, def line, why)`.
pub type Refused = (String, usize, String);
/// A target module: `(path, whole file, units, refused units)`.
pub type Target = (String, bool, Vec<Unit>, Vec<Refused>);

/// What a run measured: a whole module (`units` is `None`) or some of its units.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Measured {
    pub module: String,
    pub units: Option<Vec<String>>,
}

impl Measured {
    pub fn covers(&self, module: &str, qualname: &str) -> bool {
        self.module == module
            && self
                .units
                .as_ref()
                .is_none_or(|units| units.iter().any(|unit| unit == qualname))
    }
}

/// A run's findings by what they mean for the change.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Summary {
    pub modules_probed: usize,
    pub measured: Vec<Measured>,
    pub modules_without_drivers: Vec<String>,
    /// Modules no recorded test drove, probed with the test files importing them.
    pub modules_index_driven: Vec<String>,
    pub blocking: Vec<Value>,
    pub advisory: Vec<Value>,
    pub untested: Vec<Value>,
    pub unreached: Vec<Value>,
    pub incomplete: Vec<Value>,
    pub unmeasured: Vec<Value>,
    pub live: usize,
}
