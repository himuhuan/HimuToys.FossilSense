use std::collections::HashMap;

use crate::parser::{FactSource, FileSemanticIndex, SymbolKind, SymbolRole, TypeAlias};

use super::{byte_offset_at, completion_word_score};

const MAX_PROXIMITY_SCORE: i32 = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentFileOverlayCandidate {
    pub name: String,
    pub kind: crate::parser::SymbolKind,
    pub detail: Option<String>,
    pub match_score: i32,
    pub proximity_score: i32,
    pub source_start_byte: usize,
    pub semantic: bool,
}

pub fn current_file_overlay_candidates(
    index: &crate::parser::FileSemanticIndex,
    text: &str,
    line: u32,
    character: u32,
    prefix: &str,
    limit: usize,
) -> Vec<CurrentFileOverlayCandidate> {
    if limit == 0 {
        return Vec::new();
    }

    let cursor_byte = byte_offset_at(text, line, character).min(text.len());
    let mut by_name: HashMap<String, CurrentFileOverlayCandidate> = HashMap::new();
    let usage_stats = occurrence_usage_stats(index, cursor_byte, prefix);

    for symbol in &index.symbols {
        if !is_overlay_symbol(symbol.kind, symbol.role) {
            continue;
        }
        let Some(match_score) = completion_word_score(prefix, &symbol.name, 0) else {
            continue;
        };
        let proximity_score = usage_stats
            .get(&symbol.name)
            .map_or(0, |stats| proximity_score(stats, cursor_byte));
        keep_best(
            &mut by_name,
            CurrentFileOverlayCandidate {
                name: symbol.name.clone(),
                kind: symbol.kind,
                detail: Some("current".to_string()),
                match_score,
                proximity_score,
                source_start_byte: symbol.start_byte,
                semantic: true,
            },
            cursor_byte,
        );
    }

    for alias in &index.aliases {
        add_alias_candidate(&mut by_name, alias, prefix, cursor_byte, &usage_stats);
    }

    add_cpp_using_alias_candidates(&mut by_name, text, cursor_byte, prefix, &usage_stats);

    for record in &index.records {
        let Some(match_score) = completion_word_score(prefix, &record.display_name, 0) else {
            continue;
        };
        let proximity_score = usage_stats
            .get(&record.display_name)
            .map_or(0, |stats| proximity_score(stats, cursor_byte));
        keep_best(
            &mut by_name,
            CurrentFileOverlayCandidate {
                name: record.display_name.clone(),
                kind: SymbolKind::Type,
                detail: Some("current".to_string()),
                match_score,
                proximity_score,
                source_start_byte: record.start_byte,
                semantic: true,
            },
            cursor_byte,
        );
    }

    let fallback_stats = if should_use_raw_scan(index) {
        raw_identifier_usage_stats(text, cursor_byte, prefix)
    } else {
        usage_stats
    };
    for (name, stats) in fallback_stats {
        let Some(match_score) = completion_word_score(prefix, &name, 0) else {
            continue;
        };
        keep_best(
            &mut by_name,
            CurrentFileOverlayCandidate {
                name,
                kind: SymbolKind::GlobalVariable,
                detail: Some("text".to_string()),
                match_score,
                proximity_score: proximity_score(&stats, cursor_byte),
                source_start_byte: stats.nearest_start_byte,
                semantic: false,
            },
            cursor_byte,
        );
    }

    let mut hits: Vec<_> = by_name.into_values().collect();
    hits.sort_by(|a, b| compare_candidates(a, b, cursor_byte));
    hits.truncate(limit);
    hits
}

fn is_overlay_symbol(kind: SymbolKind, role: SymbolRole) -> bool {
    match kind {
        SymbolKind::Function => matches!(role, SymbolRole::Definition | SymbolRole::Declaration),
        SymbolKind::Macro
        | SymbolKind::Type
        | SymbolKind::EnumConstant
        | SymbolKind::GlobalVariable => role == SymbolRole::Definition,
        SymbolKind::Field => false,
    }
}

fn add_alias_candidate(
    by_name: &mut HashMap<String, CurrentFileOverlayCandidate>,
    alias: &TypeAlias,
    prefix: &str,
    cursor_byte: usize,
    usage_stats: &HashMap<String, UsageStats>,
) {
    let Some(match_score) = completion_word_score(prefix, &alias.alias, 0) else {
        return;
    };
    let proximity_score = usage_stats
        .get(&alias.alias)
        .map_or(0, |stats| proximity_score(stats, cursor_byte));
    keep_best(
        by_name,
        CurrentFileOverlayCandidate {
            name: alias.alias.clone(),
            kind: SymbolKind::Type,
            detail: Some("current".to_string()),
            match_score,
            proximity_score,
            source_start_byte: alias.start_byte,
            semantic: true,
        },
        cursor_byte,
    );
}

fn add_cpp_using_alias_candidates(
    by_name: &mut HashMap<String, CurrentFileOverlayCandidate>,
    text: &str,
    cursor_byte: usize,
    prefix: &str,
    usage_stats: &HashMap<String, UsageStats>,
) {
    for (alias, start_byte) in cpp_using_aliases_before_cursor(text, cursor_byte) {
        let Some(match_score) = completion_word_score(prefix, &alias, 0) else {
            continue;
        };
        let proximity_score = usage_stats
            .get(&alias)
            .map_or(0, |stats| proximity_score(stats, cursor_byte));
        keep_best(
            by_name,
            CurrentFileOverlayCandidate {
                name: alias,
                kind: SymbolKind::Type,
                detail: Some("current".to_string()),
                match_score,
                proximity_score,
                source_start_byte: start_byte,
                semantic: true,
            },
            cursor_byte,
        );
    }
}

fn cpp_using_aliases_before_cursor(text: &str, cursor_byte: usize) -> Vec<(String, usize)> {
    let end = cursor_byte.min(text.len());
    let bytes = text.as_bytes();
    let mut aliases = Vec::new();
    let mut index = 0usize;

    while index < end {
        if !keyword_at(bytes, index, end, b"using") {
            index += 1;
            continue;
        }

        let mut cursor = skip_ascii_whitespace(bytes, index + b"using".len(), end);
        if cursor >= end || !is_ident_start(bytes[cursor]) {
            index += b"using".len();
            continue;
        }

        let alias_start = cursor;
        cursor += 1;
        while cursor < end && is_ident_continue(bytes[cursor]) {
            cursor += 1;
        }
        let alias_end = cursor;
        cursor = skip_ascii_whitespace(bytes, cursor, end);
        if cursor < end && bytes[cursor] == b'=' {
            aliases.push((text[alias_start..alias_end].to_string(), alias_start));
        }
        index = cursor.saturating_add(1);
    }

    aliases
}

fn keyword_at(bytes: &[u8], index: usize, end: usize, keyword: &[u8]) -> bool {
    let keyword_end = index.saturating_add(keyword.len());
    keyword_end <= end
        && bytes[index..keyword_end].eq(keyword)
        && (index == 0 || !is_ident_continue(bytes[index - 1]))
        && (keyword_end >= end || !is_ident_continue(bytes[keyword_end]))
}

fn skip_ascii_whitespace(bytes: &[u8], mut index: usize, end: usize) -> usize {
    while index < end && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

#[derive(Debug, Clone)]
struct UsageStats {
    count: usize,
    nearest_start_byte: usize,
}

fn occurrence_usage_stats(
    index: &FileSemanticIndex,
    cursor_byte: usize,
    prefix: &str,
) -> HashMap<String, UsageStats> {
    let mut stats = HashMap::new();
    for occurrence in &index.occurrences {
        let occurrence_end = occurrence.start_byte.saturating_add(occurrence.name.len());
        if occurrence_end > cursor_byte {
            continue;
        }
        if occurrence_end == cursor_byte && occurrence.name == prefix {
            continue;
        }
        if completion_word_score(prefix, &occurrence.name, 0).is_none() {
            continue;
        }
        record_usage(&mut stats, occurrence.name.clone(), occurrence.start_byte);
    }
    stats
}

fn raw_identifier_usage_stats(
    text: &str,
    cursor_byte: usize,
    prefix: &str,
) -> HashMap<String, UsageStats> {
    let mut stats = HashMap::new();
    for (name, start, end) in identifier_spans_before_cursor(text, cursor_byte) {
        if end == cursor_byte && name == prefix {
            continue;
        }
        if completion_word_score(prefix, &name, 0).is_none() {
            continue;
        }
        record_usage(&mut stats, name, start);
    }
    stats
}

fn record_usage(stats: &mut HashMap<String, UsageStats>, name: String, start_byte: usize) {
    stats
        .entry(name)
        .and_modify(|entry| {
            entry.count += 1;
            if start_byte > entry.nearest_start_byte {
                entry.nearest_start_byte = start_byte;
            }
        })
        .or_insert(UsageStats {
            count: 1,
            nearest_start_byte: start_byte,
        });
}

fn identifier_spans_before_cursor(text: &str, cursor_byte: usize) -> Vec<(String, usize, usize)> {
    let mut spans = Vec::new();
    let end = cursor_byte.min(text.len());
    let bytes = text.as_bytes();
    let mut index = 0usize;

    while index < end {
        let byte = bytes[index];
        if !is_ident_start(byte) {
            index += 1;
            continue;
        }

        let start = index;
        index += 1;
        while index < end && is_ident_continue(bytes[index]) {
            index += 1;
        }
        spans.push((text[start..index].to_string(), start, index));
    }

    spans
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

fn should_use_raw_scan(index: &FileSemanticIndex) -> bool {
    index.occurrences.is_empty()
        || index.diagnostics.fallback_used
        || index.diagnostics.ast_source == FactSource::LexicalFallback
}

fn proximity_score(stats: &UsageStats, cursor_byte: usize) -> i32 {
    let distance = cursor_byte.saturating_sub(stats.nearest_start_byte);
    let distance_score = match distance {
        0..=32 => 120,
        33..=128 => 90,
        129..=512 => 60,
        513..=2048 => 30,
        _ => 10,
    };
    let frequency_score = stats.count.saturating_sub(1).min(4) as i32 * 20;
    (distance_score + frequency_score).min(MAX_PROXIMITY_SCORE)
}

fn keep_best(
    by_name: &mut HashMap<String, CurrentFileOverlayCandidate>,
    candidate: CurrentFileOverlayCandidate,
    cursor_byte: usize,
) {
    match by_name.get(&candidate.name) {
        Some(existing) if candidate_is_better(&candidate, existing, cursor_byte) => {
            by_name.insert(candidate.name.clone(), candidate);
        }
        None => {
            by_name.insert(candidate.name.clone(), candidate);
        }
        _ => {}
    }
}

fn candidate_is_better(
    candidate: &CurrentFileOverlayCandidate,
    existing: &CurrentFileOverlayCandidate,
    cursor_byte: usize,
) -> bool {
    candidate
        .semantic
        .cmp(&existing.semantic)
        .then(candidate.match_score.cmp(&existing.match_score))
        .then(candidate.proximity_score.cmp(&existing.proximity_score))
        .then_with(|| {
            source_distance(existing.source_start_byte, cursor_byte)
                .cmp(&source_distance(candidate.source_start_byte, cursor_byte))
        })
        .is_gt()
}

fn compare_candidates(
    a: &CurrentFileOverlayCandidate,
    b: &CurrentFileOverlayCandidate,
    cursor_byte: usize,
) -> std::cmp::Ordering {
    b.semantic
        .cmp(&a.semantic)
        .then(b.match_score.cmp(&a.match_score))
        .then(b.proximity_score.cmp(&a.proximity_score))
        .then_with(|| {
            source_distance(a.source_start_byte, cursor_byte)
                .cmp(&source_distance(b.source_start_byte, cursor_byte))
        })
        .then_with(|| a.name.cmp(&b.name))
}

fn source_distance(source_start_byte: usize, cursor_byte: usize) -> usize {
    if source_start_byte <= cursor_byte {
        cursor_byte - source_start_byte
    } else {
        source_start_byte - cursor_byte + cursor_byte.saturating_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor_from_marker(text: &str) -> (String, u32, u32) {
        let marker = "/*cursor*/";
        let cursor_byte = text.find(marker).expect("cursor marker");
        let text = text.replacen(marker, "", 1);
        let before = &text[..cursor_byte];
        let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let character = before[line_start..]
            .chars()
            .map(|ch| ch.len_utf16() as u32)
            .sum();
        (text, line, character)
    }

    #[test]
    fn overlay_extracts_unsaved_macros_aliases_enum_constants_functions_and_records() {
        let (text, line, character) = cursor_from_marker(
            "#define FS_MAGIC 1\n\
             typedef int FsAlias;\n\
             enum Color { FS_RED };\n\
             struct FsWidget { int id; };\n\
             int fs_do_work(void) { return 0; }\n\
             void f(void) { FS/*cursor*/ }\n",
        );
        let parsed = crate::parser::parse(std::path::Path::new("a.c"), &text);

        let hits = current_file_overlay_candidates(&parsed, &text, line, character, "FS", 20);
        let names: Vec<_> = hits.iter().map(|hit| hit.name.as_str()).collect();

        assert!(names.contains(&"FS_MAGIC"));
        assert!(names.contains(&"FsAlias"));
        assert!(names.contains(&"FS_RED"));
        assert!(names.contains(&"FsWidget"));
        assert!(names.contains(&"fs_do_work"));
        assert!(hits.iter().filter(|hit| hit.semantic).count() >= 5);
    }

    #[test]
    fn overlay_includes_function_declarations() {
        let (text, line, character) = cursor_from_marker(
            "int fs_do_work(void);\n\
             void f(void) { fs/*cursor*/ }\n",
        );
        let parsed = crate::parser::parse(std::path::Path::new("a.c"), &text);

        let hits = current_file_overlay_candidates(&parsed, &text, line, character, "fs", 20);
        let hit = hits
            .iter()
            .find(|hit| hit.name == "fs_do_work")
            .expect("function declaration overlay candidate");

        assert_eq!(hit.kind, crate::parser::SymbolKind::Function);
        assert_eq!(hit.detail.as_deref(), Some("current"));
        assert!(hit.semantic);
    }

    #[test]
    fn overlay_includes_cpp_using_aliases() {
        let (text, line, character) = cursor_from_marker(
            "using FsAlias = int;\n\
             void f(void) { Fs/*cursor*/ }\n",
        );
        let parsed = crate::parser::parse(std::path::Path::new("a.cpp"), &text);

        let hits = current_file_overlay_candidates(&parsed, &text, line, character, "Fs", 20);
        let hit = hits
            .iter()
            .find(|hit| hit.name == "FsAlias")
            .expect("C++ using alias overlay candidate");

        assert_eq!(hit.kind, crate::parser::SymbolKind::Type);
        assert_eq!(hit.detail.as_deref(), Some("current"));
        assert!(hit.semantic);
    }

    #[test]
    fn nearby_usage_scores_distance_and_frequency_without_semantic_kind() {
        let (text, line, character) = cursor_from_marker(
            "void f(void) {\n\
                 localThing();\n\
                 localThing();\n\
                 loc/*cursor*/\n\
             }\n",
        );
        let parsed = crate::parser::parse(std::path::Path::new("a.c"), &text);

        let hits = current_file_overlay_candidates(&parsed, &text, line, character, "loc", 20);
        let hit = hits
            .iter()
            .find(|hit| hit.name == "localThing")
            .expect("nearby word");

        assert!(hit.proximity_score > 0);
        assert!(hit.proximity_score <= 200);
        assert_eq!(hit.detail.as_deref(), Some("text"));
        assert_eq!(hit.kind, crate::parser::SymbolKind::GlobalVariable);
        assert!(!hit.semantic);
    }

    #[test]
    fn raw_scanning_returns_prior_word_when_ast_occurrences_are_empty() {
        let (text, line, character) = cursor_from_marker(
            "void f(void) {\n\
                 fallbackWord = 1;\n\
                 fal/*cursor*/\n\
             }\n",
        );
        let mut parsed = crate::parser::parse(std::path::Path::new("a.c"), &text);
        parsed.occurrences.clear();
        parsed.symbols.clear();
        parsed.aliases.clear();
        parsed.records.clear();

        let hits = current_file_overlay_candidates(&parsed, &text, line, character, "fal", 20);

        assert!(hits.iter().any(|hit| hit.name == "fallbackWord"
            && hit.detail.as_deref() == Some("text")
            && !hit.semantic));
    }

    #[test]
    fn short_prefix_gates_are_preserved_for_plain_substring_noise() {
        let (text, line, character) = cursor_from_marker(
            "void f(void) {\n\
                 Foobar();\n\
                 FooBar();\n\
                 ba/*cursor*/\n\
             }\n",
        );
        let mut parsed = crate::parser::parse(std::path::Path::new("a.c"), &text);
        parsed.symbols.clear();
        parsed.aliases.clear();
        parsed.records.clear();

        let hits = current_file_overlay_candidates(&parsed, &text, line, character, "ba", 20);
        let names: Vec<_> = hits.iter().map(|hit| hit.name.as_str()).collect();

        assert!(names.contains(&"FooBar"));
        assert!(!names.contains(&"Foobar"));
        assert!(!names.contains(&"ba"));
    }
}
