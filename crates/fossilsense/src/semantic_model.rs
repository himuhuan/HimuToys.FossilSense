//! Parser- and storage-neutral semantic facts shared across internal layers.

use serde::{Deserialize, Serialize};

use crate::call_model::SourceRange;

/// Version of the durable parser-fact contract.
///
/// This is deliberately independent from the SQLite schema version: changing
/// how a fact is derived must invalidate persisted rows even when their SQL
/// column layout happens to stay compatible.
pub const PARSER_FACT_VERSION: i64 = 3;

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
    pub container: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Macro,
    Type,
    EnumConstant,
    GlobalVariable,
    Field,
}

pub fn kind_from_str(s: &str) -> SymbolKind {
    match s {
        "function" => SymbolKind::Function,
        "macro" => SymbolKind::Macro,
        "type" => SymbolKind::Type,
        "enum_constant" => SymbolKind::EnumConstant,
        "field" => SymbolKind::Field,
        _ => SymbolKind::GlobalVariable,
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordRangeFidelity {
    AstExact,
    Malformed,
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
    /// Exact range of the `{ ... }` body, including both braces.
    pub body_range: SourceRange,
    /// Best-effort enclosing declaration range. When the AST proves direct
    /// ownership this includes the terminating semicolon; otherwise it falls
    /// back to the record specifier range above.
    pub declaration_range: SourceRange,
    /// BLAKE3 digest of the exact bytes covered by `declaration_range`.
    /// Durable consumers use this range-local identity to hydrate excerpts
    /// without reading or hashing the whole source file.
    pub declaration_hash: [u8; 32],
    pub range_fidelity: RecordRangeFidelity,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemberKind {
    Field,
    Method,
    StaticMethod,
    NestedType,
}
impl MemberKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Field => "field",
            Self::Method => "method",
            Self::StaticMethod => "static_method",
            Self::NestedType => "nested_type",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemberConfidence {
    InBody,
    OutOfClassOwner,
    Heuristic,
}
impl MemberConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InBody => "in_body",
            Self::OutOfClassOwner => "out_of_class_owner",
            Self::Heuristic => "heuristic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDef {
    pub record_key: String,
    pub name: String,
    pub kind: MemberKind,
    pub confidence: MemberConfidence,
    pub type_name: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DeclaratorShape {
    Identity,
    Pointer { qualifiers: Vec<String> },
    Array { extent_text: String },
    Qualified { qualifiers: Vec<String> },
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasTargetFidelity {
    AstExact,
    Heuristic,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAlias {
    pub alias: String,
    pub target: AliasTarget,
    /// Alias identifier range. These compatibility fields intentionally remain
    /// the navigation range rather than being widened to the whole typedef.
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub declaration_range: SourceRange,
    /// BLAKE3 digest of the exact bytes covered by `declaration_range`.
    /// Every declarator in one typedef statement intentionally shares it.
    pub declaration_hash: [u8; 32],
    /// Spelling shared by every declarator in the typedef, excluding `typedef`
    /// itself and excluding each declarator-specific `*`/array suffix.
    pub underlying_spelling: String,
    pub declarator_shape: DeclaratorShape,
    pub target_fidelity: AliasTargetFidelity,
    /// Stable 96-bit hexadecimal digest scoped to this individual declarator.
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolRole {
    Definition,
    Declaration,
    /// C file-scope object declaration without an initializer and without an
    /// `extern` storage-class specifier. For objects this is weaker than a full
    /// definition but stronger than a declaration-only anchor.
    TentativeDefinition,
    /// The lexical pass found an object name but could not safely distinguish
    /// a declaration from a definition (for example because its declarator is
    /// malformed or uses syntax outside the supported subset).
    UnknownDeclarationOrDefinition,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntacticRole {
    Definition,
    Declaration,
    Call,
    Write,
    Read,
    TypeUse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub line: usize,
    pub target_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub name: String,
    pub start_byte: usize,
    pub line: u32,
    pub start_col: u32,
    pub length: u32,
    pub role: SyntacticRole,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct PersistentFacts<'a> {
    pub symbols: &'a [Symbol],
    pub includes: &'a [Include],
    pub records: &'a [RecordDef],
    pub fields: &'a [FieldDef],
    pub members: &'a [MemberDef],
    pub aliases: &'a [TypeAlias],
    pub callable_anchors: &'a [crate::call_model::CallableAnchor],
    pub call_sites: &'a [crate::call_model::CallSiteFact],
}
