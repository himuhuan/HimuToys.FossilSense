# Smart Completion Phase 4-6 实现计划

> **给代理执行者：** 必须使用 `himupowers:subagent-driven-development`（推荐）或 `himupowers:executing-plans` 逐任务执行本计划。每个生产代码任务必须使用 `himupowers:test-driven-development`。
> 需求文档：`docs/smart-completion-phase4-6/requirements.md`

**目标：** 交付 smart completion Phase 4-6：普通标识符补全具备轻量 intent-aware ranking 和多通道召回，include path completion 具备 sibling/recent/frequency/depth 排序增强。
**架构：** 继续复用 Phase 0-3 的 `completion` 核心、`NameTable`、live parse cache、include table 和 LSP surface routing。Intent 是补全排序证据，不是语义绑定；多通道召回扩大内部候选池但保持 bounded；include 排序在既有 quote/angle source prior 上增加二级 evidence。
**技术栈：** Rust 2021, tower-lsp, tree-sitter parse product, existing `NameTable`, existing `resolver`, existing include graph/store queries, cargo unit/integration tests, VS Code extension pnpm compile/package.

## Implementation progress, 2026-07-05

- Task 1 completed: lightweight completion intent classifier implemented and committed as `fc31eb3`.
- Task 2 completed: intent-aware candidate kind evidence and rank context implemented and committed as `4cbdf00`.
- Task 3 completed: bounded multi-channel `NameTable` recall and source-safe recall metrics implemented and committed as `b6e801b`.
- Task 4 completed: include completion ranking evidence and edge-aware include table rebuild implemented and committed as `1d94106`.
- Task 5 completed in working tree: README, extension README, `CLAUDE.md`, requirements, and this plan synchronized for Phase 4-6 wording.
- Task 6 completed in working tree: full verification and package smoke passed; latest post-review package generated `dist/fossilsense-vscode-1.2.0_BUILD20260705_153916.vsix`; final requirements status and verification record updated.

## 全局约束

- 不依赖 clangd、ctags、compile commands、外部构建系统、编译器调用或用户构建参数。
- 不做完整 C/C++ 类型推断、宏展开、数据流分析、继承、重载、模板、命名空间或访问控制。
- 不做成员方法 schema、weak receiver inference、local history、anonymous telemetry、ML、LLM 或 auto include insertion。
- 不新增外部依赖，不新增用户 rank-weight 配置。
- 普通补全返回继续保持 `CompletionList.isIncomplete = true`。
- Include path completion 和 `.` / `->` member completion 继续在 ordinary identifier completion 前短路。
- Member completion 仍保持 field-focused，不纳入 Phase 4-6。
- `ScopeTier`、`ResolutionConfidence`、`ResolutionReason`、parser symbol kind、completion evidence 是共享 vocabulary；不得创建平行 semantic tier。
- Intent 只能调整排序，不能硬过滤候选；低置信 intent 必须接近 neutral。
- 扩大 recall 只能通过 per-channel cap 和 total cap；不得每键扫描 workspace、打开 SQLite 或遍历外部 include 树。
- 短前缀规则保持：prefix length < 3 时只接受 exact、prefix、word-boundary substring。
- Perf/debug summary 默认只输出 counts/timings/classes，不输出候选名、include path、源码片段或用户代码内容。
- 每个生产代码变更先写失败测试，确认 RED 后再实现。

## 文件结构

- 修改：`crates/fossilsense/src/completion.rs`
  - 职责：`CompletionIntent`、intent confidence、candidate kind evidence、intent-aware rank adjustment、rank metrics、source-safe summary。
  - 输入：current line/cursor prefix for classifier; `PipelineCandidate<T>` with `CandidateEvidence`.
  - 输出：intent context and ranked/truncated `CompletionPipelineOutput<T>`.
- 修改：`crates/fossilsense/src/query.rs`
  - 职责：`NameTable` channel-aware recall API, recall quotas, channel metrics, memo-compatible pool indices.
  - 输入：query prefix, `CompletionScope`, optional prior pool.
  - 输出：bounded `RankedNameHit` list, tier-agnostic pool, `CompletionRecallMetrics`.
- 修改：`crates/fossilsense/src/server.rs`
  - 职责：map parser/LSP completion kinds into completion-core kind evidence; route include ranking evidence; render final LSP items.
- 修改：`crates/fossilsense/src/server/language_server.rs`
  - 职责：compute intent before ordinary completion pipeline; call channel-aware `NameTable` recall; pass current document text into include completion.
- 修改：`crates/fossilsense/src/server/include_completion.rs`
  - 职责：`IncludeCompletionTable` ranking evidence, current-file recent include extraction, scored include candidates, source-safe include summary.
- 修改：`crates/fossilsense/src/server/indexing/cache.rs`
  - 职责：rebuild include table with indexed paths and resolved include edges.
- 修改：`crates/fossilsense/src/server/tests.rs`
  - 职责：server integration tests for intent ranking, channel recall, include routing, include table cache failure compatibility.
- 修改：`crates/fossilsense/src/query/tests.rs`
  - 职责：`NameTable` channel recall and short-prefix gate tests.
- 修改：`README.md`, `CLAUDE.md`, `extensions/vscode/README.md`
  - 职责：sync can/cannot/fallback/perf wording for Phase 4-6.
- 修改：`docs/smart-completion-phase4-6/requirements.md`
  - 职责：mark plan status, keep tracking matrix aligned with concrete tasks and validation commands.

## Task 1：Completion intent classifier

**覆盖需求：** FR1, FR7, NFR1, NFR6, NFR8

**文件：**
- 修改：`crates/fossilsense/src/completion.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`

**接口：**
- 消费：current line text, UTF-16 cursor character, current completion prefix.
- 产出：
  - `CompletionIntentKind::{Neutral, TypeName, ExpressionValue, CallTarget, MacroPreprocessor, DeclarationName}`
  - `CompletionIntentConfidence::{High, Medium, Low}`
  - `CompletionIntent { kind, confidence }`
  - `classify_completion_intent(line_text: &str, character: u32, prefix: &str) -> CompletionIntent`

- [ ] **步骤 1：写 RED 测试：preprocessor macro intent**

在 `crates/fossilsense/src/completion.rs` tests 中添加：

```rust
#[test]
fn intent_classifies_preprocessor_macro_context() {
    let intent = classify_completion_intent("#if FS_", 7, "FS_");

    assert_eq!(intent.kind, CompletionIntentKind::MacroPreprocessor);
    assert_eq!(intent.confidence, CompletionIntentConfidence::High);
}
```

- [ ] **步骤 2：写 RED 测试：call target intent**

```rust
#[test]
fn intent_classifies_call_target_before_open_paren() {
    let intent = classify_completion_intent("    FS_do(", 9, "FS_do");

    assert_eq!(intent.kind, CompletionIntentKind::CallTarget);
    assert!(intent.confidence >= CompletionIntentConfidence::Medium);
}
```

- [ ] **步骤 3：写 RED 测试：type and declaration-name contexts**

```rust
#[test]
fn intent_classifies_type_and_declaration_name_contexts() {
    let type_intent = classify_completion_intent("    struct FS_", 14, "FS_");
    assert_eq!(type_intent.kind, CompletionIntentKind::TypeName);

    let decl_intent = classify_completion_intent("    FsWidget fs_", 16, "fs_");
    assert_eq!(decl_intent.kind, CompletionIntentKind::DeclarationName);
}
```

- [ ] **步骤 4：写 RED 测试：uncertain context is neutral or expression**

```rust
#[test]
fn intent_degrades_for_uncertain_expression_context() {
    let intent = classify_completion_intent("    value = FS_", 15, "FS_");

    assert!(matches!(
        intent.kind,
        CompletionIntentKind::ExpressionValue | CompletionIntentKind::Neutral
    ));
    assert!(intent.confidence <= CompletionIntentConfidence::Medium);
}
```

- [ ] **步骤 5：运行 RED**

运行：`cargo test -p fossilsense completion::tests::intent_ -- --nocapture`

预期：新增 intent 类型或 classifier function 尚未存在，测试失败。

- [ ] **步骤 6：写最小实现**

实现规则：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionIntentKind {
    Neutral,
    TypeName,
    ExpressionValue,
    CallTarget,
    MacroPreprocessor,
    DeclarationName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CompletionIntentConfidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompletionIntent {
    pub kind: CompletionIntentKind,
    pub confidence: CompletionIntentConfidence,
}
```

Classifier uses only local lexical cues:

- `#if`, `#ifdef`, `#ifndef`, `#elif`, `#define` before cursor -> `MacroPreprocessor`.
- identifier followed by `(` at or after cursor -> `CallTarget`.
- preceding token in `struct`, `union`, `enum`, `class`, `typedef`, `using`, `sizeof`, `new` cue -> `TypeName`.
- simple `type-ish-token prefix` declaration pattern -> `DeclarationName`.
- `=`, `return`, `(` argument, condition delimiters -> `ExpressionValue`.
- uncertain or malformed line -> `Neutral` or low-confidence `ExpressionValue`.

- [ ] **步骤 7：接入 server context**

In ordinary completion path, compute:

```rust
let intent = crate::completion::classify_completion_intent(
    line_text,
    position.character,
    &prefix,
);
```

Do not change ranking in this task. Store/pass the value only after Task 2 introduces rank context.

- [ ] **步骤 8：运行 GREEN**

运行：

```bash
cargo test -p fossilsense completion::tests::intent_ -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
```

预期：intent unit tests pass; server tests remain compatible.

- [ ] **步骤 9：提交建议**

```bash
git add crates/fossilsense/src/completion.rs crates/fossilsense/src/server/language_server.rs
git commit -m "feat: classify lightweight completion intent"
```

## Task 2：Intent-aware ranker evidence

**覆盖需求：** FR2, FR3, FR4, FR5, FR6, FR8, FR16, NFR1, NFR3, NFR5, NFR6, NFR8

**文件：**
- 修改：`crates/fossilsense/src/completion.rs`
- 修改：`crates/fossilsense/src/server.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`

**接口：**
- 消费：Task 1 `CompletionIntent`; existing `CandidateEvidence`.
- 产出：
  - `CompletionCandidateKind::{Unknown, Function, Macro, Type, Variable, EnumConstant, Text}`
  - `CompletionRankContext { intent: CompletionIntent }`
  - `CandidateEvidence.kind: CompletionCandidateKind`
  - `run_evidence_aware_pipeline_with_context<T>(candidates, limit, context)`
  - Existing `run_evidence_aware_pipeline<T>` delegates to neutral context for compatibility.

- [ ] **步骤 1：写 RED ranker tests for type/expression/call/macro intents**

在 `crates/fossilsense/src/completion.rs` tests 中添加：

```rust
#[test]
fn type_intent_lifts_type_candidates_without_hiding_values() {
    let output = run_evidence_aware_pipeline_with_context(
        vec![
            candidate_with_kind("FsWidget", CandidateSource::Indexed, ScopeTier::Global, 800, CompletionCandidateKind::Type, "type"),
            candidate_with_kind("fs_widget_value", CandidateSource::Indexed, ScopeTier::Reachable, 650, CompletionCandidateKind::Variable, "value"),
        ],
        10,
        CompletionRankContext::for_intent(CompletionIntentKind::TypeName, CompletionIntentConfidence::High),
    );

    assert_eq!(output.items[0].payload, "type");
    assert!(output.items.iter().any(|candidate| candidate.payload == "value"));
}

#[test]
fn expression_intent_demotes_type_only_candidates_but_keeps_them() {
    let output = run_evidence_aware_pipeline_with_context(
        vec![
            candidate_with_kind("FsWidget", CandidateSource::Indexed, ScopeTier::Reachable, 800, CompletionCandidateKind::Type, "type"),
            candidate_with_kind("fs_value", CandidateSource::Indexed, ScopeTier::Reachable, 760, CompletionCandidateKind::Variable, "value"),
        ],
        10,
        CompletionRankContext::for_intent(CompletionIntentKind::ExpressionValue, CompletionIntentConfidence::High),
    );

    assert_eq!(output.items[0].payload, "value");
    assert!(output.items.iter().any(|candidate| candidate.payload == "type"));
}
```

Add equivalent focused tests:

```rust
#[test]
fn call_intent_lifts_functions() { /* function beats same-tier variable */ }

#[test]
fn macro_preprocessor_intent_lifts_macros() { /* macro beats same-tier type */ }

#[test]
fn declaration_name_intent_reduces_global_reuse_pressure() { /* local/text naming hint can beat weak global */ }
```

- [ ] **步骤 2：写 RED source-safe summary test**

```rust
#[test]
fn perf_summary_reports_intent_without_candidate_names() {
    let metrics = CompletionPipelineMetrics {
        intent_kind: CompletionIntentKind::CallTarget,
        intent_confidence: CompletionIntentConfidence::High,
        ..CompletionPipelineMetrics::default()
    };
    let line = completion_perf_summary("fs_", "cold", &CompletionStageTimings::default(), &metrics);

    assert!(line.contains("intent=call_target"));
    assert!(line.contains("intent_confidence=high"));
    assert!(!line.contains("fs_\""));
}
```

- [ ] **步骤 3：运行 RED**

运行：`cargo test -p fossilsense completion::tests -- --nocapture`

预期：candidate kind evidence, context-aware pipeline, and summary fields are missing.

- [ ] **步骤 4：实现 ranker integration**

Implementation notes:

- Add `kind` field to `CandidateEvidence`, defaulting to `CompletionCandidateKind::Unknown`.
- Add `CompletionRankContext` and keep `run_evidence_aware_pipeline` as a neutral wrapper.
- Add centralized intent weights:

```rust
const INTENT_STRONG_MATCH: i32 = 1_600;
const INTENT_MEDIUM_MATCH: i32 = 900;
const INTENT_BOUNDED_DEMOTION: i32 = -450;
```

- `TypeName` rewards `Type` and enum-like type evidence.
- `ExpressionValue` rewards `Variable`, `Function`, `Macro`, `EnumConstant`; demotes `Type`.
- `CallTarget` rewards `Function` and function-like `Macro`.
- `MacroPreprocessor` rewards `Macro`.
- `DeclarationName` reduces weak global indexed reuse by a bounded amount and allows current/local/text naming evidence to remain competitive.
- Low confidence scales adjustment down; neutral context returns zero adjustment.
- Guard bands still run after final score and still cap low-trust global/text candidates.

- [ ] **步骤 5：populate kind evidence in server helpers**

Update:

- `completion_items_for_local_bindings` -> `Variable`.
- `completion_items_for_current_file_overlay` -> map parser kind to `Function`, `Macro`, `Type`, `EnumConstant`, `Variable`, or `Text`.
- `completion_items_for_indexed_hits` -> same parser-kind mapping.
- local word fallback -> `Text`.
- exact indexed recovery for local words -> parser-kind mapping.

- [ ] **步骤 6：call context-aware pipeline**

In `server/language_server.rs`, replace:

```rust
let output = crate::completion::run_evidence_aware_pipeline(candidates, limit);
```

with:

```rust
let output = crate::completion::run_evidence_aware_pipeline_with_context(
    candidates,
    limit,
    crate::completion::CompletionRankContext { intent },
);
```

- [ ] **步骤 7：运行 GREEN**

运行：

```bash
cargo test -p fossilsense completion::tests -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
```

预期：ranker tests and server tests pass; existing source-safe summary assertions updated for intent fields.

- [ ] **步骤 8：提交建议**

```bash
git add crates/fossilsense/src/completion.rs crates/fossilsense/src/server.rs crates/fossilsense/src/server/language_server.rs
git commit -m "feat: rank completions with intent evidence"
```

## Task 3：Multi-channel indexed recall quotas

**覆盖需求：** FR9, FR10, FR11, FR12, FR13, NFR2, NFR3, NFR4, NFR6, NFR7, NFR8

**文件：**
- 修改：`crates/fossilsense/src/query.rs`
- 修改：`crates/fossilsense/src/query/tests.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`
- 修改：`crates/fossilsense/src/completion.rs`

**接口：**
- 消费：existing `NameTable::search_ranked_scoped_pooled`, `CompletionScope`, memo prior pool.
- 产出：
  - `CompletionRecallQuotas { total_indexed, reachable, external, unknown, global }`
  - `CompletionRecallMetrics { reachable, external, unknown, global, pool_total, indexed_returned }`
  - `NameTable::search_completion_recall_pooled(query, quotas, scope, prior_pool) -> (Vec<RankedNameHit>, Vec<usize>, CompletionRecallMetrics)`
  - `CompletionPipelineMetrics.recall_channels`

- [ ] **步骤 1：写 RED query test：channel recall keeps representation**

在 `crates/fossilsense/src/query/tests.rs` 中添加：

```rust
#[test]
fn channel_recall_keeps_reachable_and_global_representation() {
    let table = NameTable::build_with_paths(vec![
        (1, "api_reachable".to_string(), false, "inc/a.h".to_string(), "function".to_string(), false),
        (2, "api_global".to_string(), false, "other/b.c".to_string(), "function".to_string(), false),
    ]);
    let scope = scope("src/main.c", &["src/main.c", "inc/a.h"], false);
    let quotas = CompletionRecallQuotas {
        total_indexed: 4,
        reachable: 2,
        external: 1,
        unknown: 1,
        global: 2,
    };

    let (hits, pool, metrics) =
        table.search_completion_recall_pooled("api", quotas, Some(&scope), None);

    assert!(hits.iter().any(|hit| hit.name == "api_reachable"));
    assert!(hits.iter().any(|hit| hit.name == "api_global"));
    assert_eq!(metrics.reachable, 1);
    assert_eq!(metrics.global, 1);
    assert!(!pool.is_empty());
}
```

- [ ] **步骤 2：写 RED query test：short-prefix gates still apply**

```rust
#[test]
fn channel_recall_preserves_short_prefix_noise_gate() {
    let table = NameTable::build(vec![
        (1, "FooBar".to_string(), false),
        (2, "Foobar".to_string(), false),
    ]);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);

    let (hits, _, _) = table.search_completion_recall_pooled("ba", quotas, None, None);
    let names: Vec<_> = hits.iter().map(|hit| hit.name.as_str()).collect();

    assert!(names.contains(&"FooBar"));
    assert!(!names.contains(&"Foobar"));
}
```

- [ ] **步骤 3：写 RED query test：narrowed channel recall matches cold scan**

```rust
#[test]
fn channel_recall_narrowing_matches_cold_scan() {
    let table = NameTable::build(vec![
        (1, "foobar".to_string(), false),
        (2, "foobaz".to_string(), false),
        (3, "foxtrot".to_string(), false),
    ]);
    let quotas = CompletionRecallQuotas::default_for_completion_limit(100);
    let (_, pool) = table.search_ranked_scoped_pooled("fo", 100, None, None);

    let narrowed = table.search_completion_recall_pooled("foob", quotas, None, Some(&pool)).0;
    let cold = table.search_completion_recall_pooled("foob", quotas, None, None).0;

    assert_eq!(narrowed, cold);
}
```

- [ ] **步骤 4：运行 RED**

运行：`cargo test -p fossilsense query::tests::channel_recall -- --nocapture`

预期：new quota structs and channel recall method are missing.

- [ ] **步骤 5：实现 `NameTable` channel recall**

Implementation notes:

- Reuse `consider` and `score_match` so short-prefix gate and pool semantics remain identical.
- Build the tier-agnostic `pool` from every `score_match` candidate before short-prefix filtering, same as current pooled search.
- Build scored candidates that pass `min_score`.
- Partition scored candidates by `ScopeTier`:
  - `Reachable` and `Current` count toward reachable/current trusted channel.
  - `External` toward external.
  - `Unknown` toward unknown/open-scope.
  - `Global` toward global.
- Take per-channel quota after sorting by existing strict score inside channel.
- Fill remaining `total_indexed` with best not-yet-selected candidates to avoid hiding exact/prefix matches from a dense channel.
- Dedup by entry index.
- Keep existing `search_ranked_scoped_pooled` unchanged for compatibility.

- [ ] **步骤 6：wire server recall**

In `server/language_server.rs`:

- Use `CompletionRecallQuotas::default_for_completion_limit(limit)`.
- Replace indexed recall call with `table.search_completion_recall_pooled`.
- Keep `new_pools` as `Vec<Vec<usize>>`; the pool remains tier-agnostic and memo-compatible.
- Add recall metrics into `CompletionPipelineMetrics`.
- Keep local bindings, current-file overlay, and local words outside indexed channel quotas.

- [ ] **步骤 7：update perf summary**

In `completion.rs`, add source-safe fields:

```text
recall_reachable=...
recall_external=...
recall_unknown=...
recall_global=...
recall_pool=...
```

Do not print candidate names or paths.

- [ ] **步骤 8：运行 GREEN**

运行：

```bash
cargo test -p fossilsense query::tests -- --nocapture
cargo test -p fossilsense completion::tests -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
```

预期：channel recall, summary, and server tests pass; short-prefix tests keep passing.

- [ ] **步骤 9：提交建议**

```bash
git add crates/fossilsense/src/query.rs crates/fossilsense/src/query/tests.rs crates/fossilsense/src/server/language_server.rs crates/fossilsense/src/completion.rs
git commit -m "feat: add multi-channel completion recall"
```

## Task 4：Include completion ranking evidence

**覆盖需求：** FR14, FR15, FR16, FR17, NFR2, NFR3, NFR4, NFR5, NFR6, NFR7

**文件：**
- 修改：`crates/fossilsense/src/server/include_completion.rs`
- 修改：`crates/fossilsense/src/server.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`
- 修改：`crates/fossilsense/src/server/indexing/cache.rs`
- 修改：`crates/fossilsense/src/server/tests.rs`

**接口：**
- 消费：workspace file paths, resolved include edge paths, current document text, include form/partial/current dir.
- 产出：
  - `IncludeCompletionTable::build_with_edges(workspace_paths, include_edges)`
  - `CurrentIncludeEvidence { recent_targets, sibling_dirs, basename_counts }`
  - source-safe include ranking metrics returned from collection.
  - `collect_include_candidates_with_table` keeps existing behavior through a compatibility wrapper.

- [ ] **步骤 1：写 RED include ranking tests**

在 `crates/fossilsense/src/server/include_completion.rs` tests 中添加：

```rust
#[test]
fn quote_include_prefers_same_directory_and_sibling_patterns() {
    let table = IncludeCompletionTable::build_with_edges(
        vec![
            "src/driver/main.c".to_string(),
            "src/driver/main.h".to_string(),
            "src/driver/config.h".to_string(),
            "vendor/config.h".to_string(),
        ],
        vec![("src/driver/main.c".to_string(), "src/driver/config.h".to_string())],
    );
    let evidence = CurrentIncludeEvidence::from_text(
        "#include \"config.h\"\n",
        Some("src/driver/main.c"),
    );

    let items = collect_include_candidates_ranked_for_test(
        IncludeForm::Quote,
        "",
        "con",
        Some("src/driver"),
        Some(&table),
        Some(&evidence),
        20,
    );

    assert_eq!(items[0].label, "config.h");
}
```

Add tests for:

```rust
#[test]
fn basename_frequency_breaks_workspace_ties_without_overriding_form_priority() { /* common basename rises within workspace bucket */ }

#[test]
fn path_depth_penalty_prefers_shallow_comparable_headers() { /* api.h before deep/internal/api.h when other evidence equal */ }

#[test]
fn angle_include_keeps_external_root_base_priority() { /* includeRoots candidate remains above comparable workspace fallback */ }
```

- [ ] **步骤 2：写 RED cache rebuild test**

In `crates/fossilsense/src/server/tests.rs`, update or add a cache test proving `rebuild_include_table` calls edge-aware build:

```rust
#[tokio::test]
async fn include_table_rebuild_carries_include_edges_for_ranking() {
    let root = tempdir().expect("root");
    std::fs::write(root.path().join("a.c"), "#include \"b.h\"\n").expect("a");
    std::fs::write(root.path().join("b.h"), "int b;\n").expect("b");

    // Index then rebuild table; table debug/test accessor reports edge count.
    // Existing stale-cache clearing behavior must still pass on failure.
}
```

- [ ] **步骤 3：运行 RED**

运行：

```bash
cargo test -p fossilsense server::include_completion::tests -- --nocapture
cargo test -p fossilsense server::tests::include_table -- --nocapture
```

预期：edge-aware table, current include evidence, and ranked collection helper are missing.

- [ ] **步骤 4：implement include ranking data**

Implementation notes:

- Keep `IncludeCompletionTable::build(workspace_paths)` as a compatibility constructor delegating to `build_with_edges(workspace_paths, Vec::new())`.
- Store sorted/deduped `workspace_paths`.
- Add lightweight maps:
  - `basename_counts: HashMap<String, usize>`.
  - `incoming_by_src_dir: HashMap<String, HashSet<String>>` from include edges.
  - optional `paths_by_label: HashMap<String, Vec<String>>` for workspace tie evidence.
- Add `CurrentIncludeEvidence::from_text(text, current_rel_path)` using `includes::parse_include_line` per line. It stores normalized include targets and basenames only.
- Replace tuple scoring with an internal `ScoredIncludeCandidate` carrying label, kind, score, source bucket, optional workspace path.
- Scoring keeps existing base scores:
  - quote: current dir 300, workspace 250, external 200.
  - angle: external 300, workspace 250, current dir 200.
- Add bounded secondary boosts:
  - same directory: +35.
  - recent include basename/target: +30.
  - sibling/component edge evidence: +25.
  - basename frequency: +1..+20.
  - shallow path depth: subtract up to 20 for deep paths.
- Dedup by label lower-case as existing code does.

- [ ] **步骤 5：wire current text and edge-aware rebuild**

In `language_server.rs`, when include context is detected, call:

```rust
return self.complete_include(&uri, form, partial, &text).await;
```

In `server.rs`, update `complete_include` signature to accept current text and build `CurrentIncludeEvidence`.

In `server/indexing/cache.rs`, change rebuild to:

```rust
let paths = store.workspace_file_paths()?;
let edges = store.load_include_edge_paths()?;
Ok(IncludeCompletionTable::build_with_edges(paths, edges))
```

Keep failure behavior: stale include table is removed when rebuild fails.

- [ ] **步骤 6：add source-safe include summary**

Extend include perf log:

```text
[perf] include_completion total=... workspace_table=... workspace_index=... recent=... sibling=... basename=... depth_penalty=...
```

Counts only; no candidate labels or paths.

- [ ] **步骤 7：运行 GREEN**

运行：

```bash
cargo test -p fossilsense server::include_completion::tests -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
```

预期：include ranking tests pass; existing include path completion, jump-to-include, and stale-cache tests pass.

- [ ] **步骤 8：提交建议**

```bash
git add crates/fossilsense/src/server/include_completion.rs crates/fossilsense/src/server.rs crates/fossilsense/src/server/language_server.rs crates/fossilsense/src/server/indexing/cache.rs crates/fossilsense/src/server/tests.rs
git commit -m "feat: rank include completions with project evidence"
```

## Task 5：Documentation and requirement matrix sync

**覆盖需求：** FR16, FR18, NFR1, NFR5, NFR8, NFR9

**文件：**
- 修改：`CLAUDE.md`
- 修改：`README.md`
- 修改：`extensions/vscode/README.md`
- 修改：`docs/smart-completion-phase4-6/requirements.md`
- 修改：`docs/smart-completion-phase4-6/plans/2026-07-05--implementation-plan.md`

**接口：**
- 消费：Task 1-4 implemented behavior and verification results.
- 产出：docs accurately describe Phase 4-6 capabilities, limitations, source-safe logs, and excluded capabilities.

- [ ] **步骤 1：update docs wording**

Required wording:

- Ordinary identifier completion now has lightweight rule-based intent ranking for type, expression, call, macro preprocessor, and declaration-name contexts.
- Intent is evidence only; it does not do C++ type inference or binding.
- Multi-channel recall keeps bounded representation from current/local/reachable/external/unknown/global/text channels before reranking.
- Include path completion uses same-directory, sibling/component, recent include, basename frequency, and path depth evidence while retaining quote/angle search-order prior.
- Member method completion, local history, telemetry, ML, and auto include insertion are still excluded.
- Default perf/debug logs remain source-safe and do not print raw candidate names, include paths, or source snippets.

- [ ] **步骤 2：update requirements metadata and matrix**

In `docs/smart-completion-phase4-6/requirements.md`:

- Set `Status: approved-planned` when plan is saved.
- Keep each FR/NFR row mapped to Task 1-6 and a concrete command/test file.
- Keep rows `已计划` until implementation verification changes them.
- Add the final requirements approval record from 2026-07-05.

- [ ] **步骤 3：run docs consistency checks**

运行：

```bash
rg -n "Phase 4-6|intent|multi-channel|include recent|sibling|member method|ML|telemetry" README.md CLAUDE.md extensions/vscode/README.md docs/smart-completion-phase4-6/requirements.md
rg -n -e ('TO' + 'DO') -e ('TB' + 'D') -e ('待' + '确认') -e ('开放' + '问题') -e ('后续' + '再定') -e ('PLACE' + 'HOLDER') docs/smart-completion-phase4-6
```

预期：第一条命令 shows consistent Phase 4-6 capability/non-goal wording; second command returns no matches.

- [ ] **步骤 4：提交建议**

```bash
git add CLAUDE.md README.md extensions/vscode/README.md docs/smart-completion-phase4-6/requirements.md docs/smart-completion-phase4-6/plans/2026-07-05--implementation-plan.md
git commit -m "docs: describe smart completion phase 4-6"
```

## Task 6：Full verification and package smoke

**覆盖需求：** UR1-UR11, FR1-FR18, NFR1-NFR9

**文件：**
- 修改：`docs/smart-completion-phase4-6/plans/2026-07-05--implementation-plan.md`
- 修改：only if verification finds an implementation defect.

**接口：**
- 消费：Task 1-5 completed changes.
- 产出：fresh verification evidence and installable VSIX smoke artifact if package succeeds.

- [ ] **步骤 1：run targeted Rust tests**

```bash
cargo test -p fossilsense completion::tests -- --nocapture
cargo test -p fossilsense query::tests -- --nocapture
cargo test -p fossilsense server::include_completion::tests -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
```

预期：targeted tests pass.

- [ ] **步骤 2：run full Rust tests**

```bash
cargo test -p fossilsense
```

预期：all Rust tests pass.

- [ ] **步骤 3：run mini-c index smoke**

```bash
cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-phase4-6-mini.sqlite --force
```

预期：index command succeeds and reports files/symbols without failure.

- [ ] **步骤 4：run VS Code extension compile**

```bash
cd extensions/vscode
pnpm run compile
```

预期：TypeScript compile succeeds.

- [ ] **步骤 5：run VSIX package smoke**

```bash
cd extensions/vscode
pnpm run package
```

预期：package command succeeds and creates a `.vsix` under repository `dist/` with bundled `fossilsense.exe`.

- [ ] **步骤 6：record verification results**

Append an `Executed verification, 2026-07-05` section to this plan with command results, failed-command details if any, and generated VSIX path when package succeeds.

- [ ] **步骤 7：update requirements status after verification**

If all implementation and docs verification pass, update `docs/smart-completion-phase4-6/requirements.md`:

- `Status: implemented-and-verified`.
- Matrix status cells from `已计划` to `已验证`.
- Confirmation record for verification date and commands.

- [ ] **步骤 8：提交建议**

```bash
git add docs/smart-completion-phase4-6/requirements.md docs/smart-completion-phase4-6/plans/2026-07-05--implementation-plan.md
git commit -m "test: verify smart completion phase 4-6"
```

## Executed verification, 2026-07-05

- `cargo test -p fossilsense completion::tests -- --nocapture`: passed, 39 targeted tests passed.
- `cargo test -p fossilsense query::tests -- --nocapture`: passed, 31 targeted tests passed.
- `cargo test -p fossilsense server::include_completion::tests -- --nocapture`: passed, 15 targeted tests passed.
- `cargo test -p fossilsense server::tests -- --nocapture`: passed, 34 targeted tests passed.
- `cargo test -p fossilsense`: passed, 448 unit tests and 2 LSP smoke tests passed.
- `cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-phase4-6-mini.sqlite --force`: passed, indexed 2 files and 13 symbols.
- `pnpm run compile` in `extensions/vscode`: passed.
- `pnpm run package` in `extensions/vscode`: passed, generated `dist/fossilsense-vscode-1.2.0_BUILD20260705_152610.vsix`.

No failed verification commands in this run. The generated VSIX includes the bundled `extension/bin/fossilsense.exe`.

## Post-review fixes and verification, 2026-07-05

Review feedback addressed:

- Include completion secondary evidence is now capped so same-directory/recent/sibling/basename boosts cannot cross the quote/angle source bucket gap; added production-path tests for angle external and quote current-dir priority.
- Declaration-name intent now handles pointer/reference declarators such as `FsWidget *fs_`, `const FsWidget *fs_`, and `FsWidget &fs_`.
- Include perf metrics now report `same_directory` counts in addition to recent/sibling/basename/depth penalty counts.
- `git diff --check` was run after the fixes and passed.

Executed verification after review:

- `cargo test -p fossilsense completion::tests -- --nocapture`: passed, 42 targeted tests passed.
- `cargo test -p fossilsense server::include_completion::tests -- --nocapture`: passed, 17 targeted tests passed.
- `cargo test -p fossilsense server::tests -- --nocapture`: passed, 34 targeted tests passed.
- `cargo test -p fossilsense`: passed, 451 unit tests and 2 LSP smoke tests passed.
- `cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-phase4-6-mini.sqlite --force`: passed, indexed 2 files and 13 symbols.
- `pnpm run compile` in `extensions/vscode`: passed.
- `pnpm run package` in `extensions/vscode`: passed, generated `dist/fossilsense-vscode-1.2.0_BUILD20260705_153916.vsix`.

No failed verification commands in the post-review run. The generated VSIX includes the bundled `extension/bin/fossilsense.exe`.
