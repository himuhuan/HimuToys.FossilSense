use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::config::normalized_extension;

mod ast;
mod callables;
mod lexical;

use ast::collect_ast_index;
pub use ast::infer_receiver_record;
#[cfg(test)]
use lexical::compact_whitespace;
use lexical::extract_symbols_and_includes;

bitflags::bitflags! {
    /// Which AST facts to collect during `parse`. The lexical pass
    /// (symbols + includes) is always run regardless of this mask.
    ///
    /// Each bit controls a distinct collection branch inside the post-parse
    /// AST DFS. Skipping a branch returns an empty vector for that field
    /// in `FileSemanticIndex`, keeping the data structure stable.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct ParseFacts: u8 {
        /// Symbols from the lexical pass are always produced; this bit
        /// controls whether AST-derived enum constants are also merged in.
        const SYMBOLS       = 1 << 0;
        /// Include lines (lexical pass, always collected).
        const INCLUDES      = 1 << 1;
        /// Identifier occurrences with syntactic roles (AST DFS).
        const OCCURRENCES   = 1 << 2;
        /// `struct`/`union`/`class` record definitions (AST DFS).
        const RECORDS       = 1 << 3;
        /// Fields of collected records (AST DFS, requires `RECORDS`).
        const FIELDS        = 1 << 4;
        /// `typedef` type aliases (AST DFS).
        const ALIASES       = 1 << 5;
        /// Record-typed local/parameter declarations (AST DFS).
        const LOCAL_DECLS   = 1 << 6;
        /// Callable anchors and call-expression facts for relation queries.
        const CALL_RELATIONS = 1 << 7;

        /// Indexing: everything except request-time facts.
        const INDEX         = Self::SYMBOLS.bits()
                            | Self::INCLUDES.bits()
                            | Self::RECORDS.bits()
                            | Self::FIELDS.bits()
                            | Self::ALIASES.bits()
                            | Self::CALL_RELATIONS.bits();

        /// Coloring / references: occurrences + symbols + includes.
        const COLOR_REF     = Self::SYMBOLS.bits()
                            | Self::INCLUDES.bits()
                            | Self::OCCURRENCES.bits();

        /// Member completion: needs local declarations, receiver inference,
        /// and record/field/alias resolution.
        const MEMBER        = Self::SYMBOLS.bits()
                            | Self::INCLUDES.bits()
                            | Self::LOCAL_DECLS.bits()
                            | Self::RECORDS.bits()
                            | Self::FIELDS.bits()
                            | Self::ALIASES.bits();

        /// Ordinary identifier completion: lexical symbols plus local and
        /// parameter bindings, without records, fields, occurrences, or calls.
        const COMPLETION    = Self::SYMBOLS.bits()
                            | Self::INCLUDES.bits()
                            | Self::LOCAL_DECLS.bits();

        /// Semantic coloring: identifier occurrences plus local bindings.
        const COLOR_LIVE    = Self::COLOR_REF.bits()
                            | Self::LOCAL_DECLS.bits();

        /// Everything (backward-compatible default).
        const ALL           = !0;
    }
}

/// The single best-effort parse product for one file. Produced by `parse` in one
/// tree-sitter parse and one AST DFS plus one lexical pass, and consumed by the
/// indexer, semantic coloring, reference role classification, and member-
/// completion receiver inference. Symbols/includes are lexical; occurrences,
/// records, fields, aliases, and local declarations are AST-derived (empty on the
/// lexical-fallback path — see `ParseDiagnostics`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSemanticIndex {
    pub symbols: Vec<Symbol>,
    pub includes: Vec<Include>,
    /// Identifier occurrences with syntactic roles (AST-derived). Empty on the
    /// lexical-fallback path. Request-time data: the indexer does not persist it.
    pub occurrences: Vec<Occurrence>,
    pub records: Vec<RecordDef>,
    pub fields: Vec<FieldDef>,
    pub members: Vec<MemberDef>,
    pub aliases: Vec<TypeAlias>,
    pub callable_anchors: Vec<crate::call_model::CallableAnchor>,
    pub call_sites: Vec<crate::call_model::CallSiteFact>,
    /// Record-typed local/parameter declarations for positional receiver
    /// inference (AST-derived). Request-time data; not persisted.
    pub local_declarations: Vec<LocalDeclaration>,
    /// Current-function parameters and local variables for request-time
    /// identifier completion (AST-derived). Request-time data; not persisted.
    pub local_bindings: Vec<LocalBinding>,
    pub diagnostics: ParseDiagnostics,
}

/// Stable index-time projection over the fields callers persist to SQLite.
///
/// This is a borrowed view so existing `FileSemanticIndex` ownership and field
/// access remain unchanged while parser consumers migrate incrementally.
pub use crate::semantic_model::PersistentFacts;

/// Request-time AST facts used by live features such as coloring, references,
/// member completion receiver inference, and local completion evidence.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct RequestFacts<'a> {
    pub occurrences: &'a [Occurrence],
    pub local_declarations: &'a [LocalDeclaration],
    pub local_bindings: &'a [LocalBinding],
}

/// Parser fact groups with explicit request/availability state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub enum FactGroup {
    Symbols,
    Includes,
    Occurrences,
    Records,
    Fields,
    Members,
    Aliases,
    LocalDeclarations,
    LocalBindings,
    CallableAnchors,
    CallSites,
}

/// Why a requested fact group is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum FactUnavailableReason {
    /// tree-sitter did not produce a usable tree, so only lexical facts exist.
    LexicalFallback,
}

/// Availability for a fact group under the requested [`ParseFacts`] mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum FactAvailability {
    /// The caller's fact mask omitted this group.
    NotRequested,
    /// The group was requested and the parser product can be trusted for it;
    /// an empty vector still means "available, with no facts found".
    Available,
    /// The group was requested, but parser degradation prevented collection.
    Unavailable(FactUnavailableReason),
}

/// Where a group of facts in a `FileSemanticIndex` came from. R5 keeps symbol
/// extraction lexical and only labels provenance; it does not move top-level
/// symbols onto the AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactSource {
    /// Produced from the tree-sitter AST.
    Ast,
    /// Produced from the line-based lexical pass.
    Lexical,
    /// No usable tree-sitter tree: AST facts are absent, only lexical facts exist.
    LexicalFallback,
}

/// Parse-health and provenance for one `FileSemanticIndex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseDiagnostics {
    /// tree-sitter error/missing node count (0 on the lexical-fallback path).
    pub parse_error_count: usize,
    /// True only when tree-sitter could not produce a usable tree, so the AST
    /// fact vectors are empty by fallback rather than genuinely empty.
    pub fallback_used: bool,
    /// Provenance of `symbols`/`includes` (always `Lexical`).
    pub symbols_source: FactSource,
    /// Provenance of the AST fact groups: `Ast` on a usable tree, otherwise
    /// `LexicalFallback`.
    pub ast_source: FactSource,
    /// The fact mask used to produce this index. Compatibility fields still
    /// carry the same values as before; this lets callers distinguish skipped
    /// groups from requested groups that are empty or unavailable.
    pub requested_facts: ParseFacts,
}

/// A record-typed declaration in a file, used by positional receiver inference.
/// `decl_start_byte` is the byte offset of the declared identifier so the query
/// can pick the nearest declaration preceding a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDeclaration {
    pub name: String,
    pub record_type: String,
    pub decl_start_byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBindingKind {
    Parameter,
    LocalVariable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBinding {
    pub name: String,
    pub kind: LocalBindingKind,
    pub type_text: Option<String>,
    pub decl_start_byte: usize,
    pub function_start_byte: usize,
    pub function_end_byte: usize,
}

/// Coloring's macro/type/enum definition name sets, projected from an already
/// parsed `FileSemanticIndex` (no extra parse). Macro/type names come from the
/// lexical symbols; enum-constant names come from the AST enum facts that were
/// merged into `symbols`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColoringDefs {
    pub macro_defs: HashSet<String>,
    pub type_defs: HashSet<String>,
    pub enum_defs: HashSet<String>,
}

impl FileSemanticIndex {
    /// External reference headers contribute declarations but never bodies or
    /// body-derived call sites. They are navigation leaves, not analyzed code.
    pub fn retain_external_call_declarations(&mut self) {
        for anchor in &mut self.callable_anchors {
            if anchor.role == crate::call_model::AnchorRole::Definition {
                anchor.role = crate::call_model::AnchorRole::Declaration;
                anchor.body_range = None;
            }
        }
        self.call_sites.clear();
    }

    /// Borrow the persistent/index-time facts without changing the legacy field
    /// layout. Group 5 parser consumers can migrate to this projection while
    /// older call sites keep reading the public fields.
    #[allow(dead_code)]
    pub fn persistent_facts(&self) -> PersistentFacts<'_> {
        PersistentFacts {
            symbols: &self.symbols,
            includes: &self.includes,
            records: &self.records,
            fields: &self.fields,
            members: &self.members,
            aliases: &self.aliases,
            callable_anchors: &self.callable_anchors,
            call_sites: &self.call_sites,
        }
    }

    /// Borrow request-time facts without implying that every group was
    /// requested. Use [`FileSemanticIndex::fact_availability`] to distinguish
    /// skipped, available-empty, and fallback-unavailable groups.
    #[allow(dead_code)]
    pub fn request_facts(&self) -> RequestFacts<'_> {
        RequestFacts {
            occurrences: &self.occurrences,
            local_declarations: &self.local_declarations,
            local_bindings: &self.local_bindings,
        }
    }

    /// Return the availability of one fact group under the parse mask that
    /// produced this index.
    #[allow(dead_code)]
    pub fn fact_availability(&self, group: FactGroup) -> FactAvailability {
        self.diagnostics.fact_availability(group)
    }

    /// Project the coloring definition-name sets from this index's symbols. This
    /// reuses the already-extracted symbols (no re-parse): definition-role macros
    /// and types from the lexical pass, plus enum constants from the AST pass.
    pub fn coloring_defs(&self) -> ColoringDefs {
        let mut defs = ColoringDefs::default();
        for symbol in &self.symbols {
            match symbol.kind {
                SymbolKind::Macro if symbol.role == SymbolRole::Definition => {
                    defs.macro_defs.insert(symbol.name.clone());
                }
                SymbolKind::Type if symbol.role == SymbolRole::Definition => {
                    defs.type_defs.insert(symbol.name.clone());
                }
                SymbolKind::EnumConstant => {
                    defs.enum_defs.insert(symbol.name.clone());
                }
                _ => {}
            }
        }
        defs
    }
}

impl ParseDiagnostics {
    /// Availability for one group based on the requested mask and parser
    /// provenance. This is intentionally metadata-only: it does not change the
    /// existing vectors or tolerant parse behavior.
    #[allow(dead_code)]
    pub fn fact_availability(&self, group: FactGroup) -> FactAvailability {
        if matches!(group, FactGroup::Symbols | FactGroup::Includes) {
            return FactAvailability::Available;
        }

        if !self.group_requested(group) {
            return FactAvailability::NotRequested;
        }

        if self.ast_source == FactSource::LexicalFallback {
            FactAvailability::Unavailable(FactUnavailableReason::LexicalFallback)
        } else {
            FactAvailability::Available
        }
    }

    /// True when the group was requested by the parse mask or collected as a
    /// required dependency of a requested group.
    #[allow(dead_code)]
    pub fn group_requested(&self, group: FactGroup) -> bool {
        match group {
            FactGroup::Symbols | FactGroup::Includes => true,
            FactGroup::Occurrences => self.requested_facts.contains(ParseFacts::OCCURRENCES),
            FactGroup::Records => self
                .requested_facts
                .intersects(ParseFacts::RECORDS | ParseFacts::FIELDS),
            FactGroup::Fields | FactGroup::Members => {
                self.requested_facts.contains(ParseFacts::FIELDS)
            }
            FactGroup::Aliases => self.requested_facts.contains(ParseFacts::ALIASES),
            FactGroup::LocalDeclarations | FactGroup::LocalBindings => {
                self.requested_facts.contains(ParseFacts::LOCAL_DECLS)
            }
            FactGroup::CallableAnchors | FactGroup::CallSites => {
                self.requested_facts.contains(ParseFacts::CALL_RELATIONS)
            }
        }
    }
}

pub use crate::semantic_model::{
    kind_from_str, AliasTarget, FieldDef, Include, MemberConfidence, MemberDef, MemberKind,
    Occurrence, RecordConfidence, RecordDef, RecordKind, Symbol, SymbolKind, SymbolRole,
    SyntacticRole, TypeAlias,
};

/// A typedef alias mapping a new name to an underlying record tag, e.g.
/// `typedef struct Foo FooT;` records `FooT -> Foo`. Lets member completion
/// resolve a receiver typed with the alias back to the tag that owns the fields.
/// Parse `source` into the single `FileSemanticIndex` product: lexical symbols
/// and includes, plus the AST-derived occurrences, records, fields, aliases, enum
/// constants, and record-typed local declarations — one tree-sitter parse and one
/// AST DFS. The indexer, coloring, reference classification, and member-completion
/// receiver inference all consume this one product.
///
/// Reusable tree-sitter `Parser` wrapper for the index-worker file-parse loop.
///
/// Each index worker creates one `ParserHandle` and reuses it across all
/// files it parses, avoiding repeated `Parser::new()` + `set_language()` calls.
///
/// Uses one mutex around both parser state and current language so setting the
/// grammar and parsing a file are atomic relative to other users of the same
/// handle. The indexer uses one handle per Rayon worker, so this lock is not
/// contended on the hot path.
pub struct ParserHandle {
    state: Mutex<ParserState>,
}

struct ParserState {
    parser: tree_sitter::Parser,
    current_lang: Option<tree_sitter::Language>,
}

impl ParserHandle {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ParserState {
                parser: tree_sitter::Parser::new(),
                current_lang: None,
            }),
        }
    }

    /// Parse `source` with `lang`, optionally reusing `old_tree` for
    /// incremental parsing. Only calls `set_language` when switching between C
    /// and C++ (rare in practice).
    ///
    /// Returns `Err` only if `set_language` fails (e.g. unsupported language).
    pub fn parse_with_language(
        &self,
        lang: tree_sitter::Language,
        source: &str,
        old_tree: Option<&tree_sitter::Tree>,
    ) -> Result<Option<tree_sitter::Tree>, ()> {
        let mut state = self.state.lock().unwrap();
        let needs_set = state.current_lang.as_ref().is_none_or(|c| *c != lang);
        if needs_set {
            state.parser.set_language(&lang).map_err(|_| ())?;
            state.current_lang = Some(lang);
        }
        Ok(state.parser.parse(source, old_tree))
    }

    fn parse_with_language_cancel(
        &self,
        lang: tree_sitter::Language,
        source: &str,
        cancel: &AtomicBool,
    ) -> Result<Option<tree_sitter::Tree>, ()> {
        let mut state = self.state.lock().unwrap();
        if state
            .current_lang
            .as_ref()
            .is_none_or(|current| *current != lang)
        {
            state.parser.set_language(&lang).map_err(|_| ())?;
            state.current_lang = Some(lang);
        }
        let bytes = source.as_bytes();
        let mut input = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
        let mut progress = |_: &tree_sitter::ParseState| cancel.load(Ordering::Relaxed);
        let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
        Ok(state
            .parser
            .parse_with_options(&mut input, None, Some(options)))
    }
}

/// Single best-effort parse of `source` into a `FileSemanticIndex`.
///
/// Convenience wrapper that creates a temporary [`ParserHandle`]. For bulk
/// parsing (e.g. the indexer's file-parse loop), use [`parse_with_handle`]
/// to reuse a handle across files.
pub fn parse(path: &Path, source: &str) -> FileSemanticIndex {
    parse_with_handle(path, source, None, ParseFacts::ALL)
}

/// Parse `source` with an optional shared [`ParserHandle`].
///
/// When `handle` is `Some`, the caller-owned parser is reused across calls
/// (avoids repeated `Parser::new()` + `set_language()`). When `None`, a
/// temporary handle is created per call (same behaviour as [`parse`]).
///
/// `facts` controls which AST facts are collected during the DFS pass.
/// Skipped facts produce empty vectors in the returned `FileSemanticIndex`.
pub fn parse_with_handle(
    path: &Path,
    source: &str,
    handle: Option<&ParserHandle>,
    facts: ParseFacts,
) -> FileSemanticIndex {
    parse_with_handle_control(path, source, handle, facts, None)
}

fn parse_with_handle_control(
    path: &Path,
    source: &str,
    handle: Option<&ParserHandle>,
    facts: ParseFacts,
    cancel: Option<&AtomicBool>,
) -> FileSemanticIndex {
    let line_starts = line_starts(source);
    let (symbols, includes) = extract_symbols_and_includes(source, &line_starts);

    let language = language_for_path(path);

    // Use the provided handle, or create a temporary one.
    let owned_handle;
    let active_handle: &ParserHandle = match handle {
        Some(h) => h,
        None => {
            owned_handle = ParserHandle::new();
            &owned_handle
        }
    };

    let parsed_tree = match cancel {
        Some(cancel) => active_handle.parse_with_language_cancel(language, source, cancel),
        None => active_handle.parse_with_language(language, source, None),
    };
    let tree = match parsed_tree {
        Ok(Some(tree)) => tree,
        Ok(None) | Err(()) => return lexical_fallback_with_facts(symbols, includes, facts),
    };

    let ast = collect_ast_index(tree.root_node(), path, source, &line_starts, facts);

    // A usable syntax tree is authoritative for type definitions. The lexical
    // pass remains the fallback source when tree-sitter cannot produce a tree,
    // but its broad tag regex must not compete with exact AST name nodes.
    let mut symbols: Vec<_> = symbols
        .into_iter()
        .filter(|symbol| symbol.kind != SymbolKind::Type)
        .collect();
    symbols.reserve(ast.type_symbols.len() + ast.enum_constants.len());
    symbols.extend(ast.type_symbols);
    symbols.extend(ast.enum_constants);

    FileSemanticIndex {
        symbols,
        includes,
        occurrences: ast.occurrences,
        records: ast.records,
        fields: ast.fields,
        members: ast.members,
        aliases: ast.aliases,
        callable_anchors: ast.callable_anchors,
        call_sites: ast.call_sites,
        local_declarations: ast.local_declarations,
        local_bindings: ast.local_bindings,
        diagnostics: ParseDiagnostics {
            parse_error_count: ast.parse_error_count,
            fallback_used: false,
            symbols_source: FactSource::Lexical,
            ast_source: FactSource::Ast,
            requested_facts: facts,
        },
    }
}

thread_local! {
    /// Thread-local `ParserHandle` for Rayon-parallel index parsing.
    ///
    /// Each Rayon worker thread gets its own handle, so there is no cross-thread
    /// locking contention. The `RefCell` is safe because each thread accesses
    /// only its own handle sequentially (one file at a time).
    static TL_PARSER_HANDLE: RefCell<ParserHandle> = RefCell::new(ParserHandle::new());
}

/// Parse `source` using the thread-local [`ParserHandle`] and an explicit
/// [`ParseFacts`] mask.
///
/// Intended for the indexer's Rayon-parallel file-parse loop. Each Rayon worker
/// thread lazily creates its own `ParserHandle` on first call, then reuses it
/// for all subsequent files parsed on that thread.
pub fn parse_thread_local_with_facts(
    path: &Path,
    source: &str,
    facts: ParseFacts,
) -> FileSemanticIndex {
    TL_PARSER_HANDLE.with(|cell| {
        let handle = cell.borrow();
        parse_with_handle(path, source, Some(&*handle), facts)
    })
}

pub fn parse_thread_local_with_facts_cancel(
    path: &Path,
    source: &str,
    facts: ParseFacts,
    cancel: &AtomicBool,
) -> FileSemanticIndex {
    TL_PARSER_HANDLE.with(|cell| {
        let handle = cell.borrow();
        parse_with_handle_control(path, source, Some(&*handle), facts, Some(cancel))
    })
}

/// Product when tree-sitter cannot produce a usable tree: lexical symbols and
/// includes only, all AST fact vectors empty, `fallback_used = true` so a
/// consumer or test can tell AST facts are absent by fallback rather than
/// genuinely empty.
#[allow(dead_code)]
fn lexical_fallback(symbols: Vec<Symbol>, includes: Vec<Include>) -> FileSemanticIndex {
    lexical_fallback_with_facts(symbols, includes, ParseFacts::ALL)
}

fn lexical_fallback_with_facts(
    symbols: Vec<Symbol>,
    includes: Vec<Include>,
    facts: ParseFacts,
) -> FileSemanticIndex {
    FileSemanticIndex {
        symbols,
        includes,
        occurrences: Vec::new(),
        records: Vec::new(),
        fields: Vec::new(),
        members: Vec::new(),
        aliases: Vec::new(),
        callable_anchors: Vec::new(),
        call_sites: Vec::new(),
        local_declarations: Vec::new(),
        local_bindings: Vec::new(),
        diagnostics: ParseDiagnostics {
            parse_error_count: 0,
            fallback_used: true,
            symbols_source: FactSource::Lexical,
            ast_source: FactSource::LexicalFallback,
            requested_facts: facts,
        },
    }
}

fn language_for_path(path: &Path) -> tree_sitter::Language {
    if normalized_extension(path).is_some_and(|ext| {
        ["cpp", "hpp", "cc", "hh", "cxx", "hxx"]
            .iter()
            .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    }) {
        tree_sitter_cpp::LANGUAGE.into()
    } else {
        tree_sitter_c::LANGUAGE.into()
    }
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

#[cfg(test)]
mod tests;
