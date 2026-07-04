use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use crate::config::normalized_extension;

mod ast;
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

        /// Indexing: everything except request-time facts.
        const INDEX         = Self::SYMBOLS.bits()
                            | Self::INCLUDES.bits()
                            | Self::RECORDS.bits()
                            | Self::FIELDS.bits()
                            | Self::ALIASES.bits();

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
    pub aliases: Vec<TypeAlias>,
    /// Record-typed local/parameter declarations for positional receiver
    /// inference (AST-derived). Request-time data; not persisted.
    pub local_declarations: Vec<LocalDeclaration>,
    /// Current-function parameters and local variables for request-time
    /// identifier completion (AST-derived). Request-time data; not persisted.
    pub local_bindings: Vec<LocalBinding>,
    pub diagnostics: ParseDiagnostics,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub role: SymbolRole,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub signature: String,
    pub guard: Option<String>,
    /// For `Field` symbols, the enclosing record key (struct/union tag, or the
    /// typedef alias of an anonymous record). `None` for every other kind.
    pub container: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Macro,
    Type,
    EnumConstant,
    GlobalVariable,
    /// A struct/union member; carries its enclosing record in `Symbol::container`.
    Field,
}

/// Reverse of the storage mapping (`store::symbol_kind`): parse a stored kind
/// string back into the `SymbolKind` enum. Used when building the in-memory
/// `NameTable` from SQLite rows so the completion hot path can map to an LSP
/// completion item kind without re-opening the database. Unknown strings fall
/// back to `GlobalVariable` so a future/new kind never panics the name table.
pub fn kind_from_str(s: &str) -> SymbolKind {
    match s {
        "function" => SymbolKind::Function,
        "macro" => SymbolKind::Macro,
        "type" => SymbolKind::Type,
        "enum_constant" => SymbolKind::EnumConstant,
        "global_variable" => SymbolKind::GlobalVariable,
        "field" => SymbolKind::Field,
        _ => SymbolKind::GlobalVariable,
    }
}

/// A typedef alias mapping a new name to an underlying record tag, e.g.
/// `typedef struct Foo FooT;` records `FooT -> Foo`. Lets member completion
/// resolve a receiver typed with the alias back to the tag that owns the fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Struct,
    Union,
    Class,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordConfidence {
    NamedTag,
    AnonymousTypedef,
    Heuristic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDef {
    pub record_key: String,
    pub display_name: String,
    pub tag_name: Option<String>,
    pub typedef_name: Option<String>,
    pub kind: RecordKind,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub confidence: RecordConfidence,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDef {
    pub record_key: String,
    pub name: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasTarget {
    RecordKey(String),
    NamedRecord { tag: String, kind: RecordKind },
    UnresolvedTypeName(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAlias {
    pub alias: String,
    pub target: AliasTarget,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolRole {
    Definition,
    Declaration,
}

/// The syntactic role of a single identifier *occurrence* (as opposed to the
/// `SymbolRole` of a definition). Derived purely from the occurrence's position
/// in the tree-sitter tree — best-effort and never semantic. Anything we cannot
/// confidently classify falls back to `Read`; we never emit a confident-but-
/// wrong role. Shared by semantic coloring (role-gated) and reference
/// classification (grouped by role).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntacticRole {
    /// Defining site of a macro, enum constant, or a function body.
    Definition,
    /// A binding declaration (variable / parameter / function prototype name).
    Declaration,
    /// The function position of a call expression.
    Call,
    /// The left-hand side of an assignment or an increment/decrement target.
    Write,
    /// A plain value-position use, or any occurrence we cannot classify.
    Read,
    /// An identifier used in type position (`type_identifier`).
    TypeUse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub line: usize,
    pub target_text: String,
}

/// A single identifier token occurrence (name + zero-based position), used by
/// semantic coloring. Columns/length are UTF-16 code units as required by LSP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub name: String,
    pub start_byte: usize,
    pub line: u32,
    pub start_col: u32,
    pub length: u32,
    /// Best-effort syntactic role of this occurrence (coloring gate / reference
    /// grouping). `Read` when unclassifiable.
    pub role: SyntacticRole,
}

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
        let needs_set = state.current_lang.as_ref().map_or(true, |c| *c != lang);
        if needs_set {
            state.parser.set_language(&lang).map_err(|_| ())?;
            state.current_lang = Some(lang);
        }
        Ok(state.parser.parse(source, old_tree))
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

    let tree = match active_handle.parse_with_language(language, source, None) {
        Ok(Some(tree)) => tree,
        Ok(None) | Err(()) => return lexical_fallback(symbols, includes),
    };

    let ast = collect_ast_index(tree.root_node(), source, &line_starts, facts);

    let mut symbols = symbols;
    symbols.reserve(ast.enum_constants.len());
    symbols.extend(ast.enum_constants);

    FileSemanticIndex {
        symbols,
        includes,
        occurrences: ast.occurrences,
        records: ast.records,
        fields: ast.fields,
        aliases: ast.aliases,
        local_declarations: ast.local_declarations,
        local_bindings: ast.local_bindings,
        diagnostics: ParseDiagnostics {
            parse_error_count: ast.parse_error_count,
            fallback_used: false,
            symbols_source: FactSource::Lexical,
            ast_source: FactSource::Ast,
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

/// Product when tree-sitter cannot produce a usable tree: lexical symbols and
/// includes only, all AST fact vectors empty, `fallback_used = true` so a
/// consumer or test can tell AST facts are absent by fallback rather than
/// genuinely empty.
fn lexical_fallback(symbols: Vec<Symbol>, includes: Vec<Include>) -> FileSemanticIndex {
    FileSemanticIndex {
        symbols,
        includes,
        occurrences: Vec::new(),
        records: Vec::new(),
        fields: Vec::new(),
        aliases: Vec::new(),
        local_declarations: Vec::new(),
        local_bindings: Vec::new(),
        diagnostics: ParseDiagnostics {
            parse_error_count: 0,
            fallback_used: true,
            symbols_source: FactSource::Lexical,
            ast_source: FactSource::LexicalFallback,
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
