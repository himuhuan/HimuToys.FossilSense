# FossilSense

FossilSense 是一款专为大型、复杂的 C/C++ 工作区设计的“尽力而为（best-effort）”代码导航与分析工具。

它的目标**不是**替代 clangd 或 IntelliSense，而是去拯救那些**连编译模型都很难建立的真实旧工程**。在这些工程里，你通常会遇到：

* 根本拿不到可靠的 `compile_commands.json`。
* 仓库极其庞大，构建链错综复杂，充斥着各种宏和平台分支。
* SDK、底层固件、内核代码、祖传老代码和第三方库（vendored）全部杂糅在一起。
* 作为开发者，你当下的诉求是“先能跳转、搜索、补全和看懂代码”，而不是被迫先去修好整个构建系统。

基于此，FossilSense 的核心设计哲学是：**将“没有可靠的编译环境”视为常态，而非异常降级场景。**

它绝不会把简单的“文本匹配”伪装成“编译器级别的语义绑定”。FossilSense 提供的所有结果都会显式附带 `tier`（层级）、`confidence`（置信度）和 `reason`（原因），坦诚地告诉你当前结果是来自当前文件、include 链路、外部头文件，还是全局兜底（fallback）。

**📦 当前版本构建信息：**

* Rust 引擎 (crate)：`crates/fossilsense/Cargo.toml`，版本 `1.4.3`
* VS Code 扩展 (VSIX)：`extensions/vscode/package.json`，版本 `1.4.3`
* *注：VSIX 是最终交付物，已内置编译好的 Rust 原生二进制，真正做到开箱即用。*

**v1.4.3 hotfix：** 修复 schema 16 全量构建在 `finalizing` 阶段错误回收
canonical/presentation signature 字符串的问题。该错误会触发大规模 SQLite 外键扫描，
使 U-Boot 等大型工作区长时间无法进入 ready；修复后仍保持 schema 16 和既有语义能力，
无需引入新的索引格式迁移。

---

## 🎯 产品定位：VS Code 里的现代化 Source Insight

在 C/C++ 项目中，精确的语言服务强依赖于完整的编译参数（include 路径、宏、目标平台等）。一旦缺失这些，传统工具就会彻底罢工。

FossilSense 选择了另一条路：**不强求完整的编译模型，而是建立一个具备高容错能力的索引层**。
它底层基于 `tree-sitter` 和词法 fallback 来收集符号、引用候选及 include 关系，再在查询时根据文件局部性和 include 可达性为你排序。

简单来说，它就像是大型 C/C++ 仓库里的 **“Source Insight 风格导航工具”**，非常适合以下场景：

* 嵌入式、固件、驱动、内核及交叉编译项目。
* 构建脚本极其复杂、编辑器难以复用的历史遗留仓库或内部大库。
* 动辄几百万行代码的 Windows 工作区，且要求“打开代码就能立刻看”。
* *(注意：如果你的项目能完美运行 clangd 或 IntelliSense，FossilSense 并不是更好的选择。)*

> 💡 **核心原则：提供候选，但不瞎承诺（候选不是绑定）**
> FossilSense 提供的跳转、补全、引用和悬停提示，都是“尽力而为的候选结果”。我们会尽量把最靠谱的排在前面，但从不宣称自己做了编译器级别的名称解析。**把“不确定性”直白地展示给用户，好过藏着掖着。**

---

## 🏗️ 极简架构与开箱即用

FossilSense 采用前后端分离的极简架构：

```text
VS Code 扩展 (TypeScript, UI 层)
        |
        | LSP (标准输入输出)
        v
FossilSense 核心引擎 (单一 Rust 原生二进制)
  ├─ CLI 模式: 扫码、索引、调试
  └─ LSP 模式: 提供语言服务

```

**自包含，零依赖：** 扫描、解析、SQLite 存储、查询等所有重活都在 Rust 二进制中完成。用户安装 VSIX 扩展后即可直接使用，**不需要**额外安装 ctags、cscope、clangd 或 Rust 工具链。

---

## ✨ 核心能力

FossilSense 坚守“能给候选就给，能标明不确定就标明”的原则，即使解析失败也会优雅降级，绝不让工具彻底瘫痪：

* **⚡ 闪电般的工作区符号搜索：** 基于持久化的 SQLite 索引，秒级查询函数、宏、类型、枚举、全局变量。
* **🗺️ 结构化文档大纲：** 清晰展示当前 C/C++ 文件的内部结构。
* **🎯 智能跳转定义：** 按 当前文件 > 可达文件 > 外部头文件 > 全局兜底 的优先级智能排序。
* **🔍 语法感知的引用查找：** 先做精准的全词文本匹配，再按语法角色（定义/声明/调用/读/写/类型）对结果进行智能分组。
* **⌨️ 降噪与持续补全：** 结合全局索引、当前函数参数/局部变量、当前文件词表，以及低置信的 C/C++ 常用关键词和内置类型兜底候选，短前缀智能降噪，长前缀模糊匹配。
* **📁 智能项目上下文：** 根据活动文件最近的构建标记目录，为普通标识符补全增加有界的同项目召回和排序证据；可从状态栏自动、手动选择或完全关闭。
* **🧩 尽力而为的成员补全（`.` / `->`）：** 根据当前声明或窄范围的弱 receiver 线索，返回字段和 C++ 方法候选，支持跨文件 evidence；链式访问可识别数组下标、括号和解引用形态，例如 `a.mem1[n].`、`arr[i].`、`(*ptr).inner.`，对匿名嵌套 `struct/union` 也会生成可继续解析的 record evidence。
* **📝 统一的函数 Hover、Definition 与 Signature Help：** 三者共享 exact-name callable candidate 链，展示候选签名、来源路径、tier/confidence/reason，并按实参个数（arity）保留兼容候选；注释仍支持 Doxygen 或普通注释。
* **🧱 完整类型 Hover：** `struct` / `class` / `union` 使用精确 record 源码范围展示完整声明；`typedef` 在唯一可解释时附加 `aka`，同时暴露别名链的置信度和降级原因。
* **🎨 极简语义着色：** 对 TextMate 容易分错的“宏、类型名、枚举常量”以及当前函数参数/局部变量进行着色，其余交还给编辑器，避免花里胡哨和误导。
* **🔗 Include 智能分析：** 支持 `#include` 路径补全、文件跳转，以及基于 Include 的有限可达性排序。

### 补全如何排序

普通标识符补全会经过统一的 completion 模块，合并来自不同通道的证据、去重、排序再截断。默认的 `fossilsense.completion.prefixRanking = strict` 会先按 `精确名称 > 大小写不敏感的字面前缀 > 模糊匹配` 分档，再在同档内使用文件局部性、include 可达性、来源、意图、项目和历史证据排序。下划线按字面处理，因此搜索 `wns_ipc` 时，`wns_ipc_send` 会排在需要跨越额外 `_` 的 `wns__ipc_rsp_init` 前面，即使后者来自 External scope。需要旧行为时可设为 `scopeFirst`，让 scope/evidence 重新优先于名字匹配档位。该设置只作用于普通标识符补全，不改变 workspace symbol、跳转、include/member 补全、引用或着色。

同一名字匹配档内，文件局部性和 include 可达性作为“软先验”参与，并通过 guard band 阻止低置信度的全局、语言内置或文本噪音轻易反超更可信的候选。同名候选（无论来自索引、局部绑定、同工作区未同步文档的结构化 overlay、语言内置候选还是文本）会被合并为一个可解释的条目，原始文本兜底仍明确标注为 `text`，绝不伪装成语义定义。

为了保证候选池的代表性，索引召回采用有界的多通道策略：在 当前/局部、可达、外部、未知/开放作用域、全局和文本证据之间各保留一定份额，再交给统一 ranker 重排，避免单一通道霸占结果。在此基础上，补全还会根据上下文做轻量的意图判断（type、expression、call、macro preprocessor、declaration-name），把更贴合当前语境的候选类型往前排。**但意图只是排序证据**，不会做 C/C++ 类型推断、重载解析或语义绑定，也从不硬过滤掉候选。

当工作区还没有索引到标准头或外部 include 路径未配置时，普通补全也会提供一小组静态 C/C++ 语言候选，例如 `struct`、`sizeof`、`size_t`、`uint32_t`、`NULL`。这些条目只作为补全兜底：显示为 `keyword`、`builtin type` 或 `builtin constant`，不创建索引记录，不参与跳转定义、workspace symbol 或语义着色，也不会自动插入 include。

### 项目上下文如何工作

FossilSense 会把常见的源码侧构建文件当作项目标记：Make/GNUmake、`CMakeLists.txt`、QMake `*.pro`、Ninja 主文件 `build.ninja`、Visual Studio solution/project、`meson.build` 和 Bazel BUILD/WORKSPACE 主文件。任意 `.mk`、`.pri`、非主 `.ninja`、`compile_commands.json` 和构建 cache 不会成为项目根。发现过程尊重 `.gitignore`、`fossilsense.json` 范围和默认 `build` / `out` / `target` 排除，因此常见生成目录里的 Ninja 文件不会污染列表。

自动模式从补全请求文件向上选择最近的 marker 目录；嵌套项目选择最近者，同目录多个 marker 合并。状态栏可以选择 **Current Project (Auto)**、任意发现项目或 **Unspecified**。同项目只是普通标识符补全的软证据：跨项目候选不会消失，更强的当前文件/include/intent/history 证据仍可胜出；它不改变跳转、引用、着色、Hover、Signature Help、成员补全或 include 补全，也不解析构建文件内容。

`fossilsense.projectContext.mode` 支持 `auto`（默认）、`promptOnAmbiguous`（有可选项目但活动 C/C++ 文件无法自动归属时，每文件/会话提示一次）和 `off`。显式选择只保存在 VS Code workspace state。选择 **Unspecified**、配置 `off`、没有祖先 marker 或项目模型不可用时，补全严格回到没有项目证据的原有召回与排序；发现失败会在状态中标为 degraded，但不会让补全失败。

Include 路径补全保留 quote/angle 既有的搜索顺序优先级，并叠加同目录、兄弟/组件 include 边、最近使用、basename 频率和路径深度等二级证据，让最可能想要的头文件更靠前。

### 局部绑定与未保存编辑

当光标位于函数体内时，普通补全会纳入当前函数参数和声明早于光标的局部变量。这些候选来自当前打开文档的容错解析，**可覆盖未保存的编辑**。它们是 best-effort 的局部绑定提示，不是完整的 C/C++ block-scope 或模板/宏语义解析。解析失败、无法确认函数边界或 declarator 不清晰时，会自动回退到已有索引候选和当前文件词表。

同工作区所有未同步 open documents 里的宏、typedef、枚举常量、函数声明/定义和 record/type 定义，都会作为结构化 overlay evidence 参与普通补全；dirty path 会遮蔽该路径的旧索引行。当前函数参数、局部变量和附近 identifier 词表仍只从当前请求文档提取，不会被其它打开文件误当成本地作用域证据。

### 函数候选、arity 与 `.h/.c` counterpart

Hover、Go to Definition、Signature Help、函数补全文档和 Call Hierarchy 统一读取同一代 schema 16 callable facts，并在请求期叠加所有未同步 open-document overlay。调用点按 exact-name 召回候选；可可靠计算实参个数时，参数个数兼容的签名排在前面，不兼容项不会冒充匹配。Definition 在调用点优先源文件定义；光标位于函数声明或定义锚点时，若不存在严格 counterpart，也仍返回当前锚点，而不是空结果。

头文件声明与源文件定义只有在规范化签名完全一致、外部链接、source 到 header 的 include reach 闭合，且声明到定义、定义到声明两个方向都恰好一对一时才建立 counterpart。`ProjectKey` 不再是绑定门槛。开放或不完整的 include 图、未保存删除造成的 fact 缺口、同名一对多/多对一以及不支持的 declarator 都会停止严格配对，并保留普通候选及明确的 ambiguity/fallback 解释。

正式支持的调用形态是 C/C++ 自由函数的直接名、显式限定名和括号包裹名。成员调用、函数指针、callable object、宏展开、模板和参数类型重载绑定仍不支持；它们不会被伪装成自由函数绑定。Call Hierarchy 的标准与富结果也复用同一 counterpart group，仍保持 source-body-first。

### Record 与 typedef Hover

命中 `struct` / `class` / `union` 时，Hover 从精确 record anchor 读取有界源码片段，能够展示多行声明，而不再依赖单行符号签名。命中 `typedef` 时，会有界递归 alias 链；仅当当前 tier 收敛为唯一目标时显示 `aka`。歧义、环、unsupported declarator、超限、文件修订不一致或读取失败都会降级为原始签名，并标明 confidence/reason；不会猜出一个唯一类型。C++ `using T = ...` 的 alias trace/`aka` 尚不支持。

### 成员补全的范围

成员补全统一基于 member evidence，可返回字段、类/结构体内第一版方法，以及简单的 `Owner::method` 证据。弱 receiver 推断只覆盖明确声明和唯一名字相关这类窄范围，并通过 detail/documentation 标出置信度。

它仍不是完整的 C++ 类型绑定：**不做继承、重载、模板、命名空间、访问控制或表达式类型推断**，也不解析函数调用结果、复杂 cast 或宏展开。当链式解析失败时，会按前缀走全局 member fallback，保证你总能拿到候选，而不是空白。

### 本地补全历史

接受补全后，FossilSense 会在本机、当前 workspace 的缓存中记录一条匿名证据（candidate hash、kind、intent、prefix bucket），作为有上限的排序正反馈，让常用候选更靠前。它只记录正反馈，可在设置中关闭，也可通过命令一键清除；禁用或清除后，补全回到默认的排序行为。

**隐私边界明确：** 不上传任何 completion history，不做匿名 telemetry、cloud sync、ML ranker 或自动 include 插入。verbose/perf 日志也只输出分阶段耗时、来源/返回计数、intent bucket、recall channel counts 等聚合指标，**不输出候选名、accepted label、include path 或源码片段**。

---

## 🧠 Include 链路与可达性分析

在大仓库中，全局名字匹配往往会搜出一堆重名的噪音。为此，FossilSense 引入了**有限的 include 可达性分析**。

如果 `a.c` 包含了 `foo.h`，FossilSense 在为 `a.c` 提供补全和跳转时，会优先展示 `foo.h` 里的内容（即可达候选）。
但 C/C++ 的 `#include` 本身充满玄学（路径找不到、同名文件、条件编译未执行等）。当遇到这些死胡同时，FossilSense 会将作用域标记为 `open`，并在界面上暴露出 `OpenReason`。**它会继续给你提供候选词，但绝不假装自己完全看懂了 include 关系。**

*(通过 `fossilsense.includeScoping.mode` 可在 `auto` 和 `off` 之间切换该行为)。*

---

## ⚙️ 配置文件 (`fossilsense.json`)

一般情况下，无需配置即可扫描整个仓库。如有需要，可在根目录放置 `fossilsense.json`：

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp"],
  "includePaths": ["C:/TDM-GCC-64/x86_64-w64-mingw32/include"]
}

```

*说明：*

* **`include` / `exclude**`：决定**工作区内**哪些文件进入索引数据库。（前缀匹配，如 `"src"` 匹配 `src/a.c`，不匹配 `src_gen/b.c`）。
* **`includePaths`**：告诉引擎**工作区外**的 SDK 或编译器头文件在哪里（需绝对路径）。外部头文件不参与编译，不会报错，仅用于提供路径补全、跳转和有限的符号参考。

> 💡 配置文件修改后，引擎会自动触发增量重建。如果配错了，工具会友善地回退到默认值，并在状态栏提示你。

---

## 🛠️ 常用 VS Code 命令

按下 `Ctrl+Shift+P` (或 `Cmd+Shift+P`)，输入 `FossilSense`:

| 命令 | 作用 |
| --- | --- |
| `Start Server` / `Stop Server` | 手动启停语言服务进程。 |
| `Refresh Index` | 增量刷新索引（仅处理有变化的文件）。 |
| `Full Rebuild Index` | 强制清空并全量重新扫描和索引。 |
| `Find References (Grouped by Role)` | 查找引用，并按“读/写/调用”等语法角色分类展示。 |
| `Analyse Call Hierarchy` | 在 Relation Panel 打开 incoming/outgoing 关系与调用点/证据双视图；当前正式解析 C/C++ 自由函数。大结果按 scan/page/site budget 明确标为 partial，有稳定下一页时可继续加载。 |
| `Clear Completion History` | 清除当前 workspace 的本地补全接受历史。 |
| `Select Project Context` | 选择自动项目、某个发现项目或 `Unspecified`，无需重启或重建索引。 |

*状态栏会显示引擎阶段（discovering -> checking -> parsing -> indexing -> finalizing -> ready）以及当前 Auto / manual / Unspecified / Off 项目上下文。*

---

## 🤝 与其他 C/C++ 工具共存

一个工作区最好只有一个主要语言服务。FossilSense 会智能检测环境：

* 默认 (`auto` 模式)：如果检测到你同时开着 clangd 或 cpptools，会弹窗给你一次互斥提示。
* 如果官方工具已经能完美解析你的项目，请继续使用它们；**如果它们卡死了、解析失败了，欢迎切回 FossilSense**。

---

## 🚫 明确的产品边界（当前不做）

FossilSense 的克制是刻意为之的工程取舍。为了保证在“烂工程”里的绝对稳定和响应速度，我们目前**绝不**做以下事情：

* 不自己手写 C/C++ 解析器。
* 不捆绑具有 GPL 传染性的 ctags。
* 不在 VS Code 的 Node.js 进程里跑繁重的索引（保证编辑器不卡顿）。
* **不把“尽力而为”的候选结果，伪装成“百分之百准确”的语义绑定。**
* 不去死磕完整的 C++ 语义（如继承、重载、模板推导、命名空间、访问控制、表达式类型推断等）。
* 不上传 completion history，不做匿名 telemetry、cloud sync、ML ranker 或自动 include 插入。

**FossilSense 不追求“像编译器一样严谨”，它追求的是：在一个乱成一锅粥的旧工程里，你用 VS Code 打开它，立刻就能开始干活，并且清楚地知道每一个跳转结果是怎么来的。**

---

### 💻 构建与打包指南 (写给贡献者)

**Rust 引擎验证：**

```bash
cargo build && cargo test
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/mini.sqlite --force
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_architecture_fitness.ps1

```

**VS Code 扩展开发：**

```bash
cd extensions/vscode
pnpm install && pnpm compile

```

*(在 VS Code 中按 `F5` 启动调试宿主，打开 `samples/mini-c` 即可测试。)*

**打包对外发布的 VSIX (硬性交付物)：**

```bash
cd extensions/vscode
pnpm run package

```

该命令会自动编译 Rust Release 产物、组装扩展并生成独立无依赖的 `.vsix` 文件至 `dist/` 目录下。发布收尾还应运行：

```bash
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_release_hardening.ps1
```

该门禁会确认版本号、release notes、`dist/fossilsense-vscode-1.4.3_BUILD*.vsix`、VSIX 内的 `extension/bin/fossilsense.exe`，并核对包内 release-input SHA-256 与当前源码。交付说明必须精确记录最终 VSIX 文件名、VSIX SHA-256、release-input SHA-256 和打包时 source commit；打包后再改 Rust/扩展/打包关键输入会使旧包失效。直接在 VS Code 中选择 `Install from VSIX` 即可安装。
