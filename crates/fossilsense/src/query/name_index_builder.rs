use std::collections::HashMap;
use std::sync::Arc;

use crate::project_context::{ProjectContextIndex, ProjectKey};
use crate::store::views::NameTableSymbolRef;

use super::{NameEntry, NameTable};

/// Cold-build accumulator that interns borrowed SQLite text as rows are
/// visited. Only the final immutable entries survive the callback; there is no
/// workspace-sized typed-row vector between SQLite and the name index.
pub(super) struct NameIndexBuilder<'a> {
    entries: Vec<NameEntry>,
    paths: HashMap<Arc<str>, Arc<str>>,
    /// Canonical spelling -> lowercase spelling. One lookup interns both.
    names: HashMap<Arc<str>, Arc<str>>,
    project_by_path: HashMap<Arc<str>, Option<ProjectKey>>,
    project_context: Option<&'a ProjectContextIndex>,
}

impl<'a> NameIndexBuilder<'a> {
    pub(super) fn new(project_context: Option<&'a ProjectContextIndex>) -> Self {
        Self {
            entries: Vec::new(),
            paths: HashMap::new(),
            names: HashMap::new(),
            project_by_path: HashMap::new(),
            project_context,
        }
    }

    pub(super) fn push(&mut self, row: NameTableSymbolRef<'_>) {
        let path = match self.paths.get(row.path) {
            Some(path) => path.clone(),
            None => {
                let path = Arc::<str>::from(row.path);
                self.paths.insert(path.clone(), path.clone());
                path
            }
        };
        let (name, lower) = match self.names.get_key_value(row.label) {
            Some((name, lower)) => (name.clone(), lower.clone()),
            None => {
                let name = Arc::<str>::from(row.label);
                let lower = Arc::<str>::from(row.label.to_ascii_lowercase());
                self.names.insert(name.clone(), lower.clone());
                (name, lower)
            }
        };
        let project_key = if row.external {
            None
        } else if let Some(project) = self.project_by_path.get(path.as_ref()) {
            project.clone()
        } else {
            let project = self
                .project_context
                .and_then(|index| index.nearest_for_file(path.as_ref()));
            self.project_by_path.insert(path.clone(), project.clone());
            project
        };
        self.entries.push(NameEntry {
            id: row.symbol_id,
            name,
            lower,
            external: row.external,
            directly_included: row.directly_included,
            path,
            kind: crate::parser::kind_from_str(row.kind),
            project_key,
        });
    }

    pub(super) fn finish(self) -> NameTable {
        NameTable::from_interned_entries(self.entries)
    }
}
