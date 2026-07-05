# Smart Completion Phase 2-3 实现计划

> **给代理执行者：** 必须使用 `himupowers:subagent-driven-development`（推荐）或 `himupowers:executing-plans` 逐任务执行本计划。每个生产代码任务必须使用 `himupowers:test-driven-development`。
> 需求文档：`docs/smart-completion-phase2-3/requirements.md`

**目标：** 交付 smart completion Phase 2-3：普通标识符补全启用 deterministic evidence-aware ranker，并扩展 current-file open-document overlay。
**架构：** 继续复用 Phase 0-1 的 `completion` 核心模块，server 负责 LSP 上下文与渲染，rank/merge/guard/metrics 进入协议无关层。当前文件 overlay 放在 `query` 层，输入 live parse 的 `FileSemanticIndex` 与当前文本，输出普通补全候选证据。
**技术栈：** Rust 2021, tower-lsp, tree-sitter parse product, existing `NameTable`, existing `resolver`, cargo unit/integration tests, VS Code extension pnpm package.

## 全局约束

- 不依赖 clangd、ctags、compile commands、外部构建系统、编译器调用或用户构建参数。
- Phase 2-3 只改变普通标识符补全；include-path completion 和 `.` / `->` member completion 继续提前短路。
- 不做 intent classifier、多通道召回、include ranking、member methods、weak receiver inference、local history、anonymous telemetry、ML、LLM 或 auto include insertion。
- 不新增 SQLite schema migration，不新增每键 workspace scan 或 SQLite query。
- 普通补全返回继续保持 `CompletionList.isIncomplete = true`。
- `ScopeTier`、`ResolutionConfidence`、`ResolutionReason` 仍是共享 vocabulary；不得创建平行 semantic tier。
- `resolver::pack_score` 保留给 strict-policy surfaces；普通补全最终排序由 completion-owned deterministic ranker 决定。
- Raw local word 是 fallback text evidence，不得渲染成 current semantic definition。
- Perf/debug summary 默认只输出 counts/timings/classes，不输出候选名、源码片段或用户代码内容。
- 新权重和 guard threshold 必须集中在 `completion` core，不散落到 LSP handler。
- 每个生产代码变更先写失败测试，确认 RED 后再实现。

## 文件结构

- 修改：`crates/fossilsense/src/completion.rs`
  - 职责：mergeable candidate evidence、source counts、evidence-aware ranker、guard-band summary、final rank metrics、source-safe perf summary。
  - 输入：`PipelineCandidate<T>`，携带 `CandidateEvidence`、name、payload、match score、tier/confidence/source evidence。
  - 输出：ranked/truncated `CompletionPipelineOutput<T>`，metrics 包含 source/evidence counts、shadow movement、guard counts。
- 新建：`crates/fossilsense/src/query/current_file_overlay.rs`
  - 职责：从 `FileSemanticIndex` 和当前文本提取当前文件结构化 overlay candidates 与 nearby raw usage evidence。
  - 输入：`&FileSemanticIndex`, `text`, cursor line/character, prefix, limit。
  - 输出：`CurrentFileOverlayCandidate { name, kind, detail, match_score, proximity_score, source_start_byte }`。
- 修改：`crates/fossilsense/src/query.rs`
  - 职责：声明并导出 `current_file_overlay`，让 server 不直接遍历 parser internals。
- 修改：`crates/fossilsense/src/server.rs`
  - 职责：把 local binding、overlay、indexed、local word candidates 渲染为 `CompletionCandidate`；提供 final rank sortText 重写 helper。
- 修改：`crates/fossilsense/src/server/language_server.rs`
  - 职责：收集 overlay candidates，调用 evidence-aware pipeline，写入按最终顺序稳定的 `sortText`，保留 include/member short-circuit 与 memo pool。
- 修改：`crates/fossilsense/src/server/tests.rs`
  - 职责：覆盖 ordinary completion server integration、same-name merge、overlay unsaved facts、surface short-circuit compatibility。
- 修改：`crates/fossilsense/src/query/tests.rs`
  - 职责：更新 strict ordinary-completion assertions，使 strict `pack_score` 仍测试 `NameTable` recall but not final ordinary completion rank。
- 修改：`README.md`, `CLAUDE.md`, `extensions/vscode/README.md`
  - 职责：同步 Phase 2-3 can/cannot/fallback/rank explanation。

## Task 1：Completion evidence merge and deterministic ranker

**覆盖需求：** FR1, FR2, FR3, FR5, FR10, NFR3, NFR4, NFR6, NFR7

**文件：**
- 修改：`crates/fossilsense/src/completion.rs`

**接口：**
- 消费：现有 `CandidateSource`, `CandidateEvidence`, `PipelineCandidate<T>`, `CompletionPipelineMetrics`, `compare_shadow_ranks`。
- 产出：
  - `CandidateSource::CurrentFileOverlay`
  - `EvidenceSources`
  - `CandidateEvidence { primary_source, sources, tier, confidence, score, match_score, locality_score, proximity_score }`
  - `run_evidence_aware_pipeline<T>(candidates, limit) -> CompletionPipelineOutput<T>`
  - `FinalRankSummary { guarded_low_trust: usize }`

- [ ] **步骤 1：写 RED 测试：same-name evidence merges instead of discarding provenance**

```rust
#[test]
fn evidence_pipeline_merges_same_name_sources() {
    let output = run_evidence_aware_pipeline(vec![
        candidate("Widget", CandidateSource::Indexed, ScopeTier::Reachable, 800, "indexed"),
        candidate("Widget", CandidateSource::CurrentFileOverlay, ScopeTier::Current, 1000, "overlay"),
        candidate("Widget", CandidateSource::LocalWord, ScopeTier::Global, 750, "word"),
    ], 10);

    assert_eq!(output.items.len(), 1);
    let evidence = &output.items[0].evidence;
    assert!(evidence.sources.indexed);
    assert!(evidence.sources.current_file_overlay);
    assert!(evidence.sources.local_word);
    assert_eq!(output.items[0].payload, "overlay");
}
```

- [ ] **步骤 2：写 RED 测试：soft scope prior with guard bands**

```rust
#[test]
fn ranker_keeps_reachable_prefix_above_plain_global_fuzzy() {
    let output = run_evidence_aware_pipeline(vec![
        candidate("reachable_api", CandidateSource::Indexed, ScopeTier::Reachable, 800, "reach"),
        candidate("api_text_tail", CandidateSource::LocalWord, ScopeTier::Global, 250, "text"),
    ], 10);

    assert_eq!(output.items[0].payload, "reach");
    assert_eq!(output.metrics.final_rank.guarded_low_trust, 1);
}
```

- [ ] **步骤 3：写 RED 测试：strong current overlay can rise above weak reachable candidate**

```rust
#[test]
fn current_overlay_exact_can_outrank_reachable_weak_match() {
    let output = run_evidence_aware_pipeline(vec![
        candidate("new_local_type", CandidateSource::CurrentFileOverlay, ScopeTier::Current, 1000, "overlay"),
        candidate("newLocalTypeFactory", CandidateSource::Indexed, ScopeTier::Reachable, 400, "reach"),
    ], 10);

    assert_eq!(output.items[0].payload, "overlay");
}
```

- [ ] **步骤 4：运行 RED**

运行：`cargo test -p fossilsense completion::tests -- --nocapture`

预期：新增测试失败，失败原因是 `CurrentFileOverlay`、mergeable evidence、`run_evidence_aware_pipeline` 或 `final_rank` 尚未实现。

- [ ] **步骤 5：实现最小 ranker**

实现要点：

```rust
const SOURCE_LOCAL_BINDING: i32 = 12_000;
const SOURCE_CURRENT_FILE_OVERLAY: i32 = 9_000;
const SOURCE_INDEXED: i32 = 5_000;
const SOURCE_LOCAL_WORD: i32 = 0;

const SCOPE_CURRENT: i32 = 6_000;
const SCOPE_REACHABLE: i32 = 4_800;
const SCOPE_EXTERNAL: i32 = 4_200;
const SCOPE_UNKNOWN: i32 = 3_200;
const SCOPE_GLOBAL: i32 = 2_400;

const LOW_TRUST_GLOBAL_TEXT_CAP_BELOW_REACHABLE: i32 = 8_000;
```

Final score uses source prior + scope prior + confidence prior + `match_score` + bounded `proximity_score`; guard bands cap pure `LocalWord/Global` candidates unless they also carry current-file overlay or local binding evidence. Tie-breakers are final score desc, best source priority desc, match score desc, shorter name, name asc.

- [ ] **步骤 6：运行 GREEN**

运行：`cargo test -p fossilsense completion::tests -- --nocapture`

预期：completion core tests pass with source-safe summary tests updated for `current_file_overlay` and `guarded_low_trust`.

- [ ] **步骤 7：提交建议**

```bash
git add crates/fossilsense/src/completion.rs
git commit -m "feat: add evidence-aware completion ranker"
```

## Task 2：Current-file overlay extraction

**覆盖需求：** FR4, FR6, FR7, FR8, NFR2, NFR4

**文件：**
- 新建：`crates/fossilsense/src/query/current_file_overlay.rs`
- 修改：`crates/fossilsense/src/query.rs`

**接口：**
- 消费：`parser::FileSemanticIndex`, `parser::SymbolKind`, `parser::SymbolRole`, `parser::RecordDef`, `parser::TypeAlias`, `query::completion_word_score`, `query::byte_offset_at`。
- 产出：`CurrentFileOverlayCandidate` 和 `current_file_overlay_candidates(index, text, line, character, prefix, limit)`。

- [ ] **步骤 1：写 RED 测试：overlay extracts unsaved macros, aliases, enum constants, functions, and records**

在 `crates/fossilsense/src/query/current_file_overlay.rs` 的 test module 中添加：

```rust
#[test]
fn overlay_extracts_structured_current_file_facts() {
    let text = "#define FS_MAGIC 1\n\
                typedef int FsAlias;\n\
                enum Color { FS_RED };\n\
                struct FsWidget { int id; };\n\
                int fs_do_work(void);\n\
                void f(void) { FS }\n";
    let parsed = crate::parser::parse(std::path::Path::new("a.c"), text);

    let hits = current_file_overlay_candidates(&parsed, text, 5, 22, "FS", 20);
    let names: Vec<_> = hits.iter().map(|hit| hit.name.as_str()).collect();

    assert!(names.contains(&"FS_MAGIC"));
    assert!(names.contains(&"FsAlias"));
    assert!(names.contains(&"FS_RED"));
    assert!(names.contains(&"FsWidget"));
    assert!(names.contains(&"fs_do_work"));
}
```

- [ ] **步骤 2：写 RED 测试：nearby usage is bounded fallback evidence**

```rust
#[test]
fn nearby_usage_scores_distance_and_frequency_without_semantic_kind() {
    let text = "void f(void) {\n    localThing();\n    localThing();\n    loc\n}\n";
    let parsed = crate::parser::parse(std::path::Path::new("a.c"), text);

    let hits = current_file_overlay_candidates(&parsed, text, 3, 7, "loc", 20);
    let hit = hits.iter().find(|hit| hit.name == "localThing").expect("nearby word");

    assert!(hit.proximity_score > 0);
    assert_eq!(hit.detail.as_deref(), Some("text"));
}
```

- [ ] **步骤 3：运行 RED**

运行：`cargo test -p fossilsense query::current_file_overlay::tests -- --nocapture`

预期：测试模块或 functions missing。

- [ ] **步骤 4：实现最小 overlay extractor**

实现要点：

```rust
pub struct CurrentFileOverlayCandidate {
    pub name: String,
    pub kind: parser::SymbolKind,
    pub detail: Option<String>,
    pub match_score: i32,
    pub proximity_score: i32,
    pub source_start_byte: usize,
    pub semantic: bool,
}
```

Semantic overlay facts come from:

- `index.symbols` with `role == SymbolRole::Definition` and kind `Macro`, `Type`, `EnumConstant`, `Function`, `GlobalVariable`.
- `index.aliases` using `alias`.
- `index.records` using `display_name`.

Nearby usage comes from `index.occurrences` before cursor and raw identifier counts from current text when AST facts are absent. It uses `completion_word_score(prefix, name, 0)` for gating and caps `proximity_score` to a small value below structured overlay source weight.

- [ ] **步骤 5：运行 GREEN**

运行：`cargo test -p fossilsense query::current_file_overlay::tests -- --nocapture`

预期：overlay unit tests pass.

- [ ] **步骤 6：提交建议**

```bash
git add crates/fossilsense/src/query.rs crates/fossilsense/src/query/current_file_overlay.rs
git commit -m "feat: add current-file completion overlay"
```

## Task 3：Server integration and LSP rendering

**覆盖需求：** FR4, FR5, FR8, FR9, FR10, FR11, NFR2, NFR5, NFR6

**文件：**
- 修改：`crates/fossilsense/src/server.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`
- 修改：`crates/fossilsense/src/server/tests.rs`
- 修改：`crates/fossilsense/src/query/tests.rs`

**接口：**
- 消费：Task 1 `run_evidence_aware_pipeline`, Task 2 `current_file_overlay_candidates`。
- 产出：ordinary completion includes structured overlay candidates, uses final rank order, and returns LSP items with sequential `sortText`.

- [ ] **步骤 1：写 RED server test：unsaved typedef and macro appear as structured completions**

在 `crates/fossilsense/src/server/tests.rs` 中添加：

```rust
#[tokio::test]
async fn ordinary_completion_uses_unsaved_current_file_overlay() {
    let src = "#define FS_MAGIC 1\n\
               typedef int FsAlias;\n\
               void f(void) { FS }\n";
    let dir = tempdir().expect("tempdir");
    let uri = Url::from_file_path(dir.path().join("a.c")).expect("file uri");
    let service = test_backend_service();
    service.inner().open_docs.lock().await.insert(uri.clone(), (1, src.to_string()));

    let response = service.inner().completion(completion_params(uri, 2, 20)).await
        .expect("completion request").expect("completion response");
    let items = completion_items(response);

    assert!(items.iter().any(|item| item.label == "FS_MAGIC" && item.detail.as_deref() == Some("current")));
    assert!(items.iter().any(|item| item.label == "FsAlias" && item.detail.as_deref() == Some("current")));
}
```

- [ ] **步骤 2：写 RED server/helper test：final rank rewrites sortText**

```rust
#[test]
fn final_rank_sort_text_matches_pipeline_order() {
    let mut items = vec![
        CompletionItem { label: "b".into(), ..Default::default() },
        CompletionItem { label: "a".into(), ..Default::default() },
    ];

    super::apply_final_completion_sort_text(&mut items);

    assert_eq!(items[0].sort_text.as_deref(), Some("00000000"));
    assert_eq!(items[1].sort_text.as_deref(), Some("00000001"));
}
```

- [ ] **步骤 3：运行 RED**

运行：`cargo test -p fossilsense server::tests -- --nocapture`

预期：new tests fail because overlay candidates are not wired and final sortText helper is absent.

- [ ] **步骤 4：实现 server integration**

Implementation notes:

- Add `completion_items_for_current_file_overlay(hits: Vec<query::CurrentFileOverlayCandidate>) -> Vec<CompletionCandidate>`.
- Use `CandidateSource::CurrentFileOverlay`, `ScopeTier::Current`, `ResolutionConfidence::Heuristic`, and `hit.match_score`.
- Render structured overlay with `CompletionItemKind` from parser kind; nearby raw usage remains `CompletionItemKind::TEXT` and detail `text`.
- Collect overlay immediately after `local_binding_hits` from the already parsed document.
- Replace `run_compatible_pipeline(candidates, limit)` with `run_evidence_aware_pipeline(candidates, limit)`.
- After collecting `items`, call `apply_final_completion_sort_text(&mut items)` so client sorting matches server rank.
- Keep exact-indexed local word path, include short-circuit, member short-circuit, memo generation, and `isIncomplete = true`.

- [ ] **步骤 5：update strict completion tests intentionally**

In `crates/fossilsense/src/query/tests.rs`, keep tests that assert `NameTable` strict packed ordering at recall level. Move ordinary final-rank expectations to `completion::tests` or `server::tests` so docs no longer claim strict final ranking for ordinary completion.

- [ ] **步骤 6：运行 GREEN**

运行：

```bash
cargo test -p fossilsense server::tests -- --nocapture
cargo test -p fossilsense query::tests -- --nocapture
```

预期：server and query tests pass; include/member tests remain unaffected.

- [ ] **步骤 7：提交建议**

```bash
git add crates/fossilsense/src/server.rs crates/fossilsense/src/server/language_server.rs crates/fossilsense/src/server/tests.rs crates/fossilsense/src/query/tests.rs
git commit -m "feat: enable evidence-aware ordinary completion"
```

## Task 4：Documentation sync

**覆盖需求：** FR12, NFR1, NFR8

**文件：**
- 修改：`CLAUDE.md`
- 修改：`README.md`
- 修改：`extensions/vscode/README.md`
- 修改：`docs/smart-completion-phase2-3/requirements.md`

**接口：**
- 消费：Task 1-3 implemented behavior.
- 产出：docs describe Phase 2-3 behavior and requirements matrix statuses are `已计划`.

- [ ] **步骤 1：写文档补丁**

Required wording changes:

- Replace Phase 0-1 compatibility-only statements with Phase 2-3 statements for ordinary identifier completion.
- State ordinary completion now uses deterministic evidence-aware ranking with soft scope prior and guard bands.
- State goto definition, coloring, workspace symbol, include completion, and member completion are not automatically migrated to soft ranking.
- State current-file open-document overlay covers macros, aliases, enum constants, functions, record/type definitions, and nearby raw usage.
- State excluded capabilities remain excluded: intent classifier, include ranking, member methods, history, ML, telemetry, auto include insertion.

- [ ] **步骤 2：更新需求矩阵**

In `docs/smart-completion-phase2-3/requirements.md`, ensure each FR/NFR row has concrete Task numbers and status `已计划`.

- [ ] **步骤 3：运行文档检查**

运行：

```bash
rg -n "Phase 2-3|soft scope prior|evidence-aware|strict resolver-packed|intent classifier|member methods" README.md CLAUDE.md extensions/vscode/README.md docs/smart-completion-phase2-3/requirements.md
rg -n -e ('TO' + 'DO') -e ('TB' + 'D') -e ('待' + '确认') -e ('开放' + '问题') -e ('后续' + '再定') -e ('待' + '定') -e ('PLACE' + 'HOLDER') docs/smart-completion-phase2-3
```

预期：第一条命令显示 Phase 2-3 文档语义一致；第二条命令无匹配。

- [ ] **步骤 4：提交建议**

```bash
git add README.md CLAUDE.md extensions/vscode/README.md docs/smart-completion-phase2-3/requirements.md docs/smart-completion-phase2-3/plans/2026-07-05--implementation-plan.md
git commit -m "docs: describe smart completion phase 2-3"
```

## Task 5：Verification and package smoke

**覆盖需求：** UR1-UR8, FR1-FR12, NFR1-NFR8

**文件：**
- 修改：only if verification finds an implementation defect.

**接口：**
- 消费：Task 1-4 completed changes.
- 产出：fresh verification evidence and installable VSIX smoke artifact.

- [ ] **步骤 1：运行 Rust targeted tests**

```bash
cargo test -p fossilsense completion::tests -- --nocapture
cargo test -p fossilsense query::current_file_overlay::tests -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
```

预期：all targeted tests pass.

- [ ] **步骤 2：运行 Rust full tests**

```bash
cargo test -p fossilsense
```

预期：unit tests and LSP smoke tests pass.

- [ ] **步骤 3：运行 mini-c index smoke**

```bash
cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-phase2-3-mini.sqlite --force
```

预期：index command succeeds and reports indexed files/symbols without failure.

- [ ] **步骤 4：运行 VS Code extension compile and package**

```bash
cd extensions/vscode
pnpm run compile
pnpm run package
```

预期：compile succeeds and package creates a `.vsix` under repository `dist/`.

- [ ] **步骤 5：记录验证结果**

Append an `Executed verification, 2026-07-05` section to this plan with command results, test counts when available, and generated VSIX path.

- [ ] **步骤 6：提交建议**

```bash
git add docs/smart-completion-phase2-3/plans/2026-07-05--implementation-plan.md
git commit -m "test: verify smart completion phase 2-3"
```

Executed verification, 2026-07-05:

- `cargo test -p fossilsense`: passed, 430 unit tests and 2 LSP smoke tests.
- `cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-phase2-3-mini.sqlite --force`: passed, indexed 2 files and 13 symbols.
- `cd extensions/vscode && pnpm run compile`: passed.
- `cd extensions/vscode && pnpm run package`: passed, produced `dist/fossilsense-vscode-1.2.0_BUILD20260705_125511.vsix` with bundled `extension/bin/fossilsense.exe`.
