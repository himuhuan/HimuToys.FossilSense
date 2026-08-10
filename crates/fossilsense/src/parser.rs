use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::config::{ParserFrontend, SourceLanguage};
use crate::semantic_model::{
    DeclarationFact, FallbackCompletionFact, ParseOutcome, SemanticDeclarationRole,
    SemanticLanguage,
};

/// Parser-private staging record used while converting AST nodes or hard-
/// failure lexical hints into their final typed fact models. It is never
/// exposed to storage or semantic consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawDeclaration {
    name: String,
    kind: SymbolKind,
    role: SymbolRole,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
    signature: String,
    tag_kind: Option<&'static str>,
    guard: Option<String>,
    container: Option<String>,
    incomplete: bool,
}

mod ast;
mod callables;
mod declarations;
mod go;
mod lexical;
mod protobuf_c;

use ast::collect_ast_index;
pub use ast::infer_receiver_record;
#[cfg(test)]
use lexical::compact_whitespace;
use lexical::{extract_fallback_completions, scan_includes};
pub(crate) use protobuf_c::{extract_protobuf_c_declarations, ProtobufCDeclaration};

struct BackendAstProduct {
    ast: ast::AstIndex,
    package: Option<crate::semantic_model::PackageFact>,
    imports: Vec<crate::semantic_model::ImportFact>,
    build_guard: Option<String>,
}

type CollectBackendFacts = for<'tree> fn(
    tree_sitter::Node<'tree>,
    &Path,
    &str,
    &[usize],
    ParseFacts,
    SourceLanguage,
) -> BackendAstProduct;

struct ParserFrontendAdapter {
    frontend: ParserFrontend,
    scan_includes: fn(&str) -> Vec<Include>,
    collect: CollectBackendFacts,
    fallback_build_guard: fn(&str) -> Option<String>,
}

const PARSER_FRONTEND_ADAPTERS: &[ParserFrontendAdapter] = &[
    ParserFrontendAdapter {
        frontend: ParserFrontend::CFamily,
        scan_includes,
        collect: collect_c_family_backend,
        fallback_build_guard: no_build_guard,
    },
    ParserFrontendAdapter {
        frontend: ParserFrontend::Go,
        scan_includes: no_includes,
        collect: collect_go_backend,
        fallback_build_guard: go::extract_build_guard,
    },
];

fn parser_frontend_adapter(frontend: ParserFrontend) -> &'static ParserFrontendAdapter {
    PARSER_FRONTEND_ADAPTERS
        .iter()
        .find(|adapter| adapter.frontend == frontend)
        .expect("every registered parser frontend must have one adapter")
}

fn collect_c_family_backend(
    root: tree_sitter::Node<'_>,
    path: &Path,
    source: &str,
    line_starts: &[usize],
    facts: ParseFacts,
    language: SourceLanguage,
) -> BackendAstProduct {
    BackendAstProduct {
        ast: collect_ast_index(root, path, source, line_starts, facts, language),
        package: None,
        imports: Vec::new(),
        build_guard: None,
    }
}

fn collect_go_backend(
    root: tree_sitter::Node<'_>,
    path: &Path,
    source: &str,
    line_starts: &[usize],
    facts: ParseFacts,
    _language: SourceLanguage,
) -> BackendAstProduct {
    let product = go::collect_go_ast_index(root, path, source, line_starts, facts);
    BackendAstProduct {
        ast: product.ast,
        package: product.package,
        imports: product.imports,
        build_guard: product.build_guard,
    }
}

fn no_includes(_source: &str) -> Vec<Include> {
    Vec::new()
}

fn no_build_guard(_source: &str) -> Option<String> {
    None
}

bitflags::bitflags! {
    /// Which facts to collect during `parse`. Include scanning always runs;
    /// lexical completion extraction runs only after a hard AST failure.
    ///
    /// Each bit controls a distinct collection branch inside the post-parse
    /// AST DFS. Skipping a branch returns an empty vector for that field
    /// in `FileSemanticIndex`, keeping the data structure stable.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct ParseFacts: u8 {
        /// Canonical AST declaration facts.
        const DECLARATIONS  = 1 << 0;
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
        /// Request-time local/parameter declarations and lexical bindings (AST DFS).
        const LOCAL_DECLS   = 1 << 6;
        /// Callable anchors and call-expression facts for relation queries.
        const CALL_RELATIONS = 1 << 7;

        /// Indexing: everything except request-time facts.
        const INDEX         = Self::DECLARATIONS.bits()
                            | Self::INCLUDES.bits()
                            | Self::RECORDS.bits()
                            | Self::FIELDS.bits()
                            | Self::ALIASES.bits()
                            | Self::CALL_RELATIONS.bits();

        /// Coloring / references: occurrences + canonical declarations + includes.
        const COLOR_REF     = Self::DECLARATIONS.bits()
                            | Self::INCLUDES.bits()
                            | Self::OCCURRENCES.bits();

        /// Member completion: needs local declarations, receiver inference,
        /// and record/field/alias resolution.
        const MEMBER        = Self::DECLARATIONS.bits()
                            | Self::INCLUDES.bits()
                            | Self::LOCAL_DECLS.bits()
                            | Self::RECORDS.bits()
                            | Self::FIELDS.bits()
                            | Self::ALIASES.bits();

        /// Ordinary identifier completion: canonical declarations plus local
        /// and parameter bindings. Canonical declaration
        /// facts let the list attach an overlay identity directly without a
        /// post-recall semantic hydration query; a later resolve parse produces
        /// the same locator for functions and methods.
        const COMPLETION    = Self::DECLARATIONS.bits()
                            | Self::INCLUDES.bits()
                            | Self::LOCAL_DECLS.bits()
                            | Self::ALIASES.bits()
                            | Self::CALL_RELATIONS.bits();

        /// Semantic coloring: identifier occurrences plus local bindings.
        const COLOR_LIVE    = Self::COLOR_REF.bits()
                            | Self::LOCAL_DECLS.bits();

        /// Hover/type semantics: the durable type and callable facts needed to
        /// build a live-document overlay. Occurrences remain a separate opt-in.
        const HOVER_SEMANTICS = Self::DECLARATIONS.bits()
                               | Self::INCLUDES.bits()
                               | Self::RECORDS.bits()
                               | Self::FIELDS.bits()
                               | Self::ALIASES.bits()
                               | Self::CALL_RELATIONS.bits();

        /// Everything (backward-compatible default).
        const ALL           = !0;
    }
}

/// The single best-effort parse product for one file. One lightweight include
/// scan always runs; every semantic fact is derived by one tree-sitter parse
/// and AST DFS. Only a hard AST failure populates `fallback_completions`, while
/// leaving every semantic vector empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSemanticIndex {
    pub language: SemanticLanguage,
    pub includes: Vec<Include>,
    pub package: Option<crate::semantic_model::PackageFact>,
    pub imports: Vec<crate::semantic_model::ImportFact>,
    pub build_guard: Option<String>,
    pub declarations: Vec<DeclarationFact>,
    pub fallback_completions: Vec<FallbackCompletionFact>,
    pub parse_outcome: ParseOutcome,
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
    /// Current-function parameters, locals, constants, and types for request-time
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
    Declarations,
    FallbackCompletions,
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

/// Where a group of facts in a `FileSemanticIndex` came from. Semantic facts
/// are AST-derived; lexical extraction is reserved for include scanning and an
/// isolated completion-only hard-failure path.
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
    /// Provenance of include and fallback-completion scanning (always lexical).
    pub lexical_source: FactSource,
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
    LocalConstant,
    LocalType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBinding {
    pub name: String,
    pub kind: LocalBindingKind,
    pub type_text: Option<String>,
    pub decl_start_byte: usize,
    pub function_start_byte: usize,
    pub function_end_byte: usize,
    /// Lexical scope in which this binding is visible. Parameters use the
    /// function body; locals use their nearest block or Go statement scope.
    pub scope_start_byte: usize,
    pub scope_end_byte: usize,
}

/// Coloring's macro/type/enum definition name sets, projected only from the
/// canonical AST declarations in an already parsed `FileSemanticIndex`.
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
        let mut demoted_fingerprints = HashSet::new();
        for anchor in &mut self.callable_anchors {
            if anchor.role == crate::call_model::AnchorRole::Definition {
                anchor.role = crate::call_model::AnchorRole::Declaration;
                anchor.body_range = None;
                demoted_fingerprints.insert(anchor.anchor_fingerprint.clone());
            }
        }
        for declaration in &mut self.declarations {
            if demoted_fingerprints.contains(&declaration.identity.locator.fingerprint) {
                declaration.role = SemanticDeclarationRole::Declaration;
                declaration.identity.role = SemanticDeclarationRole::Declaration;
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
            language: self.language,
            parse_outcome: self.parse_outcome,
            includes: &self.includes,
            package: self.package.as_ref(),
            imports: &self.imports,
            build_guard: self.build_guard.as_deref(),
            declarations: &self.declarations,
            fallback_completions: &self.fallback_completions,
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

    /// Project coloring definition names from canonical AST declarations.
    pub fn coloring_defs(&self) -> ColoringDefs {
        let mut defs = ColoringDefs::default();
        for declaration in &self.declarations {
            match declaration.declaration_kind {
                crate::semantic_model::SemanticDeclarationKind::Macro
                    if declaration.role == SemanticDeclarationRole::Definition =>
                {
                    defs.macro_defs.insert(declaration.name.clone());
                }
                crate::semantic_model::SemanticDeclarationKind::Type
                | crate::semantic_model::SemanticDeclarationKind::Alias
                    if declaration.role == SemanticDeclarationRole::Definition =>
                {
                    defs.type_defs.insert(declaration.name.clone());
                }
                crate::semantic_model::SemanticDeclarationKind::EnumConstant => {
                    defs.enum_defs.insert(declaration.name.clone());
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
        if matches!(group, FactGroup::Includes) {
            return FactAvailability::Available;
        }

        if group == FactGroup::FallbackCompletions {
            return if self.fallback_used {
                FactAvailability::Available
            } else {
                FactAvailability::NotRequested
            };
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
            FactGroup::Includes => true,
            FactGroup::Declarations => self.requested_facts.contains(ParseFacts::DECLARATIONS),
            FactGroup::FallbackCompletions => self.fallback_used,
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
            FactGroup::CallableAnchors => self.requested_facts.contains(ParseFacts::CALL_RELATIONS),
            FactGroup::CallSites => self.requested_facts.contains(ParseFacts::CALL_RELATIONS),
        }
    }
}

pub use crate::semantic_model::{
    AliasTarget, AliasTargetFidelity, DeclaratorShape, FieldDef, Include, MemberConfidence,
    MemberDef, MemberKind, Occurrence, RecordConfidence, RecordDef, RecordKind,
    RecordRangeFidelity, SymbolKind, SymbolRole, SyntacticRole, TypeAlias,
};

#[allow(dead_code)]
pub const PARSER_FACT_VERSION: i64 = crate::semantic_model::PARSER_FACT_VERSION;

/// A typedef alias mapping a new name to an underlying record tag, e.g.
/// `typedef struct Foo FooT;` records `FooT -> Foo`. Lets member completion
/// resolve a receiver typed with the alias back to the tag that owns the fields.
/// Parse `source` into the single `FileSemanticIndex` product: lexical includes
/// plus AST-derived declarations, occurrences, records, fields, aliases, enum
/// constants, and record-typed local declarations — one tree-sitter parse and
/// one AST DFS. A hard AST failure returns only isolated fallback completions.
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
#[cfg(test)]
pub fn parse(path: &Path, source: &str) -> FileSemanticIndex {
    parse_with_language(
        path,
        source,
        SourceLanguage::default_for_path(path),
        ParseFacts::ALL,
    )
}

pub fn parse_with_language(
    path: &Path,
    source: &str,
    language: SourceLanguage,
    facts: ParseFacts,
) -> FileSemanticIndex {
    parse_with_handle_and_language(path, source, language, None, facts)
}

/// Parse `source` with an optional shared [`ParserHandle`].
///
/// When `handle` is `Some`, the caller-owned parser is reused across calls
/// (avoids repeated `Parser::new()` + `set_language()`). When `None`, a
/// temporary handle is created per call (same behaviour as [`parse`]).
///
/// `facts` controls which AST facts are collected during the DFS pass.
/// Skipped facts produce empty vectors in the returned `FileSemanticIndex`.
#[cfg(test)]
pub fn parse_with_handle(
    path: &Path,
    source: &str,
    handle: Option<&ParserHandle>,
    facts: ParseFacts,
) -> FileSemanticIndex {
    parse_with_handle_and_language(
        path,
        source,
        SourceLanguage::default_for_path(path),
        handle,
        facts,
    )
}

pub fn parse_with_handle_and_language(
    path: &Path,
    source: &str,
    language: SourceLanguage,
    handle: Option<&ParserHandle>,
    facts: ParseFacts,
) -> FileSemanticIndex {
    parse_with_handle_control(path, source, language, handle, facts, None)
        .expect("non-cancelled parse always produces a parse product")
}

fn parse_with_handle_control(
    path: &Path,
    source: &str,
    language: SourceLanguage,
    handle: Option<&ParserHandle>,
    facts: ParseFacts,
    cancel: Option<&AtomicBool>,
) -> Option<FileSemanticIndex> {
    let line_starts = line_starts(source);
    let frontend = parser_frontend_adapter(language.parser_frontend());
    // The Go adapter intentionally disables C include scanning: a cgo preamble
    // may contain textual `#include` lines inside a Go comment, but cgo
    // cross-language binding is outside the supported fact contract.
    let includes = (frontend.scan_includes)(source);

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
        Some(cancel) => active_handle.parse_with_language_cancel(
            language.tree_sitter_language(),
            source,
            cancel,
        ),
        None => active_handle.parse_with_language(language.tree_sitter_language(), source, None),
    };
    let tree = match parsed_tree {
        Ok(Some(tree)) => tree,
        Ok(None) if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) => return None,
        Ok(None) | Err(()) => {
            return Some(lexical_fallback(path, source, includes, facts, language));
        }
    };

    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return None;
    }
    if ast_is_hard_failure(tree.root_node(), source) {
        return Some(lexical_fallback(path, source, includes, facts, language));
    }

    let BackendAstProduct {
        mut ast,
        package,
        imports,
        build_guard,
    } = (frontend.collect)(
        tree.root_node(),
        path,
        source,
        &line_starts,
        facts,
        language,
    );
    let declarations = if facts.contains(ParseFacts::DECLARATIONS) {
        declarations::canonical_declarations(
            path,
            language,
            declarations::CanonicalDeclarationInputs {
                source,
                type_symbols: &ast.type_symbols,
                enum_constants: &ast.enum_constants,
                records: &ast.records,
                aliases: &ast.aliases,
                anchors: &ast.callable_anchors,
                declarations: ast.declarations,
            },
        )
    } else {
        Vec::new()
    };
    // Canonical declarations use records, aliases, and callable anchors as
    // private staging evidence. Do not leak those supporting collections into
    // request products unless their own fact groups were requested.
    if !facts.intersects(ParseFacts::RECORDS | ParseFacts::FIELDS) {
        ast.records.clear();
    }
    if !facts.contains(ParseFacts::ALIASES) {
        ast.aliases.clear();
    }
    if !facts.contains(ParseFacts::CALL_RELATIONS) {
        ast.callable_anchors.clear();
    }
    let parse_outcome = if ast.parse_error_count == 0 {
        ParseOutcome::Ast
    } else {
        ParseOutcome::PartialAst
    };

    Some(FileSemanticIndex {
        language: language.semantic_language(),
        includes,
        package,
        imports,
        build_guard,
        declarations,
        fallback_completions: Vec::new(),
        parse_outcome,
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
            lexical_source: FactSource::Lexical,
            ast_source: FactSource::Ast,
            requested_facts: facts,
        },
    })
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
pub fn parse_thread_local_with_language(
    path: &Path,
    source: &str,
    language: SourceLanguage,
    facts: ParseFacts,
) -> FileSemanticIndex {
    TL_PARSER_HANDLE.with(|cell| {
        let handle = cell.borrow();
        parse_with_handle_and_language(path, source, language, Some(&*handle), facts)
    })
}

pub fn parse_thread_local_with_language_cancel(
    path: &Path,
    source: &str,
    language: SourceLanguage,
    facts: ParseFacts,
    cancel: &AtomicBool,
) -> Option<FileSemanticIndex> {
    TL_PARSER_HANDLE.with(|cell| {
        let handle = cell.borrow();
        parse_with_handle_control(path, source, language, Some(&*handle), facts, Some(cancel))
    })
}

fn lexical_fallback(
    path: &Path,
    source: &str,
    includes: Vec<Include>,
    facts: ParseFacts,
    language: SourceLanguage,
) -> FileSemanticIndex {
    let source_guard =
        (parser_frontend_adapter(language.parser_frontend()).fallback_build_guard)(source);
    let build_guard = if language == SourceLanguage::Go {
        go::combine_build_guards(source_guard, go::filename_build_guard(path))
    } else {
        source_guard
    };
    FileSemanticIndex {
        language: language.semantic_language(),
        includes,
        package: None,
        imports: Vec::new(),
        build_guard,
        declarations: Vec::new(),
        fallback_completions: extract_fallback_completions(source, language),
        parse_outcome: ParseOutcome::LexicalFallback,
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
            lexical_source: FactSource::Lexical,
            ast_source: FactSource::LexicalFallback,
            requested_facts: facts,
        },
    }
}

fn ast_is_hard_failure(root: tree_sitter::Node<'_>, source: &str) -> bool {
    if source.trim().is_empty() {
        return false;
    }
    let mut cursor = root.walk();
    let mut has_error_or_missing = false;
    let mut has_usable_structure = false;
    for node in root.named_children(&mut cursor) {
        if node.is_error() || node.is_missing() {
            has_error_or_missing = true;
        } else if ast_node_has_usable_structure(node) {
            // Comments prove that the input is lexically readable, but they do
            // not make an otherwise wholly broken translation unit useful to
            // the AST fact collectors.  A comments-only file is still a clean
            // AST product because `has_error_or_missing` remains false.
            has_usable_structure = true;
        }
    }
    has_error_or_missing && !has_usable_structure
}

fn ast_node_has_usable_structure(node: tree_sitter::Node<'_>) -> bool {
    match node.kind() {
        "comment" | "empty_statement" => false,
        "expression_statement" => {
            let mut cursor = node.walk();
            let has_expression = node.named_children(&mut cursor).next().is_some();
            has_expression
        }
        _ => true,
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
