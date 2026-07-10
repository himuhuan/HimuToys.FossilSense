> **Status: archived** (2026-07-10)
>
> 历史交付说明。当前版本交付说明见 dist/DELIVERY-NOTE-<version>.md。
# FossilSense 交付说明 — v1.0.0（unify-completion-model）

**VSIX**: `dist/fossilsense-vscode-1.0.0.vsix`（约 4.1 MB，自包含原生二进制，安装后无需另编译 Rust）
**安装**: VS Code → Extensions → `...` → Install from VSIX，或
`code --install-extension "dist/fossilsense-vscode-1.0.0.vsix"`

OpenSpec 变更：`unify-completion-model`（1.0.0 首批 `symbol-aware-code-intelligence`：统一补全候选模型、来源标注、延迟文档解析）

## 本次能力范围

补全从三条互不相干的代码路径 + 不兼容的分数尺度，重构为统一的 `Candidate` 模型与单一加性 ranker。

### 能做什么

- **分类补全候选**：局部变量、全局函数、全局变量、宏、类型、枚举量、成员、未分类词，分别对应不同 `CompletionItemKind` 图标。
- **局部变量识别**：在当前文件中，tree-sitter 已标记为 `Declaration` 的标识符（局部变量 / 参数）会被识别为 `LocalVariable`；其余当前文件词仍作为 `Word` 兜底，保证召回不缩水。
- **统一排序**：所有候选（索引符号、当前文件词、成员字段）走同一套加性评分：`base_score + category_prior + reachability/locality + external_penalty`，只重排、不过滤。
- **轻量来源标注**：列表项的 `detail` 显示 `类别 · 定义文件基名`，外部（工具链）符号追加 `· ext`，全部来自内存，零磁盘 I/O。
- **延迟文档解析**：`completionItem/resolve` 按需读取索引中的签名并填充 `documentation`；列表响应本身不含完整文档，保持 keystroke 路径零 I/O。
- **保留既有契约**：`isIncomplete: true`、短前缀召回门（len < 3 收紧）、前缀增量池（prefix-independent narrowing）、按名字去重、空列表仍标 incomplete。

### 还不能做什么（明确边界）

- 排序权重尚未暴露为 `fossilsense.completion.*` 用户设置（代码结构已就绪，本批未配置）。
- 不做真正的语义类型绑定、重载解析、调用层次结构。
- C++ 成员补全仍只做数据成员，不处理继承 / 方法 / 模板 / 命名空间 / 访问控制。
- 局部变量识别是句法启发式：任何 `Declaration` 角色的标识符都可能被列出，不精确到作用域范围；它被软排序且明确标注为 best-effort。
- include 路径补全不受本次重构影响，行为不变。

## 验证情况

- `cargo test -p fossilsense`：**208 passed / 0 failed**（含 query/server 新增的统一模型、来源、resolve 用例）。
- `cargo build --release` 与 `pnpm run package`：均通过，产出自包含 VSIX。
- `samples/mini-c`：干净索引（5 文件 / 23 符号），符号查询工作正常。
- `example/HimuOS`：本地不存在（git-ignored），需用户手动进行 realistic-scale 验收、类别图标与权重调参。
