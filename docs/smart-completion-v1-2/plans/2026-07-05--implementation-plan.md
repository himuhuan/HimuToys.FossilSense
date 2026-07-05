# Smart Completion v1.2 Phase 0-1 实现计划

Status: implemented and verified

> **给代理执行者：** 使用 `himupowers:executing-plans` 在当前会话执行；每个生产代码任务必须使用 `himupowers:test-driven-development`。
> 需求文档：`docs/smart-completion-v1-2/requirements.md`

**目标：** 交付 v1.2.0 的 smart completion Phase 0-1：版本事实、补全观测基线、兼容 completion pipeline 抽取、文档同步与验证。
**架构：** 新增 Rust `completion` 核心模块，server 仍负责 LSP 上下文和渲染，普通补全候选进入兼容 pipeline 做 dedup/rank/truncate 和指标统计。Phase 1 不改变现有 strict resolver-packed 排序，只提供后续 Phase 2 ranker 的插入点。
**技术栈：** Rust 2021, tower-lsp, existing `NameTable`, existing `resolver`, cargo tests, VS Code extension pnpm compile.

## 全局约束

- 不依赖 clangd、ctags、compile commands、外部构建系统或编译参数。
- Phase 0-1 不启用 soft scope prior、intent classifier、ML ranker、history personalization、member method schema 或 include ranking enhancement。
- 普通补全返回继续保持 `CompletionList.isIncomplete = true`。
- Include-path completion 和 `.` / `->` member completion 继续在 ordinary pipeline 之前短路。
- 不新增 SQLite schema migration，不新增每键 workspace scan 或 broad disk IO。
- Perf/debug summary 默认只在已有 perf logging gate 打开时输出，并且只输出 counts/timings，不输出 candidate names 或 source snippets。
- 新 completion core 不创建绕开 `ScopeTier` / `ResolutionConfidence` / resolver packed score 的平行语义系统。

## 文件结构

- 新建 `crates/fossilsense/src/completion.rs`
  - 职责：协议无关候选源、evidence、兼容 ranker、source metrics、timing summary、shadow rank comparison。
  - 输入：server 已召回并渲染好的 candidate payload，带 existing score/tier/confidence/source。
  - 输出：兼容排序后的 candidate payload、source/count metrics、shadow summary。
- 修改 `crates/fossilsense/src/server.rs`
  - 职责：接入 `completion` 模块类型，保留 LSP `CompletionItem` 渲染 helper。
- 修改 `crates/fossilsense/src/server/language_server.rs`
  - 职责：把 ordinary completion 候选交给 completion pipeline，记录分阶段 timing 和 source metrics。
- 修改 `crates/fossilsense/src/server/tests.rs`
  - 职责：更新测试引用，保留 existing local binding / dedup behavior 覆盖。
- 修改 `crates/fossilsense/src/main.rs`
  - 职责：声明 `completion` 模块。
- 修改 `crates/fossilsense/Cargo.toml`, `extensions/vscode/package.json`, `README.md`, `CLAUDE.md`, `extensions/vscode/README.md`
  - 职责：版本与文档同步。

## Task 1：Version and requirements record

**覆盖需求：** FR1, FR10, NFR6

**文件：**
- 修改：`crates/fossilsense/Cargo.toml`
- 修改：`extensions/vscode/package.json`
- 修改：`README.md`
- 新建：`docs/smart-completion-v1-2/requirements.md`
- 新建：`docs/smart-completion-v1-2/plans/2026-07-05--implementation-plan.md`

**接口：**
- 产出：version facts are `1.2.0`; requirements matrix points to Tasks 1-5.

- [ ] 修改版本事实并运行 `rg -n "1\\.2\\.0|1\\.1\\.1" README.md crates/fossilsense/Cargo.toml extensions/vscode/package.json`。
- [ ] 确认 `1.1.1` 不再出现在当前版本事实中。

## Task 2：Completion core RED/GREEN

**覆盖需求：** FR2, FR3, FR4, FR5, FR6, NFR2, NFR3

**文件：**
- 新建：`crates/fossilsense/src/completion.rs`
- 修改：`crates/fossilsense/src/main.rs`

**接口：**
- 产出：`CandidateSource`, `CandidateEvidence`, `PipelineCandidate<T>`, `run_compatible_pipeline`, `CompletionPipelineMetrics`, `CompletionStageTimings`, `ShadowRankSummary`, `completion_perf_summary`.

- [ ] 写失败测试：compatible pipeline preserves source priority and score/name order.
- [ ] 写失败测试：pipeline metrics count sources before and after dedup.
- [ ] 写失败测试：shadow comparison reports rank movement.
- [ ] 写失败测试：completion perf summary omits candidate names.
- [ ] 运行 `cargo test -p fossilsense completion::tests -- --nocapture` 并确认失败。
- [ ] 写最小实现。
- [ ] 运行同一测试并确认通过。

## Task 3：Server integration

**覆盖需求：** FR3, FR4, FR6, FR7, FR8, FR9, NFR4, NFR5

**文件：**
- 修改：`crates/fossilsense/src/server.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`
- 修改：`crates/fossilsense/src/server/tests.rs`

**接口：**
- 消费：Task 2 的 completion pipeline。
- 产出：ordinary completion uses compatible pipeline; perf log includes structured summary.

- [ ] 更新 existing server tests to use completion module types.
- [ ] 运行 targeted server tests and confirm failures before integration.
- [ ] Replace server-local dedup/rank with `completion::run_compatible_pipeline`.
- [ ] Add context/recall/merge-rank/render timing fields around the existing ordinary completion path.
- [ ] Log `completion_perf_summary(...)` through existing `perf_log`.
- [ ] 运行 `cargo test -p fossilsense server::tests -- --nocapture`。

## Task 4：Documentation sync

**覆盖需求：** FR10, NFR1, NFR6

**文件：**
- 修改：`README.md`
- 修改：`CLAUDE.md`
- 修改：`extensions/vscode/README.md`

**接口：**
- 产出：用户可见 can/cannot/fallback 文档描述 Phase 0-1 groundwork.

- [ ] Add v1.2.0 smart-completion Phase 0-1 wording.
- [ ] State ranking remains compatibility-mode in this release slice.
- [ ] Run `rg -n "Phase 0-1|v1.2.0|smart completion" README.md extensions/vscode/README.md CLAUDE.md`.

## Task 5：Verification

**覆盖需求：** UR1-UR6, FR1-FR10, NFR1-NFR6

**文件：**
- 修改：only if verification finds defects.

**接口：**
- 产出：fresh verification evidence.

- [ ] Run `cargo test -p fossilsense`.
- [ ] Run `cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-v1-2-mini.sqlite --force`.
- [ ] Run `cd extensions/vscode && pnpm run compile`.
- [ ] If this becomes an external release artifact, additionally run `cd extensions/vscode && pnpm run package`.

Executed verification, 2026-07-05:

- `cargo test -p fossilsense`: passed, 414 unit tests and 2 LSP smoke tests.
- `cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-v1-2-mini.sqlite --force`: passed, 2 indexed files and 13 symbols.
- `cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-v1-2-mini.sqlite`: passed, 2 skipped unchanged files and 13 retained symbols.
- `cd extensions/vscode && pnpm run compile`: passed.
- `cd extensions/vscode && pnpm run package`: passed, produced `dist/fossilsense-vscode-1.2.0_BUILD20260705_112200.vsix`.
