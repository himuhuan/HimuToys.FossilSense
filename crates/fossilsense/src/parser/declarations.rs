use std::collections::HashSet;
use std::path::Path;

use crate::call_model::{LinkageDomain, SourcePosition, SourceRange};
use crate::semantic_model::{
    DeclarationBacking, DeclarationFact, DeclarationIdentity, DeclarationLocator, LanguageFidelity,
    LogicalEntityKey, RecordDef, RecordRangeFidelity, SemanticDeclarationKind,
    SemanticDeclarationRole, SemanticFactFidelity, SemanticFactProvenance, SemanticLanguage,
    Symbol, SymbolKind, SymbolRole, TypeAlias,
};

pub(super) fn canonical_declarations(
    path: &Path,
    symbols: &[Symbol],
    records: &[RecordDef],
    aliases: &[TypeAlias],
    anchors: &[crate::call_model::CallableAnchor],
    mut declarations: Vec<DeclarationFact>,
    lexical_fallback: bool,
) -> Vec<DeclarationFact> {
    declarations.extend(callable_declarations(path, anchors));
    declarations.extend(
        records
            .iter()
            .filter_map(|record| record_declaration(path, record)),
    );
    declarations.extend(aliases.iter().map(|alias| alias_declaration(path, alias)));

    let canonical: HashSet<_> = declarations
        .iter()
        .map(|fact| {
            (
                fact.name.clone(),
                lexical_suppression_kind(fact.declaration_kind),
            )
        })
        .collect();
    declarations.extend(symbols.iter().filter_map(|symbol| {
        let kind = declaration_kind(symbol.kind)?;
        (!canonical.contains(&(symbol.name.clone(), lexical_suppression_kind(kind))))
            .then(|| symbol_declaration(path, symbol, kind, lexical_fallback))
    }));
    declarations.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.name_range.start_byte.cmp(&right.name_range.start_byte))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| kind_rank(left.declaration_kind).cmp(&kind_rank(right.declaration_kind)))
    });
    declarations.dedup_by(|left, right| left.identity.locator == right.identity.locator);
    declarations
}

fn callable_declarations(
    path: &Path,
    anchors: &[crate::call_model::CallableAnchor],
) -> Vec<DeclarationFact> {
    anchors
        .iter()
        .filter(|anchor| {
            anchor.kind == crate::call_model::CallableKind::Function
                && anchor.role != crate::call_model::AnchorRole::Synthetic
        })
        .map(|anchor| {
            let declaration_kind =
                if anchor.owner_kind == Some(crate::call_model::OwnerKindHint::Record) {
                    SemanticDeclarationKind::Method
                } else {
                    SemanticDeclarationKind::Function
                };
            let role = match anchor.role {
                crate::call_model::AnchorRole::Declaration => SemanticDeclarationRole::Declaration,
                crate::call_model::AnchorRole::Definition => SemanticDeclarationRole::Definition,
                crate::call_model::AnchorRole::Synthetic => SemanticDeclarationRole::Unknown,
            };
            let fidelity = if anchor.syntax_error_overlap
                || anchor.signature_fidelity != crate::call_model::SignatureFidelity::AstExact
            {
                SemanticFactFidelity::Incomplete
            } else {
                SemanticFactFidelity::Authoritative
            };
            let provenance = match anchor.provenance {
                crate::call_model::FactProvenance::Ast => SemanticFactProvenance::Ast,
                crate::call_model::FactProvenance::LexicalFallback => {
                    SemanticFactProvenance::LexicalFallback
                }
                crate::call_model::FactProvenance::Synthetic => SemanticFactProvenance::Synthetic,
            };
            let guard_fingerprint = anchor.guard.as_ref().map(|guard| digest(guard));
            fact(
                path,
                anchor.name.clone(),
                anchor.qualified_name.clone(),
                declaration_kind,
                role,
                anchor.name_range,
                anchor.declaration_range,
                Some(anchor.canonical_signature.clone()),
                anchor.owner.clone(),
                anchor.linkage.clone(),
                anchor.guard.clone(),
                provenance,
                fidelity,
                anchor.anchor_fingerprint.clone(),
                guard_fingerprint,
                DeclarationBacking::CallableAnchor {
                    fingerprint: anchor.anchor_fingerprint.clone(),
                },
            )
        })
        .collect()
}

fn record_declaration(path: &Path, record: &RecordDef) -> Option<DeclarationFact> {
    let name = record
        .tag_name
        .as_ref()
        .or(record.typedef_name.as_ref())?
        .clone();
    let fidelity = if record.range_fidelity == RecordRangeFidelity::AstExact {
        SemanticFactFidelity::Authoritative
    } else {
        SemanticFactFidelity::Incomplete
    };
    Some(fact(
        path,
        name.clone(),
        name,
        SemanticDeclarationKind::Type,
        SemanticDeclarationRole::Definition,
        record.name_range,
        record.declaration_range,
        Some(record.signature.clone()),
        None,
        LinkageDomain::External,
        None,
        SemanticFactProvenance::Ast,
        fidelity,
        digest(&format!("record|{}|{}", path_text(path), record.record_key)),
        None,
        DeclarationBacking::Record {
            record_key: record.record_key.clone(),
        },
    ))
}

fn alias_declaration(path: &Path, alias: &TypeAlias) -> DeclarationFact {
    let name_range = SourceRange {
        start: SourcePosition {
            line: alias.start_line as u32,
            character: alias.start_col as u32,
        },
        end: SourcePosition {
            line: alias.end_line as u32,
            character: alias.end_col as u32,
        },
        start_byte: alias.start_byte,
        end_byte: alias.end_byte,
    };
    let fidelity = match alias.target_fidelity {
        crate::semantic_model::AliasTargetFidelity::AstExact => SemanticFactFidelity::Authoritative,
        crate::semantic_model::AliasTargetFidelity::Heuristic => SemanticFactFidelity::LowFidelity,
        crate::semantic_model::AliasTargetFidelity::Malformed => SemanticFactFidelity::Incomplete,
    };
    fact(
        path,
        alias.alias.clone(),
        alias.alias.clone(),
        SemanticDeclarationKind::Alias,
        SemanticDeclarationRole::Definition,
        name_range,
        alias.declaration_range,
        Some(alias.underlying_spelling.clone()),
        None,
        LinkageDomain::External,
        None,
        SemanticFactProvenance::Ast,
        fidelity,
        alias.fingerprint.clone(),
        None,
        DeclarationBacking::TypeAlias {
            fingerprint: alias.fingerprint.clone(),
        },
    )
}

fn symbol_declaration(
    path: &Path,
    symbol: &Symbol,
    kind: SemanticDeclarationKind,
    lexical_fallback: bool,
) -> DeclarationFact {
    let range = SourceRange {
        start: SourcePosition {
            line: symbol.start_line as u32,
            character: symbol.start_col as u32,
        },
        end: SourcePosition {
            line: symbol.end_line as u32,
            character: symbol.end_col as u32,
        },
        start_byte: symbol.start_byte,
        end_byte: symbol.end_byte,
    };
    let role = match symbol.role {
        SymbolRole::Definition => SemanticDeclarationRole::Definition,
        SymbolRole::Declaration => SemanticDeclarationRole::Declaration,
        SymbolRole::TentativeDefinition => SemanticDeclarationRole::TentativeDefinition,
        SymbolRole::UnknownDeclarationOrDefinition => SemanticDeclarationRole::Unknown,
    };
    let owner = symbol.container.clone();
    let qualified_name = owner.as_ref().map_or_else(
        || symbol.name.clone(),
        |owner| format!("{owner}::{}", symbol.name),
    );
    let guard_fingerprint = symbol.guard.as_ref().map(|guard| digest(guard));
    let fingerprint = digest(&format!(
        "symbol|{}|{}|{}|{}|{:?}",
        path_text(path),
        symbol.name,
        symbol.start_byte,
        symbol.end_byte,
        symbol.kind
    ));
    fact(
        path,
        symbol.name.clone(),
        qualified_name,
        kind,
        role,
        range,
        range,
        (!symbol.signature.is_empty()).then(|| symbol.signature.clone()),
        owner,
        LinkageDomain::Unknown,
        symbol.guard.clone(),
        SemanticFactProvenance::LexicalFallback,
        if lexical_fallback || symbol.role == SymbolRole::UnknownDeclarationOrDefinition {
            SemanticFactFidelity::LowFidelity
        } else {
            SemanticFactFidelity::Authoritative
        },
        fingerprint,
        guard_fingerprint,
        DeclarationBacking::Symbol {
            start_byte: symbol.start_byte,
            end_byte: symbol.end_byte,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn fact(
    path: &Path,
    name: String,
    qualified_name: String,
    declaration_kind: SemanticDeclarationKind,
    role: SemanticDeclarationRole,
    name_range: SourceRange,
    declaration_range: SourceRange,
    canonical_signature: Option<String>,
    owner: Option<String>,
    linkage: LinkageDomain,
    guard: Option<String>,
    provenance: SemanticFactProvenance,
    fact_fidelity: SemanticFactFidelity,
    fingerprint: String,
    guard_fingerprint: Option<String>,
    backing: DeclarationBacking,
) -> DeclarationFact {
    let path = path_text(path);
    let logical_key = LogicalEntityKey {
        qualified_name: qualified_name.clone(),
        declaration_kind,
        owner: owner.clone(),
        canonical_signature: canonical_signature.clone(),
        linkage_domain: linkage_key(&linkage),
        guard_fingerprint,
    };
    DeclarationFact {
        identity: DeclarationIdentity {
            locator: DeclarationLocator {
                workspace_id: String::new(),
                path: path.clone(),
                range: declaration_range,
                fingerprint,
            },
            logical_key,
            language: language(path.as_ref()),
            language_fidelity: LanguageFidelity::Explicit,
            provenance,
            fact_fidelity,
            role,
        },
        name,
        qualified_name,
        declaration_kind,
        role,
        path,
        name_range,
        declaration_range,
        canonical_signature,
        declarator_shape: None,
        has_initializer: None,
        owner,
        linkage,
        guard,
        backing,
    }
}

fn declaration_kind(kind: SymbolKind) -> Option<SemanticDeclarationKind> {
    Some(match kind {
        SymbolKind::Function => SemanticDeclarationKind::Function,
        SymbolKind::Macro => SemanticDeclarationKind::Macro,
        SymbolKind::Type => SemanticDeclarationKind::Type,
        SymbolKind::EnumConstant => SemanticDeclarationKind::EnumConstant,
        SymbolKind::GlobalVariable => SemanticDeclarationKind::Object,
        SymbolKind::Field => return None,
    })
}

fn kind_rank(kind: SemanticDeclarationKind) -> u8 {
    match kind {
        SemanticDeclarationKind::Function => 0,
        SemanticDeclarationKind::Method => 1,
        SemanticDeclarationKind::Object => 2,
        SemanticDeclarationKind::Type => 3,
        SemanticDeclarationKind::Alias => 4,
        SemanticDeclarationKind::EnumConstant => 5,
        SemanticDeclarationKind::Macro => 6,
    }
}

fn lexical_suppression_kind(kind: SemanticDeclarationKind) -> SemanticDeclarationKind {
    match kind {
        SemanticDeclarationKind::Method => SemanticDeclarationKind::Function,
        SemanticDeclarationKind::Alias => SemanticDeclarationKind::Type,
        kind => kind,
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn language(path: &str) -> SemanticLanguage {
    if ["cpp", "hpp", "cc", "hh", "cxx", "hxx", "inl"]
        .iter()
        .any(|extension| {
            path.to_ascii_lowercase()
                .ends_with(&format!(".{extension}"))
        })
    {
        SemanticLanguage::Cpp
    } else if path.is_empty() {
        SemanticLanguage::Unknown
    } else {
        SemanticLanguage::C
    }
}

fn linkage_key(linkage: &LinkageDomain) -> String {
    match linkage {
        LinkageDomain::External => "external".into(),
        LinkageDomain::Internal(path) => format!("internal:{path}"),
        LinkageDomain::Unknown => "unknown".into(),
    }
}

fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..24].to_string()
}
