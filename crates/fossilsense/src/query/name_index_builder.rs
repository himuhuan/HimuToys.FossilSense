use std::collections::HashMap;
use std::sync::Arc;

use crate::project_context::{ProjectContextIndex, ProjectKey};
use crate::store::views::NameTableSymbolRef;

use super::{
    CompactNameEntry, NameEntry, NameEntryRef, NameSegment, NameString, NameTable, NO_PROJECT_ID,
};

/// Cold-build accumulator that interns borrowed SQLite text into segment-local
/// arenas. Each compact entry stores only integer arena IDs plus its semantic
/// flags; the callback never creates a workspace-sized owned-row vector.
pub(super) struct NameIndexBuilder<'a> {
    entries: Vec<CompactNameEntry>,
    names: Vec<NameString>,
    name_ids: HashMap<Arc<str>, u32>,
    paths: Vec<Arc<str>>,
    path_ids: HashMap<Arc<str>, u32>,
    path_counts: Vec<usize>,
    path_is_external: Vec<bool>,
    projects: Vec<ProjectKey>,
    project_ids: HashMap<ProjectKey, u32>,
    project_by_path: HashMap<u32, u32>,
    project_context: Option<&'a ProjectContextIndex>,
}

impl<'a> NameIndexBuilder<'a> {
    pub(super) fn new(project_context: Option<&'a ProjectContextIndex>) -> Self {
        Self {
            entries: Vec::new(),
            names: Vec::new(),
            name_ids: HashMap::new(),
            paths: Vec::new(),
            path_ids: HashMap::new(),
            path_counts: Vec::new(),
            path_is_external: Vec::new(),
            projects: Vec::new(),
            project_ids: HashMap::new(),
            project_by_path: HashMap::new(),
            project_context,
        }
    }

    pub(super) fn push(&mut self, row: NameTableSymbolRef<'_>) {
        let name_id = self.intern_name(row.label, None);
        let path_id = self.intern_path(row.path, row.external);
        let project_id = if row.external {
            NO_PROJECT_ID
        } else if let Some(project_id) = self.project_by_path.get(&path_id) {
            *project_id
        } else {
            let project_id = self
                .project_context
                .and_then(|index| index.nearest_for_file(row.path))
                .map_or(NO_PROJECT_ID, |project| self.intern_project(project));
            self.project_by_path.insert(path_id, project_id);
            project_id
        };
        self.push_compact(CompactNameEntry {
            id: row.symbol_id,
            name_id,
            path_id,
            project_id,
            kind: crate::parser::kind_from_str(row.kind),
            external: row.external,
            directly_included: row.directly_included,
        });
    }

    pub(super) fn push_entry(&mut self, entry: NameEntry) {
        let name_id = self.intern_name(&entry.name, Some(&entry.lower));
        let path_id = self.intern_path(&entry.path, entry.external);
        let project_id = entry
            .project_key
            .map_or(NO_PROJECT_ID, |project| self.intern_project(project));
        self.push_compact(CompactNameEntry {
            id: entry.id,
            name_id,
            path_id,
            project_id,
            kind: entry.kind,
            external: entry.external,
            directly_included: entry.directly_included,
        });
    }

    pub(super) fn push_ref(&mut self, entry: NameEntryRef<'_>) {
        let name_id = self.intern_name(entry.name, Some(entry.lower));
        let path_id = self.intern_path(entry.path, entry.external);
        let project_id = entry
            .project_key
            .cloned()
            .map_or(NO_PROJECT_ID, |project| self.intern_project(project));
        self.push_compact(CompactNameEntry {
            id: entry.id,
            name_id,
            path_id,
            project_id,
            kind: entry.kind,
            external: entry.external,
            directly_included: entry.directly_included,
        });
    }

    pub(super) fn push_ref_with_project_context(&mut self, entry: NameEntryRef<'_>) {
        let name_id = self.intern_name(entry.name, Some(entry.lower));
        let path_id = self.intern_path(entry.path, entry.external);
        let project_id = if entry.external {
            NO_PROJECT_ID
        } else if let Some(project_id) = self.project_by_path.get(&path_id) {
            *project_id
        } else {
            let project_id = self
                .project_context
                .and_then(|index| index.nearest_for_file(entry.path))
                .map_or(NO_PROJECT_ID, |project| self.intern_project(project));
            self.project_by_path.insert(path_id, project_id);
            project_id
        };
        self.push_compact(CompactNameEntry {
            id: entry.id,
            name_id,
            path_id,
            project_id,
            kind: entry.kind,
            external: entry.external,
            directly_included: entry.directly_included,
        });
    }

    fn push_compact(&mut self, entry: CompactNameEntry) {
        self.path_counts[entry.path_id as usize] += 1;
        self.entries.push(entry);
    }

    fn intern_name(&mut self, name: &str, known_lower: Option<&str>) -> u32 {
        if let Some(id) = self.name_ids.get(name) {
            return *id;
        }
        let id = u32::try_from(self.names.len()).expect("name arena exceeds u32 IDs");
        let original = Arc::<str>::from(name);
        let lower = Arc::<str>::from(
            known_lower
                .map(str::to_owned)
                .unwrap_or_else(|| name.to_ascii_lowercase()),
        );
        self.name_ids.insert(original.clone(), id);
        self.names.push(NameString { original, lower });
        id
    }

    fn intern_path(&mut self, path: &str, external: bool) -> u32 {
        if let Some(id) = self.path_ids.get(path) {
            return *id;
        }
        let id = u32::try_from(self.paths.len()).expect("path arena exceeds u32 IDs");
        let path = Arc::<str>::from(path);
        self.path_ids.insert(path.clone(), id);
        self.paths.push(path);
        self.path_counts.push(0);
        self.path_is_external.push(external);
        id
    }

    fn intern_project(&mut self, project: ProjectKey) -> u32 {
        if let Some(id) = self.project_ids.get(&project) {
            return *id;
        }
        let id = u32::try_from(self.projects.len()).expect("project arena exceeds u32 IDs");
        self.project_ids.insert(project.clone(), id);
        self.projects.push(project);
        id
    }

    pub(super) fn finish(self) -> NameTable {
        NameTable::from_base_segment(self.finish_segment())
    }

    pub(super) fn finish_segment(self) -> NameSegment {
        NameSegment::from_compact_parts(
            self.entries,
            self.names,
            self.paths,
            self.path_ids,
            self.path_counts,
            self.path_is_external,
            self.projects,
        )
    }
}
