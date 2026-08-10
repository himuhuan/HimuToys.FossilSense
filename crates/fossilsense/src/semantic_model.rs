//! Parser- and storage-neutral semantic facts shared across internal layers.

use serde::{Deserialize, Serialize};

use crate::call_model::SourceRange;

/// Version of the durable parser-fact contract.
///
/// This is deliberately independent from the SQLite schema version: changing
/// how a fact is derived must invalidate persisted rows even when their SQL
/// column layout happens to stay compatible.
pub const PARSER_FACT_VERSION: i64 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum SemanticFamily {
    CFamily,
    Go,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseOutcome {
    Ast,
    PartialAst,
    LexicalFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKindHint {
    Function,
    Macro,
    Type,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FallbackCompletionFact {
    pub name: String,
    pub kind_hint: CompletionKindHint,
    pub range: SourceRange,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct DeclarationLocator {
    pub workspace_id: String,
    pub path: String,
    pub range: SourceRange,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct LogicalEntityKey {
    pub qualified_name: String,
    pub declaration_kind: SemanticDeclarationKind,
    pub owner: Option<String>,
    pub canonical_signature: Option<String>,
    pub linkage_domain: String,
    pub guard_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SemanticDeclarationKind {
    Function,
    Method,
    Object,
    Type,
    Alias,
    EnumConstant,
    Macro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SemanticDeclarationRole {
    Declaration,
    Definition,
    TentativeDefinition,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SemanticLanguage {
    C,
    Cpp,
    Unknown,
    Go,
}

impl SemanticLanguage {
    pub fn semantic_family(self) -> SemanticFamily {
        match self {
            Self::Go => SemanticFamily::Go,
            Self::C | Self::Cpp | Self::Unknown => SemanticFamily::CFamily,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum LanguageFidelity {
    Explicit,
    Inferred,
    Heuristic,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SemanticFactProvenance {
    Ast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SemanticFactFidelity {
    Authoritative,
    Incomplete,
    LowFidelity,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct DeclarationIdentity {
    pub locator: DeclarationLocator,
    pub logical_key: LogicalEntityKey,
    pub language: SemanticLanguage,
    pub language_fidelity: LanguageFidelity,
    pub provenance: SemanticFactProvenance,
    pub fact_fidelity: SemanticFactFidelity,
    pub role: SemanticDeclarationRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationFact {
    pub identity: DeclarationIdentity,
    pub name: String,
    pub qualified_name: String,
    pub declaration_kind: SemanticDeclarationKind,
    pub role: SemanticDeclarationRole,
    pub path: String,
    pub name_range: SourceRange,
    pub declaration_range: SourceRange,
    pub canonical_signature: Option<String>,
    pub declarator_shape: Option<DeclaratorShape>,
    pub has_initializer: Option<bool>,
    pub owner: Option<String>,
    pub linkage: crate::call_model::LinkageDomain,
    pub guard: Option<String>,
    /// Parser-established link to the richer fact that owns this declaration.
    /// Query code follows this link instead of reconstructing identity from
    /// name/path/range tuples.
    pub backing: DeclarationBacking,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DeclarationBacking {
    CallableAnchor { fingerprint: String },
    Record { record_key: String },
    TypeAlias { fingerprint: String },
    SourceRange { range: SourceRange },
    None,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Struct,
    Union,
    Class,
    Interface,
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
    /// Exact range of the tag identifier, or of the typedef identifier for an
    /// anonymous typedef record.
    pub name_range: SourceRange,
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
    FunctionPointer { signature: String },
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Include {
    pub line: usize,
    pub target_text: String,
}

/// Go package declaration for a source file. Package membership is a
/// language-front-end fact; module/import-path resolution remains an indexer
/// concern so parsing never depends on a Go toolchain.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageFact {
    pub name: String,
    pub name_range: SourceRange,
}

/// Go import declaration. The path is stored without string delimiters and the
/// optional alias preserves named, dot, and blank imports.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImportFact {
    pub path: String,
    pub alias: Option<String>,
    pub path_range: SourceRange,
    pub declaration_range: SourceRange,
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
    pub language: SemanticLanguage,
    pub parse_outcome: ParseOutcome,
    pub includes: &'a [Include],
    pub package: Option<&'a PackageFact>,
    pub imports: &'a [ImportFact],
    pub build_guard: Option<&'a str>,
    pub declarations: &'a [DeclarationFact],
    pub fallback_completions: &'a [FallbackCompletionFact],
    pub records: &'a [RecordDef],
    pub fields: &'a [FieldDef],
    pub members: &'a [MemberDef],
    pub aliases: &'a [TypeAlias],
    pub callable_anchors: &'a [crate::call_model::CallableAnchor],
    pub call_sites: &'a [crate::call_model::CallSiteFact],
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use serde_json::json;

    use super::*;
    use crate::call_model::{SourcePosition, SourceRange};

    fn sample_identity() -> DeclarationIdentity {
        DeclarationIdentity {
            locator: DeclarationLocator {
                workspace_id: "workspace".to_string(),
                path: "include/api.h".to_string(),
                range: SourceRange {
                    start: SourcePosition {
                        line: 4,
                        character: 8,
                    },
                    end: SourcePosition {
                        line: 4,
                        character: 22,
                    },
                    start_byte: 48,
                    end_byte: 62,
                },
                fingerprint: "decl-0001".to_string(),
            },
            logical_key: LogicalEntityKey {
                qualified_name: "demo::lookup".to_string(),
                declaration_kind: SemanticDeclarationKind::Function,
                owner: Some("demo".to_string()),
                canonical_signature: Some("int lookup(int)".to_string()),
                linkage_domain: "external".to_string(),
                guard_fingerprint: Some("guard-a".to_string()),
            },
            language: SemanticLanguage::Cpp,
            language_fidelity: LanguageFidelity::Inferred,
            provenance: SemanticFactProvenance::Ast,
            fact_fidelity: SemanticFactFidelity::Authoritative,
            role: SemanticDeclarationRole::Declaration,
        }
    }

    #[test]
    fn declaration_identity_serializes_with_stable_field_and_enum_names() {
        let value = serde_json::to_value(sample_identity()).expect("identity json");

        assert_eq!(
            value,
            json!({
                "locator": {
                    "workspaceId": "workspace",
                    "path": "include/api.h",
                    "range": {
                        "start": { "line": 4, "character": 8 },
                        "end": { "line": 4, "character": 22 },
                        "startByte": 48,
                        "endByte": 62
                    },
                    "fingerprint": "decl-0001"
                },
                "logicalKey": {
                    "qualifiedName": "demo::lookup",
                    "declarationKind": "function",
                    "owner": "demo",
                    "canonicalSignature": "int lookup(int)",
                    "linkageDomain": "external",
                    "guardFingerprint": "guard-a"
                },
                "language": "cpp",
                "languageFidelity": "inferred",
                "provenance": "ast",
                "factFidelity": "authoritative",
                "role": "declaration"
            })
        );
    }

    #[test]
    fn declaration_identity_round_trips_and_hashes_by_concrete_locator_and_logical_key() {
        let identity = sample_identity();
        let encoded = serde_json::to_string(&identity).expect("encoded identity");
        let decoded: DeclarationIdentity =
            serde_json::from_str(&encoded).expect("decoded identity");

        assert_eq!(decoded, identity);

        let mut first = DefaultHasher::new();
        identity.hash(&mut first);
        let mut second = DefaultHasher::new();
        decoded.hash(&mut second);
        assert_eq!(first.finish(), second.finish());

        let mut other = identity.clone();
        other.locator.fingerprint = "decl-0002".to_string();
        assert_ne!(other, identity);
    }
}
