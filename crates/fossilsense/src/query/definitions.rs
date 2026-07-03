#[cfg(test)]
use std::collections::HashSet;

use crate::model::{CandidateRange, DefinitionCandidate};
use crate::reachability::ReachScope;
use crate::resolver::{self, ResolveContext};
use crate::store::SymbolRecord;

/// Definition-preference `base_match` for a [`SymbolRecord`]: definition >
/// declaration, function in `.c`/`.cpp` over `.h`. The workspace/locality
/// terms from R1's `definition_score` are gone — tier (`scope_tier`) and
/// locality are now policy axes owned by the resolver. `base_match` is
/// purely match quality, kept structurally separate from tier.
fn definition_base_match(record: &SymbolRecord) -> i32 {
    let mut score = 0;
    if record.role == "definition" {
        score += 1000;
    }
    if record.kind == "function" && is_source_ext(&record.path) {
        score += 100;
    }
    score
}

/// Order definition candidates via the shared resolver: tier dominates
/// `base_match` (definition-preference) dominates locality. Returns the
/// records in the order the resolver comparator imposes so goto-definition can
/// build tier-aware [`DefinitionCandidate`]s in that order.
#[cfg(test)]
pub fn rank_definitions(
    mut candidates: Vec<SymbolRecord>,
    current_rel_path: &str,
    reach: Option<&HashSet<String>>,
) -> Vec<SymbolRecord> {
    // Compute a packed sort key per record and sort by it. The key encodes
    // (tier, base_match, locality) faithfully; ties fall back to path then
    // line, matching `resolver::compare_candidates`.
    candidates.sort_by(|a, b| {
        let tier_a = resolver_tier_for_record(a, current_rel_path, reach);
        let tier_b = resolver_tier_for_record(b, current_rel_path, reach);
        let score_a = resolver::pack_score(
            tier_a,
            definition_base_match(a),
            resolver::locality(&a.path, Some(current_rel_path)),
        );
        let score_b = resolver::pack_score(
            tier_b,
            definition_base_match(b),
            resolver::locality(&b.path, Some(current_rel_path)),
        );
        score_b
            .cmp(&score_a)
            .then_with(|| a.path.cmp(&b.path))
            .then(a.start_line.cmp(&b.start_line))
    });
    candidates
}

/// Resolve the [`ScopeTier`](crate::model::ScopeTier) for a [`SymbolRecord`]
/// given the goto path's current file and reachable set. The reachable set is
/// the determinate closure from `ReachGraph::reachable`; an open scope is
/// passed through as `reach.open = true` via a synthetic `ReachScope` so the
/// resolver can map an unreachable candidate to `Unknown` instead of `Global`.
#[cfg(test)]
fn resolver_tier_for_record(
    record: &SymbolRecord,
    current_rel_path: &str,
    reach: Option<&HashSet<String>>,
) -> crate::model::ScopeTier {
    use crate::model::ScopeTier;
    // Fast path: first-layer external does not need a ReachScope.
    let external = record.source == "external";
    if external && record.directly_included {
        return ScopeTier::External;
    }
    // Current file.
    if record.path == current_rel_path {
        return ScopeTier::Current;
    }
    let Some(set) = reach else {
        // No reach graph: workspace candidate is Global; a non-first-layer
        // external is also Global (no include edge).
        return ScopeTier::Global;
    };
    if set.contains(&record.path) {
        return ScopeTier::Reachable;
    }
    // We cannot tell open from closed here without the ReachScope flag. The
    // goto path only passes the file set, not the open flag, so this function
    // conservatively maps not-in-set workspace candidates to Global. The full
    // open-aware path is `rank_definitions_into_candidates_with_scope` below,
    // which receives the actual ReachScope and is the preferred entry point
    // for the LSP goto path.
    ScopeTier::Global
}

/// Build `Vec<DefinitionCandidate>` from the ranked `SymbolRecord`s, deriving
/// `tier`/`base_match`/confidence/reason via the shared resolver. Each
/// candidate's tier comes from [`resolver::scope_tier`], `base_match` from the
/// definition-preference quality, and `(confidence, reason)` from
/// [`resolver::confidence_reason_for`] (goto always queries by exact name, so
/// `exact_name = true`).
///
/// `reachable = None` means no reach graph is available (scoping disabled or
/// no index); every non-current, non-first-layer-external candidate then falls
/// back to `GlobalFallback`. **Note**: this entry point does not know whether
/// the reach set is open; the LSP goto path should call
/// [`rank_definitions_into_candidates_with_scope`] to route open-scope
/// candidates through `Unknown` rather than `Global`.
#[cfg(test)]
pub fn rank_definitions_into_candidates(
    candidates: Vec<SymbolRecord>,
    current_rel_path: &str,
    reachable: Option<&HashSet<String>>,
) -> Vec<DefinitionCandidate> {
    let ranked = rank_definitions(candidates, current_rel_path, reachable);
    ranked
        .into_iter()
        .map(|record| {
            let tier = resolver_tier_for_record(&record, current_rel_path, reachable);
            // This entry point has only the reachable set, not the open reason,
            // and maps not-in-set workspace candidates to `Global` (never
            // `Unknown`), so there is no `AmbiguousInclude` candidate to label.
            let (confidence, reason) = resolver::confidence_reason_for(tier, true, None);
            let base_match = definition_base_match(&record);
            DefinitionCandidate {
                name: record.name,
                kind: record.kind,
                role: record.role,
                path: record.path,
                range: CandidateRange {
                    start_line: record.start_line,
                    start_col: record.start_col,
                    end_line: record.end_line,
                    end_col: record.end_col,
                },
                source: record.source,
                tier,
                base_match,
                confidence,
                reason,
            }
        })
        .collect()
}

pub(super) struct RankedDefinitionRecord {
    pub candidate: DefinitionCandidate,
    pub record: SymbolRecord,
}

/// Build ranked definition candidates while preserving the source
/// [`SymbolRecord`]. Shared by goto-definition and signature help so both
/// features consume the same with-scope ranking, base-match policy, and
/// resolver tie-breakers.
pub(super) fn rank_definition_records_with_scope(
    candidates: Vec<SymbolRecord>,
    current_rel_path: &str,
    scope: Option<&ReachScope>,
) -> Vec<RankedDefinitionRecord> {
    // Build the resolver context and pre-compute (tier, base_match, locality)
    // per record so we can sort via the resolver comparator.
    let ctx = ResolveContext {
        current_path: Some(current_rel_path),
        reach: scope,
    };
    let mut keyed: Vec<(i32, String, u32, SymbolRecord, crate::model::ScopeTier, i32)> = candidates
        .into_iter()
        .map(|record| {
            let external = record.source == "external";
            let tier =
                resolver::scope_tier(&record.path, external, record.directly_included, Some(&ctx));
            let base = definition_base_match(&record);
            let loc = resolver::locality(&record.path, Some(current_rel_path));
            let score = resolver::pack_score(tier, base, loc);
            (
                score,
                record.path.clone(),
                record.start_line,
                record,
                tier,
                base,
            )
        })
        .collect();
    keyed.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    keyed
        .into_iter()
        .map(|(_, _, _, record, tier, base_match)| {
            // Pass the open scope's first cause so an `Unknown`-tier candidate
            // under an ambiguous include surfaces `Ambiguous` rather than a
            // plain `Fallback`.
            let (confidence, reason) =
                resolver::confidence_reason_for(tier, true, scope.and_then(|s| s.reason));
            let candidate = DefinitionCandidate {
                name: record.name.clone(),
                kind: record.kind.clone(),
                role: record.role.clone(),
                path: record.path.clone(),
                range: CandidateRange {
                    start_line: record.start_line,
                    start_col: record.start_col,
                    end_line: record.end_line,
                    end_col: record.end_col,
                },
                source: record.source.clone(),
                tier,
                base_match,
                confidence,
                reason,
            };
            RankedDefinitionRecord { candidate, record }
        })
        .collect()
}

/// Build `Vec<DefinitionCandidate>` from the records using the full
/// [`ReachScope`] (including `open`) for tier resolution. This is the preferred
/// entry point for the LSP goto path: an open scope routes not-proven-reachable
/// workspace candidates to `Unknown` (rather than `Global`), preserving the R1
/// "open scope does not bury unreachable" softening as a tier.
pub fn rank_definitions_into_candidates_with_scope(
    candidates: Vec<SymbolRecord>,
    current_rel_path: &str,
    scope: Option<&ReachScope>,
) -> Vec<DefinitionCandidate> {
    rank_definition_records_with_scope(candidates, current_rel_path, scope)
        .into_iter()
        .map(
            |RankedDefinitionRecord {
                 candidate,
                 record: _record,
             }| candidate,
        )
        .collect()
}

fn is_source_ext(path: &str) -> bool {
    match path.rsplit_once('.') {
        Some((_, ext)) => matches!(
            ext.to_ascii_lowercase().as_str(),
            "c" | "cc" | "cpp" | "cxx"
        ),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ResolutionConfidence, ResolutionReason};

    fn record(name: &str, kind: &str, role: &str, path: &str, line: u32) -> SymbolRecord {
        record_src(name, kind, role, path, line, "workspace")
    }

    fn record_src(
        name: &str,
        kind: &str,
        role: &str,
        path: &str,
        line: u32,
        source: &str,
    ) -> SymbolRecord {
        SymbolRecord {
            id: 0,
            name: name.to_string(),
            kind: kind.to_string(),
            role: role.to_string(),
            path: path.to_string(),
            start_line: line,
            start_col: 0,
            end_line: line,
            end_col: 0,
            signature: String::new(),
            guard: None,
            source: source.to_string(),
            directly_included: false,
        }
    }

    #[test]
    fn definition_outranks_declaration() {
        // No reach graph: both candidates are Global tier, so the tie breaks
        // by base_match — definition (1000) > declaration (0).
        let candidates = vec![
            record("foo", "function", "declaration", "include/foo.h", 1),
            record("foo", "function", "definition", "src/foo.c", 10),
        ];
        let ranked = rank_definitions(candidates, "src/main.c", None);
        assert_eq!(ranked[0].role, "definition");
        assert_eq!(ranked[0].path, "src/foo.c");
    }

    #[test]
    fn closer_path_wins_among_definitions() {
        // No reach graph: both Global tier, both definitions (equal base_match);
        // locality (shared path prefix) breaks the tie.
        let candidates = vec![
            record("foo", "function", "definition", "other/foo.c", 1),
            record("foo", "function", "definition", "src/sub/foo.c", 1),
        ];
        let ranked = rank_definitions(candidates, "src/sub/main.c", None);
        assert_eq!(ranked[0].path, "src/sub/foo.c");
    }

    #[test]
    fn external_first_layer_outranks_global_workspace() {
        // R2 behavior change: a first-layer external (directly #included)
        // outranks a global workspace candidate of the same name, because
        // External > Global and a direct include is reachability evidence.
        // (Renamed from `workspace_definition_outranks_external`; the old
        // "workspace always beats external" rule is reversed by strict-tier
        // ordering.)
        let candidates = vec![
            record_ext(
                "size_t",
                "type",
                "definition",
                "C:/mingw/include/stddef.h",
                10,
                true, // directly_included → first-layer external
            ),
            record("size_t", "type", "declaration", "src/types.h", 1),
        ];
        let ranked = rank_definitions(candidates, "src/main.c", None);
        assert_eq!(
            ranked[0].source, "external",
            "first-layer external outranks global workspace"
        );
        assert_eq!(ranked[1].source, "workspace");
    }

    #[test]
    fn reachable_declaration_outranks_unreachable_definition() {
        // R2 behavior change: with a reach graph, tier dominates the
        // definition>declaration preference (now an intra-tier base_match
        // tiebreak). A reachable declaration (Reachable) outranks an
        // unreachable definition (Global) even though the definition has
        // higher base_match.
        let candidates = vec![
            record("foo", "function", "definition", "other/c.h", 1),
            record("foo", "function", "declaration", "inc/b.h", 1),
        ];
        let reach = reachable_set(&["src/main.c", "inc/b.h"]);
        let ranked = rank_definitions(candidates, "src/main.c", Some(&reach));
        assert_eq!(
            ranked[0].path, "inc/b.h",
            "reachable declaration outranks unreachable definition"
        );
        assert_eq!(ranked[0].role, "declaration");
        assert_eq!(ranked[1].path, "other/c.h");
        assert_eq!(ranked[1].role, "definition");
    }

    #[test]
    fn external_only_definition_is_still_returned() {
        let candidates = vec![record_ext(
            "HANDLE",
            "type",
            "definition",
            "C:/mingw/include/windows.h",
            5,
            false,
        )];
        let ranked = rank_definitions(candidates, "src/main.c", None);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].name, "HANDLE");
    }

    /// External record with an explicit `directly_included` flag.
    fn record_ext(
        name: &str,
        kind: &str,
        role: &str,
        path: &str,
        line: u32,
        directly_included: bool,
    ) -> SymbolRecord {
        let mut rec = record_src(name, kind, role, path, line, "external");
        rec.directly_included = directly_included;
        rec
    }

    fn reachable_set(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn goto_with_scope_orders_current_reachable_external_unknown() {
        let mut external = record_ext("foo", "function", "definition", "C:/sdk/foo.h", 3, true);
        external.directly_included = true;
        let candidates = vec![
            record("foo", "function", "definition", "other/foo.c", 20),
            external,
            record("foo", "function", "definition", "inc/foo.h", 7),
            record("foo", "function", "definition", "src/main.c", 30),
        ];
        let scope = crate::reachability::ReachScope {
            files: ["src/main.c".to_string(), "inc/foo.h".to_string()]
                .into_iter()
                .collect(),
            open: true,
            reason: Some(crate::reachability::OpenReason::AmbiguousInclude),
        };
        let ranked =
            rank_definitions_into_candidates_with_scope(candidates, "src/main.c", Some(&scope));
        let paths: Vec<&str> = ranked
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["src/main.c", "inc/foo.h", "C:/sdk/foo.h", "other/foo.c"]
        );
        assert_eq!(ranked[3].tier, crate::model::ScopeTier::Unknown);
        assert_eq!(ranked[3].confidence, ResolutionConfidence::Ambiguous);
    }

    #[test]
    fn goto_open_unresolved_scope_uses_fallback_for_unknown_candidates() {
        let candidates = vec![record("foo", "function", "definition", "other/foo.c", 20)];
        let scope = crate::reachability::ReachScope {
            files: HashSet::new(),
            open: true,
            reason: Some(crate::reachability::OpenReason::UnresolvedInclude),
        };
        let ranked =
            rank_definitions_into_candidates_with_scope(candidates, "src/main.c", Some(&scope));
        assert_eq!(ranked[0].tier, crate::model::ScopeTier::Unknown);
        assert_eq!(ranked[0].confidence, ResolutionConfidence::Fallback);
        assert_eq!(ranked[0].reason, ResolutionReason::GlobalFallback);
    }

    #[test]
    fn confidence_reason_for_current_file_exact() {
        assert_eq!(
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::Current, true, None),
            (ResolutionConfidence::Exact, ResolutionReason::CurrentFile)
        );
        // Non-exact name in current file drops to Reachable confidence, same reason.
        assert_eq!(
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::Current, false, None),
            (
                ResolutionConfidence::Reachable,
                ResolutionReason::CurrentFile
            )
        );
    }

    #[test]
    fn confidence_reason_for_reachable_external_and_fallback() {
        assert_eq!(
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::Reachable, true, None),
            (
                ResolutionConfidence::Reachable,
                ResolutionReason::ReachableInclude
            )
        );
        assert_eq!(
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::External, true, None),
            (
                ResolutionConfidence::Heuristic,
                ResolutionReason::ExternalFirstLayer
            )
        );
        assert_eq!(
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::Unknown, true, None),
            (
                ResolutionConfidence::Fallback,
                ResolutionReason::GlobalFallback
            )
        );
        assert_eq!(
            crate::resolver::confidence_reason_for(crate::model::ScopeTier::Global, true, None),
            (
                ResolutionConfidence::Fallback,
                ResolutionReason::GlobalFallback
            )
        );
    }

    #[test]
    fn goto_candidates_label_current_file_exact() {
        // A definition in the current file labels CurrentFile / Exact.
        let candidates = vec![record("foo", "function", "definition", "src/main.c", 10)];
        let labeled = rank_definitions_into_candidates(candidates, "src/main.c", None);
        assert_eq!(labeled.len(), 1);
        assert_eq!(labeled[0].reason, ResolutionReason::CurrentFile);
        assert_eq!(labeled[0].confidence, ResolutionConfidence::Exact);
    }

    #[test]
    fn goto_candidates_label_reachable_include() {
        // A workspace definition in a reachable (determinate) file labels
        // ReachableInclude / Reachable.
        let candidates = vec![record("foo", "function", "definition", "inc/b.h", 1)];
        let reach = reachable_set(&["src/main.c", "inc/b.h"]);
        let labeled = rank_definitions_into_candidates(candidates, "src/main.c", Some(&reach));
        assert_eq!(labeled[0].reason, ResolutionReason::ReachableInclude);
        assert_eq!(labeled[0].confidence, ResolutionConfidence::Reachable);
    }

    #[test]
    fn goto_candidates_label_external_first_layer() {
        // A definition in a directly-included external header labels
        // ExternalFirstLayer / Heuristic, even with no reach graph.
        let candidates = vec![record_ext(
            "size_t",
            "type",
            "definition",
            "C:/mingw/include/stddef.h",
            1,
            true,
        )];
        let labeled = rank_definitions_into_candidates(candidates, "src/main.c", None);
        assert_eq!(labeled[0].reason, ResolutionReason::ExternalFirstLayer);
        assert_eq!(labeled[0].confidence, ResolutionConfidence::Heuristic);
    }

    #[test]
    fn goto_candidates_label_global_fallback() {
        // A workspace definition with no reach evidence labels GlobalFallback.
        let candidates = vec![record("foo", "function", "definition", "other/foo.c", 1)];
        let labeled = rank_definitions_into_candidates(candidates, "src/main.c", None);
        assert_eq!(labeled[0].reason, ResolutionReason::GlobalFallback);
        assert_eq!(labeled[0].confidence, ResolutionConfidence::Fallback);
        // Exact is never emitted for a GlobalFallback candidate.
        assert_ne!(labeled[0].confidence, ResolutionConfidence::Exact);
    }

    #[test]
    fn goto_candidates_preserve_rank_definitions_ordering() {
        // The candidate ordering must be identical to the SymbolRecord
        // ranking — both go through the same resolver comparator.
        let candidates = vec![
            record_src(
                "size_t",
                "type",
                "definition",
                "C:/mingw/include/stddef.h",
                10,
                "external",
            ),
            record("size_t", "type", "declaration", "src/types.h", 1),
            record("size_t", "type", "definition", "src/types.c", 5),
        ];
        let ranked_paths: Vec<String> = rank_definitions(candidates.clone(), "src/main.c", None)
            .into_iter()
            .map(|r| r.path)
            .collect();
        let labeled_paths: Vec<String> =
            rank_definitions_into_candidates(candidates, "src/main.c", None)
                .into_iter()
                .map(|c| c.path)
                .collect();
        assert_eq!(ranked_paths, labeled_paths);
    }

    #[test]
    fn goto_candidates_carry_indexed_facts() {
        // The candidate carries the indexed facts (name/kind/role/path/range/
        // source) alongside the tier/base_match/labels.
        let candidates = vec![record("foo", "function", "definition", "src/foo.c", 10)];
        let labeled = rank_definitions_into_candidates(candidates, "src/main.c", None);
        let c = &labeled[0];
        assert_eq!(c.name, "foo");
        assert_eq!(c.kind, "function");
        assert_eq!(c.role, "definition");
        assert_eq!(c.path, "src/foo.c");
        assert_eq!(c.source, "workspace");
        assert_eq!(c.range.start_line, 10);
        assert_eq!(c.tier, crate::model::ScopeTier::Global);
        assert_eq!(
            c.base_match, 1100,
            "definition (1000) + function in .c (100)"
        );
    }
}
