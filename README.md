# FossilSense

**不用先配齐编译环境，也能开始读代码。**

FossilSense 是可在 Windows 和 Linux 上使用、面向大型 C/C++ 仓库的 VS Code 代码导航工具，也提供实验性的 Go 支持。它帮助你查找定义、理解调用关系、浏览陌生代码，适合接手旧项目、阅读固件或分析第三方 SDK。

当前开发版本为 1.7.2。安装 VSIX 即可使用，无需准备 `compile_commands.json`，也无需额外安装 clangd、gopls、ctags、Go 或 Rust 工具链。

[发布版本](https://github.com/himuhuan/HimuToys.FossilSense/releases) · [详细使用说明](extensions/vscode/README.md) · [开发指南](AGENTS.md)

## 适合哪些项目

代码已经拿到手，但构建环境缺失、依赖难以还原，或者暂时只想弄清楚某个功能如何实现——这正是 FossilSense 的使用场景。固件、驱动、内核、历史项目，以及带有大量第三方代码的仓库，都可以从代码导航开始探索。

如果你需要基于完整编译配置的精确语义分析，FossilSense 不能替代编译器或对应的语言服务。证据不足时会保留歧义或降级信息，不保证唯一答案。

## 能帮你做什么

| 你想做的事 | FossilSense 提供的帮助 |
| --- | --- |
| 找到代码入口 | 搜索工作区符号，浏览文件中的函数和类型，跳转到定义或声明 |
| 理解一个函数 | 悬停查看签名和注释，输入参数时查看提示 |
| 追踪代码联系 | 查找引用，查看直接调用中“谁调用了它、它调用了谁”及调用位置 |
| 边读边修改 | 提供名称、局部变量、部分成员和 include/import 路径补全，感知打开文档中的未保存声明 |
| 排查同名代码 | 查看多个可能的定义或声明，以及候选的来源和不确定性 |

## 快速开始

1. 下载与扩展运行环境匹配的 `.vsix`：Windows 选择 `win32-x64`，Linux 选择 `linux-x64`；ARM64 需使用对应的 `arm64` 包。
2. 在 VS Code 扩展面板选择 **… → Install from VSIX**，安装该文件。
3. 打开 C/C++ 或 Go 项目文件夹，等待状态栏显示 **FossilSense: ready**。
4. 尝试“转到定义”、悬停和符号搜索；在命令面板输入 `FossilSense` 查看更多操作。

状态栏按工作区汇总索引状态，所有工作区发布完成后显示 **ready**；其他工作区的完成不会掩盖仍在索引、等待资源或失败的工作区。

常用操作：

- **Analyse Call Hierarchy**：光标放在函数上，查看直接调用关系。
- **Find All Possible Definitions / Declarations**：查看同名候选。
- **Refresh Index**：刷新修改过的文件；需要重新扫描整个项目时使用 **Full Rebuild Index**。
- **Start Server / Stop Server**：手动启动或停止服务。

默认无需配置。若其他扩展也为同一语言提供导航和补全，可按需要选择一个主要服务，减少重复结果。

## 按项目调整

在工作区根目录创建 `fossilsense.json`，可以限制扫描范围，并添加项目外的头文件目录：

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "includePaths": ["C:/SDK/include"]
}
```

Linux 示例为 `/opt/sdk/include`。把示例路径替换成实际目录；外部头文件目录使用绝对路径。Go 外部模块、protobuf-c 来源追溯和资源档位等可选设置见[详细使用说明](extensions/vscode/README.md)。

## 使用前了解

FossilSense 根据源码和文件关系建立本地索引，不依赖成功编译项目。它返回的是有依据的候选：缺失头文件、复杂宏、模板或间接调用等情况可能导致结果不完整，不能把所有跳转和引用当成唯一、准确的绑定。Go 支持仍为实验能力。

索引更新失败时，旧索引可能继续提供结果，但内容可能过时。请留意状态提示，必要时释放资源、重启服务并重新建立索引。

开发、构建和打包入口见 [AGENTS.md](AGENTS.md)；可复现的性能方法与结果见 [docs/benchmark](docs/benchmark/)。

## Linux 开发与打包

使用 Node.js 22、pnpm 10、PowerShell 7（`pwsh`），以及 `rust-toolchain.toml` 指定的 Rust。Linux 构建机还需 C/C++ 编译器、链接器和系统开发头文件（例如 Ubuntu 的 `build-essential`）；安装后的用户无需这些开发工具。

在仓库根目录运行 `pwsh -NoProfile -File ./build.ps1`，即可安装锁定依赖、编译 Linux 引擎并校验 VSIX。需要同时验证源码时加 `-Verify`；局部验证使用 `pwsh -NoProfile -File ./scripts/verify.ps1 -Profile Local -Scope Extension`，Rust 局部测试另用 `-Scope Rust -TestFilter <测试名>`。

产物为 `dist/fossilsense-vscode-<版本>_BUILD<时间>_<平台>-<架构>.vsix`，可用 `code --install-extension <文件路径>` 安装。Windows 和 Linux 分别在对应系统构建；Linux 包面向 glibc 环境，不用于 Alpine/musl。系统库兼容性取决于构建机，持续集成采用 Ubuntu 22.04；ARM64 包需在 ARM64 主机构建，尚未纳入持续集成矩阵。

使用 WSL、SSH 或容器远程工作区时，应在远程扩展主机安装匹配其系统和架构的包。Linux 上仅大小写不同的项目目录与外部依赖目录会分别保留。
