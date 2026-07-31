# FossilSense

FossilSense 是一款面向大型、难以构建的 C/C++ 与 Go 仓库的 VS Code 代码导航工具。它不要求 `compile_commands.json`，也不需要额外安装 clangd、gopls、ctags、Go 或 Rust 工具链。安装一个自包含 VSIX，打开工作区后即可建立索引。

当前版本：`1.5.0`。Go 后端在该版本中以实验能力提供，并遵守与 C/C++ 相同的候选式语义、容错与有界查询原则。

## 什么时候适合使用

FossilSense 主要解决一种很实际的问题：代码就在眼前，但完整编译环境很难还原。

它适合嵌入式、固件、驱动、内核、旧代码、跨平台分支很多的仓库，以及包含大量第三方 SDK 的大型 Windows 工作区。你可以先获得可用的跳转、搜索、补全和代码关系，再决定是否值得修复整套构建系统。

如果项目已经能被 clangd 或 IntelliSense 精确解析，继续使用它们通常更合适。FossilSense 提供的是 **best-effort 候选**，不是编译器级语义绑定。

## 安装与开始使用

1. 在 VS Code 中打开 `Extensions`。
2. 选择右上角 `... -> Install from VSIX`。
3. 选择 `fossilsense-vscode-1.5.0_BUILD*.vsix`。
4. 打开 C/C++ 或 Go 工作区，等待状态栏进入 `ready`。

默认无需配置。FossilSense 会扫描常见 C/C++ 与 `.go` 文件，并把索引保存在用户缓存目录，不会在源码仓库中生成数据库。

如果已激活的 clangd、Microsoft C/C++、ccls 或 Go 扩展与当前工作区已打开文件的语言匹配，FossilSense 会提示它可能启动重叠的语言服务。每种语言建议只保留一个主要 provider。

## 你会获得什么

- **跳转与搜索**：文档符号、工作区符号、头文件跳转，以及标准 **Go to Declaration** 和 **Go to Definition**。两种操作的含义保持稳定：在定义处再次执行 Definition 仍停留在定义，不会把它当成声明/定义切换命令；`goto` label 只在当前函数的 label namespace 内解析。
- **显式候选审阅**：**Find All Possible Definitions / Declarations** 展示默认跳转压制前的有界 variants，并附带 role、scope、linkage、guard、pairing 与 coverage 证据。
- **持续补全**：普通标识符、C/C++ include 路径、Go import 路径、当前函数参数与局部变量，以及有限的成员候选。
- **引用查找**：全词搜索后按定义、声明、调用、读、写和类型等语法角色分组。
- **Hover 与 Signature Help**：展示函数签名、注释和参数个数兼容候选；Record Hover 可展示完整的 `struct` / `class` / `union` 声明，唯一 `typedef` 链可显示 `aka`。
- **调用关系**：查看 C/C++ 与 Go 直接调用的一跳 incoming / outgoing 关系、调用点和候选证据。
- **未保存编辑感知**：当前工作区打开但尚未保存的结构化声明可以参与候选结果。
- **有限语义着色**：重点区分宏、类型、枚举量、参数和局部变量，避免大面积误着色。

FossilSense 会优先展示当前文件、include 可达文件和直接外部头中的候选，再使用全局 fallback。include 解析方式的证据强度会被保留：精确解析的边可提供强可达性；唯一后缀匹配和 ambiguous include 的所有可能目标只作为启发式证据；外部头证据也按当前查询来源判断，避免把其他文件的关系借给本次跳转或补全。有限语义着色允许这些有界启发式 include 目标参与宏、类型和枚举量的种类判定，但不会因为 include scope 处于 open 状态而放开无关的全库定义；种类证据冲突时仍不着色。exact-name 全局窗口触顶时会先抢救 Current 和强可达候选。遇到 include 缺失、语法不完整或结果被截断时，界面会保留降级、歧义或 coverage 信息，而不是假装结果完全精确。

Go 查询使用 package/import 图而不是套用 `#include`。同 package 文件共享可见性；`go.mod`、`go.work`、工作区 `vendor` 和用户明示的外部模块根提供有界依赖证据。无法解析的 import、build constraint、目标文件名约束和 `cgo` 边界会让范围保持 open 或降低置信度，不会静默删除其他候选。C/C++ 与 Go 属于不同语义家族，同名符号不会跨语言混入普通查询。

`1.5.0` 将 Go 的文档/工作区符号、Declaration/Definition、Find All、普通/局部/成员/import 补全、Hover、References、Signature Help、Semantic Tokens 和 Call Hierarchy 接入统一候选服务。Go package、import、build guard、声明、方法、字段、局部绑定和直接调用事实都写入与 C/C++ 共用的 typed read model；公开结果仍是 best-effort 候选，不声称完成编译器级绑定。

## 符号从哪里来，为什么补全分两段

FossilSense 的 parser 会从 C/C++ 与 Go 源码的容错 tree-sitter 语法树中提取声明；局部语法错误仍使用 AST，只有 parser 无法形成任何可用结构时才启用保守、补全专用的词法 fallback。索引器把名称、声明/定义角色、位置、签名、链接属性或 package identity、条件 guard 和文件 revision 等 typed facts 写入本地 SQLite。Hover、跳转、Signature Help、Find All 和 workspace symbol 都通过同一个候选服务读取这些事实，并叠加 include/package 可达性、项目范围和当前未保存文档，因此它们不会各自维护一套“符号真相”。

普通补全列表必须跟随每次键入即时响应，所以它先走一条只包含名称、种类、路径、作用域信号和稳定 declaration ID 的紧凑内存索引；这一步只负责快速召回，不加载全库的完整声明。选中候选、解析补全详情时，会带着同一个 ID 回到上述候选服务，水合与 Hover/跳转相同的声明事实。分开的只是高频召回路径，不是语义规则：补全详情中的签名、角色、位置和注释仍以统一事实与当前未保存内容为准。

C++ 记录类型中的方法名会作为 function-kind 名称进入普通标识符补全召回。这是有意的宽召回：它让没有接收者上下文时仍可发现方法拼写，但不代表 FossilSense 已经完成接收者绑定；`.` / `->` 成员补全仍使用独立的记录类型证据过滤候选。

## 常用命令

打开命令面板并输入 `FossilSense`：

| 命令 | 用途 |
|---|---|
| `Start Server` / `Stop Server` | 启动或停止当前工作区服务 |
| `Refresh Index` | 增量处理发生变化的文件 |
| `Full Rebuild Index` | 强制重新扫描并建立完整索引 |
| `Find All Possible Definitions / Declarations` | 查看默认跳转压制前的有界候选与不确定性证据 |
| `Find References (Grouped by Role)` | 按语法角色查看引用候选 |
| `Analyse Call Hierarchy` | 查看 C/C++ 或 Go 直接调用的 incoming / outgoing 关系和调用点；打开 Relations 面板后可切换 incoming/outgoing、刷新并逐条查看调用点与候选证据 |
| `Select Project Context` | 选择自动识别的项目范围或关闭项目证据 |
| `Clear Completion History` | 清除当前工作区的本地补全历史 |

## 可选配置

在工作区根目录创建 `fossilsense.json`，可以限制扫描范围，或加入外部头文件与 Go 模块目录：

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp", "go"],
  "includePaths": ["C:/toolchain/include"],
  "goModulePaths": ["D:/shared/device-module"],
  "languageOverrides": [
    { "glob": "legacy-c/**/*.h", "language": "c" },
    { "glob": "generated/cpp/**/*.h", "language": "cpp" },
    { "glob": "generated/go/**/*.inc", "language": "go" }
  ]
}
```

- `include` / `exclude` 控制工作区内哪些目录参与索引。
- `extensions` 控制识别的源码扩展名。
- `includePaths` 指向工作区外的 SDK 或工具链头文件目录，必须使用绝对路径。
- `goModulePaths` 指向明示的外部 Go module 根。每个根独立受文件数和字节数上限约束；不会自动扫描 GOPATH 或本机 module cache。根目录应包含 `go.mod`，否则仍可有界索引声明，但 module import path 证据可能不完整。
- `languageOverrides` 接受 `c` / `cpp` / `go`。匹配不区分大小写；工作区文件按规范化的 `/` 相对路径匹配，外部文件按规范化绝对路径匹配；多条规则命中时最后一条生效。无效规则会产生 warning，但不会丢弃其他有效配置。

语言默认值为：`.c` 使用 C；`.h/.inl/.cpp/.hpp/.cc/.hh/.cxx/.hxx` 使用 C++；`.go` 使用 Go；配置额外加入的未知扩展名仍按 C 处理。磁盘索引和未保存文档使用同一个语言判定器，不读取编辑器 `languageId` 作为另一套事实来源。

配置缺失时扫描整个工作区的默认源码类型；配置错误时会显示 warning 并降级到安全默认值。

VS Code 设置中常用的选项：

- `fossilsense.mode`：`auto`、`on` 或 `off`。
- `fossilsense.includePaths`：额外的外部头文件目录。
- `fossilsense.goModulePaths`：额外的明示外部 Go module 根；与 `fossilsense.json` 合并并使用相同的有界扫描规则。
- `fossilsense.completion.prefixRanking`：默认 `strict`，优先精确名和字面前缀；`scopeFirst` 更重视作用域证据。
- `fossilsense.projectContext.mode`：自动项目证据、歧义时询问或关闭。
- `fossilsense.semanticColoring.mode`：启用或关闭 FossilSense 着色。
- `fossilsense.includeScoping.mode`：限制 `#include` 可达性范围。`auto` 时着色与补全只接受当前文件 include 图可达的定义、直接外部头和有界启发式 include 目标，排除无关的全库定义；`off` 回到全库行为。
- `fossilsense.resourceMonitor.enabled`：在状态栏显示 FossilSense 进程内存和索引数据库磁盘占用，默认开启，每 5 秒刷新；关闭仅隐藏状态栏，不影响服务行为。
- `fossilsense.semanticIndex.memoryBudgetMB`：声明语义索引的总内存目标。常驻的紧凑补全召回索引先占用预算，剩余部分缓存 Hover、跳转和补全详情共享的声明 payload；设为 `0` 仍保留召回索引，并按需从本地数据库读取选中的事实。

## 能力边界

FossilSense 不支持完整的 C++ 继承、模板、重载决议、宏展开、访问控制、命名空间绑定或复杂表达式类型推断。成员调用、函数指针和 callable object 也不会被伪装成已经精确绑定的自由函数关系。

Go 后端不执行接口动态派发、泛型实例化、嵌入成员提升、方法集证明或表达式类型推断。selector、同名方法、函数值和间接调用在证据不足时保留多个候选或 fallback。FossilSense 不调用 Go 工具链；build constraint 与文件名中的 GOOS/GOARCH 只作为可见 guard 和排序/coverage 证据，当前没有 active target 选择。`import "C"` 会显示 unsupported language boundary，但不会推断 Go/C 跨语言绑定。

声明、Hover、跳转、着色、文档符号和调用关系只接受 AST 事实。轻量扫描始终只负责 `#include`；只有 AST 完全不可用时才产生隔离、最低优先级且不可跳转的补全提示。这类提示不进入声明表、语义候选服务或文档解析。

只要 tree-sitter 仍能形成可用树，即使树中带有语法错误，FossilSense 也保留 Partial AST 路径，不把整份文档切换到词法声明扫描。尚未形成受支持声明节点、只落在 `ERROR` 区域中的名字可能暂时不参与声明、导航、着色或补全；这样做是为了避免给半写完的文本制造错误的 canonical identity。编辑恢复出可识别结构后，这些事实会随下一次解析出现。

索引日志使用 `declarations` 表示 canonical declaration 数量。为兼容现有 VS Code 扩展，索引进度通知的 JSON 字段暂时仍名为 `symbols`，但其值同样是 canonical declaration 数量，不再代表旧的正则 symbol 记录。

引用是文本候选加语法角色分类，可能包含注释或字符串中的同名文本。导航与补全索引会区分声明、C tentative definition、完整定义和无法判定的声明/定义，并在同级候选中使用这些角色；这仍不是编译器级的链接决议。

函数声明和定义只有在规范化签名、链接属性和 include 证据足够时才会配对。C 函数签名比较会忽略参数名和无关的独立 `extern`，但仍保留参数类型等形状差异；歧义或证据不足时会保留多个普通候选或 fallback，而不是猜测唯一答案。

Find All 是有界发现入口，不是编译器或链接器结果。它会明确显示当前 `limit`、open/truncated/incomplete coverage；跨 workspace root 的无限全集、完整宏状态、C++ ABI/`extern "C"` 绑定和真实 C/C++ 或 Go build target 选择仍不支持。

这些限制是产品选择：在缺少编译参数的仓库里，稳定、可解释的候选比错误的“唯一答案”更有价值。

## 隐私

索引、补全历史和查询都在本机完成。FossilSense 不上传源码，不发送匿名 telemetry，不做 cloud sync，也不使用云端 ML ranker。补全历史只保存在当前工作区本地缓存中，可随时关闭或清除。

贡献、编译、测试和打包方法见 [AGENTS.md](AGENTS.md)。
