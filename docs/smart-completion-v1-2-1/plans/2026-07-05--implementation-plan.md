# Smart Completion v1.2.1 Phase 7-8 实现计划

> **给代理执行者：** 推荐先使用 `himupowers:himu-smart-subagents-development` 判断执行策略，再选择 `himupowers:subagent-driven-development` 或 `himupowers:executing-plans` 逐任务执行本计划。
> **调度规则：** 准备委托 subagent 前必须完成智能调度判断；默认主代理执行，只有能说明并行收益、独立审查价值或机械验证价值时才委托。
> 需求文档：`docs/smart-completion-v1-2-1/requirements.md`

**目标：** 交付 v1.2.1 的 smart completion Phase 7-8：统一 member evidence、字段+方法成员补全、窄范围 weak receiver inference、本地 completion accept history、版本与发布文档同步。
**架构：** v1.2.1 允许启动时全量重建索引，因此采用 schema version bump 和干净的 member storage/query surface。成员补全继续在普通标识符补全前短路；local history 只作为 ordinary completion 的 bounded evidence，不改变 include/member surface routing。
**技术栈：** Rust 2021, tower-lsp, tree-sitter C/C++, rusqlite bundled SQLite, existing `completion` core, VS Code extension TypeScript, pnpm/vsce packaging, cargo unit/integration tests.

## 全局约束

- 版本事实必须从 `1.2.0` 升到 `1.2.1`，包括 Rust crate、VS Code extension、README、extension README 和发布包名。
- 不依赖 clangd、ctags、compile commands、编译器调用、宏展开、外部构建系统或用户构建参数。
- 可以接受 schema mismatch 后启动时全量重建索引；不为 v1.2.0 DB 数据做复杂兼容迁移。
- 不做完整 C++ 类型推断、继承、重载、模板、命名空间、using lookup、访问控制、虚调用、operator overload 或数据流 receiver typing。
- 不做 auto include insertion，不在补全接受后修改源码。
- 不做匿名 telemetry、remote analytics、cloud sync、ML ranker、LLM completion 或模型分发。
- Member candidates 是 owner-scoped best-effort evidence，不是 compiler-grade semantic binding。
- Weak receiver inference 只覆盖明确且可标置信的窄范围；歧义时降级或拒绝。
- Member fallback 必须 prefix-only、`prefix.len() >= 2`、受 `COMPLETION_LIMIT` 控制，并保持 incomplete。
- Member fields/methods 不得泄漏进 ordinary identifier completion，除非它们本来就是已有规则下合法的 top-level indexed symbol。
- Local history 只记录本地 positive accept evidence，默认 `auto`，可设 `on` / `off`，必须可清除。
- Local history 必须 workspace-local、bounded、可老化或有容量裁剪；默认日志不输出 accepted candidate raw label、源码片段或路径。
- 禁用 local history 后，ordinary completion 在相同输入下回到 v1.2.0 deterministic evidence-aware ranker 行为。
- Completion 热路径继续使用内存 name tables、live parse cache、local word cache 和 bounded history snapshot；不得每键扫描 workspace 或打开 symbol-index SQLite 查询。
- 每个生产代码任务先写失败测试，确认 RED 后再写最小实现。
- 发布收尾必须运行 `pnpm run package` 并确认 `dist/fossilsense-vscode-1.2.1_BUILD*.vsix` 自包含原生二进制。

## 文件结构

- 修改：`crates/fossilsense/Cargo.toml`
  - 职责：Rust engine 版本号。
  - 输入：release target `1.2.1`。
  - 输出：`env!("CARGO_PKG_VERSION")` 和 binary package version 为 `1.2.1`。
- 修改：`extensions/vscode/package.json`
  - 职责：VSIX 版本、completion history 设置、clear-history command 声明。
  - 输入：extension configuration schema and command contributions。
  - 输出：version `1.2.1`; `fossilsense.completionHistory.mode`; `FossilSense: Clear Completion History`。
- 修改：`extensions/vscode/src/extension.ts`
  - 职责：读取 completion history setting，发送 initializationOptions，注册 clear-history command，重启相关配置。
  - 输入：VS Code workspace configuration。
  - 输出：server initialization options and clear-history LSP command request。
- 新建：`crates/fossilsense/src/completion_history.rs`
  - 职责：local-only history path、event storage、snapshot lookup、bounded aging、clear。
  - 输入：workspace root, accepted completion command payload, current unix time。
  - 输出：`CompletionHistorySnapshot`, `HistoryEvidence`, source-safe metrics。
- 修改：`crates/fossilsense/src/completion.rs`
  - 职责：history evidence field, bounded boost, rank context, source-safe summary fields。
  - 输入：existing `PipelineCandidate`, `CompletionRankContext`, history snapshot。
  - 输出：ranked completion items with deterministic history-aware scoring when enabled。
- 修改：`crates/fossilsense/src/parser.rs`
  - 职责：replace field-only AST product with canonical member facts while preserving field behavior.
  - 输入：tree-sitter AST and `ParseFacts` mask。
  - 输出：`MemberDef` facts with `MemberKind`, `MemberConfidence`, owner record key, range, signature。
- 修改：`crates/fossilsense/src/parser/ast.rs`
  - 职责：collect fields, in-body C++ methods, and simple out-of-class method owner evidence。
  - 输入：AST nodes for class/struct body and qualified function declarators。
  - 输出：`Vec<MemberDef>` plus existing records/aliases/local declarations。
- 修改：`crates/fossilsense/src/parser/tests.rs`
  - 职责：member parser RED/GREEN tests。
- 修改：`crates/fossilsense/src/model.rs`
  - 职责：canonical `MemberCandidate` re-export/definition if shared outside parser/store。
  - 输入：store query rows and parser member facts。
  - 输出：best-effort member candidates with tier/confidence vocabulary。
- 修改：`crates/fossilsense/src/store/schema.rs`
  - 职责：schema version bump and `members` storage。
  - 输入：current schema version 8。
  - 输出：schema version 9 with `members` lookup indexes; old `fields` data dropped on mismatch。
- 修改：`crates/fossilsense/src/store/writes.rs`
  - 职责：write parser members into `members` with record id mapping。
  - 输入：`FileSemanticIndex.members`。
  - 输出：member rows tied to `record_defs` rows。
- 修改：`crates/fossilsense/src/store/queries.rs`
  - 职责：owner-scoped member queries and fallback member-name queries。
  - 输入：record ids, prefix, resolve context。
  - 输出：`Vec<MemberCandidate>` with kind, signature, tier, member confidence。
- 修改：`crates/fossilsense/src/store/tests/members.rs`
  - 职责：schema/model/store compatibility tests for fields and methods。
- 修改：`crates/fossilsense/src/store/tests/resilience_schema.rs`
  - 职责：schema rebuild/drop tests for version 9 member schema。
- 修改：`crates/fossilsense/src/query.rs`
  - 职责：weak receiver helper primitives and any member fallback match helpers that belong outside store。
  - 输入：receiver name, prefix, record candidates。
  - 输出：normalized receiver/name-correlation helper results。
- 修改：`crates/fossilsense/src/server/member_completion.rs`
  - 职责：field+method member completion rendering, weak receiver fallback, source-safe member metrics。
  - 输入：current document snapshot, receiver/prefix, roots, reach scope, store member queries。
  - 输出：LSP `CompletionResponse` with field/method/nested-type candidates。
- 修改：`crates/fossilsense/src/server/language_server.rs`
  - 职责：parse completion history mode, expose LSP commands, attach accept commands to ordinary completion items, handle history commands。
  - 输入：initialization options, completion item accept command args。
  - 输出：history events recorded/cleared and completion rank context with history snapshot。
- 修改：`crates/fossilsense/src/server/options.rs`
  - 职责：parse completion history mode and declare command constants if not placed in server module。
  - 输入：initialization options JSON。
  - 输出：`CompletionHistoryMode` and command identifiers。
- 修改：`crates/fossilsense/src/server.rs`
  - 职责：module wiring and Backend state fields for history mode/store。
  - 输入：constructed `Backend`。
  - 输出：shared state available to language server handlers。
- 修改：`crates/fossilsense/src/server/tests.rs`
  - 职责：member completion integration, ordinary completion non-leakage, history command/ranking server tests。
- 修改：`extensions/vscode/src/test/config.test.ts`
  - 职责：history mode configuration normalization tests。
- 新建或修改：`extensions/vscode/src/test/completionHistory.test.ts`
  - 职责：clear command and initialization option tests for extension-side history plumbing。
- 修改：`README.md`, `CLAUDE.md`, `extensions/vscode/README.md`
  - 职责：v1.2.1 can/cannot/fallback/privacy/rebuild wording。
- 修改：`docs/smart-completion-v1-2-1/requirements.md`
  - 职责：matrix status and validation commands。

## Task 1：Version and extension configuration surface

**覆盖需求：** FR1, FR15, NFR9

**文件：**
- 修改：`crates/fossilsense/Cargo.toml`
- 修改：`extensions/vscode/package.json`
- 修改：`extensions/vscode/src/extension.ts`
- 修改：`extensions/vscode/src/config.ts`
- 修改：`extensions/vscode/src/test/config.test.ts`
- 新建：`extensions/vscode/src/test/completionHistory.test.ts`

**接口：**
- 消费：current extension configuration helpers。
- 产出：
  - `fossilsense.completionHistory.mode`: `"auto" | "on" | "off"`, default `"auto"`。
  - command `fossilsense.clearCompletionHistory`。
  - LSP command constant `fossilsense.lsp.clearCompletionHistory`。
  - initialization option `fossilsense.completionHistory.mode`。

- [ ] **步骤 1：写 RED 测试：history mode config normalization**

在 `extensions/vscode/src/test/config.test.ts` 增加：

```ts
import { normalizeOnOffAuto } from '../config';

suite('completion history config', () => {
  test('normalizes completion history mode values', () => {
    assert.strictEqual(normalizeOnOffAuto('auto', 'auto'), 'auto');
    assert.strictEqual(normalizeOnOffAuto('on', 'auto'), 'on');
    assert.strictEqual(normalizeOnOffAuto('off', 'auto'), 'off');
    assert.strictEqual(normalizeOnOffAuto('weird', 'auto'), 'auto');
  });
});
```

- [ ] **步骤 2：写 RED 测试：extension sends completionHistory initialization option**

在 `extensions/vscode/src/test/completionHistory.test.ts` 新建测试，使用现有 test style 构造配置读取函数。目标断言：

```ts
assert.deepStrictEqual(completionHistoryInitializationOptions('auto'), {
  completionHistory: { mode: 'auto' },
});
```

如果当前 `extension.ts` 没有可测试 helper，先提取纯函数：

```ts
export function completionHistoryInitializationOptions(mode: string) {
  return { completionHistory: { mode: normalizeOnOffAuto(mode, 'auto') } };
}
```

- [ ] **步骤 3：运行 RED**

运行：

```bash
cd extensions/vscode
pnpm run compile
node out/test/config.test.js
node out/test/completionHistory.test.js
```

预期：新增 helper、setting 或 test target 尚不存在，测试失败。

- [ ] **步骤 4：写最小实现**

修改 `extensions/vscode/package.json`：

```json
{
  "command": "fossilsense.clearCompletionHistory",
  "title": "FossilSense: Clear Completion History"
}
```

新增配置：

```json
"fossilsense.completionHistory.mode": {
  "type": "string",
  "enum": ["auto", "on", "off"],
  "default": "auto",
  "description": "Controls local-only accepted-completion history used as a bounded ranking signal. 'auto' and 'on' enable it; 'off' disables recording and ranking."
}
```

修改 `extension.ts`：

```ts
const CLEAR_COMPLETION_HISTORY_COMMAND = 'fossilsense.clearCompletionHistory';
const CLEAR_COMPLETION_HISTORY_LSP_COMMAND = 'fossilsense.lsp.clearCompletionHistory';

function completionHistoryModeFromConfig(): string {
  return normalizeOnOffAuto(
    vscode.workspace.getConfiguration('fossilsense').get<string>('completionHistory.mode', 'auto'),
    'auto',
  );
}
```

将 initialization options 合并进 `fossilsense`：

```ts
completionHistory: {
  mode: completionHistoryMode,
},
```

注册 clear command：

```ts
vscode.commands.registerCommand(CLEAR_COMPLETION_HISTORY_COMMAND, () => clearCompletionHistory());
```

实现：

```ts
async function clearCompletionHistory(): Promise<void> {
  if (!client) {
    void vscode.window.showWarningMessage('FossilSense server is not running. Start it first.');
    return;
  }
  await client.sendRequest(ExecuteCommandRequest.type, {
    command: CLEAR_COMPLETION_HISTORY_LSP_COMMAND,
    arguments: [],
  });
  void vscode.window.showInformationMessage('FossilSense completion history cleared for this workspace.');
}
```

把 `fossilsense.completionHistory.mode` 加入 configuration restart watch。

修改 `crates/fossilsense/Cargo.toml` 和 `extensions/vscode/package.json` version 为 `1.2.1`。

- [ ] **步骤 5：运行 GREEN**

运行：

```bash
cd extensions/vscode
pnpm run compile
node out/test/config.test.js
node out/test/completionHistory.test.js
```

预期：TypeScript compile 和新增配置测试通过。

- [ ] **步骤 6：提交**

```bash
git add crates/fossilsense/Cargo.toml extensions/vscode/package.json extensions/vscode/src/extension.ts extensions/vscode/src/config.ts extensions/vscode/src/test/config.test.ts extensions/vscode/src/test/completionHistory.test.ts
git commit -m "chore: prepare v1.2.1 completion history settings"
```

## Task 2：Canonical member model and schema rebuild

**覆盖需求：** FR2, FR3, FR4, FR7, NFR5, NFR6, NFR7

**文件：**
- 修改：`crates/fossilsense/src/parser.rs`
- 修改：`crates/fossilsense/src/model.rs`
- 修改：`crates/fossilsense/src/store/schema.rs`
- 修改：`crates/fossilsense/src/store/writes.rs`
- 修改：`crates/fossilsense/src/store/queries.rs`
- 修改：`crates/fossilsense/src/store/tests/members.rs`
- 修改：`crates/fossilsense/src/store/tests/resilience_schema.rs`

**接口：**
- 消费：`RecordDef`, `record_key_to_id`, existing `ResolveContext`。
- 产出：
  - `parser::MemberKind::{Field, Method, StaticMethod, NestedType}`。
  - `parser::MemberConfidence::{InBody, OutOfClassOwner, Heuristic}`。
  - `parser::MemberDef { record_key, name, kind, confidence, start_byte, end_byte, start_line, start_col, end_line, end_col, signature }`。
  - `FileSemanticIndex.members: Vec<MemberDef>`。
  - `model::MemberCandidate { name, kind, signature, tier, confidence, owner_path }`。
  - SQLite `members` table and indexes。

- [ ] **步骤 1：写 RED 测试：schema version and members table**

在 `crates/fossilsense/src/store/tests/resilience_schema.rs` 增加：

```rust
#[test]
fn current_schema_has_members_table_and_version_9_or_newer() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let store = IndexStore::open(&db, dir.path()).expect("store");

    let version: String = store.conn.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    ).expect("version");
    assert!(version.parse::<i64>().expect("numeric version") >= 9);

    store.conn
        .prepare("SELECT record_id, name, kind, confidence, signature FROM members LIMIT 1")
        .expect("members table exists");
}
```

- [ ] **步骤 2：写 RED 测试：old schema drops fields data on open**

在同文件增加：

```rust
#[test]
fn opening_v8_schema_drops_old_field_rows_for_full_rebuild() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    {
        let conn = rusqlite::Connection::open(&db).expect("conn");
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
             INSERT INTO meta (key, value) VALUES ('schema_version', '8');
             CREATE TABLE files (id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL UNIQUE,
                 extension TEXT NOT NULL, size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
                 hash TEXT NOT NULL, indexed_at INTEGER NOT NULL, status TEXT NOT NULL,
                 error TEXT, source TEXT NOT NULL DEFAULT 'workspace',
                 directly_included INTEGER NOT NULL DEFAULT 0,
                 unresolved_includes INTEGER NOT NULL DEFAULT 0,
                 ambiguous_includes INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE record_defs (id INTEGER PRIMARY KEY AUTOINCREMENT, file_id INTEGER NOT NULL,
                 display_name TEXT NOT NULL, tag_name TEXT, typedef_name TEXT, kind TEXT NOT NULL,
                 start_byte INTEGER NOT NULL, end_byte INTEGER NOT NULL, start_line INTEGER NOT NULL,
                 start_col INTEGER NOT NULL, end_line INTEGER NOT NULL, end_col INTEGER NOT NULL,
                 signature TEXT NOT NULL, confidence TEXT NOT NULL);
             CREATE TABLE fields (id INTEGER PRIMARY KEY AUTOINCREMENT, record_id INTEGER NOT NULL,
                 name TEXT NOT NULL, start_byte INTEGER NOT NULL, end_byte INTEGER NOT NULL,
                 start_line INTEGER NOT NULL, start_col INTEGER NOT NULL, end_line INTEGER NOT NULL,
                 end_col INTEGER NOT NULL, signature TEXT NOT NULL);
             INSERT INTO files (path, extension, size, mtime_ns, hash, indexed_at, status)
             VALUES ('old.h', 'h', 1, 1, 'hash', 1, 'ok');",
        ).expect("seed v8");
    }

    let store = IndexStore::open(&db, dir.path()).expect("migrate");
    let count: i64 = store.conn
        .query_row("SELECT COUNT(*) FROM members", [], |row| row.get(0))
        .expect("count members");
    assert_eq!(count, 0);
}
```

- [ ] **步骤 3：写 RED 测试：fields are stored as member rows**

在 `crates/fossilsense/src/store/tests/members.rs` 增加或改写：

```rust
#[test]
fn struct_fields_are_persisted_as_field_members() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "point.h", "struct Point { int x; int y; };");

    let reader = IndexStore::open_readonly(&db).expect("reader");
    let records = reader.resolve_record_candidates(&["Point"], None).expect("records");
    let members = reader.members_for_records(
        &[records[0].id],
        None,
        None,
    ).expect("members");

    let names: Vec<_> = members.iter().map(|m| (m.name.as_str(), m.kind.as_str())).collect();
    assert!(names.contains(&("x", "field")));
    assert!(names.contains(&("y", "field")));
}
```

- [ ] **步骤 4：运行 RED**

运行：

```bash
cargo test -p fossilsense store::tests::resilience_schema::current_schema_has_members_table_and_version_9_or_newer -- --nocapture
cargo test -p fossilsense store::tests::members::struct_fields_are_persisted_as_field_members -- --nocapture
```

预期：`members` table、parser member model、store query API 尚不存在，测试失败。

- [ ] **步骤 5：写最小实现：parser/model types**

在 `parser.rs` 中添加：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Field,
    Method,
    StaticMethod,
    NestedType,
}

impl MemberKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MemberKind::Field => "field",
            MemberKind::Method => "method",
            MemberKind::StaticMethod => "static_method",
            MemberKind::NestedType => "nested_type",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberConfidence {
    InBody,
    OutOfClassOwner,
    Heuristic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDef {
    pub record_key: String,
    pub name: String,
    pub kind: MemberKind,
    pub confidence: MemberConfidence,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub signature: String,
}
```

Update `FileSemanticIndex`:

```rust
pub members: Vec<MemberDef>,
```

Keep `FieldDef` only as a short-lived compatibility alias if needed by tests during this task; by the end of Task 2 production write paths use `members`.

In `model.rs` add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCandidate {
    pub name: String,
    pub kind: crate::parser::MemberKind,
    pub signature: String,
    pub tier: ScopeTier,
    pub confidence: crate::parser::MemberConfidence,
    pub owner_path: String,
}
```

- [ ] **步骤 6：写最小实现：schema**

In `store/schema.rs`:

```rust
pub(crate) const SCHEMA_VERSION: i64 = 9;
```

Drop old and new member tables during rebuild:

```sql
DROP TABLE IF EXISTS members;
DROP TABLE IF EXISTS fields;
```

Create:

```sql
CREATE TABLE IF NOT EXISTS members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    record_id INTEGER NOT NULL REFERENCES record_defs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    confidence TEXT NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    start_line INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    signature TEXT NOT NULL
);
```

Indexes:

```sql
CREATE INDEX IF NOT EXISTS idx_members_record_id ON members(record_id);
CREATE INDEX IF NOT EXISTS idx_members_name ON members(name);
CREATE INDEX IF NOT EXISTS idx_members_kind ON members(kind);
```

Remove `idx_fields_*` from current lookup indexes or leave their `DROP INDEX IF EXISTS` only.

- [ ] **步骤 7：写最小实现：writes and queries**

In `store/writes.rs`, replace `field_stmt` with:

```rust
let mut member_stmt = tx.prepare(
    "INSERT INTO members (
        record_id, name, kind, confidence, start_byte, end_byte,
        start_line, start_col, end_line, end_col, signature
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
)?;
```

Write each `index.members` row if `record_key_to_id` contains the owner key.

In `store/queries.rs` add:

```rust
pub fn members_for_records(
    &self,
    record_ids: &[i64],
    prefix: Option<&str>,
    ctx: Option<&crate::resolver::ResolveContext<'_>>,
) -> Result<Vec<crate::model::MemberCandidate>>
```

For Task 2, `ctx` may only be used by callers that already know candidate owner tier; implementation can return `ScopeTier::Global` and Task 4 will make tier projection exact. Keep the signature stable now.

Keep a compatibility wrapper for old tests:

```rust
pub fn fields_for_records(&self, record_ids: &[i64]) -> Result<Vec<String>> {
    Ok(self.members_for_records(record_ids, None, None)?
        .into_iter()
        .filter(|member| member.kind == crate::parser::MemberKind::Field)
        .map(|member| member.name)
        .collect())
}
```

- [ ] **步骤 8：运行 GREEN**

运行：

```bash
cargo test -p fossilsense store::tests::resilience_schema -- --nocapture
cargo test -p fossilsense store::tests::members -- --nocapture
```

预期：schema v9、old data drop、field-as-member storage and compatibility tests pass.

- [ ] **步骤 9：提交**

```bash
git add crates/fossilsense/src/parser.rs crates/fossilsense/src/model.rs crates/fossilsense/src/store/schema.rs crates/fossilsense/src/store/writes.rs crates/fossilsense/src/store/queries.rs crates/fossilsense/src/store/tests/members.rs crates/fossilsense/src/store/tests/resilience_schema.rs
git commit -m "feat: add unified member schema"
```

## Task 3：Parser method member extraction

**覆盖需求：** FR4, FR5, FR6, NFR1, NFR5, NFR7

**文件：**
- 修改：`crates/fossilsense/src/parser.rs`
- 修改：`crates/fossilsense/src/parser/ast.rs`
- 修改：`crates/fossilsense/src/parser/tests.rs`
- 修改：`crates/fossilsense/src/store/tests/members.rs`

**接口：**
- 消费：Task 2 `MemberDef`, `MemberKind`, `MemberConfidence`, `FileSemanticIndex.members`。
- 产出：
  - `collect_body_members(...)` collecting fields and method members.
  - `collect_out_of_class_method_members(...)` for simple `Owner::method` subset.
  - parser tests proving method facts without full C++ semantic claims.

- [ ] **步骤 1：写 RED 测试：in-body class methods**

在 `crates/fossilsense/src/parser/tests.rs` 增加：

```rust
#[test]
fn parses_class_body_methods_as_members() {
    let source = r#"
        class Widget {
        public:
            int width;
            void resize(int w);
            static int count();
        };
    "#;
    let index = parse(std::path::Path::new("widget.cpp"), source);

    assert!(index.members.iter().any(|member|
        member.name == "width" && member.kind == MemberKind::Field
    ));
    assert!(index.members.iter().any(|member|
        member.name == "resize" && member.kind == MemberKind::Method
    ));
    assert!(index.members.iter().any(|member|
        member.name == "count" && member.kind == MemberKind::StaticMethod
    ));
}
```

- [ ] **步骤 2：写 RED 测试：method signatures are source snippets**

```rust
#[test]
fn method_member_signature_uses_declaration_text() {
    let source = "struct Widget { void resize(int width); };";
    let index = parse(std::path::Path::new("widget.hpp"), source);
    let method = index.members.iter().find(|m| m.name == "resize").expect("method");

    assert_eq!(method.kind, MemberKind::Method);
    assert!(method.signature.contains("void resize(int width)"));
    assert_eq!(method.confidence, MemberConfidence::InBody);
}
```

- [ ] **步骤 3：写 RED 测试：simple out-of-class owner**

```rust
#[test]
fn parses_simple_out_of_class_method_owner_as_lower_confidence() {
    let source = r#"
        class Widget { void resize(); };
        void Widget::resize() {}
    "#;
    let index = parse(std::path::Path::new("widget.cpp"), source);
    let matches: Vec<_> = index.members.iter()
        .filter(|member| member.name == "resize")
        .collect();

    assert!(matches.iter().any(|m| m.confidence == MemberConfidence::InBody));
    assert!(matches.iter().any(|m| m.confidence == MemberConfidence::OutOfClassOwner));
}
```

- [ ] **步骤 4：运行 RED**

运行：

```bash
cargo test -p fossilsense parser::tests::parses_class_body_methods_as_members -- --nocapture
cargo test -p fossilsense parser::tests::parses_simple_out_of_class_method_owner_as_lower_confidence -- --nocapture
```

预期：method collection 尚未实现，测试失败。

- [ ] **步骤 5：实现 in-body method extraction**

Rename `collect_body_fields` to `collect_body_members` in `parser/ast.rs`.

For each `field_declaration` child:

- Existing non-function declarators produce `MemberKind::Field`.
- Declarators whose unwrapped shape contains `function_declarator` produce `MemberKind::Method`.
- Declarations whose type/signature starts with static storage or whose node contains `storage_class_specifier` text `static` produce `MemberKind::StaticMethod`.
- Anonymous nested struct/union field flattening remains for field members.

Add helpers:

```rust
fn declarator_contains_kind(node: tree_sitter::Node<'_>, kind: &str) -> bool
fn method_member_kind(declaration: tree_sitter::Node<'_>, declarator: tree_sitter::Node<'_>, source: &str) -> MemberKind
fn push_member(record_key: &str, id_node: tree_sitter::Node<'_>, kind: MemberKind, confidence: MemberConfidence, signature: String, ...)
```

Field signature remains the field declaration text. Method signature is compacted field declaration text.

- [ ] **步骤 6：实现 simple out-of-class subset**

During AST walk, when a `function_definition` or declaration-like function node contains a `qualified_identifier` or scoped identifier text matching `Owner::method`, produce a `MemberDef` with:

```rust
record_key: format!("owner:{owner_name}")
confidence: MemberConfidence::OutOfClassOwner
kind: MemberKind::Method
```

Task 4 will resolve `owner:{name}` to record ids. Do not support nested namespaces or template owners in this helper. If owner text contains `<`, `>`, more than one `::` segment before method, or cannot be read cleanly, skip.

- [ ] **步骤 7：update store write mapping for owner-name record keys**

In `store/writes.rs`, when a member `record_key` is not in `record_key_to_id` and starts with `owner:`, look up a record in the same file update batch by `display_name`, `tag_name`, or `typedef_name`. If exactly one record matches, write the member row with `OutOfClassOwner` confidence. If zero or more than one match, skip the out-of-class member row.

- [ ] **步骤 8：运行 GREEN**

运行：

```bash
cargo test -p fossilsense parser::tests -- --nocapture
cargo test -p fossilsense store::tests::members -- --nocapture
```

预期：parser member tests pass; existing field tests still pass.

- [ ] **步骤 9：提交**

```bash
git add crates/fossilsense/src/parser.rs crates/fossilsense/src/parser/ast.rs crates/fossilsense/src/parser/tests.rs crates/fossilsense/src/store/writes.rs crates/fossilsense/src/store/tests/members.rs
git commit -m "feat: parse method members"
```

## Task 4：Owner-scoped and fallback member queries

**覆盖需求：** FR7, FR9, FR11, NFR2, NFR3, NFR5, NFR7

**文件：**
- 修改：`crates/fossilsense/src/store/queries.rs`
- 修改：`crates/fossilsense/src/store/tests/members.rs`
- 修改：`crates/fossilsense/src/query.rs`
- 修改：`crates/fossilsense/src/query/tests.rs`

**接口：**
- 消费：Task 2-3 `members` table and `MemberCandidate`。
- 产出：
  - `IndexStore::members_for_records(record_ids, prefix, ctx) -> Result<Vec<MemberCandidate>>` with owner tier.
  - `IndexStore::fallback_member_candidates(prefix, limit, ctx) -> Result<Vec<MemberCandidate>>` prefix-only and capped.
  - `query::normalized_receiver_record_hint(receiver_name: &str) -> String` for weak correlation support.

- [ ] **步骤 1：写 RED 测试：members_for_records returns fields and methods with kind**

在 `crates/fossilsense/src/store/tests/members.rs` 增加：

```rust
#[test]
fn members_for_records_returns_fields_and_methods() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "widget.hpp",
        "struct Widget { int width; void resize(); static int count(); };");

    let reader = IndexStore::open_readonly(&db).expect("reader");
    let records = reader.resolve_record_candidates(&["Widget"], None).expect("records");
    let members = reader.members_for_records(&[records[0].id], None, None).expect("members");

    assert!(members.iter().any(|m| m.name == "width" && m.kind == MemberKind::Field));
    assert!(members.iter().any(|m| m.name == "resize" && m.kind == MemberKind::Method));
    assert!(members.iter().any(|m| m.name == "count" && m.kind == MemberKind::StaticMethod));
}
```

- [ ] **步骤 2：写 RED 测试：fallback is prefix-only and capped**

```rust
#[test]
fn fallback_member_candidates_are_prefix_only_and_capped() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("index.sqlite");
    let mut store = IndexStore::open(&db, dir.path()).expect("store");
    upsert_source(&mut store, "widget.hpp",
        "struct Widget { int width; int window; void wipe(); void draw(); };");

    let reader = IndexStore::open_readonly(&db).expect("reader");
    let members = reader.fallback_member_candidates("wi", 2, None).expect("fallback");
    let names: Vec<_> = members.iter().map(|m| m.name.as_str()).collect();

    assert!(names.contains(&"width"));
    assert!(names.contains(&"window") || names.contains(&"wipe"));
    assert!(!names.contains(&"draw"));
    assert!(members.len() <= 2);
}
```

- [ ] **步骤 3：写 RED 测试：member names do not enter NameTable**

Update the existing name table leakage test:

```rust
assert!(
    !table.search_ranked("resize", 10).iter().any(|hit| hit.name == "resize"),
    "member methods must not leak into ordinary completion NameTable"
);
```

- [ ] **步骤 4：运行 RED**

运行：

```bash
cargo test -p fossilsense store::tests::members::members_for_records_returns_fields_and_methods -- --nocapture
cargo test -p fossilsense store::tests::members::fallback_member_candidates_are_prefix_only_and_capped -- --nocapture
```

预期：member query/fallback APIs incomplete，测试失败。

- [ ] **步骤 5：实现 store queries**

Implement `members_for_records`:

- Build `IN (...)` chunks for record ids.
- Optional prefix filters with `LIKE 'prefix%' COLLATE NOCASE`.
- Join `record_defs` and `files` to compute tier via `resolver::scope_tier`.
- Map `kind` and `confidence` strings back to parser enums.
- Sort by owner tier desc, member kind priority (`Field` before `Method` for exact field-heavy C behavior when score ties), prefix exact/start quality, signature, then name.

Implement `fallback_member_candidates`:

- SQL prefix-only:

```sql
SELECT m.name, m.kind, m.confidence, m.signature, f.path, f.source, f.directly_included
FROM members m
JOIN record_defs r ON r.id = m.record_id
JOIN files f ON f.id = r.file_id
WHERE m.name LIKE ?1 ESCAPE '\\' COLLATE NOCASE
```

- Aggregate same name/kind by highest tier and frequency.
- Cap by `limit`.
- Do not support subsequence or contains matching.

Update `load_symbol_names_with_paths` remains `symbols WHERE s.kind != 'field'`; members are not in `symbols`, so no ordinary leakage.

- [ ] **步骤 6：implement receiver hint helper**

In `query.rs`:

```rust
pub fn normalized_receiver_record_hint(receiver_name: &str) -> String {
    receiver_name
        .trim_start_matches(|c: char| c == '_' || c.is_ascii_digit())
        .to_ascii_lowercase()
}
```

Add tests for `widget -> widget`, `pWidget -> pwidget` if the first implementation does not strip Hungarian prefixes. Keep helper deliberately simple; Task 5 decides how to use it.

- [ ] **步骤 7：运行 GREEN**

运行：

```bash
cargo test -p fossilsense store::tests::members -- --nocapture
cargo test -p fossilsense query::tests -- --nocapture
```

预期：member query tests and existing query tests pass.

- [ ] **步骤 8：提交**

```bash
git add crates/fossilsense/src/store/queries.rs crates/fossilsense/src/store/tests/members.rs crates/fossilsense/src/query.rs crates/fossilsense/src/query/tests.rs
git commit -m "feat: query scoped member candidates"
```

## Task 5：Member completion rendering and weak receiver inference

**覆盖需求：** FR8, FR9, FR10, FR11, FR17, NFR1, NFR2, NFR3, NFR8, NFR9

**文件：**
- 修改：`crates/fossilsense/src/server/member_completion.rs`
- 修改：`crates/fossilsense/src/server/tests.rs`
- 修改：`crates/fossilsense/src/server/options.rs`
- 修改：`crates/fossilsense/src/model.rs`
- 修改：`crates/fossilsense/src/query.rs`

**接口：**
- 消费：Task 4 `members_for_records`, `fallback_member_candidates`, existing `resolve_record_candidates`。
- 产出：
  - field/method/nested-type LSP completion items.
  - weak receiver owner selection.
  - source-safe member completion perf summary.

- [ ] **步骤 1：写 RED server test：resolved receiver returns method and field**

在 `crates/fossilsense/src/server/tests.rs` 增加 async integration test following existing completion test setup:

```rust
#[tokio::test]
async fn member_completion_returns_fields_and_methods_for_resolved_receiver() {
    let backend = test_backend_with_indexed_source(
        "widget.hpp",
        "struct Widget { int width; void resize(); };",
        "main.cpp",
        "#include \"widget.hpp\"\nvoid f(Widget *w) { w->r }\n",
    ).await;
    let uri = test_uri("main.cpp");

    let response = backend.completion(completion_params(uri, 1, 25)).await
        .expect("completion")
        .expect("response");
    let items = completion_items(response);

    assert!(items.iter().any(|item| item.label == "resize" && item.kind == Some(CompletionItemKind::METHOD)));
    assert!(items.iter().any(|item| item.label == "width" && item.kind == Some(CompletionItemKind::FIELD)));
}
```

If existing helpers differ, create a focused helper in the test module that writes temp files, indexes them, opens the document, and calls completion.

- [ ] **步骤 2：写 RED server test：fallback gate still blocks one-char prefix**

```rust
#[tokio::test]
async fn member_fallback_still_blocks_one_character_prefix() {
    let backend = test_backend_with_indexed_source(
        "widget.hpp",
        "struct Widget { int width; void wipe(); };",
        "main.cpp",
        "void f() { make_widget()->w }\n",
    ).await;
    let uri = test_uri("main.cpp");

    let response = backend.completion(completion_params(uri, 0, 26)).await
        .expect("completion")
        .expect("response");
    assert!(completion_items(response).is_empty());
}
```

- [ ] **步骤 3：写 RED unit test：weak receiver unique correlation**

Add pure helper tests in `server/member_completion.rs` tests or `query/tests.rs`:

```rust
#[test]
fn weak_receiver_uses_unique_record_name_correlation_only() {
    let records = vec![
        record_candidate("Widget", 1, ScopeTier::Reachable),
    ];
    assert_eq!(weak_receiver_record_ids("widget", &records), vec![1]);

    let ambiguous = vec![
        record_candidate("Widget", 1, ScopeTier::Reachable),
        record_candidate("Widget", 2, ScopeTier::Global),
    ];
    assert!(weak_receiver_record_ids("widget", &ambiguous).is_empty());
}
```

- [ ] **步骤 4：运行 RED**

运行：

```bash
cargo test -p fossilsense server::tests::member_completion_returns_fields_and_methods_for_resolved_receiver -- --nocapture
cargo test -p fossilsense server::tests::member_fallback_still_blocks_one_character_prefix -- --nocapture
```

预期：server still renders field-only or helpers missing，测试失败。

- [ ] **步骤 5：implement member rendering**

In `server/member_completion.rs`:

- Replace `field_to_tier` with `member_to_best: HashMap<(String, MemberKind), MemberPresentation>`.
- For resolved record ids, call `members_for_records(&record_ids, Some(&prefix), Some(&ctx))`.
- Use existing highest owner tier behavior for resolved records before broadening to lower owner tiers.
- Map LSP kind:

```rust
fn lsp_kind_for_member(kind: MemberKind) -> CompletionItemKind {
    match kind {
        MemberKind::Field => CompletionItemKind::FIELD,
        MemberKind::Method | MemberKind::StaticMethod => CompletionItemKind::METHOD,
        MemberKind::NestedType => CompletionItemKind::CLASS,
    }
}
```

- Detail string includes restrained kind/confidence:

```text
method reachable
field ambiguous
```

- Documentation uses existing `completion_scope_label` plus member confidence:

```text
FossilSense: method member candidate (reachable, reachable_include, in_body)
```

- Sort by:
  1. `resolver::pack_score(owner_tier, base_match, 0)`;
  2. exact/prefix match;
  3. `Field` before `Method` only when scores tie;
  4. member name.

- [ ] **步骤 6：implement weak receiver fallback**

After explicit local declaration resolution fails and before global fallback:

- Query candidate records by normalized receiver hint across stores.
- Accept exactly one highest-confidence record id across all roots if name correlation is unique.
- Mark resulting member items with low-confidence detail, for example `heuristic receiver`.
- If more than one possible owner id or owner display name matches, decline and use normal fallback.

Do not inspect arbitrary call expressions or assignment history.

- [ ] **步骤 7：implement source-safe member perf summary**

When `perf_logging_enabled`, log only:

```text
[perf] member_completion total=... resolved_owner=... weak_owner=... fallback=... fields=... methods=... returned=...
```

No raw member names, receiver names, file paths, or source snippets.

- [ ] **步骤 8：运行 GREEN**

运行：

```bash
cargo test -p fossilsense server::tests -- --nocapture
cargo test -p fossilsense store::tests::members -- --nocapture
cargo test -p fossilsense query::tests -- --nocapture
```

预期：member rendering tests pass; ordinary completion non-leakage remains green.

- [ ] **步骤 9：提交**

```bash
git add crates/fossilsense/src/server/member_completion.rs crates/fossilsense/src/server/tests.rs crates/fossilsense/src/server/options.rs crates/fossilsense/src/model.rs crates/fossilsense/src/query.rs
git commit -m "feat: complete member methods"
```

## Task 6：Local completion history storage and command plumbing

**覆盖需求：** FR12, FR13, FR15, FR17, NFR4, NFR5, NFR9

**文件：**
- 新建：`crates/fossilsense/src/completion_history.rs`
- 修改：`crates/fossilsense/src/main.rs`
- 修改：`crates/fossilsense/src/pathing.rs`
- 修改：`crates/fossilsense/src/server.rs`
- 修改：`crates/fossilsense/src/server/options.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`
- 修改：`crates/fossilsense/src/server/tests.rs`
- 修改：`extensions/vscode/src/extension.ts`
- 修改：`extensions/vscode/src/test/completionHistory.test.ts`

**接口：**
- 消费：Task 1 configuration, existing `execute_command` path, completion items.
- 产出：
  - `CompletionHistoryMode::{Auto, On, Off}`。
  - `completion_history_path(workspace: &Path) -> Result<PathBuf>`。
  - `CompletionAcceptEvent { workspace_hash, candidate_hash, kind, intent, prefix_bucket, accepted_at }`。
  - `CompletionHistoryStore::{open, record_accept, snapshot, clear}`。
  - LSP commands `fossilsense.lsp.completionAccepted` and `fossilsense.lsp.clearCompletionHistory`。

- [ ] **步骤 1：写 RED Rust tests：history path and bounded storage**

In `crates/fossilsense/src/completion_history.rs` tests:

```rust
#[test]
fn history_store_records_accepts_without_raw_label() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("history.json");
    let mut store = CompletionHistoryStore::open(&path).expect("store");

    store.record_accept(CompletionAcceptEvent {
        workspace_hash: "workspace".to_string(),
        candidate_hash: candidate_hash("Widget::resize", "method"),
        kind: "method".to_string(),
        intent: "call_target".to_string(),
        prefix_bucket: "r".to_string(),
        accepted_at: 10,
    }).expect("record");

    let text = std::fs::read_to_string(&path).expect("read history");
    assert!(!text.contains("Widget::resize"));
    assert!(text.contains("call_target"));
}
```

Add:

```rust
#[test]
fn history_clear_removes_events_for_workspace() {
    /* record two workspace hashes, clear one, assert the other remains */
}
```

- [ ] **步骤 2：写 RED server command tests**

In `server/tests.rs`:

```rust
#[tokio::test]
async fn execute_command_records_completion_accept_when_history_enabled() {
    let backend = test_backend();
    backend.set_completion_history_mode_for_test(CompletionHistoryMode::On).await;

    backend.execute_command(ExecuteCommandParams {
        command: COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
        arguments: vec![serde_json::json!({
            "workspaceHash": "test",
            "candidateHash": "abc",
            "kind": "function",
            "intent": "call_target",
            "prefixBucket": "pr"
        })],
        work_done_progress_params: Default::default(),
    }).await.expect("command");

    assert_eq!(backend.history_snapshot_for_test("test").await.total_accepts(), 1);
}
```

Add a disabled-mode test asserting no event is stored when mode is `Off`.

- [ ] **步骤 3：运行 RED**

运行：

```bash
cargo test -p fossilsense completion_history -- --nocapture
cargo test -p fossilsense server::tests::execute_command_records_completion_accept_when_history_enabled -- --nocapture
```

预期：module, commands, and Backend state missing，测试失败。

- [ ] **步骤 4：implement history module**

Add module in `main.rs`:

```rust
mod completion_history;
```

In `pathing.rs`:

```rust
pub fn default_completion_history_path(workspace: &Path) -> Result<PathBuf> {
    let index = default_index_path(workspace)?;
    Ok(index.with_file_name("completion_history.json"))
}
```

In `completion_history.rs`:

- JSON file schema with version field and vector/map of entries.
- Store only hashes and buckets, not raw labels or source snippets.
- Bounded cap:

```rust
const MAX_HISTORY_ENTRIES: usize = 4096;
```

- On write, sort newest first and truncate to cap.
- Use atomic-ish write: write temp file in same directory, then rename.
- `candidate_hash(label, kind)` uses `blake3` over `kind + "\0" + label`, returning first 16 hex chars.

- [ ] **步骤 5：implement server commands**

In `server/options.rs` or `language_server.rs`:

```rust
pub(super) const COMPLETION_ACCEPTED_LSP_COMMAND: &str = "fossilsense.lsp.completionAccepted";
pub(super) const CLEAR_COMPLETION_HISTORY_LSP_COMMAND: &str = "fossilsense.lsp.clearCompletionHistory";
```

Add both to `execute_command_provider.commands`.

Parse initialization option:

```rust
completionHistory: { mode: "auto" | "on" | "off" }
```

Backend fields:

```rust
completion_history_mode: Arc<tokio::sync::Mutex<CompletionHistoryMode>>,
completion_history: Arc<tokio::sync::Mutex<HashMap<PathBuf, CompletionHistoryStore>>>,
```

`execute_command` handles:

- `COMPLETION_ACCEPTED_LSP_COMMAND`: validate args, ignore invalid args, no raw logging.
- `CLEAR_COMPLETION_HISTORY_LSP_COMMAND`: clear all workspace root history files and in-memory stores, log count only.

- [ ] **步骤 6：extension clear command test and implementation**

Update `completionHistory.test.ts` to assert `clearCompletionHistoryRequest()` returns:

```ts
{
  command: 'fossilsense.lsp.clearCompletionHistory',
  arguments: [],
}
```

Keep extension command from Task 1 wired to this request.

- [ ] **步骤 7：运行 GREEN**

运行：

```bash
cargo test -p fossilsense completion_history -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
cd extensions/vscode
pnpm run compile
node out/test/completionHistory.test.js
```

预期：history storage, command handling, and extension plumbing tests pass.

- [ ] **步骤 8：提交**

```bash
git add crates/fossilsense/src/completion_history.rs crates/fossilsense/src/main.rs crates/fossilsense/src/pathing.rs crates/fossilsense/src/server.rs crates/fossilsense/src/server/options.rs crates/fossilsense/src/server/language_server.rs crates/fossilsense/src/server/tests.rs extensions/vscode/src/extension.ts extensions/vscode/src/test/completionHistory.test.ts
git commit -m "feat: record local completion history"
```

## Task 7：History-aware ordinary completion ranking

**覆盖需求：** FR14, FR16, FR17, NFR2, NFR3, NFR4, NFR8

**文件：**
- 修改：`crates/fossilsense/src/completion.rs`
- 修改：`crates/fossilsense/src/completion_history.rs`
- 修改：`crates/fossilsense/src/server.rs`
- 修改：`crates/fossilsense/src/server/language_server.rs`
- 修改：`crates/fossilsense/src/server/tests.rs`

**接口：**
- 消费：Task 6 `CompletionHistorySnapshot`。
- 产出：
  - `CandidateEvidence.history_score: i32` or `history: Option<HistoryEvidence>`。
  - `CompletionRankContext { intent, history }`。
  - accept command attached to ordinary completion items。
  - source-safe history metrics in perf summary。

- [ ] **步骤 1：写 RED ranker test：bounded history boost**

In `completion.rs` tests:

```rust
#[test]
fn history_boost_lifts_comparable_candidate_but_not_current_local() {
    let history = CompletionHistorySnapshot::from_test_accepts(vec![
        ("global_fn_hash", "function", "call_target", "gl", 4),
    ]);
    let output = run_evidence_aware_pipeline_with_context(
        vec![
            candidate_with_history_hash("global_fn", CandidateSource::Indexed, ScopeTier::Global, 820, CompletionCandidateKind::Function, "global", "global_fn_hash"),
            candidate_with_history_hash("local_value", CandidateSource::LocalBinding, ScopeTier::Current, 760, CompletionCandidateKind::Variable, "local", "local_hash"),
        ],
        10,
        CompletionRankContext {
            intent: CompletionIntent { kind: CompletionIntentKind::CallTarget, confidence: CompletionIntentConfidence::High },
            history,
        },
    );

    assert_eq!(output.items[0].payload, "local");
    assert!(output.metrics.history_boosted >= 1);
}
```

- [ ] **步骤 2：写 RED ranker test：disabled parity**

```rust
#[test]
fn neutral_history_context_preserves_existing_order() {
    let candidates = vec![
        candidate("alpha", CandidateSource::Indexed, ScopeTier::Reachable, 700, "a"),
        candidate("beta", CandidateSource::Indexed, ScopeTier::Global, 900, "b"),
    ];
    let without = run_evidence_aware_pipeline(candidates.clone(), 10);
    let disabled = run_evidence_aware_pipeline_with_context(
        candidates,
        10,
        CompletionRankContext::default(),
    );

    assert_eq!(
        without.items.iter().map(|i| &i.payload).collect::<Vec<_>>(),
        disabled.items.iter().map(|i| &i.payload).collect::<Vec<_>>()
    );
}
```

- [ ] **步骤 3：写 RED server test：ordinary items carry accept command when enabled**

In `server/tests.rs`:

```rust
#[tokio::test]
async fn ordinary_completion_items_attach_history_accept_command_when_enabled() {
    let backend = test_backend_with_indexed_source(
        "main.c",
        "int print_value(void); void f(void) { pri }",
        "main.c",
        "int print_value(void); void f(void) { pri }",
    ).await;
    backend.set_completion_history_mode_for_test(CompletionHistoryMode::On).await;

    let response = backend.completion(completion_params(test_uri("main.c"), 0, 40)).await
        .expect("completion")
        .expect("response");
    let item = completion_items(response).into_iter()
        .find(|item| item.label == "print_value")
        .expect("print_value");

    assert_eq!(item.command.as_ref().map(|c| c.command.as_str()), Some(COMPLETION_ACCEPTED_LSP_COMMAND));
}
```

- [ ] **步骤 4：运行 RED**

运行：

```bash
cargo test -p fossilsense completion::tests::history_boost_lifts_comparable_candidate_but_not_current_local -- --nocapture
cargo test -p fossilsense server::tests::ordinary_completion_items_attach_history_accept_command_when_enabled -- --nocapture
```

预期：history fields/rank context/command attachment missing，测试失败。

- [ ] **步骤 5：implement history rank evidence**

In `completion.rs`:

```rust
const HISTORY_MAX_BOOST: i32 = 700;
const HISTORY_REPEAT_STEP: i32 = 120;
```

Add to evidence:

```rust
pub(crate) history_key: Option<String>,
pub(crate) history_score: i32,
```

Add to metrics:

```rust
pub history_boosted: usize,
pub history_max_boost: i32,
```

History boost calculation:

- Snapshot lookup by `(candidate_hash, kind, intent, prefix_bucket)`.
- Cap at `HISTORY_MAX_BOOST`.
- Apply after intent/match/source scoring but before final guard checks.
- Guard bands still prevent history-only global/text candidates from jumping ahead of protected current/local evidence.

- [ ] **步骤 6：attach history keys and commands in server**

In ordinary completion candidate builders:

- Set `evidence.history_key = Some(candidate_hash(&label, kind_str))`.
- When mode is enabled, before returning final items attach:

```rust
item.command = Some(Command {
    title: "FossilSense completion accepted".to_string(),
    command: COMPLETION_ACCEPTED_LSP_COMMAND.to_string(),
    arguments: Some(vec![serde_json::json!({
        "workspaceHash": workspace_hash,
        "candidateHash": candidate_hash,
        "kind": kind_str,
        "intent": intent.kind.as_summary_str(),
        "prefixBucket": prefix_bucket(&prefix),
    })]),
});
```

Do not attach to include path completion or member completion in v1.2.1.

Build history snapshot before `spawn_blocking` and pass it into `CompletionRankContext`.

- [ ] **步骤 7：source-safe perf summary**

Extend `completion_perf_summary`:

```text
history_enabled=...
history_boosted=...
history_max_boost=...
```

No raw labels or hashes in default perf line.

- [ ] **步骤 8：运行 GREEN**

运行：

```bash
cargo test -p fossilsense completion::tests -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
cargo test -p fossilsense completion_history -- --nocapture
```

预期：history ranker and command attachment tests pass; disabled parity passes.

- [ ] **步骤 9：提交**

```bash
git add crates/fossilsense/src/completion.rs crates/fossilsense/src/completion_history.rs crates/fossilsense/src/server.rs crates/fossilsense/src/server/language_server.rs crates/fossilsense/src/server/tests.rs
git commit -m "feat: rank completions with local history"
```

## Task 8：Documentation and requirement matrix sync

**覆盖需求：** FR18, NFR1, NFR4, NFR7, NFR10

**文件：**
- 修改：`CLAUDE.md`
- 修改：`README.md`
- 修改：`extensions/vscode/README.md`
- 修改：`extensions/vscode/package.json`
- 修改：`docs/smart-completion-v1-2-1/requirements.md`
- 修改：`docs/smart-completion-v1-2-1/plans/2026-07-05--implementation-plan.md`

**接口：**
- 消费：Task 1-7 implemented behavior。
- 产出：current docs accurately describe v1.2.1 capabilities, limits, privacy, fallback, full rebuild behavior, and excluded capabilities.

- [ ] **步骤 1：update current docs wording**

Required wording in current docs:

- v1.2.1 includes member evidence for fields and first-version C++ methods.
- Member completion remains best-effort owner evidence, not full C++ type binding.
- Unsupported C++ semantics remain excluded: inheritance, overloads, templates, namespaces, access control, expression typing.
- Weak receiver inference is narrow and confidence-labeled.
- Local completion history is local-only, bounded, clearable, disableable, and uses accepted-completion positive feedback only.
- No telemetry, ML ranker, cloud sync, or auto include insertion.
- Schema mismatch may trigger a full rebuild on first v1.2.1 launch.
- Source-safe logs do not print candidate names or accepted labels by default.

- [ ] **步骤 2：update package descriptions**

In `extensions/vscode/package.json`, update relevant descriptions:

- `fossilsense.completion.mode` mentions field/method member completion.
- New `fossilsense.completionHistory.mode` explains local-only positive feedback.
- Clear history command is present and named consistently.

- [ ] **步骤 3：update requirements matrix**

In `docs/smart-completion-v1-2-1/requirements.md`:

- Set `Status: approved-planned` if not already set.
- Change matrix statuses from `已设计` to `已计划`.
- Replace generic `Plan Task` labels with concrete `Task N` references where needed.
- Add user approval record:

```text
- 2026-07-05: User approved `docs/smart-completion-v1-2-1/requirements.md` and requested implementation planning.
```

- [ ] **步骤 4：run docs grep**

运行：

```bash
rg -n -F "1.2.1" README.md CLAUDE.md extensions/vscode/README.md extensions/vscode/package.json docs/smart-completion-v1-2-1
rg -n -F "local history" README.md CLAUDE.md extensions/vscode/README.md docs/smart-completion-v1-2-1
rg -n -F "member" README.md CLAUDE.md extensions/vscode/README.md docs/smart-completion-v1-2-1
rg -n -e ('TO' + 'DO') -e ('TB' + 'D') -e ('待' + '确认') -e ('开放' + '问题') -e ('后续' + '再定') -e ('PLACE' + 'HOLDER') docs/smart-completion-v1-2-1 README.md CLAUDE.md extensions/vscode/README.md
```

预期：前三条显示 current docs mention v1.2.1 member/history boundaries; placeholder scan returns no matches.

- [ ] **步骤 5：运行 docs-related tests**

运行：

```bash
cd extensions/vscode
pnpm run compile
pnpm run test
```

预期：extension compile and tests pass.

- [ ] **步骤 6：提交**

```bash
git add CLAUDE.md README.md extensions/vscode/README.md extensions/vscode/package.json docs/smart-completion-v1-2-1/requirements.md docs/smart-completion-v1-2-1/plans/2026-07-05--implementation-plan.md
git commit -m "docs: describe smart completion v1.2.1"
```

### Task 8 documentation sync note, 2026-07-05

- Current README, CLAUDE, extension README, and package metadata describe v1.2.1 member evidence for fields and first-version C++ methods.
- Docs state member completion remains best-effort owner evidence, not complete C++ binding; inheritance, overloads, templates, namespaces, access control, and expression typing remain out of scope.
- Docs state local completion history is local-only, bounded, clearable, disableable, uses positive accept feedback only, and does not upload telemetry or raw accepted labels.
- Docs mention schema mismatch may trigger a first-launch full rebuild for v1.2.1.

## Task 9：Full verification and package smoke

**覆盖需求：** FR1-FR19, NFR1-NFR10

**文件：**
- 修改：`docs/smart-completion-v1-2-1/requirements.md`
- 修改：`docs/smart-completion-v1-2-1/plans/2026-07-05--implementation-plan.md`
- 修改：only if verification finds a defect in production code or tests.

**接口：**
- 消费：Task 1-8 completed changes。
- 产出：fresh verification record and installable v1.2.1 VSIX artifact.

- [ ] **步骤 1：run targeted Rust tests**

```bash
cargo test -p fossilsense parser::tests -- --nocapture
cargo test -p fossilsense store::tests::members -- --nocapture
cargo test -p fossilsense store::tests::resilience_schema -- --nocapture
cargo test -p fossilsense completion_history -- --nocapture
cargo test -p fossilsense completion::tests -- --nocapture
cargo test -p fossilsense server::tests -- --nocapture
```

预期：targeted tests pass.

- [ ] **步骤 2：run full Rust test suite**

```bash
cargo test -p fossilsense
```

预期：all Rust tests pass.

- [ ] **步骤 3：run mini-c index smoke with schema rebuild**

```bash
cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-v1-2-1-mini.sqlite --force
```

预期：index command succeeds and reports files/symbols without failure.

- [ ] **步骤 4：run VS Code extension tests**

```bash
cd extensions/vscode
pnpm run compile
pnpm run test
```

预期：TypeScript compile and extension unit tests pass.

- [ ] **步骤 5：run whitespace and docs checks**

```bash
git diff --check
rg -n -e ('TO' + 'DO') -e ('TB' + 'D') -e ('待' + '确认') -e ('开放' + '问题') -e ('后续' + '再定') -e ('PLACE' + 'HOLDER') docs/smart-completion-v1-2-1
```

预期：no whitespace errors and no placeholder matches.

- [ ] **步骤 6：run VSIX package smoke**

```bash
cd extensions/vscode
pnpm run package
```

预期：package command succeeds and creates `dist/fossilsense-vscode-1.2.1_BUILD*.vsix` with bundled `extension/bin/fossilsense.exe`.

- [ ] **步骤 7：record verification results**

Append an `Executed verification, 2026-07-05` section to this plan with:

- command names,
- pass/fail result,
- failed command output summary if any,
- generated VSIX path.

- [ ] **步骤 8：update requirements status**

If all checks pass, update `docs/smart-completion-v1-2-1/requirements.md`:

- `Status: implemented-and-verified`.
- Matrix status cells from `已计划` to `已验证`.
- Confirmation record with verification commands and generated VSIX path.

- [ ] **步骤 9：提交**

```bash
git add docs/smart-completion-v1-2-1/requirements.md docs/smart-completion-v1-2-1/plans/2026-07-05--implementation-plan.md
git commit -m "test: verify smart completion v1.2.1"
```

## Executed verification, 2026-07-05

| Command | Result |
|---|---|
| `cargo test -p fossilsense parser::tests -- --nocapture` | PASS: 31 parser tests. |
| `cargo test -p fossilsense store::tests::members -- --nocapture` | PASS: 10 member store/query tests. |
| `cargo test -p fossilsense store::tests::resilience_schema -- --nocapture` | PASS: 7 schema resilience tests. |
| `cargo test -p fossilsense completion_history -- --nocapture` | PASS: 3 history/options tests. |
| `cargo test -p fossilsense completion::tests -- --nocapture` | PASS: 45 completion/ranker tests. |
| `cargo test -p fossilsense server::tests -- --nocapture` | PASS: 39 server tests. |
| `cargo test -p fossilsense` | PASS: 472 unit tests and 2 LSP smoke tests. |
| `cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-v1-2-1-mini.sqlite --force` | PASS: indexed 2 files, 13 symbols, no failures. |
| `pnpm run compile` in `extensions/vscode` | PASS. |
| `pnpm run test` in `extensions/vscode` | PASS. |
| `git diff --check` | PASS: no whitespace errors. |
| Placeholder scan over `docs/smart-completion-v1-2-1` | PASS: no matches. |
| `pnpm run package` in `extensions/vscode` | PASS: generated VSIX with bundled `extension/bin/fossilsense.exe`. |

Generated VSIX: `dist/fossilsense-vscode-1.2.1_BUILD20260705_180741.vsix`.
