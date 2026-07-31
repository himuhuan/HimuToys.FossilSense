use std::collections::BinaryHeap;

use super::*;

const PRIORITY_SOURCE_PROBE_MULTIPLIER: usize = 8;
const PRIORITY_SOURCE_RECOVERY: u8 = 1;
const PRIORITY_SOURCE_PROJECT: u8 = 2;
const PRIORITY_SOURCE_SCOPE: u8 = 3;
const PRIORITY_SOURCE_CHANNELS: usize = 3;

struct PrefixHeapEntry<'a> {
    table: &'a NameTable,
    segment_slot: usize,
    family_slot: usize,
    sorted_position: usize,
    index: usize,
}

impl PartialEq for PrefixHeapEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl Eq for PrefixHeapEntry<'_> {}

impl PartialOrd for PrefixHeapEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrefixHeapEntry<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let left = self.table.entry(self.index);
        let right = other.table.entry(other.index);
        // Active rows must win before lexical ordering. Otherwise a replaced
        // base path can consume the entire bounded cursor before the newest
        // delta segment is even considered.
        self.table
            .is_active_index(self.index)
            .cmp(&other.table.is_active_index(other.index))
            // Reverse the lexical order so BinaryHeap behaves as a min-heap.
            .then_with(|| {
                right
                    .lower
                    .cmp(left.lower)
                    .then_with(|| right.name.cmp(left.name))
                    .then_with(|| other.index.cmp(&self.index))
            })
    }
}

struct ShortPrefixHeapEntry<'a> {
    table: &'a NameTable,
    segment_slot: usize,
    family_slot: usize,
    position: usize,
    index: usize,
}

struct PriorityPrefixHeapEntry<'a> {
    table: &'a NameTable,
    segment_slot: usize,
    posting: &'a [u32],
    position: usize,
    source_rank: PrioritySourceRank,
    prefix_only: bool,
    index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrioritySourceRank {
    order: usize,
    priority: u8,
    channel: PrioritySourceChannel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrioritySourceChannel {
    Scope = 0,
    Project = 1,
    Recovery = 2,
}

impl PrioritySourceChannel {
    const ORDERED: [Self; PRIORITY_SOURCE_CHANNELS] = [Self::Scope, Self::Project, Self::Recovery];

    fn slot(self) -> usize {
        self as usize
    }
}

fn partition_priority_capacity(
    total: usize,
    enabled: [bool; PRIORITY_SOURCE_CHANNELS],
) -> [usize; PRIORITY_SOURCE_CHANNELS] {
    let mut limits = [0usize; PRIORITY_SOURCE_CHANNELS];
    let mut remaining = total;
    while remaining > 0 {
        let mut assigned = false;
        for channel in PrioritySourceChannel::ORDERED {
            let slot = channel.slot();
            if !enabled[slot] {
                continue;
            }
            limits[slot] += 1;
            remaining -= 1;
            assigned = true;
            if remaining == 0 {
                break;
            }
        }
        if !assigned {
            break;
        }
    }
    limits
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PrioritySourceKind {
    PathPrefix,
    ProjectPrefix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PrioritySourceKey {
    segment_slot: usize,
    family_slot: usize,
    source_id: u32,
    kind: PrioritySourceKind,
}

struct PendingPrioritySource<'a> {
    segment_slot: usize,
    posting: &'a [u32],
    position: usize,
    source_priority: u8,
}

struct PrioritySourceSetup<'table, 'metrics, 'cancel> {
    table: &'table NameTable,
    heap: BinaryHeap<PriorityPrefixHeapEntry<'table>>,
    seen: HashSet<PrioritySourceKey>,
    attempt_limits: [usize; PRIORITY_SOURCE_CHANNELS],
    probe_limits: [usize; PRIORITY_SOURCE_CHANNELS],
    attempts_by_channel: [usize; PRIORITY_SOURCE_CHANNELS],
    probes_by_channel: [usize; PRIORITY_SOURCE_CHANNELS],
    attempts: usize,
    probes: usize,
    metrics: &'metrics mut RecallScanMetrics,
    cancellation: Option<&'cancel dyn super::CompletionQueryCancellation>,
    cancelled: bool,
}

impl<'table, 'metrics, 'cancel> PrioritySourceSetup<'table, 'metrics, 'cancel> {
    fn new(
        table: &'table NameTable,
        limit: usize,
        enabled: [bool; PRIORITY_SOURCE_CHANNELS],
        metrics: &'metrics mut RecallScanMetrics,
        cancellation: Option<&'cancel dyn super::CompletionQueryCancellation>,
    ) -> Self {
        let probe_limit = limit
            .saturating_mul(PRIORITY_SOURCE_PROBE_MULTIPLIER)
            .min(COMPLETION_PRIORITY_METADATA_PROBE_LIMIT);
        Self {
            table,
            heap: BinaryHeap::new(),
            seen: HashSet::with_capacity(probe_limit),
            attempt_limits: partition_priority_capacity(limit, enabled),
            probe_limits: partition_priority_capacity(probe_limit, enabled),
            attempts_by_channel: [0; PRIORITY_SOURCE_CHANNELS],
            probes_by_channel: [0; PRIORITY_SOURCE_CHANNELS],
            attempts: 0,
            probes: 0,
            metrics,
            cancellation,
            cancelled: false,
        }
    }

    fn channel_attempts_are_full(&self, channel: PrioritySourceChannel) -> bool {
        let slot = channel.slot();
        self.attempts_by_channel[slot] >= self.attempt_limits[slot]
    }

    fn channel_probes_are_full(&self, channel: PrioritySourceChannel) -> bool {
        let slot = channel.slot();
        self.probes_by_channel[slot] >= self.probe_limits[slot]
    }

    fn channel_is_full(&self, channel: PrioritySourceChannel) -> bool {
        self.channel_attempts_are_full(channel) || self.channel_probes_are_full(channel)
    }

    fn any_channel_is_full(&self) -> bool {
        PrioritySourceChannel::ORDERED.into_iter().any(|channel| {
            let slot = channel.slot();
            (self.attempt_limits[slot] > 0 || self.probe_limits[slot] > 0)
                && self.channel_is_full(channel)
        })
    }

    fn try_push(
        &mut self,
        channel: PrioritySourceChannel,
        key: PrioritySourceKey,
        posting: &'table [u32],
        position: usize,
        source_priority: u8,
        required_prefix: Option<&str>,
    ) -> bool {
        if self.seen.contains(&key) {
            return true;
        }
        if !self.consume_probe(channel) {
            return false;
        }
        if !self.seen.insert(key) {
            return true;
        }
        self.push_registered(
            channel,
            key.segment_slot,
            posting,
            position,
            source_priority,
            required_prefix,
        )
    }

    fn consume_probe(&mut self, channel: PrioritySourceChannel) -> bool {
        if self.channel_is_full(channel) {
            return false;
        }
        if self
            .probes
            .is_multiple_of(super::COMPLETION_CANCELLATION_CHECK_INTERVAL)
            && self.metrics.check(self.cancellation)
        {
            self.cancelled = true;
            return false;
        }

        let channel_slot = channel.slot();
        self.probes += 1;
        self.probes_by_channel[channel_slot] += 1;
        self.metrics.priority_source_probes += 1;
        true
    }

    fn register_source(&mut self, key: PrioritySourceKey) -> bool {
        self.seen.insert(key)
    }

    fn push_registered(
        &mut self,
        channel: PrioritySourceChannel,
        segment_slot: usize,
        posting: &'table [u32],
        position: usize,
        source_priority: u8,
        required_prefix: Option<&str>,
    ) -> bool {
        if self.channel_attempts_are_full(channel) {
            return false;
        }
        let channel_slot = channel.slot();
        let source_rank = PrioritySourceRank {
            order: self.attempts,
            priority: source_priority,
            channel,
        };
        if self.table.push_priority_prefix_cursor(
            &mut self.heap,
            segment_slot,
            posting,
            position,
            source_rank,
            required_prefix,
        ) {
            self.attempts += 1;
            self.attempts_by_channel[channel_slot] += 1;
            self.metrics.priority_source_attempts += 1;
            self.metrics.priority_sources_initialized += 1;
        }
        true
    }
}

impl PartialEq for PriorityPrefixHeapEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
            && self.source_rank == other.source_rank
            && self.position == other.position
    }
}

impl Eq for PriorityPrefixHeapEntry<'_> {}

impl PartialOrd for PriorityPrefixHeapEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityPrefixHeapEntry<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let left = self.table.entry(self.index);
        let right = other.table.entry(other.index);
        self.table
            .is_active_index(self.index)
            .cmp(&other.table.is_active_index(other.index))
            .then_with(|| self.source_rank.priority.cmp(&other.source_rank.priority))
            .then_with(|| {
                self.source_rank
                    .channel
                    .slot()
                    .cmp(&other.source_rank.channel.slot())
            })
            .then_with(|| right.lower.cmp(left.lower))
            .then_with(|| right.name.cmp(left.name))
            .then_with(|| other.index.cmp(&self.index))
            .then_with(|| other.source_rank.order.cmp(&self.source_rank.order))
            .then_with(|| other.position.cmp(&self.position))
    }
}

struct FuzzyPostingHeapEntry<'a> {
    table: &'a NameTable,
    segment_slot: usize,
    family_slot: usize,
    token: u32,
    position: usize,
    name_id: u32,
}

impl PartialEq for FuzzyPostingHeapEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.name_id == other.name_id
            && self.token == other.token
            && self.segment_slot == other.segment_slot
            && self.family_slot == other.family_slot
            && self.position == other.position
    }
}

impl Eq for FuzzyPostingHeapEntry<'_> {}

impl PartialOrd for FuzzyPostingHeapEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FuzzyPostingHeapEntry<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let (left_segment, _) = self.table.segment_with_offset(self.segment_slot);
        let (right_segment, _) = other.table.segment_with_offset(other.segment_slot);
        let left = &left_segment.names[self.name_id as usize].original;
        let right = &right_segment.names[other.name_id as usize].original;
        // Newer delta segments precede base/older segments so a large set of
        // shadowed fuzzy names cannot starve the active replacement segment.
        self.segment_slot.cmp(&other.segment_slot).then_with(|| {
            right
                .len()
                .cmp(&left.len())
                .then_with(|| right.cmp(left))
                .then_with(|| other.token.cmp(&self.token))
                .then_with(|| other.family_slot.cmp(&self.family_slot))
                .then_with(|| other.name_id.cmp(&self.name_id))
                .then_with(|| other.position.cmp(&self.position))
        })
    }
}

impl PartialEq for ShortPrefixHeapEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl Eq for ShortPrefixHeapEntry<'_> {}

impl PartialOrd for ShortPrefixHeapEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ShortPrefixHeapEntry<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse best-first ordering so BinaryHeap yields the smallest static
        // completion key (shortest name, then lexical/role/path/id) first.
        self.table
            .is_active_index(self.index)
            .cmp(&other.table.is_active_index(other.index))
            .then_with(|| {
                static_head_order(self.table.entry(other.index), self.table.entry(self.index))
            })
            .then_with(|| other.index.cmp(&self.index))
    }
}

fn static_head_order(left: NameEntryRef<'_>, right: NameEntryRef<'_>) -> std::cmp::Ordering {
    left.name
        .len()
        .cmp(&right.name.len())
        .then_with(|| left.name.cmp(right.name))
        .then_with(|| {
            completion_role_recall_priority(right.role)
                .cmp(&completion_role_recall_priority(left.role))
        })
        .then_with(|| left.path.cmp(right.path))
        .then_with(|| left.id.cmp(&right.id))
}

#[cfg(test)]
mod heap_contract_tests {
    use super::*;

    #[test]
    fn short_prefix_heap_order_distinguishes_duplicate_declaration_slots() {
        let table = NameTable::build_with_paths(vec![
            (
                7,
                "alpha".to_string(),
                false,
                "same.h".to_string(),
                "function".to_string(),
                false,
            ),
            (
                7,
                "alpha".to_string(),
                false,
                "same.h".to_string(),
                "function".to_string(),
                false,
            ),
        ]);
        let left = ShortPrefixHeapEntry {
            table: &table,
            segment_slot: 0,
            family_slot: 0,
            position: 0,
            index: 0,
        };
        let right = ShortPrefixHeapEntry {
            table: &table,
            segment_slot: 0,
            family_slot: 0,
            position: 1,
            index: 1,
        };

        assert!(left != right);
        assert_ne!(left.cmp(&right), std::cmp::Ordering::Equal);
    }
}

impl NameTable {
    /// Entry indices whose lowercased name starts with `needle_lower` (the exact
    /// and prefix tiers), found by binary search over the sorted index. Returns
    /// the same set a full scan would classify as exact/prefix, in sorted order.
    pub fn prefix_candidates(&self, needle_lower: &str) -> Vec<usize> {
        self.prefix_candidates_filtered(needle_lower, None)
    }

    fn prefix_candidates_filtered(
        &self,
        needle_lower: &str,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
    ) -> Vec<usize> {
        if needle_lower.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.extend_prefix_matches(&self.base, 0, needle_lower, semantic_family, &mut out);
        for (delta_index, delta) in self.deltas.iter().enumerate() {
            self.extend_prefix_matches(
                delta,
                self.delta_offsets[delta_index],
                needle_lower,
                semantic_family,
                &mut out,
            );
        }
        out.sort_by(|left, right| {
            self.entry(*left)
                .lower
                .cmp(self.entry(*right).lower)
                .then_with(|| self.entry(*left).name.cmp(self.entry(*right).name))
        });
        out
    }

    pub(super) fn segment_with_offset(&self, segment_slot: usize) -> (&NameSegment, usize) {
        if segment_slot == 0 {
            (&self.base, 0)
        } else {
            let delta = segment_slot - 1;
            (&self.deltas[delta], self.delta_offsets[delta])
        }
    }

    fn active_segment_slot_for_path(&self, path: &str) -> Option<usize> {
        match self.path_overrides.get(path) {
            None => self.base.path_ids.contains_key(path).then_some(0),
            Some(Some(delta)) => Some(delta + 1),
            Some(None) => None,
        }
    }

    fn push_priority_prefix_cursor<'a>(
        &'a self,
        heap: &mut BinaryHeap<PriorityPrefixHeapEntry<'a>>,
        segment_slot: usize,
        posting: &'a [u32],
        position: usize,
        source_rank: PrioritySourceRank,
        required_prefix: Option<&str>,
    ) -> bool {
        let (_, offset) = self.segment_with_offset(segment_slot);
        let Some(&local) = posting.get(position) else {
            return false;
        };
        let index = offset + local as usize;
        if required_prefix.is_some_and(|needle| !self.entry(index).lower.starts_with(needle)) {
            return false;
        }
        heap.push(PriorityPrefixHeapEntry {
            table: self,
            segment_slot,
            posting,
            position,
            source_rank,
            prefix_only: required_prefix.is_some(),
            index,
        });
        true
    }

    fn segment_is_mixed(&self, segment_slot: usize) -> bool {
        let (segment, _) = self.segment_with_offset(segment_slot);
        let active_paths = if segment_slot == 0 {
            self.active_base_paths.as_ref()
        } else {
            self.active_delta_paths[segment_slot - 1].as_ref()
        };
        !active_paths.is_empty() && active_paths.len() != segment.paths.len()
    }

    /// Probe compact fuzzy name IDs, rather than a whole lexical path posting,
    /// when a mixed segment may contain a stale barrier. Sole-path metadata
    /// rejects tombstoned names without expanding declaration rows. The probe
    /// count is separately bounded/observable; only expanded declarations
    /// consume the shared candidate budget.
    fn bounded_priority_fuzzy_candidates(
        &self,
        tokens: &[u32],
        budget: usize,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
        metrics: &mut RecallScanMetrics,
    ) -> (Vec<usize>, bool) {
        if tokens.is_empty() || budget == 0 {
            return (Vec::new(), false);
        }
        let probe_limit = budget
            .saturating_mul(super::COMPLETION_CANCELLATION_CHECK_INTERVAL)
            .min(COMPLETION_PRIORITY_METADATA_PROBE_LIMIT);
        let mut probes = 0usize;
        let mut declaration_probes = 0usize;
        let mut inspected = 0usize;
        let mut output = Vec::new();
        let mut expanded_names = HashSet::new();

        for &token in tokens {
            for segment_slot in 0..=self.deltas.len() {
                if !self.segment_is_mixed(segment_slot) {
                    continue;
                }
                let (segment, offset) = self.segment_with_offset(segment_slot);
                for &family_slot in semantic_family_slots(semantic_family) {
                    for &name_id in segment.fuzzy_postings_by_family[family_slot].posting(token) {
                        if probes >= probe_limit {
                            return (output, true);
                        }
                        if probes.is_multiple_of(super::COMPLETION_CANCELLATION_CHECK_INTERVAL)
                            && metrics.check(cancellation)
                        {
                            return (Vec::new(), true);
                        }
                        probes += 1;
                        metrics.priority_fuzzy_name_probes += 1;
                        if !expanded_names.insert((segment_slot, family_slot, name_id)) {
                            continue;
                        }

                        let sole_path = segment.sole_path_by_name[name_id as usize];
                        if sole_path != MULTI_PATH_ID
                            && self.active_segment_slot_for_path(
                                segment.paths[sole_path as usize].as_ref(),
                            ) != Some(segment_slot)
                        {
                            continue;
                        }

                        for &local in
                            self.segment_entries_for_name(segment_slot, family_slot, name_id)
                        {
                            let index = offset + local;
                            if sole_path == MULTI_PATH_ID {
                                if declaration_probes >= probe_limit {
                                    return (output, true);
                                }
                                if declaration_probes
                                    .is_multiple_of(super::COMPLETION_CANCELLATION_CHECK_INTERVAL)
                                    && metrics.check(cancellation)
                                {
                                    return (Vec::new(), true);
                                }
                                declaration_probes += 1;
                                metrics.priority_fuzzy_declaration_probes += 1;
                                if !self.is_active_index(index) {
                                    continue;
                                }
                            }
                            if inspected >= budget {
                                return (output, true);
                            }
                            inspected += 1;
                            metrics.entries_inspected += 1;
                            metrics.fuzzy_entries_inspected += 1;
                            metrics.fuzzy_posting_entries_inspected += 1;
                            output.push(index);
                        }
                    }
                }
            }
        }
        (output, false)
    }

    /// Reserve a small part of the same request budget for candidates backed
    /// by reachability/direct-include or selected-project evidence. Every
    /// source posting is already partitioned by semantic family and ordered by
    /// name, so unrelated global declarations cannot consume this channel.
    /// Cursor construction itself is request-bounded and observable: a large
    /// scope or a mixed active/stale segment cannot create thousands of hidden
    /// heap sources ahead of a four-row priority budget.
    fn bounded_priority_prefix_candidates(
        &self,
        needle_lower: &str,
        budget: usize,
        request: &super::CompletionRecallQuery<'_>,
        metrics: &mut RecallScanMetrics,
    ) -> (Vec<usize>, bool) {
        let scope = request.scope;
        let active_project = request.active_project;
        let semantic_family = request.semantic_family;
        let cancellation = request.cancellation;
        if needle_lower.is_empty() || budget == 0 {
            return (Vec::new(), false);
        }

        let mut scope_paths = Vec::new();
        if let Some(scope) = scope {
            scope_paths.reserve(
                scope.reach.files.len()
                    + scope.reach.heuristic_files.len()
                    + scope.direct_external_files.len()
                    + usize::from(scope.current_path.is_some()),
            );
            scope_paths.extend(scope.current_path.as_deref());
            scope_paths.extend(scope.reach.files.iter().map(String::as_str));
            scope_paths.extend(scope.reach.heuristic_files.iter().map(String::as_str));
            scope_paths.extend(scope.direct_external_files.iter().map(String::as_str));
            scope_paths.sort_unstable();
            scope_paths.dedup();
        }
        let project_enabled = active_project.is_some_and(|project| match semantic_family {
            Some(family) => self.has_project_for_family(project, family),
            None => self
                .active_project_family_counts
                .get(project)
                .is_some_and(|counts| counts.iter().any(|count| *count > 0)),
        });
        let recovery_enabled = (0..=self.deltas.len()).any(|segment_slot| {
            if !self.segment_is_mixed(segment_slot) {
                return false;
            }
            let (segment, _) = self.segment_with_offset(segment_slot);
            semantic_family_slots(semantic_family)
                .iter()
                .any(|family_slot| {
                    !segment.prefix_paths_by_family[*family_slot]
                        .paths_for_prefix(needle_lower)
                        .is_empty()
                })
        });
        let source_limit = budget.min(COMPLETION_PRIORITY_METADATA_PROBE_LIMIT);
        let mut setup = PrioritySourceSetup::new(
            self,
            source_limit,
            [!scope_paths.is_empty(), project_enabled, recovery_enabled],
            metrics,
            cancellation,
        );

        // Scope-backed paths are most relevant and are initialized before the
        // generic stale-segment recovery channel. The source key is shared
        // with recovery, so the same path/family/prefix cursor is never opened
        // twice when the current file also lives in a mixed segment.
        'scope_paths: for path in scope_paths {
            if setup.channel_is_full(PrioritySourceChannel::Scope) || setup.cancelled {
                break;
            }
            if !setup.consume_probe(PrioritySourceChannel::Scope) {
                break;
            }
            let Some(segment_slot) = self.active_segment_slot_for_path(path) else {
                continue;
            };
            let (segment, _) = self.segment_with_offset(segment_slot);
            let Some(path_id) = segment.path_ids.get(path).copied() else {
                continue;
            };
            for (family_ordinal, &family_slot) in
                semantic_family_slots(semantic_family).iter().enumerate()
            {
                if family_ordinal > 0 && !setup.consume_probe(PrioritySourceChannel::Scope) {
                    break 'scope_paths;
                }
                let posting = segment.path_postings_by_family[family_slot].posting(path_id);
                let start = posting
                    .partition_point(|local| segment.entry(*local as usize).lower < needle_lower);
                let key = PrioritySourceKey {
                    segment_slot,
                    family_slot,
                    source_id: path_id,
                    kind: PrioritySourceKind::PathPrefix,
                };
                if !setup.register_source(key) {
                    continue;
                }
                if !setup.push_registered(
                    PrioritySourceChannel::Scope,
                    segment_slot,
                    posting,
                    start,
                    PRIORITY_SOURCE_SCOPE,
                    Some(needle_lower),
                ) {
                    break 'scope_paths;
                }
            }
        }

        // Each explicit evidence channel owns a partition of both cursor slots
        // and metadata probes. Scope initializes first so a path supported by
        // both channels keeps its stronger evidence. For mixed segments, the
        // compact project-position posting reaches active project paths without
        // scanning unrelated prefix paths or a stale project declaration head.
        if let Some(project) = active_project.filter(|_| project_enabled) {
            'project_paths: for segment_slot in 0..=self.deltas.len() {
                if setup.channel_is_full(PrioritySourceChannel::Project) || setup.cancelled {
                    break;
                }
                if !self.segment_is_mixed(segment_slot) {
                    continue;
                }
                let (segment, _) = self.segment_with_offset(segment_slot);
                let Some(project_id) = segment
                    .by_project
                    .get(project)
                    .map(|postings| postings.project_id)
                else {
                    continue;
                };
                for &family_slot in semantic_family_slots(semantic_family) {
                    for &pair_position in segment.prefix_paths_by_family[family_slot]
                        .project_positions_for_prefix(project_id, needle_lower)
                    {
                        if !setup.consume_probe(PrioritySourceChannel::Project) {
                            break 'project_paths;
                        }
                        let pair = segment.prefix_paths_by_family[family_slot].pairs
                            [pair_position as usize];
                        let path_id = pair.path_id;
                        if self
                            .active_segment_slot_for_path(segment.paths[path_id as usize].as_ref())
                            != Some(segment_slot)
                        {
                            continue;
                        }
                        let key = PrioritySourceKey {
                            segment_slot,
                            family_slot,
                            source_id: path_id,
                            kind: PrioritySourceKind::PathPrefix,
                        };
                        if !setup.register_source(key) {
                            continue;
                        }
                        let posting = segment.path_postings_by_family[family_slot].posting(path_id);
                        let start = posting.partition_point(|local| {
                            segment.entry(*local as usize).lower < needle_lower
                        });
                        if !setup.push_registered(
                            PrioritySourceChannel::Project,
                            segment_slot,
                            posting,
                            start,
                            PRIORITY_SOURCE_PROJECT,
                            Some(needle_lower),
                        ) {
                            break 'project_paths;
                        }
                    }
                }
            }

            'project_segments: for segment_slot in 0..=self.deltas.len() {
                if setup.channel_is_full(PrioritySourceChannel::Project) || setup.cancelled {
                    break;
                }
                let (segment, _) = self.segment_with_offset(segment_slot);
                let Some(postings) = segment.by_project.get(project) else {
                    continue;
                };
                for &family_slot in semantic_family_slots(semantic_family) {
                    let posting = &postings.by_family[family_slot];
                    let start = posting.partition_point(|local| {
                        segment.entry(*local as usize).lower < needle_lower
                    });
                    if !setup.try_push(
                        PrioritySourceChannel::Project,
                        PrioritySourceKey {
                            segment_slot,
                            family_slot,
                            source_id: 0,
                            kind: PrioritySourceKind::ProjectPrefix,
                        },
                        posting,
                        start,
                        PRIORITY_SOURCE_PROJECT,
                        Some(needle_lower),
                    ) {
                        break 'project_segments;
                    }
                }
            }
        }

        let (continuous, boundary) = fuzzy_query_tokens(needle_lower);
        let mut fuzzy_tokens = Vec::with_capacity(2);
        if let Some(token) = self.rarest_fuzzy_posting(&continuous, semantic_family) {
            fuzzy_tokens.push(token);
        }
        if let Some(token) = self.rarest_fuzzy_posting(&boundary, semantic_family) {
            if !fuzzy_tokens.contains(&token) {
                fuzzy_tokens.push(token);
            }
        }

        // A single lexical cursor per segment can be pinned behind a stale
        // sibling even when its current head is active. The compact name-head
        // index yields only paths relevant to this prefix; lexical path order
        // and unrelated paths therefore cannot spend the source budget.
        let recovery_slot = PrioritySourceChannel::Recovery.slot();
        let recovery_source_limit = setup.attempt_limits[recovery_slot];
        let mut project_recovery_sources = Vec::with_capacity(recovery_source_limit);
        let mut generic_recovery_sources = Vec::with_capacity(recovery_source_limit);
        'recovery_segments: for segment_slot in 0..=self.deltas.len() {
            if setup.channel_is_full(PrioritySourceChannel::Recovery) || setup.cancelled {
                break;
            }
            let (segment, _) = self.segment_with_offset(segment_slot);
            if !self.segment_is_mixed(segment_slot) {
                continue;
            }
            for &family_slot in semantic_family_slots(semantic_family) {
                for pair in
                    segment.prefix_paths_by_family[family_slot].paths_for_prefix(needle_lower)
                {
                    if !setup.consume_probe(PrioritySourceChannel::Recovery) {
                        break 'recovery_segments;
                    }
                    let path_id = pair.path_id;
                    if self.active_segment_slot_for_path(segment.paths[path_id as usize].as_ref())
                        != Some(segment_slot)
                    {
                        continue;
                    }
                    let posting = segment.path_postings_by_family[family_slot].posting(path_id);
                    let start = posting.partition_point(|local| {
                        segment.entry(*local as usize).lower < needle_lower
                    });
                    let Some(&local) = posting.get(start) else {
                        continue;
                    };
                    if !segment
                        .entry(local as usize)
                        .lower
                        .starts_with(needle_lower)
                    {
                        continue;
                    }
                    let key = PrioritySourceKey {
                        segment_slot,
                        family_slot,
                        source_id: path_id,
                        kind: PrioritySourceKind::PathPrefix,
                    };
                    if !setup.register_source(key) {
                        continue;
                    }
                    let is_active_project = active_project.is_some_and(|project| {
                        let project_id = segment.entries[local as usize].project_id;
                        project_id != NO_PROJECT_ID
                            && &segment.projects[project_id as usize] == project
                    });
                    let pending = PendingPrioritySource {
                        segment_slot,
                        posting,
                        position: start,
                        source_priority: if is_active_project {
                            PRIORITY_SOURCE_PROJECT
                        } else {
                            PRIORITY_SOURCE_RECOVERY
                        },
                    };
                    let sources = if is_active_project {
                        &mut project_recovery_sources
                    } else {
                        &mut generic_recovery_sources
                    };
                    if sources.len() < recovery_source_limit {
                        sources.push(pending);
                    }
                    if project_recovery_sources.len() >= recovery_source_limit
                        && generic_recovery_sources.len() >= recovery_source_limit
                    {
                        break 'recovery_segments;
                    }
                }
            }
        }

        for pending in project_recovery_sources
            .into_iter()
            .chain(generic_recovery_sources)
        {
            if !setup.push_registered(
                PrioritySourceChannel::Recovery,
                pending.segment_slot,
                pending.posting,
                pending.position,
                pending.source_priority,
                Some(needle_lower),
            ) {
                break;
            }
        }

        if setup.cancelled {
            return (Vec::new(), true);
        }
        let sources_truncated = setup.any_channel_is_full();
        let candidate_limits = setup.attempt_limits;
        let mut heap = std::mem::take(&mut setup.heap);
        drop(setup);

        let mut output = Vec::new();
        let mut inspected = 0usize;
        let mut inspected_by_channel = [0usize; PRIORITY_SOURCE_CHANNELS];
        let mut deferred = Vec::new();
        let mut deferred_probes = 0usize;
        // First honor the same per-channel reservation used for source setup.
        // A long high-priority cursor is held at its current head once its
        // share is full, allowing the other initialized evidence channels to
        // contribute before any unused share is redistributed.
        while inspected < budget {
            let Some(next) = heap.pop() else {
                break;
            };
            let channel_slot = next.source_rank.channel.slot();
            if inspected_by_channel[channel_slot] >= candidate_limits[channel_slot] {
                if deferred_probes.is_multiple_of(super::COMPLETION_CANCELLATION_CHECK_INTERVAL)
                    && metrics.check(cancellation)
                {
                    return (Vec::new(), true);
                }
                deferred_probes += 1;
                deferred.push(next);
                continue;
            }
            if metrics.should_cancel(cancellation) {
                return (
                    Vec::new(),
                    sources_truncated || !heap.is_empty() || !deferred.is_empty(),
                );
            }
            inspected_by_channel[channel_slot] += 1;
            inspected += 1;
            metrics.entries_inspected += 1;
            metrics.prefix_entries_inspected += 1;
            if self.is_active_index(next.index) {
                output.push(next.index);
            }
            let _ = self.push_priority_prefix_cursor(
                &mut heap,
                next.segment_slot,
                next.posting,
                next.position + 1,
                next.source_rank,
                next.prefix_only.then_some(needle_lower),
            );
        }
        heap.extend(deferred);
        // Channels with no matching cursor leave capacity behind. Reuse that
        // remainder without changing final semantic ordering or the hard row
        // budget.
        while inspected < budget {
            if metrics.should_cancel(cancellation) {
                return (Vec::new(), sources_truncated || !heap.is_empty());
            }
            let Some(next) = heap.pop() else {
                break;
            };
            inspected += 1;
            metrics.entries_inspected += 1;
            metrics.prefix_entries_inspected += 1;
            if self.is_active_index(next.index) {
                output.push(next.index);
            }
            let _ = self.push_priority_prefix_cursor(
                &mut heap,
                next.segment_slot,
                next.posting,
                next.position + 1,
                next.source_rank,
                next.prefix_only.then_some(needle_lower),
            );
        }
        let prefix_truncated = sources_truncated || !heap.is_empty();
        let remaining = budget.saturating_sub(inspected);
        let (fuzzy_output, fuzzy_truncated) = self.bounded_priority_fuzzy_candidates(
            &fuzzy_tokens,
            remaining,
            semantic_family,
            cancellation,
            metrics,
        );
        if metrics.cancelled {
            return (Vec::new(), true);
        }
        output.extend(fuzzy_output);
        (output, prefix_truncated || fuzzy_truncated)
    }

    fn push_prefix_cursor<'a>(
        &'a self,
        heap: &mut BinaryHeap<PrefixHeapEntry<'a>>,
        segment_slot: usize,
        family_slot: usize,
        sorted_position: usize,
        needle_lower: &str,
    ) {
        let (segment, offset) = self.segment_with_offset(segment_slot);
        let Some(&local) = segment.sorted_by_family[family_slot].get(sorted_position) else {
            return;
        };
        if !segment.entry(local).lower.starts_with(needle_lower) {
            return;
        }
        heap.push(PrefixHeapEntry {
            table: self,
            segment_slot,
            family_slot,
            sorted_position,
            index: offset + local,
        });
    }

    /// Visit at most `budget` rows from the per-segment sorted prefix ranges
    /// without materializing or sorting the complete range. Returned indices
    /// are active; `entries_inspected` also counts stale/tombstoned rows that
    /// consumed cursor work. `truncated` means more prefix rows remain.
    fn bounded_prefix_candidates(
        &self,
        needle_lower: &str,
        budget: usize,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
        metrics: &mut RecallScanMetrics,
    ) -> (Vec<usize>, bool) {
        if needle_lower.is_empty() {
            return (Vec::new(), false);
        }
        let mut heap = BinaryHeap::new();
        for segment_slot in 0..=self.deltas.len() {
            let (segment, _) = self.segment_with_offset(segment_slot);
            for &family_slot in semantic_family_slots(semantic_family) {
                let sorted = &segment.sorted_by_family[family_slot];
                let start =
                    sorted.partition_point(|&index| segment.entry(index).lower < needle_lower);
                self.push_prefix_cursor(&mut heap, segment_slot, family_slot, start, needle_lower);
            }
        }

        let mut output = Vec::new();
        let mut inspected = 0usize;
        while inspected < budget {
            if metrics.should_cancel(cancellation) {
                return (Vec::new(), !heap.is_empty());
            }
            let Some(next) = heap.pop() else {
                break;
            };
            inspected += 1;
            metrics.entries_inspected += 1;
            metrics.prefix_entries_inspected += 1;
            if self.is_active_index(next.index) {
                output.push(next.index);
            }
            self.push_prefix_cursor(
                &mut heap,
                next.segment_slot,
                next.family_slot,
                next.sorted_position + 1,
                needle_lower,
            );
        }
        (output, !heap.is_empty())
    }

    fn push_short_prefix_cursor<'a>(
        &'a self,
        heap: &mut BinaryHeap<ShortPrefixHeapEntry<'a>>,
        segment_slot: usize,
        family_slot: usize,
        head: u8,
        position: usize,
    ) {
        let (segment, offset) = self.segment_with_offset(segment_slot);
        let Some(&local) = segment.short_prefix_heads_by_family[family_slot]
            .get(&head)
            .and_then(|indices| indices.get(position))
        else {
            return;
        };
        heap.push(ShortPrefixHeapEntry {
            table: self,
            segment_slot,
            family_slot,
            position,
            index: offset + local as usize,
        });
    }

    /// Single-character prefix postings are preordered by static completion
    /// quality. This prevents a bounded lexical cursor from returning only the
    /// alphabetically earliest slice of a very large one-letter range.
    fn bounded_short_prefix_candidates(
        &self,
        head: u8,
        budget: usize,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
        metrics: &mut RecallScanMetrics,
    ) -> (Vec<usize>, bool) {
        let mut heap = BinaryHeap::new();
        for segment_slot in 0..=self.deltas.len() {
            for &family_slot in semantic_family_slots(semantic_family) {
                self.push_short_prefix_cursor(&mut heap, segment_slot, family_slot, head, 0);
            }
        }

        let mut output = Vec::new();
        let mut inspected = 0usize;
        while inspected < budget {
            if metrics.should_cancel(cancellation) {
                return (Vec::new(), !heap.is_empty());
            }
            let Some(next) = heap.pop() else {
                break;
            };
            inspected += 1;
            metrics.entries_inspected += 1;
            metrics.prefix_entries_inspected += 1;
            if self.is_active_index(next.index) {
                output.push(next.index);
            }
            self.push_short_prefix_cursor(
                &mut heap,
                next.segment_slot,
                next.family_slot,
                head,
                next.position + 1,
            );
        }
        (output, !heap.is_empty())
    }

    fn fuzzy_posting_len(
        &self,
        token: u32,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
    ) -> usize {
        let mut total = 0usize;
        for segment_slot in 0..=self.deltas.len() {
            let (segment, _) = self.segment_with_offset(segment_slot);
            for &family_slot in semantic_family_slots(semantic_family) {
                total = total.saturating_add(
                    segment.fuzzy_postings_by_family[family_slot]
                        .posting(token)
                        .len(),
                );
            }
        }
        total
    }

    fn rarest_fuzzy_posting(
        &self,
        tokens: &[u32],
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
    ) -> Option<u32> {
        tokens
            .iter()
            .copied()
            .filter_map(|token| {
                let len = self.fuzzy_posting_len(token, semantic_family);
                (len > 0).then_some((len, token))
            })
            .min()
            .map(|(_, token)| token)
    }

    fn push_fuzzy_posting_cursor<'a>(
        &'a self,
        heap: &mut BinaryHeap<FuzzyPostingHeapEntry<'a>>,
        segment_slot: usize,
        family_slot: usize,
        token: u32,
        position: usize,
    ) {
        let (segment, _) = self.segment_with_offset(segment_slot);
        let Some(&local) = segment.fuzzy_postings_by_family[family_slot]
            .posting(token)
            .get(position)
        else {
            return;
        };
        heap.push(FuzzyPostingHeapEntry {
            table: self,
            segment_slot,
            family_slot,
            token,
            position,
            name_id: local,
        });
    }

    fn segment_entries_for_name(
        &self,
        segment_slot: usize,
        family_slot: usize,
        name_id: u32,
    ) -> &[usize] {
        let (segment, _) = self.segment_with_offset(segment_slot);
        let target = &segment.names[name_id as usize];
        let sorted = &segment.sorted_by_family[family_slot];
        let compare = |local: usize| {
            let entry = segment.entries[local];
            let name = &segment.names[entry.name_id as usize];
            name.lower
                .cmp(&target.lower)
                .then_with(|| name.original.cmp(&target.original))
        };
        let start = sorted.partition_point(|&local| compare(local).is_lt());
        let end = sorted.partition_point(|&local| !compare(local).is_gt());
        &sorted[start..end]
    }

    /// Traverse the rarest continuous and boundary-initial trigram postings as
    /// alternative fuzzy match classes. Posting tokens only generate a bounded
    /// superset; `consider` still validates and scores the complete query.
    fn bounded_fuzzy_posting_candidates(
        &self,
        needle: &str,
        budget: usize,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
        metrics: &mut RecallScanMetrics,
    ) -> (Vec<usize>, bool) {
        let (continuous, boundary) = fuzzy_query_tokens(needle);
        let mut selected_tokens = Vec::with_capacity(2);
        if let Some(token) = self.rarest_fuzzy_posting(&continuous, semantic_family) {
            selected_tokens.push(token);
        }
        if let Some(token) = self.rarest_fuzzy_posting(&boundary, semantic_family) {
            selected_tokens.push(token);
        }

        let mut output = Vec::new();
        let mut expanded_names = HashSet::new();
        let mut any_truncated = false;
        let mut remaining_budget = budget;
        let source_count = selected_tokens.len();
        for (source_index, token) in selected_tokens.into_iter().enumerate() {
            let sources_left = source_count - source_index;
            let source_budget = remaining_budget / sources_left;
            let mut heap = BinaryHeap::new();
            for segment_slot in 0..=self.deltas.len() {
                for &family_slot in semantic_family_slots(semantic_family) {
                    self.push_fuzzy_posting_cursor(&mut heap, segment_slot, family_slot, token, 0);
                }
            }

            let mut inspected = 0usize;
            while inspected < source_budget {
                if metrics.should_cancel(cancellation) {
                    return (Vec::new(), any_truncated || !heap.is_empty());
                }
                let Some(next) = heap.pop() else {
                    break;
                };
                inspected += 1;
                metrics.entries_inspected += 1;
                metrics.fuzzy_entries_inspected += 1;
                metrics.fuzzy_posting_entries_inspected += 1;
                self.push_fuzzy_posting_cursor(
                    &mut heap,
                    next.segment_slot,
                    next.family_slot,
                    next.token,
                    next.position + 1,
                );
                if !expanded_names.insert((next.segment_slot, next.family_slot, next.name_id)) {
                    continue;
                }
                let (_, offset) = self.segment_with_offset(next.segment_slot);
                let locals = self.segment_entries_for_name(
                    next.segment_slot,
                    next.family_slot,
                    next.name_id,
                );
                for &local in locals {
                    if inspected >= source_budget {
                        any_truncated = true;
                        break;
                    }
                    inspected += 1;
                    metrics.entries_inspected += 1;
                    metrics.fuzzy_entries_inspected += 1;
                    metrics.fuzzy_posting_entries_inspected += 1;
                    let index = offset + local;
                    if self.is_active_index(index) {
                        output.push(index);
                    }
                }
            }
            remaining_budget = remaining_budget.saturating_sub(inspected);
            any_truncated |= !heap.is_empty();
        }
        (output, any_truncated)
    }

    #[cfg(test)]
    pub fn exact_name_hits_scoped(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
    ) -> Vec<RankedNameHit> {
        self.exact_name_hits_scoped_filtered(name, limit, scope, None)
    }

    pub fn exact_name_hits_scoped_for_family(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
        semantic_family: crate::semantic_model::SemanticFamily,
    ) -> Vec<RankedNameHit> {
        self.exact_name_hits_scoped_filtered(name, limit, scope, Some(semantic_family))
    }

    fn exact_name_hits_scoped_filtered(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
    ) -> Vec<RankedNameHit> {
        if name.is_empty() || limit == 0 {
            return Vec::new();
        }
        let needle = name.to_ascii_lowercase();
        let indices: Vec<usize> = self
            .prefix_candidates_filtered(&needle, semantic_family)
            .into_iter()
            .filter(|index| {
                let entry = self.entry(*index);
                entry.lower == needle
                    && semantic_family.is_none_or(|family| entry.semantic_family == family)
            })
            .collect();
        let ctx_owned: Option<ResolveContext<'_>> = scope.map(|s| s.resolve_context());
        self.rank_indices(&needle, limit, ctx_owned.as_ref(), &indices)
    }

    pub fn len(&self) -> usize {
        self.active_len
    }

    /// In-memory equivalent of `store::kind_counts_by_names_scoped`, restricted
    /// to the colorable kinds (macro / type / enum_constant). Those kinds are
    /// always `role = 'definition'` in the index, so counting entries of those
    /// kinds reproduces the SQL definition-count exactly — without opening the
    /// database on the coloring hot path.
    ///
    /// The in-scope gate is delegated to the shared [`resolver::scope_tier`]
    /// policy through [`resolver::coloring_scope_allows`]: determinate
    /// in-scope tiers (`Current`, `Reachable`, or first-layer `External`) count,
    /// as do bounded paths in `ReachScope::heuristic_files`. A candidate that
    /// is `Unknown` only because the scope is open does **not** count, so
    /// unresolved includes cannot admit unrelated whole-index definitions.
    /// `scope = None` (scoping disabled, no graph, or no current file)
    /// preserves the prior unscoped fallback
    /// `workspace OR directly_included` by synthesizing a context whose
    /// reachable set contains every workspace file: workspace → `Reachable`
    /// (colors), first-layer external → `External` (colors), non-first-layer
    /// external → `Global` (does not color). Names with no colorable in-scope
    /// definition are absent from the result (they resolve to no color),
    /// matching the SQL behavior.
    #[cfg(test)]
    pub fn colorable_kind_counts(
        &self,
        names: &HashSet<&str>,
        scope: Option<&CompletionScope>,
    ) -> HashMap<String, HashMap<String, usize>> {
        self.colorable_kind_counts_filtered(names, scope, None)
    }

    pub fn colorable_kind_counts_for_family(
        &self,
        names: &HashSet<&str>,
        scope: Option<&CompletionScope>,
        semantic_family: crate::semantic_model::SemanticFamily,
    ) -> HashMap<String, HashMap<String, usize>> {
        self.colorable_kind_counts_filtered(names, scope, Some(semantic_family))
    }

    fn colorable_kind_counts_filtered(
        &self,
        names: &HashSet<&str>,
        scope: Option<&CompletionScope>,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
    ) -> HashMap<String, HashMap<String, usize>> {
        let mut counts: HashMap<String, HashMap<String, usize>> = HashMap::new();
        if names.is_empty() {
            return counts;
        }
        // Synthesize a context for the unscoped fallback (scope = None): a
        // closed scope whose reachable set contains every workspace file. The
        // resolver then maps workspace → Reachable, first-layer external →
        // External, non-first-layer external → Global — reproducing the prior
        // `workspace OR directly_included` gate via the shared primitive
        // rather than a per-entry ad-hoc test.
        let ctx_owned: Option<ResolveContext<'_>> = match scope {
            Some(s) => Some(s.resolve_context()),
            None => Some(ResolveContext {
                current_path: None,
                reach: Some(self.all_workspace_reach.as_ref()),
                direct_external_files: None,
            }),
        };
        let ctx_ref = ctx_owned.as_ref();
        for index in self.active_indices() {
            let entry = self.entry(index);
            if semantic_family.is_some_and(|family| entry.semantic_family != family) {
                continue;
            }
            let kind = match entry.kind {
                ParserKind::Macro => "macro",
                ParserKind::Type => "type",
                ParserKind::EnumConstant => "enum_constant",
                // Non-colorable kinds never affect `resolve_kind`; skip them.
                _ => continue,
            };
            if !names.contains(&entry.name) {
                continue;
            }
            let in_scope = resolver::coloring_scope_allows(
                entry.path,
                entry.external,
                self.directly_included_for(entry),
                ctx_ref,
            );
            if !in_scope {
                continue;
            }
            *counts
                .entry(entry.name.to_string())
                .or_default()
                .entry(kind.to_string())
                .or_insert(0) += 1;
        }
        counts
    }

    /// Return up to `limit` matching symbol ids, best match first.
    #[cfg(test)]
    pub fn search(&self, query: &str, limit: usize) -> Vec<i64> {
        self.search_ranked(query, limit)
            .into_iter()
            .map(|hit| hit.id)
            .collect()
    }

    /// Return up to `limit` matching symbol names with their ranking metadata.
    ///
    /// Unscoped fast path: when the exact/prefix candidates already fill the
    /// limit, no lower-scored fuzzy match (boundary-substring 650 at best) can
    /// enter the unscoped top-N (the minimum exact/prefix score is 750), so the
    /// full scan is skipped via the prefix index. Otherwise falls back to the
    /// full scan, which is identical to scoped search with `scope = None`.
    pub fn search_ranked(&self, query: &str, limit: usize) -> Vec<RankedNameHit> {
        let trimmed = query.trim();
        if !trimmed.is_empty() && limit > 0 {
            let needle = trimmed.to_ascii_lowercase();
            let candidates = self.prefix_candidates(&needle);
            if candidates.len() >= limit {
                return self.rank_indices(&needle, limit, None, &candidates);
            }
        }
        self.search_ranked_scoped(query, limit, None)
    }

    /// Reachability-scoped variant of [`search_ranked`]. When `scope` is set,
    /// candidates are re-ranked by whether their defining file is the current
    /// file, reachable via `#include`, or neither — without filtering any out.
    pub fn search_ranked_scoped(
        &self,
        query: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
    ) -> Vec<RankedNameHit> {
        self.search_ranked_scoped_pooled(query, limit, scope, None)
            .0
    }

    /// Pooled/narrowable scoped search. Returns the ranked hits plus a
    /// *tier-agnostic* candidate pool: every entry whose `score_match` is `Some`
    /// for `query`, regardless of the short-prefix recall gate. Because a prefix
    /// of a subsequence is itself a subsequence, the matches of any extending
    /// prefix are a subset of this pool — so a follow-up keystroke can re-score
    /// `prior_pool` instead of the whole table and still produce identical hits.
    ///
    /// `prior_pool = Some(pool)` restricts the scan to those indices (narrowing);
    /// `None` scans the whole table (a cold query). Callers must only narrow when
    /// the new prefix extends the prefix that produced `prior_pool`.
    ///
    /// Ranking is strict-tier lexicographic via [`resolver::pack_score`]: tier
    /// dominates `base_match` (fuzzy match quality), which dominates locality.
    /// The narrowing pool / prefix-index fast paths are unchanged — they gate on
    /// `base_match`, which is unchanged per entry, so pooling stays valid.
    pub fn search_ranked_scoped_pooled(
        &self,
        query: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<RankedNameHit>, Vec<usize>) {
        let ctx_owned: Option<ResolveContext<'_>> = scope.map(|s| s.resolve_context());
        let ctx_ref = ctx_owned.as_ref();
        let query = query.trim();
        if query.is_empty() {
            // Empty query: rank by tier first, then name. The packed score
            // encodes (tier, 0, locality) so sorting by score desc reproduces
            // the strict-tier order; ties on tier break by name asc.
            let scored: Vec<ScoredCandidate> = self
                .active_indices()
                .map(|index| {
                    let entry = self.entry(index);
                    let tier = resolver::scope_tier(
                        entry.path,
                        entry.external,
                        self.directly_included_for(entry),
                        ctx_ref,
                    );
                    let loc = resolver::locality(entry.path, ctx_ref.and_then(|c| c.current_path));
                    let score = resolver::pack_score(tier, 0, loc);
                    ScoredCandidate {
                        score,
                        name_len: entry.name.len(),
                        index,
                        tier,
                        base_match: 0,
                    }
                })
                .collect();
            let hits = self.scored_to_hits(top_scored(scored, limit, self));
            // An empty query establishes no usable narrowing base.
            return (hits, Vec::new());
        }

        let needle = query.to_ascii_lowercase();
        // Short-prefix recall tightening (D3): for needles shorter than 3
        // characters, require a minimum raw score of 650 so only exact, prefix,
        // and word-boundary-substring hits qualify. Plain substrings (500) and
        // all subsequence tiers (400/200) are dropped, eliminating the
        // random-looking long tail at 2 chars. At len >= 3 the full tier set
        // (including camelCase-initials subsequences) is restored. The
        // threshold is applied to the raw `score_match` output (the per-entry
        // `base_match`), before tier/locality packing, so an external
        // boundary-substr hit still passes.
        let min_score = if needle.len() < SHORT_PREFIX_MIN_LEN {
            SHORT_PREFIX_MIN_SCORE
        } else {
            0
        };
        let mut scored: Vec<ScoredCandidate> = Vec::new();
        let mut pool: Vec<usize> = Vec::new();
        match prior_pool {
            Some(indices) => {
                for &i in indices {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
            None => {
                for i in self.active_indices() {
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                }
            }
        }

        let hits = self.rank_scored(scored, limit, ctx_ref);
        (hits, pool)
    }

    #[allow(dead_code)]
    pub fn search_completion_recall_pooled(
        &self,
        query: &str,
        quotas: CompletionRecallQuotas,
        scope: Option<&CompletionScope>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        self.search_completion_recall_pooled_with_project(query, quotas, scope, None, prior_pool)
    }

    pub fn search_completion_recall_pooled_with_project(
        &self,
        query: &str,
        quotas: CompletionRecallQuotas,
        scope: Option<&CompletionScope>,
        active_project: Option<&ProjectKey>,
        prior_pool: Option<&[usize]>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        self.search_completion_recall_pooled_with_project_filtered(super::CompletionRecallQuery {
            query,
            quotas,
            scope,
            active_project,
            prior_pool,
            semantic_family: None,
            cancellation: None,
            candidate_budget: usize::MAX,
        })
    }

    #[cfg(test)]
    pub(crate) fn search_completion_recall_pooled_controlled(
        &self,
        query: super::CompletionRecallQuery<'_>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        debug_assert!(query.cancellation.is_some());
        self.search_completion_recall_pooled_bounded(query)
    }

    pub(crate) fn search_completion_recall_pooled_bounded(
        &self,
        query: super::CompletionRecallQuery<'_>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        self.search_completion_recall_pooled_with_project_filtered(query)
    }

    fn search_completion_recall_pooled_with_project_filtered(
        &self,
        query: super::CompletionRecallQuery<'_>,
    ) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
        let total_limit = query.quotas.total_indexed;
        let (mut scored, mut pool, mut scan_metrics) = self.scored_pool_for_query(&query);
        if scan_metrics.cancelled {
            return cancelled_recall(scan_metrics);
        }
        if scan_metrics.check(query.cancellation) {
            return cancelled_recall(scan_metrics);
        }
        if let Some(semantic_family) = query.semantic_family {
            scored
                .retain(|candidate| self.entry(candidate.index).semantic_family == semantic_family);
            pool.retain(|index| self.entry(*index).semantic_family == semantic_family);
        }
        if scan_metrics.check(query.cancellation) {
            return cancelled_recall(scan_metrics);
        }
        let reserved = query
            .quotas
            .reachable
            .saturating_add(query.quotas.external)
            .saturating_add(query.quotas.unknown)
            .saturating_add(query.quotas.global)
            .saturating_add(query.quotas.same_project);
        let Some(global_top) = top_scored_controlled(
            scored.iter().copied(),
            |_| true,
            total_limit.saturating_add(reserved),
            self,
            query.cancellation,
            &mut scan_metrics,
        ) else {
            return cancelled_recall(scan_metrics);
        };

        let mut selected_indices = HashSet::new();
        let mut selected = Vec::new();
        let Some(reachable) = top_scored_controlled(
            scored.iter().copied(),
            |candidate| channel_for_tier(candidate.tier) == ScopeChannel::Reachable,
            query.quotas.reachable,
            self,
            query.cancellation,
            &mut scan_metrics,
        ) else {
            return cancelled_recall(scan_metrics);
        };
        take_channel(
            &reachable,
            ScopeChannel::Reachable,
            query.quotas.reachable,
            &mut selected_indices,
            &mut selected,
        );
        let same_project = if let Some(key) = query.active_project {
            let Some(top) = top_scored_controlled(
                scored.iter().copied(),
                |candidate| self.entry(candidate.index).project_key == Some(key),
                query.quotas.same_project,
                self,
                query.cancellation,
                &mut scan_metrics,
            ) else {
                return cancelled_recall(scan_metrics);
            };
            top
        } else {
            Vec::new()
        };
        take_same_project(
            self,
            &same_project,
            query.active_project,
            query.quotas.same_project,
            &mut selected_indices,
            &mut selected,
        );
        for (channel, quota) in [
            (ScopeChannel::External, query.quotas.external),
            (ScopeChannel::Unknown, query.quotas.unknown),
            (ScopeChannel::Global, query.quotas.global),
        ] {
            let Some(channel_top) = top_scored_controlled(
                scored.iter().copied(),
                |candidate| channel_for_tier(candidate.tier) == channel,
                quota,
                self,
                query.cancellation,
                &mut scan_metrics,
            ) else {
                return cancelled_recall(scan_metrics);
            };
            take_channel(
                &channel_top,
                channel,
                quota,
                &mut selected_indices,
                &mut selected,
            );
        }

        for candidate in &global_top {
            if selected.len() >= total_limit {
                break;
            }
            if selected_indices.insert(candidate.index) {
                selected.push(*candidate);
            }
        }

        if scan_metrics.check(query.cancellation) {
            return cancelled_recall(scan_metrics);
        }
        sort_scored(&mut selected, self);
        selected.truncate(total_limit);
        let hits = self.scored_to_hits(selected);
        let mut metrics = recall_metrics(&hits, pool.len(), query.active_project);
        metrics.entries_inspected = scan_metrics.entries_inspected;
        metrics.prefix_entries_inspected = scan_metrics.prefix_entries_inspected;
        metrics.fuzzy_entries_inspected = scan_metrics.fuzzy_entries_inspected;
        metrics.fuzzy_posting_entries_inspected = scan_metrics.fuzzy_posting_entries_inspected;
        metrics.fuzzy_sample_entries_inspected = scan_metrics.fuzzy_sample_entries_inspected;
        metrics.priority_source_probes = scan_metrics.priority_source_probes;
        metrics.priority_source_attempts = scan_metrics.priority_source_attempts;
        metrics.priority_sources_initialized = scan_metrics.priority_sources_initialized;
        metrics.priority_fuzzy_name_probes = scan_metrics.priority_fuzzy_name_probes;
        metrics.priority_fuzzy_declaration_probes = scan_metrics.priority_fuzzy_declaration_probes;
        metrics.selection_entries_inspected = scan_metrics.selection_entries_inspected;
        metrics.active_entries_total = scan_metrics.active_entries_total;
        metrics.candidate_budget = scan_metrics.candidate_budget;
        metrics.cancellation_checks = scan_metrics.cancellation_checks;
        metrics.truncated = scan_metrics.truncated;
        (hits, pool, metrics)
    }

    /// Score entry `i` against `needle`: push it into the tier-agnostic `pool`
    /// when it matches at all, and into `scored` (with the resolver's packed
    /// sort key) when it also clears the short-prefix gate. The packed score
    /// encodes `(tier, base_match, locality)` so tier strictly dominates
    /// `base_match`; the pool gates only on `base_match` (unchanged per entry),
    /// so narrowing stays valid across keystrokes.
    fn consider(
        &self,
        i: usize,
        needle: &str,
        min_score: i32,
        ctx: Option<&ResolveContext<'_>>,
        scored: &mut Vec<ScoredCandidate>,
        pool: &mut Vec<usize>,
    ) {
        if !self.is_active_index(i) {
            return;
        }
        let entry = self.entry(i);
        if let Some(base_match) = score_match(needle, entry) {
            pool.push(i);
            if base_match < min_score {
                return;
            }
            let tier = resolver::scope_tier(
                entry.path,
                entry.external,
                self.directly_included_for(entry),
                ctx,
            );
            let loc = resolver::locality(entry.path, ctx.and_then(|c| c.current_path));
            let score = resolver::pack_score(tier, base_match, loc);
            scored.push(ScoredCandidate {
                score,
                name_len: entry.name.len(),
                index: i,
                tier,
                base_match,
            });
        }
    }

    /// Rank a set of candidate indices for the unscoped fast path: score, sort,
    /// and truncate exactly as the full scan would.
    fn rank_indices(
        &self,
        needle: &str,
        limit: usize,
        ctx: Option<&ResolveContext<'_>>,
        candidates: &[usize],
    ) -> Vec<RankedNameHit> {
        let mut scored: Vec<ScoredCandidate> = Vec::new();
        let mut pool: Vec<usize> = Vec::new();
        for &i in candidates {
            self.consider(i, needle, 0, ctx, &mut scored, &mut pool);
        }
        self.rank_scored(scored, limit, ctx)
    }

    /// Sort `(score, name_len, index)` tuples best-first and resolve them into
    /// `RankedNameHit`s, truncated to `limit`. The `score` is the resolver's
    /// packed key; the hit also carries the per-entry `tier` and `base_match`
    /// so callers can dedup by `(tier, confidence)` and derive labels without
    /// re-deriving the tier.
    fn rank_scored(
        &self,
        scored: Vec<ScoredCandidate>,
        limit: usize,
        _ctx: Option<&ResolveContext<'_>>,
    ) -> Vec<RankedNameHit> {
        self.scored_to_hits(top_scored(scored, limit, self))
    }

    fn scored_pool_for_query(
        &self,
        request: &super::CompletionRecallQuery<'_>,
    ) -> (Vec<ScoredCandidate>, Vec<usize>, RecallScanMetrics) {
        let scope = request.scope;
        let prior_pool = request.prior_pool;
        let semantic_family = request.semantic_family;
        let cancellation = request.cancellation;
        let candidate_budget = request.candidate_budget;
        let ctx_owned: Option<ResolveContext<'_>> = scope.map(|s| s.resolve_context());
        let ctx_ref = ctx_owned.as_ref();
        let query = request.query.trim();
        let mut scan_metrics = RecallScanMetrics {
            active_entries_total: self.active_len,
            candidate_budget,
            ..RecallScanMetrics::default()
        };
        let source_slot_len = self.candidate_slot_len(semantic_family);
        if query.is_empty() {
            let mut scored = Vec::new();
            let inspected = source_slot_len.min(candidate_budget);
            for ordinal in 0..inspected {
                if scan_metrics.should_cancel(cancellation) {
                    return (Vec::new(), Vec::new(), scan_metrics);
                }
                let source_ordinal = if inspected == source_slot_len {
                    ordinal
                } else {
                    ((ordinal as u128 * source_slot_len as u128) / inspected as u128) as usize
                };
                let index = self
                    .candidate_index_at(semantic_family, source_ordinal)
                    .expect("candidate source ordinal must be in range");
                scan_metrics.entries_inspected += 1;
                scan_metrics.fuzzy_entries_inspected += 1;
                scan_metrics.fuzzy_sample_entries_inspected += 1;
                if !self.is_active_index(index) {
                    continue;
                }
                let entry = self.entry(index);
                let tier = resolver::scope_tier(
                    entry.path,
                    entry.external,
                    self.directly_included_for(entry),
                    ctx_ref,
                );
                let loc = resolver::locality(entry.path, ctx_ref.and_then(|c| c.current_path));
                scored.push(ScoredCandidate {
                    score: resolver::pack_score(tier, 0, loc),
                    name_len: entry.name.len(),
                    index,
                    tier,
                    base_match: 0,
                });
            }
            scan_metrics.truncated = inspected < source_slot_len;
            if scan_metrics.cancel_after_scan(cancellation) {
                return (Vec::new(), Vec::new(), scan_metrics);
            }
            return (scored, Vec::new(), scan_metrics);
        }

        let needle = query.to_ascii_lowercase();
        let min_score = if needle.len() < SHORT_PREFIX_MIN_LEN {
            SHORT_PREFIX_MIN_SCORE
        } else {
            0
        };
        let mut scored = Vec::new();
        let mut pool = Vec::new();
        match prior_pool {
            Some(indices) => {
                let inspected = indices.len().min(candidate_budget);
                for &i in &indices[..inspected] {
                    if scan_metrics.should_cancel(cancellation) {
                        return (Vec::new(), Vec::new(), scan_metrics);
                    }
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                    scan_metrics.entries_inspected += 1;
                    scan_metrics.fuzzy_entries_inspected += 1;
                    scan_metrics.fuzzy_sample_entries_inspected += 1;
                }
                scan_metrics.truncated = inspected < indices.len();
            }
            None if source_slot_len <= candidate_budget => {
                for ordinal in 0..source_slot_len {
                    if scan_metrics.should_cancel(cancellation) {
                        return (Vec::new(), Vec::new(), scan_metrics);
                    }
                    let i = self
                        .candidate_index_at(semantic_family, ordinal)
                        .expect("candidate source ordinal must be in range");
                    self.consider(i, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                    scan_metrics.entries_inspected += 1;
                    scan_metrics.fuzzy_entries_inspected += 1;
                    scan_metrics.fuzzy_sample_entries_inspected += 1;
                }
            }
            None => {
                // Scope/project evidence has strict ranking priority over
                // unrelated global candidates. Reserve part of the same hard
                // budget for compact path/project postings before the general
                // lexical and fuzzy channels.
                let priority_budget = candidate_budget / 8;
                let (priority_indices, priority_truncated) = self
                    .bounded_priority_prefix_candidates(
                        &needle,
                        priority_budget,
                        request,
                        &mut scan_metrics,
                    );
                if scan_metrics.cancelled {
                    return (Vec::new(), Vec::new(), scan_metrics);
                }
                let mut seen = HashSet::with_capacity(priority_indices.len());
                for index in priority_indices {
                    if seen.insert(index) {
                        self.consider(index, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                    }
                }

                // Prefix rows carry the strongest lexical evidence. Reserve a
                // quarter of the request budget for a deterministic workspace-
                // wide fuzzy sample so substring/camel/subsequence fallback is
                // never silently converted into a prefix-only hard filter.
                let remaining = candidate_budget.saturating_sub(scan_metrics.entries_inspected);
                let prefix_budget = remaining.saturating_mul(3) / 4;
                let (prefix_indices, prefix_truncated) = if needle.len() == 1 {
                    self.bounded_short_prefix_candidates(
                        needle.as_bytes()[0],
                        prefix_budget,
                        semantic_family,
                        cancellation,
                        &mut scan_metrics,
                    )
                } else {
                    self.bounded_prefix_candidates(
                        &needle,
                        prefix_budget,
                        semantic_family,
                        cancellation,
                        &mut scan_metrics,
                    )
                };
                if scan_metrics.cancelled {
                    return (Vec::new(), Vec::new(), scan_metrics);
                }
                for index in prefix_indices {
                    if seen.insert(index) {
                        self.consider(index, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                    }
                }

                let remaining = candidate_budget.saturating_sub(scan_metrics.entries_inspected);
                let posting_budget = remaining.saturating_mul(3) / 4;
                let (posting_indices, posting_truncated) = self.bounded_fuzzy_posting_candidates(
                    &needle,
                    posting_budget,
                    semantic_family,
                    cancellation,
                    &mut scan_metrics,
                );
                if scan_metrics.cancelled {
                    return (Vec::new(), Vec::new(), scan_metrics);
                }
                for index in posting_indices {
                    if seen.insert(index) {
                        self.consider(index, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                    }
                }

                let remaining = candidate_budget.saturating_sub(scan_metrics.entries_inspected);
                let fuzzy_samples = remaining.min(source_slot_len);
                for ordinal in 0..fuzzy_samples {
                    if scan_metrics.should_cancel(cancellation) {
                        return (Vec::new(), Vec::new(), scan_metrics);
                    }
                    let source_ordinal = ((ordinal as u128 * source_slot_len as u128)
                        / fuzzy_samples as u128) as usize;
                    let index = self
                        .candidate_index_at(semantic_family, source_ordinal)
                        .expect("candidate source ordinal must be in range");
                    scan_metrics.entries_inspected += 1;
                    scan_metrics.fuzzy_entries_inspected += 1;
                    scan_metrics.fuzzy_sample_entries_inspected += 1;
                    if seen.insert(index) {
                        self.consider(index, &needle, min_score, ctx_ref, &mut scored, &mut pool);
                    }
                }
                scan_metrics.truncated = priority_truncated
                    || prefix_truncated
                    || posting_truncated
                    || fuzzy_samples < source_slot_len;
            }
        }
        if scan_metrics.cancel_after_scan(cancellation) {
            return (Vec::new(), Vec::new(), scan_metrics);
        }
        (scored, pool, scan_metrics)
    }

    fn candidate_slot_len(
        &self,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
    ) -> usize {
        match semantic_family {
            None => self.slot_len,
            Some(family) => {
                let family_slot = semantic_family_slot(family);
                (0..=self.deltas.len())
                    .map(|segment_slot| {
                        self.segment_with_offset(segment_slot).0.sorted_by_family[family_slot].len()
                    })
                    .sum()
            }
        }
    }

    fn candidate_index_at(
        &self,
        semantic_family: Option<crate::semantic_model::SemanticFamily>,
        mut ordinal: usize,
    ) -> Option<usize> {
        let Some(family) = semantic_family else {
            return (ordinal < self.slot_len).then_some(ordinal);
        };
        let family_slot = semantic_family_slot(family);
        for segment_slot in 0..=self.deltas.len() {
            let (segment, offset) = self.segment_with_offset(segment_slot);
            let indices = &segment.sorted_by_family[family_slot];
            if ordinal < indices.len() {
                return Some(offset + indices[ordinal]);
            }
            ordinal -= indices.len();
        }
        None
    }

    fn scored_to_hits(&self, scored: Vec<ScoredCandidate>) -> Vec<RankedNameHit> {
        scored
            .into_iter()
            .map(|candidate| {
                let entry = self.entry(candidate.index);
                RankedNameHit {
                    id: entry.id,
                    score: candidate.score,
                    tier: candidate.tier,
                    base_match: candidate.base_match,
                    name_len: candidate.name_len,
                    name: entry.name.to_string(),
                    kind: entry.kind,
                    role: entry.role,
                    semantic_family: entry.semantic_family,
                    project_key: entry.project_key.cloned(),
                }
            })
            .collect()
    }
}

fn cancelled_recall(
    scan_metrics: RecallScanMetrics,
) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics) {
    (
        Vec::new(),
        Vec::new(),
        CompletionRecallMetrics {
            entries_inspected: scan_metrics.entries_inspected,
            prefix_entries_inspected: scan_metrics.prefix_entries_inspected,
            fuzzy_entries_inspected: scan_metrics.fuzzy_entries_inspected,
            fuzzy_posting_entries_inspected: scan_metrics.fuzzy_posting_entries_inspected,
            fuzzy_sample_entries_inspected: scan_metrics.fuzzy_sample_entries_inspected,
            priority_source_probes: scan_metrics.priority_source_probes,
            priority_source_attempts: scan_metrics.priority_source_attempts,
            priority_sources_initialized: scan_metrics.priority_sources_initialized,
            priority_fuzzy_name_probes: scan_metrics.priority_fuzzy_name_probes,
            priority_fuzzy_declaration_probes: scan_metrics.priority_fuzzy_declaration_probes,
            selection_entries_inspected: scan_metrics.selection_entries_inspected,
            active_entries_total: scan_metrics.active_entries_total,
            candidate_budget: scan_metrics.candidate_budget,
            cancellation_checks: scan_metrics.cancellation_checks,
            cancelled: true,
            truncated: scan_metrics.truncated,
            ..CompletionRecallMetrics::default()
        },
    )
}

#[derive(Default)]
struct RecallScanMetrics {
    entries_inspected: usize,
    prefix_entries_inspected: usize,
    fuzzy_entries_inspected: usize,
    fuzzy_posting_entries_inspected: usize,
    fuzzy_sample_entries_inspected: usize,
    priority_source_probes: usize,
    priority_source_attempts: usize,
    priority_sources_initialized: usize,
    priority_fuzzy_name_probes: usize,
    priority_fuzzy_declaration_probes: usize,
    selection_entries_inspected: usize,
    active_entries_total: usize,
    candidate_budget: usize,
    cancellation_checks: usize,
    cancelled: bool,
    truncated: bool,
}

impl RecallScanMetrics {
    fn should_cancel(
        &mut self,
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
    ) -> bool {
        if !self
            .entries_inspected
            .is_multiple_of(super::COMPLETION_CANCELLATION_CHECK_INTERVAL)
        {
            return false;
        }
        self.check(cancellation)
    }

    fn cancel_after_scan(
        &mut self,
        cancellation: Option<&dyn super::CompletionQueryCancellation>,
    ) -> bool {
        self.check(cancellation)
    }

    fn check(&mut self, cancellation: Option<&dyn super::CompletionQueryCancellation>) -> bool {
        let Some(cancellation) = cancellation else {
            return false;
        };
        self.cancellation_checks += 1;
        self.cancelled = cancellation.is_cancelled();
        self.cancelled
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeChannel {
    Reachable,
    External,
    Unknown,
    Global,
}

fn channel_for_tier(tier: ScopeTier) -> ScopeChannel {
    match tier {
        ScopeTier::Current | ScopeTier::Reachable => ScopeChannel::Reachable,
        ScopeTier::External => ScopeChannel::External,
        ScopeTier::Unknown => ScopeChannel::Unknown,
        ScopeTier::Global => ScopeChannel::Global,
    }
}

fn take_channel(
    scored: &[ScoredCandidate],
    channel: ScopeChannel,
    quota: usize,
    selected_indices: &mut HashSet<usize>,
    selected: &mut Vec<ScoredCandidate>,
) {
    if quota == 0 {
        return;
    }
    let mut taken = 0;
    for candidate in scored {
        if taken >= quota {
            break;
        }
        if channel_for_tier(candidate.tier) != channel {
            continue;
        }
        if selected_indices.insert(candidate.index) {
            selected.push(*candidate);
            taken += 1;
        }
    }
}

fn take_same_project(
    table: &NameTable,
    scored: &[ScoredCandidate],
    active_project: Option<&ProjectKey>,
    quota: usize,
    selected_indices: &mut HashSet<usize>,
    selected: &mut Vec<ScoredCandidate>,
) {
    let Some(key) = active_project else {
        return;
    };
    if quota == 0 {
        return;
    }
    let mut taken = 0;
    for candidate in scored {
        if taken >= quota {
            break;
        }
        if table.entry(candidate.index).project_key != Some(key) {
            continue;
        }
        if selected_indices.insert(candidate.index) {
            selected.push(*candidate);
            taken += 1;
        }
    }
}

pub(super) fn sort_scored(scored: &mut [ScoredCandidate], table: &NameTable) {
    scored.sort_by(|a, b| scored_order(a, b, table));
}

fn scored_order(a: &ScoredCandidate, b: &ScoredCandidate, table: &NameTable) -> std::cmp::Ordering {
    let a_entry = table.entry(a.index);
    let b_entry = table.entry(b.index);
    b.score
        .cmp(&a.score)
        .then(a.name_len.cmp(&b.name_len))
        .then_with(|| a_entry.name.cmp(b_entry.name))
        // Role is a same-logical-name recall tie-break only; it must not
        // reorder different labels before fuzzy/name quality has spoken.
        .then_with(|| {
            completion_role_recall_priority(b_entry.role)
                .cmp(&completion_role_recall_priority(a_entry.role))
        })
        .then_with(|| a_entry.path.cmp(b_entry.path))
        .then_with(|| a_entry.id.cmp(&b_entry.id))
}

fn top_scored_controlled<I, F>(
    scored: I,
    mut include: F,
    limit: usize,
    table: &NameTable,
    cancellation: Option<&dyn super::CompletionQueryCancellation>,
    scan_metrics: &mut RecallScanMetrics,
) -> Option<Vec<ScoredCandidate>>
where
    I: IntoIterator<Item = ScoredCandidate>,
    F: FnMut(&ScoredCandidate) -> bool,
{
    if limit == 0 {
        return Some(Vec::new());
    }
    if scan_metrics.check(cancellation) {
        return None;
    }

    let mut retained = BinaryHeap::with_capacity(limit);
    let mut block_len = 0usize;
    for candidate in scored {
        if include(&candidate) {
            let candidate = WorstScoredCandidate { candidate, table };
            if retained.len() < limit {
                retained.push(candidate);
            } else if retained
                .peek()
                .is_some_and(|worst| candidate.cmp(worst).is_lt())
            {
                retained.pop();
                retained.push(candidate);
            }
        }
        block_len += 1;
        scan_metrics.selection_entries_inspected =
            scan_metrics.selection_entries_inspected.saturating_add(1);
        if block_len == super::COMPLETION_CANCELLATION_CHECK_INTERVAL {
            block_len = 0;
            if scan_metrics.check(cancellation) {
                return None;
            }
        }
    }
    if block_len != 0 && scan_metrics.check(cancellation) {
        return None;
    }
    let mut retained: Vec<_> = retained
        .into_iter()
        .map(|candidate| candidate.candidate)
        .collect();
    sort_scored(&mut retained, table);
    Some(retained)
}

struct WorstScoredCandidate<'a> {
    candidate: ScoredCandidate,
    table: &'a NameTable,
}

impl PartialEq for WorstScoredCandidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for WorstScoredCandidate<'_> {}

impl PartialOrd for WorstScoredCandidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WorstScoredCandidate<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        scored_order(&self.candidate, &other.candidate, self.table)
    }
}

pub(super) fn top_scored(
    mut scored: Vec<ScoredCandidate>,
    limit: usize,
    table: &NameTable,
) -> Vec<ScoredCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    if scored.len() > limit {
        scored.select_nth_unstable_by(limit, |a, b| scored_order(a, b, table));
        scored.truncate(limit);
    }
    sort_scored(&mut scored, table);
    scored
}

fn recall_metrics(
    hits: &[RankedNameHit],
    pool_total: usize,
    active_project: Option<&ProjectKey>,
) -> CompletionRecallMetrics {
    let mut metrics = CompletionRecallMetrics {
        pool_total,
        indexed_returned: hits.len(),
        ..CompletionRecallMetrics::default()
    };
    for hit in hits {
        match channel_for_tier(hit.tier) {
            ScopeChannel::Reachable => metrics.reachable += 1,
            ScopeChannel::External => metrics.external += 1,
            ScopeChannel::Unknown => metrics.unknown += 1,
            ScopeChannel::Global => metrics.global += 1,
        }
        if active_project.is_some() && hit.project_key.as_ref() == active_project {
            metrics.same_project += 1;
        }
    }
    metrics
}

/// Score a single name against an already-lowercased query. `None` means no
/// match (not even a subsequence). Higher is better.
fn score_match(needle: &str, entry: NameEntryRef<'_>) -> Option<i32> {
    let hay = entry.lower;

    if hay == needle {
        return Some(1000);
    }
    if hay.starts_with(needle) {
        return Some(800);
    }
    if let Some(at) = hay.find(needle) {
        let boundary = is_boundary(entry.name.as_bytes(), at);
        return Some(if boundary { 650 } else { 500 });
    }
    subsequence_match(needle.as_bytes(), entry.name.as_bytes(), hay.as_bytes())
        .map(|all_boundary| if all_boundary { 400 } else { 200 })
}

/// Subsequence test that first asks whether an all-boundary path exists, then
/// falls back to an unrestricted path. A single greedy pass cannot answer both:
/// an internal occurrence may precede a later boundary occurrence of the same
/// byte (`dbdtn` in `device_bind_driver_to_node`).
fn subsequence_match(needle: &[u8], orig: &[u8], lower: &[u8]) -> Option<bool> {
    let mut boundary_qi = 0;
    for (index, &byte) in lower.iter().enumerate() {
        if boundary_qi < needle.len() && byte == needle[boundary_qi] && is_boundary(orig, index) {
            boundary_qi += 1;
        }
    }
    if boundary_qi == needle.len() {
        return Some(true);
    }

    let mut qi = 0;
    for &byte in lower {
        if qi < needle.len() && byte == needle[qi] {
            qi += 1;
        }
    }
    (qi == needle.len()).then_some(false)
}
