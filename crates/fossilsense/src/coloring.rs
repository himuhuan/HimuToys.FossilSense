//! Protocol-agnostic semantic-coloring logic: resolve each identifier to a
//! colorable kind (macro / type) and encode the kept tokens in LSP relative
//! form. Kept free of `tower-lsp` request types so it can be unit-tested.

use std::collections::{HashMap, HashSet};

use crate::model::Occurrence;
use crate::parser::{LocalBinding, LocalBindingKind, SyntacticRole};

/// Legend index of the `macro` token type (declared first in the legend).
pub const TOKEN_TYPE_MACRO: u32 = 0;
/// Legend index of the `type` token type (declared second in the legend).
pub const TOKEN_TYPE_TYPE: u32 = 1;
/// Legend index of the `enumMember` token type (declared third in the legend).
pub const TOKEN_TYPE_ENUM_MEMBER: u32 = 2;
/// Legend index of the `parameter` token type (declared fourth in the legend).
pub const TOKEN_TYPE_PARAMETER: u32 = 3;
/// Legend index of the `variable` token type (declared fifth in the legend).
pub const TOKEN_TYPE_VARIABLE: u32 = 4;

/// The only kinds FossilSense colors. Everything else is left to TextMate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorKind {
    Macro,
    Type,
    EnumMember,
    Parameter,
    Variable,
}

impl ColorKind {
    pub fn token_type(self) -> u32 {
        match self {
            ColorKind::Macro => TOKEN_TYPE_MACRO,
            ColorKind::Type => TOKEN_TYPE_TYPE,
            ColorKind::EnumMember => TOKEN_TYPE_ENUM_MEMBER,
            ColorKind::Parameter => TOKEN_TYPE_PARAMETER,
            ColorKind::Variable => TOKEN_TYPE_VARIABLE,
        }
    }
}

/// An identifier occurrence resolved to a colorable kind, in absolute position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColoredToken {
    pub line: u32,
    pub start: u32,
    pub length: u32,
    pub token_type: u32,
}

/// One token in LSP relative encoding form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelativeToken {
    pub delta_line: u32,
    pub delta_start: u32,
    pub length: u32,
    pub token_type: u32,
    pub token_modifiers: u32,
}

/// Resolve a single name to a colorable kind. Current-file definitions take
/// priority over the index; on the index path the colorable kind with the most
/// definitions wins, and a tie yields `None` (we stay honest rather than guess).
pub fn resolve_kind(
    name: &str,
    macro_defs: &HashSet<String>,
    type_defs: &HashSet<String>,
    enum_defs: &HashSet<String>,
    index_counts: &HashMap<String, HashMap<String, usize>>,
) -> Option<ColorKind> {
    if macro_defs.contains(name) {
        return Some(ColorKind::Macro);
    }
    if type_defs.contains(name) {
        return Some(ColorKind::Type);
    }
    if enum_defs.contains(name) {
        return Some(ColorKind::EnumMember);
    }

    let counts = index_counts.get(name)?;
    let candidates = [
        (ColorKind::Macro, counts.get("macro").copied().unwrap_or(0)),
        (ColorKind::Type, counts.get("type").copied().unwrap_or(0)),
        (
            ColorKind::EnumMember,
            counts.get("enum_constant").copied().unwrap_or(0),
        ),
    ];
    let max = candidates
        .iter()
        .map(|(_, count)| *count)
        .max()
        .unwrap_or(0);
    if max == 0 {
        // Only non-colorable kinds (function, global variable, field).
        return None;
    }
    let leaders: Vec<ColorKind> = candidates
        .iter()
        .filter(|(_, count)| *count == max)
        .map(|(kind, _)| *kind)
        .collect();
    // A unique dominant colorable kind wins; a tie stays honest and emits nothing.
    match leaders.as_slice() {
        [kind] => Some(*kind),
        _ => None,
    }
}

/// Whether an occurrence's syntactic role is consistent with the colorable kind
/// resolved by name. This is suppress-only: it never changes which kind a token
/// gets, it only declines to color positions that contradict the kind.
///
/// - A binding `Declaration` (a variable / parameter that merely shares a name
///   with a known type/macro/enum) is never the colored entity → suppress.
/// - A `Type` colors only in genuine type position (`TypeUse`), at its defining
///   site (`Definition`), or in the neutral/unknown `Read` case (name-based
///   fallback, so existing behavior is preserved). A type name used as a value
///   (`Call`/`Write`) is a shadowing variable → suppress.
/// - Macros and enum constants are values: every non-`Declaration` role colors.
fn role_allows_color(kind: ColorKind, role: SyntacticRole) -> bool {
    if role == SyntacticRole::Declaration {
        return false;
    }
    match kind {
        ColorKind::Type => matches!(
            role,
            SyntacticRole::TypeUse | SyntacticRole::Definition | SyntacticRole::Read
        ),
        ColorKind::Macro | ColorKind::EnumMember => true,
        ColorKind::Parameter | ColorKind::Variable => !matches!(role, SyntacticRole::TypeUse),
    }
}

/// Resolve every occurrence, drop the ones we cannot color or whose role
/// contradicts the kind, and return the kept tokens sorted by position. Kind
/// resolution is cached per name; the role gate is applied per occurrence.
#[cfg(test)]
pub fn classify_occurrences(
    occurrences: &[Occurrence],
    macro_defs: &HashSet<String>,
    type_defs: &HashSet<String>,
    enum_defs: &HashSet<String>,
    index_counts: &HashMap<String, HashMap<String, usize>>,
) -> Vec<ColoredToken> {
    classify_occurrences_with_locals(
        occurrences,
        macro_defs,
        type_defs,
        enum_defs,
        &[],
        index_counts,
    )
}

/// Like [`classify_occurrences`], but first colors best-effort current-function
/// parameter/local bindings from the open-document parse. Local bindings are
/// request-time facts, not compiler-grade scope resolution.
pub fn classify_occurrences_with_locals(
    occurrences: &[Occurrence],
    macro_defs: &HashSet<String>,
    type_defs: &HashSet<String>,
    enum_defs: &HashSet<String>,
    local_bindings: &[LocalBinding],
    index_counts: &HashMap<String, HashMap<String, usize>>,
) -> Vec<ColoredToken> {
    let mut cache: HashMap<&str, Option<ColorKind>> = HashMap::new();
    let mut tokens = Vec::new();

    for occ in occurrences {
        if let Some(kind) = local_kind_for_occurrence(occ, local_bindings) {
            tokens.push(ColoredToken {
                line: occ.line,
                start: occ.start_col,
                length: occ.length,
                token_type: kind.token_type(),
            });
            continue;
        }

        let kind = *cache.entry(occ.name.as_str()).or_insert_with(|| {
            resolve_kind(&occ.name, macro_defs, type_defs, enum_defs, index_counts)
        });
        if let Some(kind) = kind {
            if !role_allows_color(kind, occ.role) {
                continue;
            }
            tokens.push(ColoredToken {
                line: occ.line,
                start: occ.start_col,
                length: occ.length,
                token_type: kind.token_type(),
            });
        }
    }

    tokens.sort_by(|a, b| a.line.cmp(&b.line).then(a.start.cmp(&b.start)));
    tokens
}

fn local_kind_for_occurrence(
    occ: &Occurrence,
    local_bindings: &[LocalBinding],
) -> Option<ColorKind> {
    if occ.role == SyntacticRole::TypeUse {
        return None;
    }

    local_bindings
        .iter()
        .filter(|binding| {
            binding.name == occ.name && local_binding_visible_at_occurrence(binding, occ.start_byte)
        })
        .max_by_key(|binding| binding.decl_start_byte)
        .map(|binding| match binding.kind {
            LocalBindingKind::Parameter => ColorKind::Parameter,
            LocalBindingKind::LocalVariable => ColorKind::Variable,
        })
}

fn local_binding_visible_at_occurrence(binding: &LocalBinding, byte_offset: usize) -> bool {
    byte_offset == binding.decl_start_byte
        || (binding.function_start_byte <= byte_offset
            && byte_offset <= binding.function_end_byte
            && binding.decl_start_byte < byte_offset)
}

/// Keep only tokens whose start line falls within `[start_line, end_line]`.
pub fn filter_by_line_range(
    tokens: Vec<ColoredToken>,
    start_line: u32,
    end_line: u32,
) -> Vec<ColoredToken> {
    tokens
        .into_iter()
        .filter(|token| token.line >= start_line && token.line <= end_line)
        .collect()
}

/// Encode position-sorted tokens into LSP relative form. `deltaLine` is the line
/// gap from the previous token; `deltaStart` is the column gap on the same line,
/// resetting to the absolute column when the line advances.
pub fn encode_relative(tokens: &[ColoredToken]) -> Vec<RelativeToken> {
    let mut encoded = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    for token in tokens {
        let delta_line = token.line - prev_line;
        let delta_start = if delta_line == 0 {
            token.start - prev_start
        } else {
            token.start
        };
        encoded.push(RelativeToken {
            delta_line,
            delta_start,
            length: token.length,
            token_type: token.token_type,
            token_modifiers: 0,
        });
        prev_line = token.line;
        prev_start = token.start;
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occ(name: &str, line: u32, start: u32) -> Occurrence {
        occ_role(name, line, start, SyntacticRole::Read)
    }

    fn occ_role(name: &str, line: u32, start: u32, role: SyntacticRole) -> Occurrence {
        Occurrence {
            name: name.to_string(),
            start_byte: ((line as usize) << 16) + start as usize,
            line,
            start_col: start,
            length: name.chars().count() as u32,
            role,
        }
    }

    fn names(values: &[&str]) -> HashSet<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn counts(pairs: &[(&str, &[(&str, usize)])]) -> HashMap<String, HashMap<String, usize>> {
        pairs
            .iter()
            .map(|(name, kinds)| {
                let inner = kinds
                    .iter()
                    .map(|(k, c)| (k.to_string(), *c))
                    .collect::<HashMap<_, _>>();
                (name.to_string(), inner)
            })
            .collect()
    }

    #[test]
    fn local_macro_definition_wins_over_index() {
        // Index says FOO is a type, but the current file #defines it as a macro.
        let kind = resolve_kind(
            "FOO",
            &names(&["FOO"]),
            &names(&[]),
            &names(&[]),
            &counts(&[("FOO", &[("type", 3)])]),
        );
        assert_eq!(kind, Some(ColorKind::Macro));
    }

    #[test]
    fn local_type_definition_colors_as_type() {
        let kind = resolve_kind(
            "widget_t",
            &names(&[]),
            &names(&["widget_t"]),
            &names(&[]),
            &HashMap::new(),
        );
        assert_eq!(kind, Some(ColorKind::Type));
    }

    #[test]
    fn dominant_index_kind_wins() {
        let kind = resolve_kind(
            "AMBI",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &counts(&[("AMBI", &[("macro", 3), ("type", 1)])]),
        );
        assert_eq!(kind, Some(ColorKind::Macro));
    }

    #[test]
    fn tied_index_kinds_emit_no_token() {
        let kind = resolve_kind(
            "TIE",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &counts(&[("TIE", &[("macro", 2), ("type", 2)])]),
        );
        assert_eq!(kind, None);
    }

    #[test]
    fn macro_vs_function_colors_as_macro() {
        // Function is not a colorable kind, so a macro+function name is a macro.
        let kind = resolve_kind(
            "WRAP",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &counts(&[("WRAP", &[("macro", 1), ("function", 1)])]),
        );
        assert_eq!(kind, Some(ColorKind::Macro));
    }

    #[test]
    fn non_colorable_index_kind_emits_no_token() {
        let kind = resolve_kind(
            "helper",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &counts(&[("helper", &[("function", 2)])]),
        );
        assert_eq!(kind, None);
    }

    #[test]
    fn unknown_name_emits_no_token() {
        let kind = resolve_kind(
            "nobody",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &HashMap::new(),
        );
        assert_eq!(kind, None);
    }

    #[test]
    fn local_enum_constant_colors_as_enum_member() {
        let kind = resolve_kind(
            "RED",
            &names(&[]),
            &names(&[]),
            &names(&["RED"]),
            &HashMap::new(),
        );
        assert_eq!(kind, Some(ColorKind::EnumMember));
    }

    #[test]
    fn indexed_enum_constant_colors_as_enum_member() {
        let kind = resolve_kind(
            "GREEN",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &counts(&[("GREEN", &[("enum_constant", 1)])]),
        );
        assert_eq!(kind, Some(ColorKind::EnumMember));
    }

    #[test]
    fn local_enum_definition_overrides_index() {
        // Index records it as a macro, but the current file defines it as an enum.
        let kind = resolve_kind(
            "MODE_A",
            &names(&[]),
            &names(&[]),
            &names(&["MODE_A"]),
            &counts(&[("MODE_A", &[("macro", 3)])]),
        );
        assert_eq!(kind, Some(ColorKind::EnumMember));
    }

    #[test]
    fn macro_vs_enum_tie_emits_no_token() {
        let kind = resolve_kind(
            "TIE",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &counts(&[("TIE", &[("macro", 2), ("enum_constant", 2)])]),
        );
        assert_eq!(kind, None);
    }

    #[test]
    fn dominant_enum_kind_wins() {
        let kind = resolve_kind(
            "STATUS_OK",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &counts(&[("STATUS_OK", &[("enum_constant", 3), ("macro", 1)])]),
        );
        assert_eq!(kind, Some(ColorKind::EnumMember));
    }

    #[test]
    fn field_and_global_kinds_emit_no_token() {
        let kind = resolve_kind(
            "count",
            &names(&[]),
            &names(&[]),
            &names(&[]),
            &counts(&[("count", &[("field", 5), ("global_variable", 1)])]),
        );
        assert_eq!(kind, None);
    }

    #[test]
    fn classify_keeps_only_colorable_and_sorts() {
        let occurrences = vec![
            occ("plain_var", 5, 4),
            occ("FOO", 2, 8),
            occ("widget_t", 2, 0),
        ];
        let tokens = classify_occurrences(
            &occurrences,
            &names(&["FOO"]),
            &names(&["widget_t"]),
            &names(&[]),
            &HashMap::new(),
        );
        // plain_var dropped; remaining sorted by (line, start).
        assert_eq!(tokens.len(), 2);
        assert_eq!(
            (tokens[0].line, tokens[0].start, tokens[0].token_type),
            (2, 0, TOKEN_TYPE_TYPE)
        );
        assert_eq!(
            (tokens[1].line, tokens[1].start, tokens[1].token_type),
            (2, 8, TOKEN_TYPE_MACRO)
        );
    }

    #[test]
    fn relative_encoding_same_and_cross_line() {
        let tokens = vec![
            ColoredToken {
                line: 2,
                start: 4,
                length: 3,
                token_type: TOKEN_TYPE_MACRO,
            },
            ColoredToken {
                line: 2,
                start: 10,
                length: 8,
                token_type: TOKEN_TYPE_TYPE,
            },
            ColoredToken {
                line: 5,
                start: 2,
                length: 4,
                token_type: TOKEN_TYPE_TYPE,
            },
        ];
        let encoded = encode_relative(&tokens);
        // First token: absolute (deltaLine=2 from 0, deltaStart=4).
        assert_eq!(encoded[0].delta_line, 2);
        assert_eq!(encoded[0].delta_start, 4);
        // Same line: deltaLine=0, column delta 10-4=6.
        assert_eq!(encoded[1].delta_line, 0);
        assert_eq!(encoded[1].delta_start, 6);
        // New line: deltaLine=3, deltaStart resets to absolute column 2.
        assert_eq!(encoded[2].delta_line, 3);
        assert_eq!(encoded[2].delta_start, 2);
        assert!(encoded.iter().all(|t| t.token_modifiers == 0));
    }

    #[test]
    fn pipeline_colors_indexed_macro_type_and_local_def() {
        use crate::parser::parse;
        use crate::store::{FileFingerprint, IndexStore};
        use std::path::Path;
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, dir.path()).expect("store");

        // Index a header defining a macro (PAGE_SIZE) and a type (widget_t).
        let header = "#define PAGE_SIZE 4096\ntypedef struct widget widget_t;\n";
        let header_index = parse(Path::new("defs.h"), header);
        store
            .upsert_file_index(
                &FileFingerprint {
                    path: "defs.h".to_string(),
                    extension: "h".to_string(),
                    size: header.len() as u64,
                    mtime_ns: 1,
                    hash: "h".to_string(),
                },
                &header_index,
            )
            .expect("upsert header");

        // Current file uses the indexed names plus a locally-#defined macro.
        let current = "#define LOCAL 1\nint use(widget_t *w) { return PAGE_SIZE + LOCAL; }\n";
        let targets = parse(Path::new("use.c"), current);
        let defs = targets.coloring_defs();

        let reader = IndexStore::open_readonly(&db).expect("readonly");
        let wanted: Vec<&str> = targets
            .occurrences
            .iter()
            .map(|occ| occ.name.as_str())
            .filter(|name| !defs.macro_defs.contains(*name) && !defs.type_defs.contains(*name))
            .collect();
        let counts = reader.kind_counts_by_names(&wanted).expect("counts");

        let tokens = classify_occurrences(
            &targets.occurrences,
            &defs.macro_defs,
            &defs.type_defs,
            &defs.enum_defs,
            &counts,
        );

        let kind_of = |name: &str| -> Option<u32> {
            let occ = targets.occurrences.iter().find(|occ| occ.name == name)?;
            tokens
                .iter()
                .find(|token| token.line == occ.line && token.start == occ.start_col)
                .map(|token| token.token_type)
        };

        assert_eq!(kind_of("PAGE_SIZE"), Some(TOKEN_TYPE_MACRO)); // from index
        assert_eq!(kind_of("widget_t"), Some(TOKEN_TYPE_TYPE)); // from index
        assert_eq!(kind_of("LOCAL"), Some(TOKEN_TYPE_MACRO)); // current-file definition
        assert_eq!(kind_of("use"), None); // function name is not colored
    }

    #[test]
    fn pipeline_colors_local_and_indexed_enum_constants() {
        use crate::parser::parse;
        use crate::store::{FileFingerprint, IndexStore};
        use std::path::Path;
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, dir.path()).expect("store");

        // Index a header defining an enum whose constants live cross-file.
        let header = "enum Status { STATUS_OK, STATUS_FAIL };\n";
        let header_index = parse(Path::new("status.h"), header);
        store
            .upsert_file_index(
                &FileFingerprint {
                    path: "status.h".to_string(),
                    extension: "h".to_string(),
                    size: header.len() as u64,
                    mtime_ns: 1,
                    hash: "h".to_string(),
                },
                &header_index,
            )
            .expect("upsert header");

        // Current file: a locally-defined enum constant plus a cross-file one.
        let current = "enum Local { LOCAL_A };\nint use(void) { return LOCAL_A + STATUS_OK; }\n";
        let targets = parse(Path::new("use.c"), current);
        let defs = targets.coloring_defs();

        let reader = IndexStore::open_readonly(&db).expect("readonly");
        let wanted: Vec<&str> = targets
            .occurrences
            .iter()
            .map(|occ| occ.name.as_str())
            .filter(|name| {
                !defs.macro_defs.contains(*name)
                    && !defs.type_defs.contains(*name)
                    && !defs.enum_defs.contains(*name)
            })
            .collect();
        let counts = reader.kind_counts_by_names(&wanted).expect("counts");

        let tokens = classify_occurrences(
            &targets.occurrences,
            &defs.macro_defs,
            &defs.type_defs,
            &defs.enum_defs,
            &counts,
        );

        let kind_of = |name: &str| -> Option<u32> {
            let occ = targets.occurrences.iter().find(|occ| occ.name == name)?;
            tokens
                .iter()
                .find(|token| token.line == occ.line && token.start == occ.start_col)
                .map(|token| token.token_type)
        };

        assert_eq!(kind_of("LOCAL_A"), Some(TOKEN_TYPE_ENUM_MEMBER)); // current-file enum
        assert_eq!(kind_of("STATUS_OK"), Some(TOKEN_TYPE_ENUM_MEMBER)); // cross-file enum
        assert_eq!(kind_of("use"), None); // function name is not colored
    }

    #[test]
    fn pipeline_colors_local_defs_without_index() {
        use crate::parser::parse;
        use std::path::Path;

        // No index/NameTable available: current-file definitions must still color.
        let current = "#define LOCAL 1\ntypedef int my_t;\nmy_t v = LOCAL;\n";
        let targets = parse(Path::new("use.c"), current);
        let defs = targets.coloring_defs();
        let tokens = classify_occurrences(
            &targets.occurrences,
            &defs.macro_defs,
            &defs.type_defs,
            &defs.enum_defs,
            &HashMap::new(),
        );
        assert!(tokens.iter().any(|t| t.token_type == TOKEN_TYPE_MACRO));
        assert!(tokens.iter().any(|t| t.token_type == TOKEN_TYPE_TYPE));
    }

    #[test]
    fn pipeline_colors_current_function_parameters_and_locals() {
        use crate::parser::parse;
        use std::path::Path;

        let current =
            "int f(int count) {\n    int cursor_limit = count;\n    return cursor_limit;\n}\n";
        let targets = parse(Path::new("use.c"), current);
        let defs = targets.coloring_defs();
        let tokens = classify_occurrences_with_locals(
            &targets.occurrences,
            &defs.macro_defs,
            &defs.type_defs,
            &defs.enum_defs,
            &targets.local_bindings,
            &HashMap::new(),
        );

        let token_type_at = |name: &str, nth: usize| -> Option<u32> {
            let mut occurrences: Vec<_> = targets
                .occurrences
                .iter()
                .filter(|occ| occ.name == name)
                .collect();
            occurrences.sort_by_key(|occ| occ.start_byte);
            let occ = *occurrences.get(nth)?;
            tokens
                .iter()
                .find(|token| token.line == occ.line && token.start == occ.start_col)
                .map(|token| token.token_type)
        };

        assert_eq!(token_type_at("count", 0), Some(TOKEN_TYPE_PARAMETER));
        assert_eq!(token_type_at("count", 1), Some(TOKEN_TYPE_PARAMETER));
        assert_eq!(token_type_at("cursor_limit", 0), Some(TOKEN_TYPE_VARIABLE));
        assert_eq!(token_type_at("cursor_limit", 1), Some(TOKEN_TYPE_VARIABLE));
    }

    #[test]
    fn local_coloring_does_not_color_uses_before_declaration() {
        use crate::parser::parse;
        use std::path::Path;

        let current =
            "int f(void) {\n    future_value;\n    int future_value;\n    future_value;\n}\n";
        let targets = parse(Path::new("use.c"), current);
        let defs = targets.coloring_defs();
        let tokens = classify_occurrences_with_locals(
            &targets.occurrences,
            &defs.macro_defs,
            &defs.type_defs,
            &defs.enum_defs,
            &targets.local_bindings,
            &HashMap::new(),
        );

        let token_type_at = |nth: usize| -> Option<u32> {
            let mut occurrences: Vec<_> = targets
                .occurrences
                .iter()
                .filter(|occ| occ.name == "future_value")
                .collect();
            occurrences.sort_by_key(|occ| occ.start_byte);
            let occ = *occurrences.get(nth)?;
            tokens
                .iter()
                .find(|token| token.line == occ.line && token.start == occ.start_col)
                .map(|token| token.token_type)
        };

        assert_eq!(token_type_at(0), None);
        assert_eq!(token_type_at(1), Some(TOKEN_TYPE_VARIABLE));
        assert_eq!(token_type_at(2), Some(TOKEN_TYPE_VARIABLE));
    }

    #[test]
    fn role_gate_suppresses_type_in_declarator_but_keeps_type_use() {
        // `Color` is a known type. A genuine type-position use colors; a variable
        // declaration that merely shares the name does not.
        let occurrences = vec![
            occ_role("Color", 0, 0, SyntacticRole::TypeUse),
            occ_role("Color", 1, 4, SyntacticRole::Declaration),
        ];
        let tokens = classify_occurrences(
            &occurrences,
            &names(&[]),
            &names(&["Color"]),
            &names(&[]),
            &HashMap::new(),
        );
        assert_eq!(tokens.len(), 1, "only the type use is colored");
        assert_eq!(tokens[0].line, 0);
        assert_eq!(tokens[0].token_type, TOKEN_TYPE_TYPE);
    }

    #[test]
    fn role_gate_suppresses_enum_and_macro_in_declarator() {
        // A variable named like an enum constant / macro is not colored at its
        // declaration, but value uses are.
        let enum_occurrences = vec![
            occ_role("RED", 0, 8, SyntacticRole::Read),
            occ_role("RED", 1, 4, SyntacticRole::Declaration),
        ];
        let tokens = classify_occurrences(
            &enum_occurrences,
            &names(&[]),
            &names(&[]),
            &names(&["RED"]),
            &HashMap::new(),
        );
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].line, 0);
        assert_eq!(tokens[0].token_type, TOKEN_TYPE_ENUM_MEMBER);

        let macro_occurrences = vec![
            occ_role("FOO", 0, 8, SyntacticRole::Read),
            occ_role("FOO", 1, 4, SyntacticRole::Declaration),
        ];
        let tokens = classify_occurrences(
            &macro_occurrences,
            &names(&["FOO"]),
            &names(&[]),
            &names(&[]),
            &HashMap::new(),
        );
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].line, 0);
        assert_eq!(tokens[0].token_type, TOKEN_TYPE_MACRO);
    }

    #[test]
    fn role_gate_read_is_neutral_fallback() {
        // Read is the unknown/neutral role: a type-named Read occurrence still
        // colors, preserving the prior name-based behavior (no regression).
        let occurrences = vec![occ_role("widget_t", 0, 0, SyntacticRole::Read)];
        let tokens = classify_occurrences(
            &occurrences,
            &names(&[]),
            &names(&["widget_t"]),
            &names(&[]),
            &HashMap::new(),
        );
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].token_type, TOKEN_TYPE_TYPE);
    }

    #[test]
    fn pipeline_suppresses_variable_named_like_indexed_type() {
        use crate::parser::parse;
        use crate::store::{FileFingerprint, IndexStore};
        use std::path::Path;
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, dir.path()).expect("store");

        // Index a header that defines `Color` as a type.
        let header = "typedef int Color;\n";
        let header_index = parse(Path::new("color.h"), header);
        store
            .upsert_file_index(
                &FileFingerprint {
                    path: "color.h".to_string(),
                    extension: "h".to_string(),
                    size: header.len() as u64,
                    mtime_ns: 1,
                    hash: "h".to_string(),
                },
                &header_index,
            )
            .expect("upsert header");

        // Current file: a real type use on line 0, a variable declaration that
        // shadows the type name on line 1.
        let current = "Color c;\nint Color;\n";
        let targets = parse(Path::new("use.c"), current);
        let defs = targets.coloring_defs();

        let reader = IndexStore::open_readonly(&db).expect("readonly");
        let wanted: Vec<&str> = targets
            .occurrences
            .iter()
            .map(|occ| occ.name.as_str())
            .filter(|name| !defs.type_defs.contains(*name))
            .collect();
        let counts = reader.kind_counts_by_names(&wanted).expect("counts");

        let tokens = classify_occurrences(
            &targets.occurrences,
            &defs.macro_defs,
            &defs.type_defs,
            &defs.enum_defs,
            &counts,
        );

        let type_at_line = |line: u32| -> bool {
            tokens
                .iter()
                .any(|t| t.line == line && t.token_type == TOKEN_TYPE_TYPE)
        };
        assert!(type_at_line(0), "genuine type use colored");
        assert!(
            !type_at_line(1),
            "shadowing variable declaration not colored"
        );
    }

    #[test]
    fn parity_in_memory_counts_match_sql_coloring() {
        use crate::parser::parse;
        use crate::query::NameTable;
        use crate::store::{FileFingerprint, IndexStore};
        use std::collections::HashSet;
        use std::path::Path;
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let db = dir.path().join("index.sqlite");
        let mut store = IndexStore::open(&db, dir.path()).expect("store");

        let mut index_file = |path: &str, src: &str| {
            let idx = parse(Path::new(path), src);
            store
                .upsert_file_index(
                    &FileFingerprint {
                        path: path.to_string(),
                        extension: path.rsplit('.').next().unwrap().to_string(),
                        size: src.len() as u64,
                        mtime_ns: 1,
                        hash: "h".to_string(),
                    },
                    &idx,
                )
                .expect("upsert");
        };

        index_file(
            "defs.h",
            "#define PAGE_SIZE 4096\ntypedef struct widget widget_t;\nenum Color { RED, GREEN };\n",
        );
        // `AMBI` is a macro in one header and a type tag in another → a tie that
        // resolves to no color unless the scope drops one side.
        index_file("a.h", "#define AMBI 1\n");
        index_file("b.h", "struct AMBI { int x; };\n");

        let current = "widget_t w;\nint use(void) { return PAGE_SIZE + RED + AMBI; }\n";
        let targets = parse(Path::new("use.c"), current);
        let defs = targets.coloring_defs();

        let mut wanted: Vec<&str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for occ in &targets.occurrences {
            if defs.macro_defs.contains(&occ.name)
                || defs.type_defs.contains(&occ.name)
                || defs.enum_defs.contains(&occ.name)
            {
                continue;
            }
            if seen.insert(occ.name.as_str()) {
                wanted.push(occ.name.as_str());
            }
        }
        let wanted_set: HashSet<&str> = wanted.iter().copied().collect();

        let reader = IndexStore::open_readonly(&db).expect("readonly");
        let table =
            NameTable::build_with_paths(reader.load_symbol_names_with_paths().expect("names"));

        let classify = |counts: &HashMap<String, HashMap<String, usize>>| {
            classify_occurrences(
                &targets.occurrences,
                &defs.macro_defs,
                &defs.type_defs,
                &defs.enum_defs,
                counts,
            )
        };

        // Unscoped: SQL `workspace OR directly_included` vs in-memory
        // `colorable_kind_counts` with `None` (synthesizes an all-workspace
        // reachable set, preserving the prior unscoped gate via `scope_tier`).
        let sql_unscoped = reader
            .kind_counts_by_names_scoped(&wanted, None)
            .expect("sql unscoped");
        let mem_unscoped = table.colorable_kind_counts(&wanted_set, None);
        assert_eq!(
            classify(&sql_unscoped),
            classify(&mem_unscoped),
            "unscoped coloring parity"
        );

        // Scoped: a determinate set including defs.h + a.h but excluding b.h, so
        // AMBI's type side drops and it resolves to a macro under both paths.
        let scope_files: HashSet<String> = ["use.c", "defs.h", "a.h"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let sql_scoped = reader
            .kind_counts_by_names_scoped(&wanted, Some(&scope_files))
            .expect("sql scoped");
        // Build a CompletionScope carrying the reachable set + closed scope,
        // routed through the shared `scope_tier` primitive.
        let mem_scope = crate::query::CompletionScope {
            current_path: Some("use.c".to_string()),
            reach: crate::reachability::ReachScope {
                files: scope_files,
                open: false,
                reason: None,
            },
        };
        let mem_scoped = table.colorable_kind_counts(&wanted_set, Some(&mem_scope));
        assert_eq!(
            classify(&sql_scoped),
            classify(&mem_scoped),
            "scoped coloring parity"
        );

        // Sanity: the scope actually changed AMBI's outcome (tie → macro), so the
        // parity above is non-trivial.
        assert_ne!(
            classify(&sql_unscoped),
            classify(&sql_scoped),
            "scope is expected to change the colored set"
        );
    }

    #[test]
    fn range_filter_keeps_only_in_range_lines() {
        let tokens = vec![
            ColoredToken {
                line: 1,
                start: 0,
                length: 3,
                token_type: TOKEN_TYPE_MACRO,
            },
            ColoredToken {
                line: 10,
                start: 0,
                length: 3,
                token_type: TOKEN_TYPE_MACRO,
            },
        ];
        let kept = filter_by_line_range(tokens, 5, 20);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].line, 10);
    }
}
