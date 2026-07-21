use super::*;

pub(super) fn hash_table_bytes<K, V>(capacity: usize) -> usize {
    // hashbrown stores a control byte beside each bucket. This remains an
    // estimate rather than an allocator promise, so the process-level gate is
    // still authoritative.
    capacity.saturating_mul(size_of::<(K, V)>().saturating_add(1))
}

fn string_set_bytes(values: &HashSet<String>) -> usize {
    hash_table_bytes::<String, ()>(values.capacity()).saturating_add(
        values
            .iter()
            .fold(0usize, |bytes, value| bytes.saturating_add(value.len())),
    )
}

impl NameTable {
    pub(crate) fn accounted_bytes(&self) -> usize {
        let arc_header = size_of::<usize>().saturating_mul(2);
        let delta_segments = self.deltas.iter().fold(0usize, |bytes, segment| {
            bytes.saturating_add(segment.accounted_bytes())
        });
        let path_overrides = self.path_overrides.iter().fold(0usize, |bytes, (path, _)| {
            bytes.saturating_add(path.len()).saturating_add(arc_header)
        });
        let direct_overrides = self
            .direct_include_overrides
            .keys()
            .fold(0usize, |bytes, path| bytes.saturating_add(path.len()));
        let reach = self.all_workspace_reach.as_ref();

        size_of::<Self>()
            .saturating_add(arc_header)
            .saturating_add(self.base.accounted_bytes())
            .saturating_add(
                self.deltas
                    .capacity()
                    .saturating_mul(size_of::<Arc<NameSegment>>()),
            )
            .saturating_add(delta_segments)
            .saturating_add(hash_table_bytes::<Arc<str>, Option<usize>>(
                self.path_overrides.capacity(),
            ))
            .saturating_add(path_overrides)
            .saturating_add(
                self.delta_offsets
                    .capacity()
                    .saturating_mul(size_of::<usize>()),
            )
            .saturating_add(hash_table_bytes::<String, bool>(
                self.direct_include_overrides.capacity(),
            ))
            .saturating_add(direct_overrides)
            .saturating_add(arc_header)
            .saturating_add(size_of::<ReachScope>())
            .saturating_add(string_set_bytes(&reach.files))
            .saturating_add(string_set_bytes(&reach.heuristic_files))
    }

    #[allow(dead_code)]
    pub fn with_updated_paths(
        &self,
        paths: &HashSet<String>,
        names: Vec<(i64, String, bool, String, String, bool)>,
    ) -> Self {
        let fresh_entries = names.into_iter().map(name_entry);
        self.with_updated_entries(paths, fresh_entries)
    }

    pub(crate) fn with_updated_declaration_name_rows_with_project_context(
        &self,
        paths: &HashSet<String>,
        rows: Vec<DeclarationNameRow>,
        project_context: Option<&ProjectContextIndex>,
    ) -> Self {
        self.with_updated_entries(paths, declaration_name_entries(rows, project_context))
    }

    pub fn project_indices(&self, key: &ProjectKey) -> Option<Vec<usize>> {
        let mut indices = Vec::new();
        if let Some(base) = self.base.by_project.get(key) {
            indices.extend(
                base.iter()
                    .copied()
                    .filter(|index| self.is_active_index(*index)),
            );
        }
        for (delta_index, delta) in self.deltas.iter().enumerate() {
            let Some(project) = delta.by_project.get(key) else {
                continue;
            };
            let offset = self.delta_offsets[delta_index];
            indices.extend(
                project
                    .iter()
                    .map(|index| offset + index)
                    .filter(|index| self.is_active_index(*index)),
            );
        }
        (!indices.is_empty()).then_some(indices)
    }

    /// Re-derive build-marker ownership over this already-published name
    /// generation. Marker-only refreshes use this instead of reopening SQLite,
    /// so an overlapping index writer cannot leak partially committed rows into
    /// the runtime snapshot.
    pub fn with_project_context(&self, project_context: Option<&ProjectContextIndex>) -> Self {
        let mut builder = name_index_builder::NameIndexBuilder::new(project_context);
        for index in self.active_indices() {
            builder.push_ref_with_project_context(self.entry(index));
        }
        let mut rebuilt = builder.finish();
        rebuilt.direct_include_overrides = self.direct_include_overrides.clone();
        rebuilt
    }

    /// Apply sparse first-layer external evidence from a request-local dirty
    /// include graph. This is an O(changed external paths) clone and leaves the
    /// compact base/delta segments shared.
    pub fn with_direct_include_overrides(&self, overrides: &HashMap<String, bool>) -> Self {
        if overrides.is_empty() {
            return Self {
                base: self.base.clone(),
                deltas: self.deltas.clone(),
                path_overrides: self.path_overrides.clone(),
                delta_offsets: self.delta_offsets.clone(),
                active_len: self.active_len,
                slot_len: self.slot_len,
                direct_include_overrides: self.direct_include_overrides.clone(),
                all_workspace_reach: self.all_workspace_reach.clone(),
            };
        }
        let mut merged = self.direct_include_overrides.as_ref().clone();
        merged.extend(
            overrides
                .iter()
                .map(|(path, included)| (path.clone(), *included)),
        );
        Self {
            base: self.base.clone(),
            deltas: self.deltas.clone(),
            path_overrides: self.path_overrides.clone(),
            delta_offsets: self.delta_offsets.clone(),
            active_len: self.active_len,
            slot_len: self.slot_len,
            direct_include_overrides: Arc::new(merged),
            all_workspace_reach: self.all_workspace_reach.clone(),
        }
    }

    fn with_updated_entries(
        &self,
        paths: &HashSet<String>,
        fresh_entries: impl IntoIterator<Item = NameEntry>,
    ) -> Self {
        let fresh_entries: Vec<NameEntry> = fresh_entries.into_iter().collect();
        let fresh_segment = Arc::new(NameSegment::from_entries(fresh_entries));
        let mut deltas = self.deltas.as_ref().clone();
        let delta_index = deltas.len();
        let mut offsets = self.delta_offsets.as_ref().clone();
        offsets.push(self.slot_len);
        let fresh_slots = fresh_segment.entries.len();

        let mut overrides = self.path_overrides.as_ref().clone();
        let mut active_len = self.active_len;
        for path in paths {
            let old_count = match overrides.get(path.as_str()) {
                Some(Some(previous_delta)) => self.deltas[*previous_delta].path_count(path),
                Some(None) => 0,
                None => self.base.path_count(path),
            };
            let fresh_count = fresh_segment.path_count(path);
            active_len = active_len.saturating_sub(old_count) + fresh_count;
            let interned_path = fresh_segment
                .interned_path(path)
                .unwrap_or_else(|| Arc::<str>::from(path.as_str()));
            overrides.insert(interned_path, (fresh_count > 0).then_some(delta_index));
        }

        let mut all_workspace_reach = self.all_workspace_reach.as_ref().clone();
        for path in paths {
            all_workspace_reach.files.remove(path);
        }
        for (path, external) in fresh_segment
            .paths
            .iter()
            .zip(&fresh_segment.path_is_external)
        {
            if !external {
                all_workspace_reach.files.insert(path.to_string());
            }
        }
        deltas.push(fresh_segment);
        Self {
            base: self.base.clone(),
            deltas: Arc::new(deltas),
            path_overrides: Arc::new(overrides),
            delta_offsets: Arc::new(offsets),
            active_len,
            slot_len: self.slot_len + fresh_slots,
            direct_include_overrides: self.direct_include_overrides.clone(),
            all_workspace_reach: Arc::new(all_workspace_reach),
        }
    }

    pub(super) fn directly_included_for(&self, entry: NameEntryRef<'_>) -> bool {
        if !entry.external {
            return false;
        }
        self.direct_include_overrides
            .get(entry.path)
            .copied()
            .unwrap_or(entry.directly_included)
    }
}

pub(super) fn name_entry(
    (id, name, external, path, kind, directly_included): (i64, String, bool, String, String, bool),
) -> NameEntry {
    name_entry_parts(
        id,
        name,
        external,
        path,
        kind,
        SymbolRole::Definition,
        directly_included,
        None,
    )
}

pub(super) fn declaration_name_entries(
    rows: Vec<DeclarationNameRow>,
    project_context: Option<&ProjectContextIndex>,
) -> Vec<NameEntry> {
    let mut project_by_path = HashMap::<String, Option<ProjectKey>>::new();
    rows.into_iter()
        .map(|row| {
            let project_key = if row.external {
                None
            } else if let Some(project) = project_by_path.get(&row.path) {
                project.clone()
            } else {
                let project = project_context.and_then(|index| index.nearest_for_file(&row.path));
                project_by_path.insert(row.path.clone(), project.clone());
                project
            };
            let lower = row.name.to_ascii_lowercase();
            NameEntry {
                id: row.id,
                name: Arc::from(row.name),
                lower: Arc::from(lower),
                external: row.external,
                directly_included: row.directly_included,
                path: Arc::from(row.path),
                kind: parser_kind_from_declaration_kind(row.declaration_kind),
                role: symbol_role_from_declaration_role(row.role),
                project_key,
            }
        })
        .collect()
}

pub(super) fn parser_kind_from_declaration_kind(
    kind: crate::semantic_model::SemanticDeclarationKind,
) -> ParserKind {
    match kind {
        crate::semantic_model::SemanticDeclarationKind::Function
        | crate::semantic_model::SemanticDeclarationKind::Method => ParserKind::Function,
        crate::semantic_model::SemanticDeclarationKind::Object => ParserKind::GlobalVariable,
        crate::semantic_model::SemanticDeclarationKind::Type
        | crate::semantic_model::SemanticDeclarationKind::Alias => ParserKind::Type,
        crate::semantic_model::SemanticDeclarationKind::EnumConstant => ParserKind::EnumConstant,
        crate::semantic_model::SemanticDeclarationKind::Macro => ParserKind::Macro,
    }
}

pub(super) fn symbol_role_from_declaration_role(
    role: crate::semantic_model::SemanticDeclarationRole,
) -> SymbolRole {
    match role {
        crate::semantic_model::SemanticDeclarationRole::Declaration => SymbolRole::Declaration,
        crate::semantic_model::SemanticDeclarationRole::Definition => SymbolRole::Definition,
        crate::semantic_model::SemanticDeclarationRole::TentativeDefinition => {
            SymbolRole::TentativeDefinition
        }
        crate::semantic_model::SemanticDeclarationRole::Unknown => {
            SymbolRole::UnknownDeclarationOrDefinition
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn name_entry_parts(
    id: i64,
    name: String,
    external: bool,
    path: String,
    kind: String,
    role: SymbolRole,
    directly_included: bool,
    project_key: Option<ProjectKey>,
) -> NameEntry {
    let lower = name.to_ascii_lowercase();
    NameEntry {
        id,
        name: Arc::from(name),
        lower: Arc::from(lower),
        external,
        directly_included,
        path: Arc::from(path),
        kind: parser_kind_from_str(&kind),
        role,
        project_key,
    }
}

fn parser_kind_from_str(kind: &str) -> ParserKind {
    match kind {
        "function" => ParserKind::Function,
        "macro" => ParserKind::Macro,
        "type" => ParserKind::Type,
        "enum_constant" => ParserKind::EnumConstant,
        "field" => ParserKind::Field,
        _ => ParserKind::GlobalVariable,
    }
}
