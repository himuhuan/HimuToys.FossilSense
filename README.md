# FossilSense

FossilSense 是一款面向大型、难以构建的 C/C++ 与 Go 仓库的 VS Code 代码导航工具。它不要求 `compile_commands.json`，也不需要额外安装 clangd、gopls、ctags、Go 或 Rust 工具链。安装一个自包含 VSIX，打开工作区后即可建立索引。

当前版本：`1.7.0`。本版本统一了 C 声明提取与容错恢复规则，让同一条语句中的多个声明、函数指针、typedef、条件编译和局部作用域保持一致的身份与原文位置。头文件结合配置与源码来源选择分析语言；存在无法解释的区域时，仍保留健康声明，并明确显示部分覆盖或未知状态。

新增只读 `query explain` 命令，可逐阶段排查声明发现过程，并区分磁盘修改、旧索引、召回截断与默认展示省略。固定 C 样本通过人工契约和 Clang 独立对照验证；这是开发验收能力，安装与使用仍不需要 Clang。protobuf-c 来源追溯继续默认关闭，Go 后端继续作为实验能力提供。FossilSense 不展开任意宏，也不声称具备完整 C++ 编译器语义。

## 什么时候适合使用

FossilSense 主要解决一种很实际的问题：代码就在眼前，但完整编译环境很难还原。

它适合嵌入式、固件、驱动、内核、旧代码、跨平台分支很多的仓库，以及包含大量第三方 SDK 的大型 Windows 工作区。你可以先获得可用的跳转、搜索、补全和代码关系，再决定是否值得修复整套构建系统。

如果项目已经能被 clangd 或 IntelliSense 精确解析，继续使用它们通常更合适。FossilSense 提供的是 **best-effort 候选**，不是编译器级语义绑定。

## 安装与开始使用

1. 在 VS Code 中打开 `Extensions`。
2. 选择右上角 `... -> Install from VSIX`。
3. 选择 `fossilsense-vscode-1.7.0_BUILD*.vsix`。
4. 打开 C/C++ 或 Go 工作区，等待状态栏进入 `ready`。

默认无需配置。FossilSense 会扫描常见 C/C++ 与 `.go` 文件，并把索引保存在用户缓存目录，不会在源码仓库中生成数据库。

如果已激活的 clangd、Microsoft C/C++、ccls 或 Go 扩展与当前工作区已打开文件的语言匹配，FossilSense 会提示它可能启动重叠的语言服务。每种语言建议只保留一个主要 provider。

## 你会获得什么

- **跳转与搜索**：文档符号、工作区符号、头文件跳转，以及标准 **Go to Declaration** 和 **Go to Definition**。两种操作的含义保持稳定：在定义处再次执行 Definition 仍停留在定义，不会把它当成声明/定义切换命令；`goto` label 只在当前函数的 label namespace 内解析。
- **显式候选审阅**：**Find All Possible Definitions / Declarations** 展示默认跳转压制前的有界 variants，并附带 role、scope、linkage、guard、pairing 与 coverage 证据。
- **持续补全**：普通标识符、C/C++ include 路径、Go import 路径、当前函数参数与局部变量，以及有限的成员候选。
- **引用查找**：全词搜索后按定义、声明、调用、读、写和类型等语法角色分组。
- **Hover 与 Signature Help**：展示函数签名、注释和参数个数兼容候选；Record Hover 可展示完整的 `struct` / `class` / `union` 声明，唯一 `typedef` 链可显示 `aka`。可选的 protobuf-c 来源追溯会在生成类型悬停下方列出匹配的 proto 文件、声明行和匹配依据。
- **调用关系**：查看 C/C++ 与 Go 直接调用的一跳 incoming / outgoing 关系、调用点和候选证据。
- **未保存编辑感知**：当前工作区打开但尚未保存的结构化声明可以参与候选结果。
- **有限语义着色**：重点区分宏、类型、枚举量、参数和局部变量，避免大面积误着色。

FossilSense 会优先展示当前文件、include 可达文件和直接外部头中的候选，再使用全局 fallback。include 解析方式的证据强度会被保留：精确解析的边可提供强可达性；唯一后缀匹配和 ambiguous include 的所有可能目标只作为启发式证据；外部头证据也按当前查询来源判断，避免把其他文件的关系借给本次跳转或补全。有限语义着色允许这些有界启发式 include 目标参与宏、类型和枚举量的种类判定，但不会因为 include scope 处于 open 状态而放开无关的全库定义；种类证据冲突时仍不着色。exact-name 全局窗口触顶时会先抢救 Current 和强可达候选。遇到 include 缺失、语法不完整或结果被截断时，界面会保留降级、歧义或 coverage 信息，而不是假装结果完全精确。

Go 查询使用 package/import 图而不是套用 `#include`。同 package 文件共享可见性；`go.mod`、`go.work`、工作区 `vendor` 和用户明示的外部模块根提供有界依赖证据。无法解析的 import、build constraint、目标文件名约束和 `cgo` 边界会让范围保持 open 或降低置信度，不会静默删除其他候选。C/C++ 与 Go 属于不同语义家族，同名符号不会跨语言混入普通查询。

`1.5.0` 将 Go 的文档/工作区符号、Declaration/Definition、Find All、普通/局部/成员/import 补全、Hover、References、Signature Help、Semantic Tokens 和 Call Hierarchy 接入统一候选服务。Go package、import、build guard、声明、方法、字段、局部绑定和直接调用事实都写入与 C/C++ 共用的 typed read model；公开结果仍是 best-effort 候选，不声称完成编译器级绑定。

当参数或局部对象的所属类型有明确证据时，字段与直接方法名称的悬停、定义跳转和声明跳转共用成员事实，定位成员自己的位置；字段悬停的注释来自同一文件修订。支持普通对象、指针、有限成员链、成员声明，以及 `struct S s = {.state = 1}` 这类简单初始化器。Go 目前限于有证据的同 package 类型；数组下标、复杂初始化器和未能证明所属类型的表达式不做可靠跳转，成员补全仍可提供探索候选。方法只返回已有证据的位置，尚不保证已找到对应实现。升级后旧索引会自动重新建立，以保存成员类型的命名空间信息。

## 符号从哪里来，为什么补全分两段

跳转与悬停会先判断光标处是变量、类型、标签还是成员，并共用当前文档的解析结果。已经确定查找范围但无法解析时，会返回空结果；注释和字符串中的名称也不会触发普通符号跳转。成员所属类型尚未证明时，不提供默认成员跳转。需要更广泛地探索同名候选时，可使用 **Find All Possible Definitions / Declarations**；成员补全仍保留其候选提示。

FossilSense 的 parser 会从 C/C++ 与 Go 源码的容错 tree-sitter 语法树中提取声明；局部语法错误仍使用 AST，只有 parser 无法形成任何可用结构时才启用保守、补全专用的词法 fallback。该降级路径会把能够安全识别的简单 C 全局变量声明中的多个名称分别保留为最低优先级补全提示，但仍不为它们生成可跳转的声明身份。对明确按 C 解析的文件，同一条声明中的多个函数、对象、函数指针和 `typedef` 会逐项建立事实；返回函数指针的函数、字段函数指针及数组/指针结合顺序也按各自 declarator 判定，不会由同条声明中的兄弟或参数名代替外层实体名。单条声明最多处理 256 个直接 declarator，单个结构链最多处理 128 层；超限、畸形或尚不支持的结构不会被猜成确定实体，已经安全解码的兄弟仍会保留。文件级 `struct`、`union` 和 `enum` 前置声明会作为声明事实保存，并与后续完整定义分别用于 Declaration 和 Definition；普通类型使用和函数内前置 tag 不会被这条规则提升到全局。函数体内定义的 C 枚举值、record 和 `typedef` 只参与当前文档的局部查询，不写入工作区索引。函数、对象、宏、record、`typedef`、枚举量和成员会保留同一份条件编译证据；`#elif`、`#else` 和嵌套分支保持不同候选，FossilSense 不求解条件真假，也不会据此猜测唯一结果。单条条件证据最多保留 128 层和 8 KiB，超限会显示为未知。GNU C 中独立的 `__attribute__((weak))` 不再把同一函数的声明与定义拆成不同身份；展示签名仍保留源码属性，ABI 属性不会被忽略。按 C 解析的 protobuf-c 生成头文件支持位于 `struct` 与真实类型名之间的单 token 导出宏，字段和类型定义不会因此错归到导出宏名下；独占一行的 `PROTOBUF_C__BEGIN_DECLS` 和 `PROTOBUF_C__END_DECLS` 也不会干扰紧随其后的首个 `typedef`。这些 protobuf-c 规则与 C/C++ 装饰恢复共用同一份等长文本，换行和原始位置不会移动；若编辑冲突、越过声明边界、改变目标外健康声明或超过固定预算，parser 会拒绝该轮恢复并保留最后可接受结果。恢复得到的声明仍标记为降级事实，不会因为重解析没有语法错误就升级为确定结果。索引器把名称、声明/定义角色、位置、签名、链接属性或 package identity、条件 guard 和文件 revision 等 typed facts 写入本地 SQLite。Hover、跳转、Signature Help、Find All 和 workspace symbol 都通过同一个候选服务读取这些事实，并叠加 include/package 可达性、项目范围和当前未保存文档，因此它们不会各自维护一套“符号真相”。

这里的局部查询范围并不等于完整的局部 record 导航：枚举值和 `typedef` 可直接参与已有局部查询；record tag 只保留带函数/块范围和 C tag 名称空间的请求事实，不会覆盖同名普通变量，本版本不承诺其完整导航。

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

在工作区根目录创建 `fossilsense.json`，可以限制扫描范围，加入外部头文件与 Go 模块目录，或选择启用 protobuf-c 来源追溯：

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp", "go"],
  "includePaths": ["C:/toolchain/include"],
  "goModulePaths": ["D:/shared/device-module"],
  "protobufC": {
    "enabled": true,
    "protoPaths": ["proto", "D:/shared/protocols"]
  },
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
- `protobufC.enabled` 默认是 `false`。启用后，`protobufC.protoPaths` 可以使用相对工作区路径或绝对路径；只有实际出现在 include 关系中的 `*.pb-c.h` 才会尝试关联。无效或超过单目录扫描上限的路径会跳过，其他有效路径继续工作。单个 proto 文件的声明提取上限为 16 MiB，并另有固定的词法单元预算；触顶时会显示状态提示，不会继续无界增长内存。
- `languageOverrides` 接受 `c` / `cpp` / `go`。匹配不区分大小写；工作区文件按规范化的 `/` 相对路径匹配，外部文件按规范化绝对路径匹配；多条规则命中时最后一条生效。无效规则会产生 warning，但不会丢弃其他有效配置。

语言默认值为：`.c` 使用 C；`.h/.inl/.cpp/.hpp/.cc/.hh/.cxx/.hxx` 使用 C++；`.go` 使用 Go；配置额外加入的未知扩展名仍按 C 处理。没有显式覆盖时，文件名以 `.pb-c.h` 结尾且源码前 64 KiB 的真实代码中出现完整的 `PROTOBUF_C__BEGIN_DECLS` 标记，会按 C 解析并启用已有的 protobuf-c 声明恢复。注释、字符串和预处理指令中的同名文本不算证据；这项解析兼容不依赖 `protobufC.enabled`。

磁盘索引和未保存文档使用同一个语言判定器，保存语言的选择来源及规则版本，不读取编辑器 `languageId` 作为另一套事实来源。显式 `languageOverrides` 始终优先；扩展名和生成文件规则属于推断。普通 `.h/.inl` 保留 C++ 默认值，并记录共享头文件可能适用于 C/C++ 的歧义；已知纯 C 目录可通过现有覆盖配置明确指定。更改语言覆盖后，即使源码未修改，也会重新解析；语言来源明确不表示声明已经达到编译器级精度。

配置缺失时扫描整个工作区的默认源码类型；配置错误时会显示 warning 并降级到安全默认值。

VS Code 设置中常用的选项：

- `fossilsense.mode`：`auto`、`on` 或 `off`。
- `fossilsense.includePaths`：额外的外部头文件目录。
- `fossilsense.goModulePaths`：额外的明示外部 Go module 根；与 `fossilsense.json` 合并并使用相同的有界扫描规则。
- `fossilsense.protobufC.enabled`：显式启用或关闭 protobuf-c 来源追溯。编辑器中显式设置的值优先于项目配置；未显式设置时继承 `fossilsense.json`。
- `fossilsense.protobufC.protoPaths`：额外的 proto 绝对目录；与项目配置合并、规范化并去重。
- `fossilsense.completion.prefixRanking`：默认 `strict`，优先精确名和字面前缀；`scopeFirst` 更重视作用域证据。
- `fossilsense.projectContext.mode`：自动项目证据、歧义时询问或关闭。
- `fossilsense.semanticColoring.mode`：启用或关闭 FossilSense 着色。
- `fossilsense.includeScoping.mode`：限制 `#include` 可达性范围。`auto` 时着色与补全只接受当前文件 include 图可达的定义、直接外部头和有界启发式 include 目标，排除无关的全库定义；`off` 回到全库行为。
- `fossilsense.resourceMonitor.enabled`：在状态栏显示 FossilSense 进程内存和索引数据库磁盘占用，默认开启，每 2 秒刷新；悬停可查看代码名称索引、声明详情缓存、文件关系图、打开文档等大类。名称索引还会归并显示名称字符串、路径/项目、召回 posting 和固定开销。它们是结构估算，用于解释趋势；Windows Private Bytes 或 Linux/macOS RSS 才是发布门禁依据。旧服务器未提供细分字段时，扩展继续显示原有摘要。关闭仅隐藏状态栏，不影响服务行为。
- `fossilsense.semanticIndex.memoryBudgetMB`：声明语义索引的总内存目标。常驻的紧凑补全召回索引先占用预算，剩余部分缓存 Hover、跳转和补全详情共享的声明 payload；设为 `0` 仍保留召回索引，并按需从本地数据库读取选中的事实。

完整索引、增量索引、读取模型生成、名称压实和项目上下文刷新共用一个进程级构建额度，首版同一时间只运行一个重型构建。资源不足时状态栏会显示 `waiting for resources`，每个工作区只保留最新待处理目标；资源释放后自动重试。新的保存会合并到待处理 dirty 集合，并可协作取消已经过期的后台名称压实；关闭服务器或移除工作区也会停止等待和可取消阶段。等待期间旧索引仍继续响应 Hover、跳转和补全，成功构建的新数据库与读取模型只在最终校验后一起切换。

索引解析采用按字节预约的并行波次，并在写入后释放该轮事实。可用内存减少时会缩小批次并降低解析并发，旧解析线程完全退出后才建立更小的线程池。只有超出预测额度的文件需要独占重试，同批已经成功的文件不会重复解析。资源不足或输入在读取期间变化时，本次更新失败，已发布索引继续可用。后台整理的重试间隔不会阻止新编辑进入构建队列。预约是内部估算，不能视为整个进程的内存硬上限。

重型构建共享 256 MiB 临时预留，使用 512 MiB 进程压力目标并保留 32 MiB 未归因空间。这里的预留是准入估算，不是操作系统分配器的硬上限；`semanticIndex.memoryBudgetMB` 仍只表示声明名称索引和详情缓存的局部目标，不包含运行时、文件关系图、打开文档或新旧两代并存的进程峰值。Windows 以 Private Bytes、Linux/macOS 以 RSS 作为完整进程门禁；采样不可用会明确标记，不能按 0 内存放行。

部分大型仓库在旧索引继续服务时，可能因内存准入不足而无法完成完整重建。失败后旧索引仍可查询，但仓库级结果可能停留在旧版本；一次独立索引成功不代表同样的仓库能完成热重建。

少量保存积累的名称增量优先单独整理，并共享原有主体索引；大范围替换或删除仍会触发完整整理。

## 能力边界

FossilSense 不支持完整的 C++ 继承、模板、重载决议、宏展开、访问控制、命名空间绑定或复杂表达式类型推断。成员调用、函数指针和 callable object 也不会被伪装成已经精确绑定的自由函数关系。

Go 后端不执行接口动态派发、泛型实例化、嵌入成员提升、方法集证明或表达式类型推断。selector、同名方法、函数值和间接调用在证据不足时保留多个候选或 fallback。FossilSense 不调用 Go 工具链；build constraint 与文件名中的 GOOS/GOARCH 只作为可见 guard 和排序/coverage 证据，当前没有 active target 选择。`import "C"` 会显示 unsupported language boundary，但不会推断 Go/C 跨语言绑定。

protobuf-c 来源追溯只识别 proto 的 `package`、顶层或嵌套 `message` 和 `enum` 声明，并保留多个合理候选。它不分析字段、枚举成员、`service`、`oneof`、`map`、选项、导入关系或生成函数；不监听 proto 目录，也不读取未保存的 proto 内容。proto 修改后需要重新建立索引。“转到定义”仍指向生成的 C/C++ 声明。

声明、Hover、跳转、着色、文档符号和调用关系只接受 AST 事实。轻量扫描始终只负责 `#include`；只有 AST 完全不可用时才产生隔离、最低优先级且不可跳转的补全提示。这类提示不进入声明表、语义候选服务或文档解析。

C 声明解析使用固定样本核对完整声明集合、角色、作用域和原文位置，并在明确编译配置下与固定版本 Clang 独立对照。此验证只覆盖样本及其配置，不代表真实仓库中的所有宏和分支均已验证；安装和使用仍不依赖 Clang。生成的 protobuf-c 头文件中，声明边界宏、结构体导出宏和对齐属性可在同一文件内组合恢复，并保留邻近健康声明的位置与可信度。

排查某个名字为什么没有出现时，可以运行 VSIX 内置引擎或自行构建的引擎：`fossilsense query explain <workspace> <file> <name> [--line N --col N] [--db PATH] [--json]`。文件必须是工作区相对路径，不接受上级路径或指向工作区外的链接；位置采用从 1 开始的行号和 UTF-16 列号，两个参数必须同时提供。未指定位置时，命令列出最多 64 处同名位置并提示歧义。它分别报告文件纳入、语言选择、区域覆盖、解析事实、持久化、召回和展示阶段，并区分已找到、不存在、部分覆盖、未知、未执行与截断。磁盘内容或语言配置与索引不一致时会标明过期，不把当前解析结果与旧索引串成错误的因果结论。

该命令只读取已保存的磁盘文件和一个固定的数据库快照，不读取编辑器未保存内容，不修改配置、迁移数据库或重建索引。持久化后的诊断复用 `query def` 的持久声明查询与展示规则；字段和局部绑定会显示本次解析事实，并注明它们使用不同的持久化或请求路径。每个事实阶段最多输出 256 条事实，区域详情最多 256 条，整份 JSON 最多 2 MiB；生产精确名称召回仍保持 256 个候选，未进入召回窗口的项目不会被误称为“展示省略”。超过 16 MiB 的源文件停止本次磁盘分析并标记预算不足。正常完成诊断（包括未找到、无索引或截断）退出 0；参数或路径越界退出 2；实际读取或运行错误退出 1。

声明是否可用与声明是否收集完整是两个不同状态。即使文件存在语法错误或未知声明宏，已经识别的健康声明仍可用于查询；未展开的宏不会凭参数猜测生成名称。悬停信息和 Find All 的查询结果会保留声明覆盖状态：已检查完整、部分覆盖或未知，并说明语法恢复失败、未知宏、语言歧义、暂不支持的声明形式或预算耗尽。未请求的事实和旧索引缺少的覆盖证据不会被当作完整。

覆盖核对每文件最多检查 4,096 个候选区域和 262,144 个语法节点，最多保存 1,024 个缺口；触顶会标记截断。缺口保留原始字节和 UTF-16 位置，未保存文档的状态会随当前文档版本更新，不会混用旧索引。普通补全列表不读取这些详细缺口。Find All 的 LSP 命令参数可选 `includeCoverageDetails: true`，按需返回当前文件最多 64 个缺口及其文档版本、内容指纹和截断状态。局部 C 类型定义以及语法树未识别为声明的类型别名括号写法仍可能不产生持久声明，此时保留缺口说明；完整覆盖也不等同于编译器级唯一绑定。

只要 tree-sitter 仍能形成可用树，即使树中带有语法错误，FossilSense 也保留 Partial AST 路径，不把整份文档切换到词法声明扫描。尚未形成受支持声明节点、只落在 `ERROR` 区域中的名字可能暂时不参与声明、导航、着色或补全；这样做是为了避免给半写完的文本制造错误的 canonical identity。编辑恢复出可识别结构后，这些事实会随下一次解析出现。

索引日志使用 `declarations` 表示 canonical declaration 数量。为兼容现有 VS Code 扩展，索引进度通知的 JSON 字段暂时仍名为 `symbols`，但其值同样是 canonical declaration 数量，不再代表旧的正则 symbol 记录。

引用是文本候选加语法角色分类，可能包含注释或字符串中的同名文本。导航与补全索引会区分声明、C tentative definition、完整定义和无法判定的声明/定义，并在同级候选中使用这些角色；这仍不是编译器级的链接决议。

函数声明和定义只有在规范化签名、链接属性和 include 证据足够时才会配对。C 函数签名比较会忽略参数名和无关的独立 `extern`，但仍保留参数类型等形状差异；歧义或证据不足时会保留多个普通候选或 fallback，而不是猜测唯一答案。

Find All 是有界发现入口，不是编译器或链接器结果。它会明确显示当前 `limit`、open/truncated/incomplete coverage；跨 workspace root 的无限全集、完整宏状态、C++ ABI/`extern "C"` 绑定和真实 C/C++ 或 Go build target 选择仍不支持。

这些限制是产品选择：在缺少编译参数的仓库里，稳定、可解释的候选比错误的“唯一答案”更有价值。

## 隐私

索引、补全历史和查询都在本机完成。FossilSense 不上传源码，不发送匿名 telemetry，不做 cloud sync，也不使用云端 ML ranker。补全历史只保存在当前工作区本地缓存中，可随时关闭或清除。

贡献、编译、测试和打包方法见 [AGENTS.md](AGENTS.md)。
