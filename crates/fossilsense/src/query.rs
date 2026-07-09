//! Protocol-agnostic query logic: in-memory fuzzy name table, definition
//! ranking, cursor-word extraction and symbol-kind mapping. Kept free of
//! `tower-lsp` request types so the scoring/ranking can be unit-tested.

#[cfg(test)]
use crate::model::ScopeTier;
#[cfg(test)]
use crate::parser::SymbolKind as ParserKind;

#[allow(dead_code)]
mod current_file_overlay;
mod definitions;
mod hover;
mod local_completion;
mod lsp_kinds;
mod name_table;
mod signatures;
mod text;

#[allow(unused_imports)]
pub use current_file_overlay::{current_file_overlay_candidates, CurrentFileOverlayCandidate};
pub use definitions::rank_definitions_into_candidates_with_scope;
pub use hover::{
    hover_markdown_for_candidate, leading_comment_markdown, rank_hover_candidates,
    RankedHoverCandidate, HOVER_CANDIDATE_LIMIT,
};
pub use local_completion::{local_completion_candidates, LocalCompletionCandidate};
pub use lsp_kinds::{lsp_kind_from_parser, lsp_symbol_kind};
pub use name_table::{
    CompletionRecallMetrics, CompletionRecallQuotas, CompletionScope, NameTable, RankedNameHit,
};
pub use signatures::{
    call_context_at, rank_function_signature_candidates, signature_parts, signature_parts_for_name,
    CallContext, ParameterSpan, RankedSignatureCandidate, SignatureParts, SIGNATURE_HELP_LIMIT,
};
pub use text::{
    byte_offset_at, completion_prefix_at, completion_word_score, is_member_completion_context,
    member_access_chain_at, word_at,
};

/// Default cap on workspace-symbol results handed back to the editor.
pub const WORKSPACE_SYMBOL_LIMIT: usize = 200;

pub const COMPLETION_LIMIT: usize = 100;
pub const COMPLETION_LOCALITY_BONUS: i32 = 50;
pub const MIN_PREFIX_LEN: usize = 1;
pub const MEMBER_COMPLETION_MIN_PREFIX_LEN: usize = 2;

#[allow(dead_code)]
pub fn normalized_receiver_record_hint(receiver_name: &str) -> String {
    receiver_name
        .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_digit())
        .to_ascii_lowercase()
}

/// Prefix lengths below this value use a tightened recall threshold
/// (`SHORT_PREFIX_MIN_SCORE`); at this length and above the full fuzzy tier
/// set (including subsequence / camelCase-initials matches) is restored.
pub const SHORT_PREFIX_MIN_LEN: usize = 3;

/// Minimum raw `score_match` accepted for short prefixes (len < 3): keeps the
/// exact (1000), prefix (800), and word-boundary-substring (650) tiers, drops
/// plain substrings (500) and all subsequence tiers (400/200).
pub const SHORT_PREFIX_MIN_SCORE: i32 = 650;

#[cfg(test)]
mod tests;
