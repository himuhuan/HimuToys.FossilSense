# v1.1 Rich Hover Comments 实现计划

Status: implemented and verified. The checkboxes below record the executed TDD/checkpoint sequence for v1.1.0.

> **给代理执行者：** 必须使用 `himupowers:subagent-driven-development`（推荐）或 `himupowers:executing-plans` 逐任务执行本计划。
> 需求文档：`docs/v1-1-rich-hover-comments/requirements.md`

**目标：** 发布 v1.1.0 的自洽阶段成果：Markdown hover、Doxygen/普通注释渲染、Grouped References 默认隐藏范围、版本与文档同步、可验证打包路径。
**架构：** Rust 侧新增 `query::hover` 纯逻辑和 `server::hover` LSP 入口；hover 复用 exact-name store lookup 与 resolver ranking。VS Code 侧抽出纯 `referencesView` label builder，让 UI 开关有可测边界。
**技术栈：** Rust 2021, tower-lsp, rusqlite, existing tree-sitter/parser facts, TypeScript, vscode-languageclient, pnpm scripts, cargo tests.

## 全局约束

- 不依赖 clangd、ctags、compile commands、外部构建系统或编译参数。
- Hover 返回 ranked candidate，不是语义绑定；必须继续显示 tier/confidence/reason。
- 新能力复用 `DefinitionCandidate`, `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, `ReachScope` 和共享 resolver。
- v1.1.0 不做 SQLite schema migration；注释在 hover 请求期从当前文本或候选文件读取。
- 请求期不扫描 workspace；只做 exact-name DB lookup 与 capped candidate source reads。
- Comment parsing failure must not suppress signature hover.
- Grouped References 默认隐藏 `:line` / range suffix；`fossilsense.references.showRanges = true` 才显示。
- 文档必须写清 can / cannot / fallback。

## 文件结构

- 新建 `crates/fossilsense/src/query/hover.rs`
  - 职责：hover candidate ranking、leading comment extraction、comment cleanup、Markdown assembly。
  - 输入：`Vec<SymbolRecord>`、current relative path、optional `ReachScope`、source text、candidate start line。
  - 输出：`RankedHoverCandidate`、Markdown string、optional cleaned comment markdown。
  - 错误策略：空 vec / `None` / bounded plain text fallback。
- 修改 `crates/fossilsense/src/query.rs`
  - 职责：declare/export hover helpers and limits.
- 新建 `crates/fossilsense/src/server/hover.rs`
  - 职责：LSP hover orchestration, source text lookup, `Hover` response assembly.
- 修改 `crates/fossilsense/src/server.rs`
  - 职责：import hover LSP types and declare module.
- 修改 `crates/fossilsense/src/server/language_server.rs`
  - 职责：advertise hover provider and forward `LanguageServer::hover`.
- 新建 `extensions/vscode/src/referencesView.ts`
  - 职责：pure grouped-reference QuickPick row model, including range-label switch.
- 修改 `extensions/vscode/src/extension.ts`
  - 职责：use `referencesView`, read `fossilsense.references.showRanges`.
- 新建 `extensions/vscode/src/test/referencesView.test.ts`
  - 职责：verify default hidden range and opt-in visible range.
- 修改 `extensions/vscode/src/config.ts` and `extensions/vscode/src/test/config.test.ts`
  - 职责：normalization helper for boolean-ish settings if needed.
- 修改 `extensions/vscode/package.json`
  - 职责：version v1.1.0, hover description, new setting, test script.
- 修改 `extensions/vscode/pnpm-lock.yaml` if package metadata update changes lockfile.
- 修改 `crates/fossilsense/Cargo.toml`, `README.md`, `extensions/vscode/README.md`
  - 职责：v1.1.0 facts and hover/settings documentation.
- 新建 `docs/v1-1-rich-hover-comments/report.md`
  - 职责：final implementation report.

### Task 1：Hover candidate ranking pure helper

**覆盖需求：** FR3, FR8, NFR2, NFR3, NFR5

**文件：**
- 新建：`crates/fossilsense/src/query/hover.rs`
- 修改：`crates/fossilsense/src/query.rs`
- 测试：`crates/fossilsense/src/query/hover.rs`

**接口：**
- 消费：`rank_definitions_into_candidates_with_scope(records, current_rel_path, scope)`
- 消费：`store::SymbolRecord`
- 产出：`RankedHoverCandidate { candidate: DefinitionCandidate, signature: String, guard: Option<String> }`
- 产出：`rank_hover_candidates(records, current_rel_path, scope, limit) -> Vec<RankedHoverCandidate>`
- 产出：`HOVER_CANDIDATE_LIMIT: usize = 4`

- [x] **步骤 1：写失败测试**

Add tests for preserving signatures across all symbol kinds, reachability ordering, and cap-after-ranking.

- [x] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense query::hover -- --nocapture`

预期：编译失败或新增 tests 失败 because `query::hover` APIs do not exist.

- [x] **步骤 3：写最小实现**

Implement `rank_hover_candidates` by filtering out no symbol kind, mapping record identity to signature/guard, delegating ranking to `rank_definitions_into_candidates_with_scope`, and taking the cap after ranking.

- [x] **步骤 4：运行测试并确认通过**

运行：`cargo test -p fossilsense query::hover -- --nocapture`

预期：hover candidate tests pass.

### Task 2：Leading comments and Markdown rendering

**覆盖需求：** FR4, FR5, FR6, FR7, FR8, FR9, NFR2, NFR5

**文件：**
- 修改：`crates/fossilsense/src/query/hover.rs`
- 测试：`crates/fossilsense/src/query/hover.rs`

**接口：**
- 消费：`RankedHoverCandidate`
- 产出：`leading_comment_markdown(source, symbol_start_line) -> Option<String>`
- 产出：`hover_markdown_for_candidate(candidate, comment_markdown) -> String`

- [x] **步骤 1：写失败测试**

Cover:
- `/// @brief` + `@param` + `@return`.
- Ordinary contiguous `//` comments.
- Block comments with decorative leading `*`.
- Malformed or unknown Doxygen commands degrading to readable prose.
- Comment cap preserving signature rendering.
- Markdown includes no raw candidate range.
- Review follow-up: blank lines and inline trailing block comments do not attach to the next symbol; `@param[in]` / `@param[out]` render as parameters.

- [x] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense query::hover -- --nocapture`

预期：new comment/markdown tests fail because helpers are absent.

- [x] **步骤 3：写最小实现**

Implement bounded upward scan from `symbol_start_line`, delimiter stripping, Doxygen command normalization, Markdown escaping where needed, signature fenced code blocks, and tier/confidence/reason evidence. Treat blank/code lines as attachment boundaries outside a block comment.

- [x] **步骤 4：运行测试并确认通过**

运行：`cargo test -p fossilsense query::hover -- --nocapture`

预期：all query hover tests pass.

### Task 3：LSP hover provider integration

**覆盖需求：** FR1, FR2, FR3, FR4, FR7, FR8, FR9, NFR1, NFR3, NFR4, NFR6

**文件：**
- 新建：`crates/fossilsense/src/server/hover.rs`
- 修改：`crates/fossilsense/src/server.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`
- 测试：`crates/fossilsense/src/server/hover.rs`

**接口：**
- 消费：`query::word_at`, `query::rank_hover_candidates`, `query::leading_comment_markdown`, `query::hover_markdown_for_candidate`
- 消费：`Backend::document_snapshot`, `root_for_uri`, `reach_scope_for`, `unwrap_query`
- 产出：`Backend::provide_hover(params) -> LspResult<Option<Hover>>`

- [x] **步骤 1：写失败测试**

Add pure server helper tests proving Markdown hover contents produce `MarkupKind::Markdown`, empty candidate list returns `None`, candidate-source read failures still render the signature-only candidate, and oversized candidate source files fall back to signature-only hover.

- [x] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense server::hover -- --nocapture`

预期：compile/tests fail because server hover module is absent.

- [x] **步骤 3：写最小实现**

Advertise `hover_provider`, implement `LanguageServer::hover`, query exact-name candidates in `spawn_blocking`, resolve source text for each capped candidate with a per-file byte limit, and return `Hover { contents: HoverContents::Markup(...), range: None }`.

- [x] **步骤 4：运行测试并确认通过**

运行：

```bash
cargo test -p fossilsense query::hover -- --nocapture
cargo test -p fossilsense server::hover -- --nocapture
```

预期：hover unit tests pass.

### Task 4：Grouped References range-label switch

**覆盖需求：** FR10, NFR4, NFR7

**文件：**
- 新建：`extensions/vscode/src/referencesView.ts`
- 新建：`extensions/vscode/src/test/referencesView.test.ts`
- 修改：`extensions/vscode/src/extension.ts`
- 修改：`extensions/vscode/package.json`

**接口：**
- 产出：`groupedReferencePickRows(items, showRanges, asRelativePath)`
- 产出 setting `fossilsense.references.showRanges`

- [x] **步骤 1：写失败测试**

Add tests asserting default labels omit `:line`, enabled labels include `:line`, and role separators remain.

- [x] **步骤 2：运行测试并确认失败**

运行：`cd extensions/vscode && node out/test/referencesView.test.js`

预期：compiled test missing or failing before implementation.

- [x] **步骤 3：写最小实现**

Create pure row builder, import it in `extension.ts`, read `fossilsense.references.showRanges`, update contributed configuration and package test script.

- [x] **步骤 4：运行测试并确认通过**

运行：

```bash
cd extensions/vscode
pnpm exec tsc -p ./
node out/test/referencesView.test.js
```

预期：compile and references view test pass.

### Task 5：Version and documentation sync

**覆盖需求：** FR11, FR12, NFR2, NFR8

**文件：**
- 修改：`crates/fossilsense/Cargo.toml`
- 修改：`extensions/vscode/package.json`
- 修改：`extensions/vscode/pnpm-lock.yaml` if package metadata changes
- 修改：`README.md`
- 修改：`extensions/vscode/README.md`
- 新建：`docs/v1-1-rich-hover-comments/report.md`

**接口：**
- 产出：v1.1.0 version facts and delivery report.

- [x] **步骤 1：写失败检查**

运行：`rg -n "1\\.0\\.[02]|hoverProvider|textDocument/hover|showRanges" README.md extensions/vscode/README.md extensions/vscode/package.json crates/fossilsense/Cargo.toml`

预期：old version facts and missing hover/settings docs are visible.

- [x] **步骤 2：修改版本与文档**

Set Rust crate and VS Code package version to `1.1.0`, update README capabilities, settings, limitations, package description, and write report with approach, subagent usage, tradeoffs, validation, and remaining scoped-out work.

- [x] **步骤 3：运行文档检查**

运行：`rg -n "1\\.0\\.[02]|no hover|no signature help" README.md extensions/vscode/README.md extensions/vscode/package.json crates/fossilsense/Cargo.toml`

预期：no stale version or negative hover/signature claims remain in updated current docs.

### Task 6：Integration verification and VSIX packaging

**覆盖需求：** FR1-FR12, NFR1-NFR8

**文件：**
- 修改：only if verification finds issues.

**接口：**
- 产出：verification evidence and VSIX when local pnpm policy permits.

- [x] **步骤 1：完整 Rust 验证**

运行：

```bash
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/v1-1-hover-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/v1-1-hover-mini.sqlite
```

- [x] **步骤 2：TypeScript 验证**

运行：

```bash
cd extensions/vscode
pnpm exec tsc -p ./
node out/test/config.test.js
node out/test/status.test.js
node out/test/serverPath.test.js
node out/test/conflicts.test.js
node out/test/referencesView.test.js
```

- [x] **步骤 3：Package 验证**

运行：`cd extensions/vscode && pnpm run package`

预期：`dist/fossilsense-vscode-1.1.0_BUILD<timestamp>.vsix` exists. If pnpm approve-builds policy blocks package scripts, record the exact blocker and the successful lower-level verification.

## 需求覆盖检查

- UR1 覆盖：Task 1, Task 2, Task 3, Task 5, Task 6。
- UR2 覆盖：Task 2, Task 3, Task 5。
- UR3 覆盖：Task 2, Task 3。
- UR4 覆盖：Task 1, Task 2, Task 3, Task 5。
- UR5 覆盖：Task 4, Task 5。
- UR6 覆盖：Task 5, Task 6。

## 最终验证命令

```bash
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/v1-1-hover-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/v1-1-hover-mini.sqlite
cd extensions/vscode
pnpm exec tsc -p ./
node out/test/config.test.js
node out/test/status.test.js
node out/test/serverPath.test.js
node out/test/conflicts.test.js
node out/test/referencesView.test.js
pnpm run package
```
