# Parser Fact Projections

Status: current

`parse(path, source)` remains the single tolerant parser entry point and still
returns `FileSemanticIndex`. The compatibility fields remain public so existing
callers and tests can migrate incrementally.

## Fact Masks

`ParseFacts` controls AST fact collection. The lexical pass for symbols and
includes always runs.

- `ParseFacts::INDEX`: persistent index facts, excluding request-time facts.
- `ParseFacts::COLOR_REF`: symbols, includes, and occurrences for coloring and
  reference role classification.
- `ParseFacts::MEMBER`: member-completion receiver facts plus record/member and
  alias facts.
- `ParseFacts::ALL`: backward-compatible default used by `parse()`.

Skipped groups still appear as empty vectors in `FileSemanticIndex`; callers
must use availability metadata when the distinction matters.

## Projections

`FileSemanticIndex::persistent_facts()` returns borrowed index-time facts:

- symbols
- includes
- records
- fields
- members
- aliases

`FileSemanticIndex::request_facts()` returns borrowed request-time facts:

- occurrences
- local declarations
- local bindings

The projections do not allocate and do not change ownership. They only make the
intended consumer contract explicit.

## Availability

`FileSemanticIndex::fact_availability(group)` and
`ParseDiagnostics::fact_availability(group)` return `FactAvailability`:

- `Available`: requested and trustworthy, even if the vector is empty.
- `NotRequested`: skipped by the `ParseFacts` mask.
- `Unavailable(FactUnavailableReason::LexicalFallback)`: requested, but
  tree-sitter did not produce a usable tree and only lexical facts exist.

`FactGroup::Symbols` and `FactGroup::Includes` are always available because the
lexical pass is unconditional. AST-derived groups become unavailable only on
lexical fallback.

## Consumer Expectations

- The indexer and store writes consume `persistent_facts()`.
- Semantic coloring, references, local completion overlay, ordinary completion
  service, and member completion consume `request_facts()` and availability
  where skipped/fallback state affects interpretation.
- Compatibility field access remains valid for parser internals and tests that
  intentionally compare the legacy shape.
- The refactor does not add parser dependencies, parser algorithms, or new
  C/C++ semantic claims.

## Verification

Focused coverage for this contract lives in:

- `parser::tests::parse_fact_masks_document_current_field_contents`
- `parser::tests::availability_distinguishes_empty_skipped_and_fallback_ast_vectors`
- `store::tests::parser_consumer_migration`

The architecture fitness check verifies that parser code remains independent of
store, server, and indexer details.
