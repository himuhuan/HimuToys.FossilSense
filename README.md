# FossilSense

FossilSense 是一款面向大型、难以构建的 C/C++ 仓库的 VS Code 代码导航工具。它不要求 `compile_commands.json`，也不需要额外安装 clangd、ctags 或 Rust 工具链。安装一个自包含 VSIX，打开工作区后即可建立索引。

当前版本：`1.4.4`。

## 什么时候适合使用

FossilSense 主要解决一种很实际的问题：代码就在眼前，但完整编译环境很难还原。

它适合嵌入式、固件、驱动、内核、旧代码、跨平台分支很多的仓库，以及包含大量第三方 SDK 的大型 Windows 工作区。你可以先获得可用的跳转、搜索、补全和代码关系，再决定是否值得修复整套构建系统。

如果项目已经能被 clangd 或 IntelliSense 精确解析，继续使用它们通常更合适。FossilSense 提供的是 **best-effort 候选**，不是编译器级语义绑定。

## 安装与开始使用

1. 在 VS Code 中打开 `Extensions`。
2. 选择右上角 `... -> Install from VSIX`。
3. 选择 `fossilsense-vscode-1.4.4_BUILD*.vsix`。
4. 打开 C/C++ 工作区，等待状态栏进入 `ready`。

默认无需配置。FossilSense 会扫描常见 C/C++ 文件，并把索引保存在用户缓存目录，不会在源码仓库中生成数据库。

如果工作区同时启用了 clangd、Microsoft C/C++ 或 ccls，FossilSense 会提示选择主要语言服务。一个工作区建议只保留一个主要 C/C++ provider。

## 你会获得什么

- **跳转与搜索**：文档符号、工作区符号、头文件跳转，以及标准 **Go to Declaration** 和 **Go to Definition**。两种操作的含义保持稳定：在定义处再次执行 Definition 仍停留在定义，不会把它当成声明/定义切换命令；`goto` label 只在当前函数的 label namespace 内解析。
- **显式候选审阅**：**Find All Possible Definitions / Declarations** 展示默认跳转压制前的有界 variants，并附带 role、scope、linkage、guard、pairing 与 coverage 证据。
- **持续补全**：普通标识符、include 路径、当前函数参数与局部变量，以及有限的 `.` / `->` 成员候选。
- **引用查找**：全词搜索后按定义、声明、调用、读、写和类型等语法角色分组。
- **Hover 与 Signature Help**：展示函数签名、注释和参数个数兼容候选；Record Hover 可展示完整的 `struct` / `class` / `union` 声明，唯一 `typedef` 链可显示 `aka`。
- **调用关系**：查看 C/C++ 自由函数的一跳 incoming / outgoing 关系、调用点和候选证据。
- **未保存编辑感知**：当前工作区打开但尚未保存的结构化声明可以参与候选结果。
- **有限语义着色**：重点区分宏、类型、枚举量、参数和局部变量，避免大面积误着色。

FossilSense 会优先展示当前文件、include 可达文件和直接外部头中的候选，再使用全局 fallback。`1.4.4` 会保留 include 解析方式的证据强度：精确解析的边可提供强可达性；唯一后缀匹配和 ambiguous include 的所有可能目标只作为启发式证据；外部头证据也按当前查询来源判断，避免把其他文件的关系借给本次跳转或补全。有限语义着色允许这些有界启发式 include 目标参与宏、类型和枚举量的种类判定，但不会因为 include scope 处于 open 状态而放开无关的全库定义；种类证据冲突时仍不着色。exact-name 全局窗口触顶时会先抢救 Current 和强可达候选。遇到 include 缺失、语法不完整或结果被截断时，界面会保留降级、歧义或 coverage 信息，而不是假装结果完全精确。

## 常用命令

打开命令面板并输入 `FossilSense`：

| 命令 | 用途 |
|---|---|
| `Start Server` / `Stop Server` | 启动或停止当前工作区服务 |
| `Refresh Index` | 增量处理发生变化的文件 |
| `Full Rebuild Index` | 强制重新扫描并建立完整索引 |
| `Find All Possible Definitions / Declarations` | 查看默认跳转压制前的有界候选与不确定性证据 |
| `Find References (Grouped by Role)` | 按语法角色查看引用候选 |
| `Analyse Call Hierarchy` | 查看自由函数 incoming / outgoing 关系和调用点 |
| `Select Project Context` | 选择自动识别的项目范围或关闭项目证据 |
| `Clear Completion History` | 清除当前工作区的本地补全历史 |

## 可选配置

在工作区根目录创建 `fossilsense.json`，可以限制扫描范围或加入外部头文件目录：

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp"],
  "includePaths": ["C:/toolchain/include"]
}
```

- `include` / `exclude` 控制工作区内哪些目录参与索引。
- `extensions` 控制识别的源码扩展名。
- `includePaths` 指向工作区外的 SDK 或工具链头文件目录，必须使用绝对路径。

配置缺失时扫描整个工作区的默认源码类型；配置错误时会显示 warning 并降级到安全默认值。

VS Code 设置中常用的选项：

- `fossilsense.mode`：`auto`、`on` 或 `off`。
- `fossilsense.includePaths`：额外的外部头文件目录。
- `fossilsense.completion.prefixRanking`：默认 `strict`，优先精确名和字面前缀；`scopeFirst` 更重视作用域证据。
- `fossilsense.projectContext.mode`：自动项目证据、歧义时询问或关闭。
- `fossilsense.semanticColoring.mode`：启用或关闭 FossilSense 着色。

## 能力边界

FossilSense 不支持完整的 C++ 继承、模板、重载决议、宏展开、访问控制、命名空间绑定或复杂表达式类型推断。成员调用、函数指针和 callable object 也不会被伪装成已经精确绑定的自由函数关系。

引用是文本候选加语法角色分类，可能包含注释或字符串中的同名文本。导航与补全索引会区分声明、C tentative definition、完整定义和无法判定的声明/定义，并在同级候选中使用这些角色；这仍不是编译器级的链接决议。

函数声明和定义只有在规范化签名、链接属性和 include 证据足够时才会配对。C 函数签名比较会忽略参数名和无关的独立 `extern`，但仍保留参数类型等形状差异；歧义或证据不足时会保留多个普通候选或 fallback，而不是猜测唯一答案。

Find All 是有界发现入口，不是编译器链接结果。它会明确显示当前 `limit`、open/truncated/incomplete coverage；跨 workspace root 的无限全集、完整宏状态、C++ ABI/`extern "C"` 绑定和真实 build target 选择仍不支持。

这些限制是产品选择：在缺少编译参数的仓库里，稳定、可解释的候选比错误的“唯一答案”更有价值。

## 隐私

索引、补全历史和查询都在本机完成。FossilSense 不上传源码，不发送匿名 telemetry，不做 cloud sync，也不使用云端 ML ranker。补全历史只保存在当前工作区本地缓存中，可随时关闭或清除。

贡献、编译、测试和打包方法见 [CLAUDE.md](CLAUDE.md)。
