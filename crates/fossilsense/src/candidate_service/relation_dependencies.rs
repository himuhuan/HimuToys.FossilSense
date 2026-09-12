//! Thin relation projections over the existing type and include owners.
#![allow(dead_code)] // Public semantic API consumed by the follow-up relation backend.
use super::relation_facts::*;
use super::relation_types::*;
use super::*;
use crate::semantic_model::relations::ReceiverFact;

pub struct FileRelationPage {
    pub edges: Vec<crate::store::views::IncludeEdgeRow>,
    pub coverage: RelationCoverage,
}
impl RelationQueryContext<'_> {
    pub fn type_of(
        &self,
        path: &str,
        receiver: &ReceiverFact,
        control: &RelationControl<'_>,
    ) -> Result<Option<TypeCandidateBundle>> {
        if control.cancelled() {
            return Ok(None);
        }
        let Some(ty) = receiver.type_name.as_deref() else {
            return Ok(None);
        };
        let mut remaining = control.scan_limit.min(64);
        let spelling = clean_type(ty);
        let tag = spelling
            .strip_prefix("struct ")
            .or_else(|| spelling.strip_prefix("union "))
            .or_else(|| spelling.strip_prefix("enum "));
        if receiver.tag_domain || tag.is_some() {
            self.service(path)
                .type_candidates_for_member_domain(
                    tag.unwrap_or(&spelling).trim(),
                    crate::parser::LookupDomain::Tag,
                    &mut remaining,
                )
                .map(Some)
        } else {
            self.service(path)
                .type_candidates_with_budget(&spelling, &mut remaining)
                .map(Some)
        }
    }
    pub fn includes(
        &self,
        path: &str,
        incoming: bool,
        cursor: &RelationCursor,
        control: &RelationControl<'_>,
    ) -> Result<FileRelationPage> {
        let mut coverage = RelationCoverage::default();
        let mut edges = Vec::new();
        if control.cancelled() {
            coverage.cancelled = true;
            return Ok(FileRelationPage { edges, coverage });
        }
        let mut next = cursor.clone();
        let limit = control.scan_limit.clamp(1, 256);
        let overlay = self
            .overlays
            .include_relations
            .get(&(incoming, path.to_owned()))
            .map(Vec::as_slice)
            .unwrap_or_default();
        if !next.overlay_done {
            edges.extend(
                overlay
                    .iter()
                    .skip(next.overlay_offset)
                    .take(limit)
                    .cloned(),
            );
            coverage.scanned = edges.len();
            coverage.partial |= edges
                .iter()
                .any(|e| e.resolution == crate::includes::ResolutionKind::SuffixMatch);
            next.overlay_offset += coverage.scanned;
            next.overlay_done = next.overlay_offset >= overlay.len();
            if !next.overlay_done {
                coverage.next = Some(next);
                return Ok(FileRelationPage { edges, coverage });
            }
        }
        coverage.partial |= self.overlays.include_open_sources.contains(path);
        if !incoming && self.overlays.shadows(path) {
            coverage.partial |= self.overlays.effective_reach_graph.is_none();
        } else if let Some(read) = &self.read {
            if coverage.scanned == limit {
                coverage.next = Some(next);
                return Ok(FileRelationPage { edges, coverage });
            }
            let (rows, more, unresolved) = read.read(|store| {
                store.include_table_view().relation_page(
                    path,
                    incoming,
                    next.durable_after,
                    limit - coverage.scanned,
                )
            })?;
            coverage.partial |= unresolved;
            coverage.scanned += rows.len();
            if more {
                next.durable_after = rows.last().map_or(next.durable_after, |(id, _)| *id);
                coverage.next = Some(next);
            }
            edges.extend(
                rows.into_iter()
                    .map(|(_, r)| r)
                    .filter(|r| !self.overlays.shadows(&r.source_path)),
            );
        }
        coverage.partial |= edges
            .iter()
            .any(|e| e.resolution == crate::includes::ResolutionKind::SuffixMatch);
        coverage.cancelled = control.cancelled();
        if coverage.cancelled {
            edges.clear();
        }
        Ok(FileRelationPage { edges, coverage })
    }
}
