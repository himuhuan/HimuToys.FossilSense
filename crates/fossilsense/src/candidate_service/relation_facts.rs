//! Indexed dirty facts and bounded durable recall for all relation owners.
use super::*;
use crate::query;
use crate::store::views::relation_facts::{RelationFact, RelationFactLookup, RelationFactRow};

#[derive(Debug, Clone, Default)]
pub(super) struct RelationOverlayIndex {
    rows: Vec<RelationFactRow>,
    keys: HashMap<(u8, u8, String), Vec<usize>>,
    pub(super) groups: HashMap<String, u16>,
    pub(super) files: HashMap<String, crate::semantic_model::relations::RelationFacts>,
}
impl RelationOverlayIndex {
    pub(super) fn insert(&mut self, file: &FileCandidateOverlay) {
        self.groups.insert(file.path.clone(), file.relation_groups);
        self.files.insert(file.path.clone(), file.relations.clone());
        let facts = file
            .relations
            .binding_sites
            .iter()
            .cloned()
            .map(RelationFact::Binding)
            .chain(
                file.relations
                    .explicit_bases
                    .iter()
                    .cloned()
                    .map(RelationFact::Base),
            )
            .chain(
                file.relations
                    .indirect_assignments
                    .iter()
                    .cloned()
                    .map(RelationFact::Assignment),
            )
            .chain(
                file.relations
                    .macros
                    .iter()
                    .cloned()
                    .map(RelationFact::Macro),
            );
        let revision_hash = file
            .text
            .as_ref()
            .map(|s| blake3::hash(s.as_bytes()).to_hex().to_string())
            .unwrap_or_default();
        for fact in facts {
            let (kind, name, target) = match &fact {
                RelationFact::Binding(f) => (0, f.spelling.as_str(), ""),
                RelationFact::Base(f) => (1, f.derived_name.as_str(), f.base_name.as_str()),
                RelationFact::Assignment(f) => (
                    2,
                    f.member.as_deref().unwrap_or(&f.slot.spelling),
                    f.target_name.as_deref().unwrap_or(""),
                ),
                RelationFact::Macro(f) => (3, f.name.as_str(), ""),
            };
            let index = self.rows.len();
            for (key, value) in [
                (0, name),
                (1, file.path.as_str()),
                (2, target),
                (3, fact.source().enclosing_callable.as_deref().unwrap_or("")),
            ] {
                self.keys
                    .entry((kind, key, value.to_owned()))
                    .or_default()
                    .push(index);
            }
            self.rows.push(RelationFactRow {
                id: index as i64,
                path: file.path.clone(),
                revision_hash: revision_hash.clone(),
                fact,
            });
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationCursor {
    pub lookup_offset: usize,
    pub overlay_offset: usize,
    pub durable_after: i64,
    pub overlay_done: bool,
}

#[derive(Debug, Default)]
pub struct RelationCoverage {
    pub scanned: usize,
    pub next: Option<RelationCursor>,
    pub partial: bool,
    pub unavailable: bool,
    pub cancelled: bool,
    #[allow(dead_code)] // Exposed by typed relation APIs before transport integration.
    pub stale: bool,
    pub cycle: bool,
}

pub struct RelationControl<'a> {
    pub scan_limit: usize,
    pub cancellation: Option<&'a dyn query::CompletionQueryCancellation>,
}
impl Default for RelationControl<'_> {
    fn default() -> Self {
        Self {
            scan_limit: 256,
            cancellation: None,
        }
    }
}
impl RelationControl<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancellation.is_some_and(|c| c.is_cancelled())
    }
}

impl CandidateQueryService<'_> {
    pub(super) fn relation_fact_page(
        &self,
        kind: u8,
        lookup: RelationFactLookup<'_>,
        cursor: &RelationCursor,
        control: &RelationControl<'_>,
    ) -> Result<(Vec<RelationFactRow>, RelationCoverage)> {
        let group = [
            FactGroup::BindingSites,
            FactGroup::ExplicitBases,
            FactGroup::IndirectAssignments,
            FactGroup::MacroFacts,
        ][kind as usize];
        let mut coverage = RelationCoverage::default();
        for path in &self.overlays.shadowed_paths {
            if let RelationFactLookup::Path(selected) = lookup {
                if selected != path {
                    continue;
                }
            }
            if self.overlays.semantic_family_for_path(path) != Some(self.semantic_family) {
                continue;
            }
            let unavailable = self.overlays.unavailable_paths.contains(path);
            let state = self
                .overlays
                .declaration_coverage_by_path
                .get(path)
                .map(|s| s.fact_coverage(group))
                .unwrap_or_default();
            coverage.partial |=
                unavailable || state != crate::semantic_model::FactCoverage::Complete;
            coverage.unavailable |= matches!(lookup, RelationFactLookup::Path(_)) && unavailable;
        }

        if control.cancelled() {
            coverage.cancelled = true;
            return Ok((Vec::new(), coverage));
        }
        let limit = control.scan_limit.clamp(1, 256);
        let (key, value) = match lookup {
            RelationFactLookup::Name(v) => (0, v),
            RelationFactLookup::Path(v) => (1, v),
            RelationFactLookup::Target(v) => (2, v),
            RelationFactLookup::Caller(v) => (3, v),
        };
        let mut result = Vec::new();
        let mut next = cursor.clone();
        if !cursor.overlay_done {
            let ids = self
                .overlays
                .relation_facts
                .keys
                .get(&(kind, key, value.to_owned()))
                .map(Vec::as_slice)
                .unwrap_or_default();
            for index in ids.iter().skip(cursor.overlay_offset).take(limit) {
                next.overlay_offset += 1;
                coverage.scanned += 1;
                let row = &self.overlays.relation_facts.rows[*index];
                if self.overlays.semantic_family_for_path(&row.path) == Some(self.semantic_family) {
                    result.push(row.clone());
                }
            }
            next.overlay_done = next.overlay_offset >= ids.len();
            if !next.overlay_done || coverage.scanned == limit {
                coverage.next = Some(next);
                return Ok((result, coverage));
            }
        }
        if matches!(lookup,RelationFactLookup::Path(path) if self.overlays.shadows(path)) {
            return Ok((result, coverage));
        }
        if let Some(handle) = self.handle {
            let page = handle.read(|store| {
                store.relation_fact_view().scan(
                    kind,
                    lookup,
                    self.semantic_family,
                    next.durable_after,
                    limit - coverage.scanned,
                )
            })?;
            coverage.unavailable |= page.unavailable;
            coverage.partial |= page.partial;
            coverage.scanned += page.rows.len();
            result.extend(
                page.rows
                    .into_iter()
                    .filter(|r| !self.overlays.shadows(&r.path)),
            );
            if let Some(after) = page.next {
                next.durable_after = after;
                coverage.next = Some(next);
            }
        }
        if self
            .overlays
            .relation_facts
            .groups
            .iter()
            .any(|(path, mask)| {
                self.overlays.semantic_family_for_path(path) == Some(self.semantic_family)
                    && mask & (1 << (12 + kind)) == 0
            })
        {
            coverage.partial = true;
        }
        coverage.cancelled = control.cancelled();
        if coverage.cancelled {
            result.clear();
        }
        Ok((result, coverage))
    }
}
