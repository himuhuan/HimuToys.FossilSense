# CLAUDE.md

## 工作方式

FossilSense 的实现事实只来自**当前源码、测试、清单和脚本**。文档只能帮助定位，不能证明功能已经存在。

处理任何问题前必须主动阅读相关源码：

1. 使用 `rg` / `rg --files` 找到入口、调用链、配置和测试。
2. 阅读实际实现，再形成判断或修改方案。
3. 命令、版本和打包行为必须从 `Cargo.toml`、`package.json`、`build.ps1` 和 `scripts/` 核对。
4. 文档与实现冲突时，以源码和测试为准，并同步修正文档。

不要从历史计划、评审稿、交付报告或 AI 会话记录推断当前架构。仓库不保存这些中间过程文档，也不要新建 research、plan、report、delivery note、实现总结或提示词副本。`docs/` 只允许保存可复现的性能测试方法和结果。

## 项目定位

FossilSense `1.4.4` 是一个面向大型 Windows C/C++ 工作区的 VS Code 代码导航与分析工具。它把“缺少可靠编译环境”视为正常场景：用户不需要先准备 `compile_commands.json`、clangd、ctags 或完整构建链。

核心原则：

- **候选不是绑定**：跳转、补全、引用、Hover 和调用关系是 best-effort 结果，不得伪装成编译器级精确语义。
- **容错优先**：语法错误、宏、缺失 include 和不完整配置应触发明确降级或 fallback，不应让服务崩溃。
- **大仓库优先**：查询必须有界，热路径避免磁盘 IO 和全库复制，后台发布不能阻塞旧快照继续服务。
- **开箱即用**：对外 VSIX 必须包含 Rust 原生二进制，不依赖用户额外安装工具链。
- **不确定性可见**：结果需要保留 confidence、reason、ambiguity、coverage 或 truncation 等证据。

当前明确不做完整 C++ 语义绑定，包括继承、模板、重载决议、宏展开、访问控制和表达式类型推断。遇到 unsupported 形态时保守返回候选或降级，不猜测唯一答案。

## 源码入口

```text
VS Code 扩展  extensions/vscode/src
        │ LSP over stdio
        ▼
Rust 引擎      crates/fossilsense/src
```

优先从这些入口继续向下读，不要依赖静态模块说明：

- `crates/fossilsense/src/main.rs`：CLI 的 `scan`、`index`、`query`、`lsp` 入口。
- `crates/fossilsense/src/server.rs` 与 `server/`：LSP 生命周期和协议适配。
- `crates/fossilsense/src/indexer.rs` 与 `indexer/`：扫描、解析、索引和发布。
- `crates/fossilsense/src/parser.rs` 与 `parser/`：C/C++ 容错事实提取。
- `crates/fossilsense/src/query.rs`、`query/`、`resolver.rs`：候选查询和排序。
- `crates/fossilsense/src/store.rs` 与 `store/`：SQLite schema、读视图和写入。
- `extensions/vscode/package.json`：客户可见命令、配置、版本和打包入口。
- `scripts/`：CI、架构约束、发布验证和性能基准。

新增能力先搜索现有模型和服务，优先复用已有语义事实、resolver、candidate service、read view 和 request snapshot。不要仅凭文档名称创建平行的 `smart` / `semantic` 模型。

## 修改守则

- 修改前先读实现和相邻测试；修改后添加能覆盖失败模式的测试。
- parser、store、query、server 与扩展之间保持现有依赖方向，协议转换留在边界。
- SQLite 查询必须窄且有界；不能把全库事实复制到请求内存中。
- open document、索引 generation 和 runtime snapshot 不能混代；发布失败不能暴露半更新状态。
- 排序证据不能偷偷变成 hard filter。无法证明唯一时必须保留歧义或 fallback。
- 新依赖需要说明运行时、许可证、平台和 VSIX 自包含影响。
- 用户可见行为变化需要同步根 `README.md` 和 `extensions/vscode/README.md`，但不要写施工过程。

## 功能验证

日常修复和小功能应先使用 `samples/mini-c` 或其他小型代码库快速验证：

```powershell
cargo test -p fossilsense
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/mini.sqlite --force

Set-Location extensions/vscode
pnpm run test
```

完整仓库门禁：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify.ps1 -SkipInstall
```

### 大型代码库性能硬门禁

mini-c 只适合快速功能验证。以下情况必须使用 **U-Boot 或 Wine** 至少一个大型代码库执行 release full-index 性能测试：

- 对外发版本。
- 重大功能新增或调整。
- 架构、索引、存储、解析、查询、并发或发布流程调整。

样本放在本地 `samples/u-boot` 或 `samples/wine`，不提交仓库。先构建 release，再运行对应 case：

```powershell
cargo build --release -p fossilsense

# 二选一；发布前可同时运行
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 `
  -Repeats 1 -IncludeFullIndex -CaseFilter u-boot-full-index -TimeoutSeconds 60

powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 `
  -Repeats 1 -IncludeFullIndex -CaseFilter wine-full-index -TimeoutSeconds 60
```

`60s` 是硬门禁，不是观察值。任一必测 full-index case 的进程耗时或输出 `elapsed_ms` **高于 60,000 ms，即判定此次功能失败**；不能用平均值、机器波动或“小样本已经通过”放行。报告同时保留样本版本、机器信息、命令、`elapsed_ms`、`write_ms`、峰值内存和数据库大小。详细复现方法见 `docs/benchmark/`。

## 编译

环境：Windows PowerShell、stable Rust、Node.js 22、pnpm 10。

Rust：

```powershell
cargo build -p fossilsense
cargo test -p fossilsense
cargo fmt --all -- --check
cargo clippy -p fossilsense --all-targets -- -D warnings
```

VS Code 扩展：

```powershell
Set-Location extensions/vscode
pnpm install --frozen-lockfile
pnpm run compile
pnpm run test
```

## 打包与发布

仓库根目录的一键入口会安装依赖、运行 Rust/扩展测试、创建自包含 VSIX 并执行发布验证：

```powershell
.\build.ps1
```

仅手动打包时：

```powershell
Set-Location extensions/vscode
pnpm install --frozen-lockfile
pnpm run package
Set-Location ../..
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_release_hardening.ps1
```

产物位于：

```text
dist/fossilsense-vscode-<version>_BUILD<YYYYMMDD_HHMMSS>.vsix
```

`pnpm run package` 会构建 release Rust 二进制、打包扩展、生成 `release-build.json` 输入指纹并把二进制放入 VSIX。打包后若 Rust、扩展或打包关键输入发生变化，旧 VSIX 必须作废并重新生成。

发布完成时直接在提交、PR 或发布页记录最终 VSIX 文件名、VSIX SHA-256、release-input SHA-256、source commit、验证结果、能力边界和大型仓库性能数据；不要为这些信息新增仓库内过程文档。

## 文档边界

仓库长期维护的 Markdown 仅包括：

- `CLAUDE.md`：基本工程守则、验证、编译和打包方法，不超过 500 行。
- 根 `README.md`：面向客户的产品介绍、安装和使用。
- `extensions/vscode/README.md`：VSIX Marketplace 内容。
- `docs/benchmark/`：可复现的性能测试方法与结果。

完成任务前再次检查：实现是否与测试一致、README 是否只描述真实能力、是否误加中间文档，以及重大变更是否通过大型仓库 `60s` 门禁。
