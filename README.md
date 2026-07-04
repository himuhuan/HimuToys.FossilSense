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

* Rust 引擎 (crate)：`crates/fossilsense/Cargo.toml`，版本 `1.1.0`
* VS Code 扩展 (VSIX)：`extensions/vscode/package.json`，版本 `1.1.0`
* *注：VSIX 是最终交付物，已内置编译好的 Rust 原生二进制，真正做到开箱即用。*

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
* **🎯 智能跳转定义：** 按当前文件 > 可达文件 > 外部头文件 > 全局兜底 的优先级智能排序。
* **🔍 语法感知的引用查找：** 先进行精准的全词文本匹配，再按语法角色（定义/声明/调用/读/写/类型）对结果进行智能分组。
* **⌨️ 降噪与持续补全：** 结合全局索引、当前函数参数/局部变量和当前文件词表，短前缀智能降噪，长前缀模糊匹配。
* **🧩 尽力而为的成员补全（`.` / `->`）：** 专为 C 语言结构体场景优化，即使跨文件也能根据当前声明推断字段候选。
* **📝 信息丰富的 Hover & Signature Help：** 展示候选签名、来源路径、置信度，并自动提取和渲染代码中的 Doxygen 或普通注释。
* **🎨 极简语义着色：** 只对 TextMate 容易分错的“宏、类型名、枚举常量”进行着色，其余交还给编辑器，避免花里胡哨和误导。
* **🔗 Include 智能分析：** 支持 `#include` 路径补全、文件跳转，以及基于 Include 的有限可达性排序。

普通标识符补全会在光标位于函数体内时，有限纳入当前函数参数和声明早于光标的局部变量。这些候选来自当前 open document 的容错解析，可覆盖未保存编辑；它们是 best-effort 局部绑定提示，不是完整 C/C++ block-scope 或模板/宏语义解析。解析失败、无法确认函数边界或 declarator 不清晰时，会回退到已有索引候选和当前文件词表。

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

*状态栏会实时显示引擎的当前工作阶段（discovering -> checking -> parsing -> indexing -> finalizing -> ready），让你对进度心里有数。*

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
* 不去死磕完整的 C++ 语义（如复杂的继承、重载、模板推导、命名空间等）。

**FossilSense 不追求“像编译器一样严谨”，它追求的是：在一个乱成一锅粥的旧工程里，你用 VS Code 打开它，立刻就能开始干活，并且清楚地知道每一个跳转结果是怎么来的。**

---

### 💻 构建与打包指南 (写给贡献者)

**Rust 引擎验证：**

```bash
cargo build && cargo test
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/release-check-mini.sqlite --force

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

该命令会自动编译 Rust Release 产物、组装扩展并生成独立无依赖的 `.vsix` 文件至 `dist/` 目录下。直接在 VS Code 中选择 `Install from VSIX` 即可安装。
