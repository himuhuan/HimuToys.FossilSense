> **Status: archived** (2026-07-10)
>
> 历史交付说明。当前版本交付说明见 dist/DELIVERY-NOTE-<version>.md。
# FossilSense 交付说明 — v1.0.1（slop-remediation-phase1）

**VSIX**: `dist/fossilsense-vscode-1.0.1.vsix`（约 4.1 MB，自包含原生二进制，安装后无需另编译 Rust）
**安装**: VS Code → Extensions → `...` → Install from VSIX，或
`code --install-extension "dist/fossilsense-vscode-1.0.1.vsix"`

OpenSpec 变更：`slop-remediation-phase1`（`docs/fossilsense-1.1.0-slop-remediation.md` §15 Phase 1「先止血」——只修廉价高价的正确性泄漏与文档漂移，不动事实模型与排序管线）。

## 本次能力范围

1.0.0 之上的**修补版本**：导航/resolve 的符号身份正确性止血 + 文档对齐到已发布行为。无 schema 变更，复用既有 `symbols.container` 列与 `kind = 'field'` 过滤。

### 能做什么（Phase 1 改动）

- **导航查询面不再返回字段**：新增 `store::symbols_by_name_for_navigation(name)`（`symbols_by_name` SQL 加 `AND s.kind != 'field'`，复用 `SELECT_SYMBOL_JOIN`）。跳转到定义（LSP 与 CLI）、补全 resolve 的「按名字」分支全部改走它。`symbols_by_name()` 降级为带文档注释的原始低层查询（仅测试 / 字段 id 路径使用），UI 导航入口不再直接调用。效果：同名字段 + 顶层符号时跳转不落字段；只作为字段存在的名字，导航返回空候选。
- **同名字段按接收者记录 resolve**：新增 `store::field_by_container_and_name(container, name)`（`kind = 'field' AND container = ?1 AND name = ?2 LIMIT 1`，复用 `SELECT_SYMBOL_JOIN`），应用与 `fields_by_record_scoped` 相同的 `resolve_alias` 展开。成员补全项在 `data` 携带 `{"fieldName", "container"}`；`resolve_completion_item` 按 `symbolId` → `container`+`fieldName` → `symbolName` 三分支解析。效果：`struct Widget { int status; }` 与 `struct Device { const char *status; }` 在 `struct Widget *` 接收者上 resolve 得 `int status;`；`typedef struct Widget WidgetT;` 别名接收者也能经 alias 目标命中。
- **接收者不可推断的字段回退标为 best-effort**：全局字段回退成员项（`resolved_hit == false`）在 `data` 带 `{"fallback": true}`；resolve 命中 fallback 时跳过精确签名查找，保留短 best-effort `detail`（如 `field · best-effort`），不产生 `documentation` 飞出面板、不冒充某条记录的精确字段签名。
- **文档与 OpenSpec 对齐**：README / 扩展 README / CLAUDE references 统一改述为 **role-sorted text references**（整词文本发现 → 解析命中文件按角色排序 → 纯 LSP `Location`，无编辑器可见角色字段）；补全 resolve 统一描述为**只写 `detail`（一行干净签名）、不出 `documentation` 飞出面板**；词汇统一为 **Indexed / Inferred / Best-effort**；README 定位回归 1.0.0「SourceInsight 级主力工具、best-effort + 标注」框架。修正 `reference-classification` 与 `completion-candidate-provenance` 两条 change spec 的漂移措辞；解决 README 内部「include-path completion 既有又无」的矛盾（确认已发布）。

### 还不能做什么（Phase 2/3 延后，非本次回归）

- **补全 overfetch + 最终排序**：候选预截断仍在最终 ranker 之外；locality cap / score budget 未引入。
- **include 多义 → open scope**：多义 include 仍被当作确定可达；reachability 的 ambiguity / confidence 概念未落地。
- **降级状态未穿透到 status/output**：degraded/heuristic/fallback 状态目前只体现在补全项 `detail` 标注与 `isIncomplete`，未做可诊断面板汇报。
- **`records` / `fields` / `definitions` schema 拆分**：`symbols` 单表仍是事实模型底座；record identity / file scope / alias scope 根本治理是 Phase 3。`field_by_container_and_name` 仍以字符串 `container` 为身份，同记录重名字段或跨文件 container 串碰撞的歧义**未解决**（严格优于「按名字取第一条」，但不是终局）。
- **references 发现/排序算法本身、completion ranking、reachability、coloring 行为**：均未改动；Phase 1 不新增自定义 role-payload 请求。
- **hover**：仍不提供（保留「不给会误导的精确假象」底线）。

## 验证情况

- `cargo test -p fossilsense`：**231 passed / 0 failed**（含新增 5 个 store/pipeline 测试：导航排除字段、字段唯一名无导航候选、同名字段按接收者记录 resolve、别名接收者经 alias 命中、回退项 best-effort 标记）。
- `cargo build -p fossilsense`：无警告（`symbols_by_name` 降级后加 `#[allow(dead_code)]`，保留供测试 / 未来 field-id 路径）。
- `pnpm run compile`（`extensions/vscode`）：tsc 通过。
- `pnpm run package`：产出 `dist/fossilsense-vscode-1.0.1.vsix`（自包含 `fossilsense.exe`，4.1 MB）。
- `samples/mini-c` 与 `example/HimuOS` 的 realistic-scale 验收需用户手动进行（后者本地 git-ignored）。

## 升级建议

从 1.0.0 升级后，建议对既有工作区执行一次 **`FossilSense: Full Rebuild Index`**，让旧索引在新的导航/resolve 路径上完整复算（schema 未变，增量刷新也能工作，全量重建用于稳态验证）。
