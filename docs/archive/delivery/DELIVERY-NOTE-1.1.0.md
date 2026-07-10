> **Status: archived** (2026-07-10)
>
> 历史交付说明。当前版本交付说明见 dist/DELIVERY-NOTE-<version>.md。
# FossilSense 交付说明 - v1.1.0（rich-hover-comments）

**VSIX**: `dist/fossilsense-vscode-1.1.0_BUILD20260704_010312.vsix`（约 4.27 MB，自包含原生 `fossilsense.exe`）

**安装**:

```powershell
code --install-extension "dist/fossilsense-vscode-1.1.0_BUILD20260704_010312.vsix"
```

或 VS Code -> Extensions -> `...` -> Install from VSIX。

## 本次能力范围

v1.1.0 是 rich hover + UI 降噪版本。它继续保持 FossilSense 的基本定位：在没有可靠编译环境、没有 clangd/compile_commands 的大型 C/C++ 工作区里，提供 best-effort candidate 导航与索引体验。

### 能做什么

- **Rich Hover**：LSP 现在声明 `hoverProvider`。对 indexed identifier hover 时，FossilSense 走 exact-name 查询和既有 resolver 排序，返回 Markdown hover。
- **Hover 里显示候选证据**：hover 包含 signature、candidate kind/role/path、tier/confidence/reason。它明确是 ranked candidate，不伪装为编译级绑定。
- **Doxygen / 普通注释渲染**：支持紧邻候选的 `///`, `//`, `/** ... */`, `/* ... */` 前导注释；常见 Doxygen 命令 `@brief`, `@param`, `@param[in]`, `@param[out]`, `@return`, `@retval`, `@note`, `@warning` 会渲染成可读 Markdown。
- **不规范注释降级**：装饰星号、未知 `@command`、不完整或普通注释尽量转成可读文本；空白/代码边界不会误绑定旧注释，读不到候选源文件或文件超过 hover 读取上限时仍显示 signature 和 ranking evidence。
- **Grouped References 降噪**：`FossilSense: Find References (Grouped by Role)` 默认隐藏每条结果的 `:line` 后缀，只显示相对路径和 role；设置 `fossilsense.references.showRanges = true` 可恢复 line suffix。
- **版本对齐**：Rust crate、VS Code package / VSIX 均为 `1.1.0`。

### 还不能做什么

- 不做 clangd 级类型语义 hover、overload resolution、模板/命名空间查找、宏展开、表达式类型 hover。
- 不做字段/member hover；字段仍走 member completion 的 record/field 模型。
- 不持久化注释到 SQLite schema；v1.1.0 在 hover 请求期只读取少量 ranked candidate source，并对磁盘候选源文件设置单文件大小上限。
- 不支持完整 Doxygen renderer、HTML、图片、跨引用链接或所有 Doxygen 命令。
- 不改变 references 的发现语义：标准 References 仍是 whole-word text candidates，并且仍可能命中 comments/strings。

## 验证情况

已通过：

```powershell
cargo fmt
cargo check -p fossilsense
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/v1-1-hover-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/v1-1-hover-mini.sqlite
cd extensions/vscode
pnpm exec tsc -p ./
pnpm run test
pnpm run package
```

结果摘要：

- Rust tests：387 passed / 0 failed。
- LSP smoke：2 passed / 0 failed，包含 hover provider 与 hover Markdown 响应检查。
- `samples/mini-c` force index：2 files indexed，13 symbols。
- `samples/mini-c` incremental index：2 files skipped，13 symbols retained。
- TypeScript tests：通过，包括 `referencesView.test.js`。
- VSIX：`dist/fossilsense-vscode-1.1.0_BUILD20260704_010312.vsix` 已生成。
- VSIX inspection：内含 `extension/package.json` version `1.1.0` 和 `extension/bin/fossilsense.exe`。

## 升级建议

从旧版本升级后，建议对重要工作区执行一次 **FossilSense: Full Rebuild Index**。本版本没有 schema migration，但全量重建可以让 hover、definition、completion、references 都基于同一批新索引事实验证。
