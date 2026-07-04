# FossilSense

FossilSense 是一个面向大型 Windows C/C++ 工作区的 **best-effort 代码导航与分析工具**。它解决的不是“如何替代 clangd / IntelliSense”，而是在很多真实旧工程里，编译模型本身就很难建立：

- 没有可靠的 `compile_commands.json`。
- 仓库很大，构建链复杂，宏和平台分支很多。
- SDK、固件、内核、老代码、vendored 第三方代码混在一起。
- 用户真正需要的是先能跳转、搜索、补全和理解代码，而不是先修完整构建系统。

因此 FossilSense 的基本假设是：**没有可靠编译环境是基本盘，不是降级场景**。

它不会把文本匹配或启发式结果伪装成编译级语义绑定。FossilSense 提供的是带有 `tier`、`confidence`、`reason` 的候选结果：当前文件、include 可达文件、外部头文件、全局 fallback、歧义 include 等状态都应该被显式表达出来。

当前版本事实：

- Rust engine crate：`crates/fossilsense/Cargo.toml`，版本 `1.1.0`。
- VS Code 扩展 / VSIX：`extensions/vscode/package.json`，版本 `1.1.0`。
- VSIX 是可安装交付物，会把 Rust 原生二进制一起打进扩展包。

## 产品定位

在 C/C++ 项目里，精确语言服务通常依赖编译参数。没有 include path、宏定义、目标平台、编译选项时，工具很难知道一个名字到底绑定到哪个声明。

FossilSense 选择的路线是另一种：它不要求先拿到完整编译模型，而是先建立一个可容错的索引层。这个索引层基于 tree-sitter 和词法 fallback 收集符号、引用候选、include 关系、record / field / alias 等信息，然后在查询时根据当前文件、include 可达性和路径局部性排序。

这意味着 FossilSense 更像是大型 C/C++ 仓库里的“SourceInsight 风格导航工具”，而不是一个完整 C++ 语义分析器。它适合用在以下场景：

- 嵌入式、固件、驱动、内核、交叉编译项目。
- 老仓库或内部仓库，构建脚本复杂到很难被编辑器复用。
- Windows 工作区，代码量很大，但用户希望打开后立刻可用。
- clangd / IntelliSense 当前能工作得很好时，FossilSense 不是更好的选择。

> **候选不是绑定**
>
> FossilSense 的跳转、补全、引用、hover 和着色都属于 best-effort candidate。它会尽量把候选排序得有用，但不会声称自己完成了编译器级名称解析。这里的关键是把“不确定性”暴露出来，而不是藏起来。

## 架构

FossilSense 由一个 VS Code 扩展和一个 Rust 原生二进制组成：

```text
VS Code 扩展 (TypeScript, extensions/vscode)
        |
        | LSP over stdio
        v
fossilsense 单一 Rust 原生二进制 (crates/fossilsense)
  - CLI: scan / index
  - LSP: lsp
```

VS Code 扩展负责启动 server、桥接配置、注册命令和显示状态。真正的扫描、解析、索引、SQLite 存储、查询和 LSP 服务都在 Rust 二进制里完成。

二进制查找顺序：

1. `fossilsense.serverPath`
2. 扩展内置 `bin/`
3. 仓库 `target/release` 或 `target/debug`

这种结构的结果是，VSIX 可以做到自包含安装：用户不需要额外装 ctags、cscope、clangd 或 Rust 工具链。

## 当前能力

FossilSense 当前主要提供以下能力：

- **工作区符号**：从持久 SQLite 索引中查询函数、宏、类型、枚举常量、全局变量等候选。
- **文档大纲**：对当前 C/C++ 文件提供结构化符号视图。
- **跳转定义候选**：按 current / reachable / external / global 等 scope tier 排序。
- **Find All References**：基于 whole-word 文本命中，再用语法角色分组为 definition / declaration / call / write / type / read。
- **轻量补全**：基于索引和当前文件词表，支持前缀、短前缀降噪、截断后持续 re-query。
- **成员补全**：对 `.` / `->` 做 C-oriented 的 degraded field completion。
- **Signature Help**：在简单函数调用中展示索引到的函数签名候选。
- **Rich Hover**：展示候选签名、路径、tier / confidence / reason，以及可恢复的 Doxygen 或普通前置注释。
- **语义着色**：只给宏、类型、枚举常量做有限着色，其它内容交给 TextMate。
- **include 分析**：支持 `#include` 路径补全、跳转头文件、有限 include 可达性排序。
- **外部头文件索引**：通过 `fossilsense.includePaths` 指向 SDK / toolchain include 目录。
- **增量索引**：文件事件合并、debounce、手动 refresh、full rebuild。
- **CLI 检查**：`scan` / `index` 可用于无编辑器调试和发布验证。

这些能力都遵循同一个原则：能给候选就给候选，能标注不确定性就标注不确定性；解析失败或配置缺口应该降级，而不是让整个工具不可用。

## Include 与可达性

对于大仓库而言，单纯全局名字匹配很容易产生噪声。FossilSense 因此引入了有限的 include 可达性分析。

当 `a.c` include 了 `foo.h`，而 `foo.h` 又能解析到工作区内唯一文件时，FossilSense 可以把 `foo.h` 里的定义看作对 `a.c` 更可信。补全和跳转会优先显示这类 reachable candidate。

但 include 在 C/C++ 里本身可能是不确定的：

- include 路径可能解析不到。
- suffix 匹配可能命中多个同名头。
- 遍历 include 图可能达到深度或节点上限。
- 条件编译和宏展开没有被执行。

在这种情况下，FossilSense 会把 scope 标成 open，并通过 `OpenReason` 暴露原因。补全会继续给候选，但不会假装 include 图已经闭合。

`fossilsense.includeScoping.mode` 控制这套逻辑：

| 值 | 行为 |
|---|---|
| `"auto"` | 默认。根据 include 可达集收窄着色和排序；不确定时回退到更开放行为。 |
| `"off"` | 关闭 include scoping，回到全局索引行为。 |

> **着色与补全的差异**
>
> 着色更保守。若可达性不确定，宁可不把某个名字着成“已证明可达”。补全则更偏向召回，会继续给候选，只是在排序和标签上表达可信度。

## 外部头文件(`includePaths`)

`fossilsense.includePaths` 用来告诉 FossilSense：哪些目录是外部参考头文件，例如 MinGW、TDM-GCC、SDK 或厂商库 include 目录。

它和 `fossilsense.json` 里的 `include` 不是一个概念：

- `include` / `exclude` 用来选择工作区内哪些文件进入索引。
- `includePaths` 用来指向工作区外的头文件参考目录。

外部头文件的作用是提供 `#include` 路径补全、跳转头文件和有限符号候选。外部符号可搜索、可补全，但一般排在工作区符号之后。被工作区文件直接 include 的第一层外部头也可以参与语义着色；传递 include 的外部头只作为导航参考。

FossilSense 不编译外部头，因此错平台头文件不会造成编译错误。目录缺失、重复、不可访问或过大时，会 warning 后跳过或降级为仅路径解析。

## 配置(`fossilsense.json`)

可以在工作区根目录放一个可选的 `fossilsense.json`：

```json
{
  "include": ["src/", "include/"],
  "exclude": ["src/generated/"],
  "extensions": ["c", "h", "cpp", "hpp"],
  "includePaths": ["C:/TDM-GCC-64/x86_64-w64-mingw32/include"]
}
```

所有字段都是可选的。没有配置文件时，默认扫描整个仓库里的常见 C/C++ 源文件。

| 字段 | 类型 | 默认值 | 含义 |
|---|---|---|---|
| `include` | `string[]` | `[]` | 限定工作区扫描子树。非空时只扫描这些目录。 |
| `exclude` | `string[]` | `[]` | 排除目录，叠加 `.gitignore` 和内置排除规则。 |
| `extensions` | `string[]` | `["c","h","cpp","hpp","cc","hh","cxx","hxx","inl"]` | 进入索引的文件扩展名。 |
| `includePaths` | `string[]` | `[]` | 外部头文件参考目录，要求绝对路径。 |

路径按仓库相对路径处理，统一使用 `/`。`include` / `exclude` 使用 segment-boundary 前缀匹配，因此 `"src"` 会匹配 `src/a.c`，但不会匹配 `src_gen/b.c`。

配置文件新增、修改或删除后，会触发自动 reindex。坏 JSON 或字段类型错误会回退默认值，并在输出面板 / 状态栏中提示。

## VS Code 命令

| 命令 | 说明 |
|---|---|
| `FossilSense: Start Server` | 手动启动 language server。 |
| `FossilSense: Stop Server` | 停止 language server。 |
| `FossilSense: Refresh Index` | 增量刷新索引，跳过未变化文件。 |
| `FossilSense: Full Rebuild Index` | 强制全量重新扫描和索引。 |
| `FossilSense: Find References (Grouped by Role)` | 按语法角色查看引用候选。 |

状态栏会显示索引阶段：

```text
discovering -> checking -> parsing -> indexing -> finalizing -> ready
```

失败时状态为 `failed`。当配置回退或存在 warning 时，状态栏会显示提示标记。

## 补全

FossilSense 的补全是 index-level / text-level 的，不是完整语义补全。

主标识符补全来自两类数据：

- 索引里的函数、宏、类型、枚举常量、全局变量等符号。
- 当前打开文件里的 identifier word。

补全结果始终标记为 `isIncomplete = true`。这意味着 VS Code 会随着每次输入重新请求完整前缀，避免 top-N 截断后长名字永远回不来的问题。

短前缀会更保守：

- 前缀长度 `< 3` 时，只接受 exact、prefix 和词边界子串，避免噪声长尾。
- 前缀长度 `>= 3` 时，恢复更完整的 fuzzy recall。

非当前文件的索引候选会在 detail / documentation 中标注 `reachable`、`external`、`global`、`ambiguous` 等信息。当前文件候选不额外标注，避免 UI 噪声。

## 成员补全(`.` / `->`)

成员补全是一个明确降级的能力，目前偏向 C 的 struct / union field 场景。

当光标位于 `p->` 或 `x.` 后面时，FossilSense 会尝试从当前文件里的简单声明推断 receiver 的 record 类型，例如：

```c
struct Foo *p;
p->
```

如果能猜到 `Foo`，则从索引中查找 `Foo` 的字段，即使字段定义在其它文件里也可以作为候选。若猜不到 receiver 类型，则回退到全局字段名前缀候选。

它不做完整表达式类型推断，因此以下场景不会被精确支持：

- `get()->`
- `a.b.`
- 继承、模板、命名空间、访问控制。
- C++ 静态成员、重载、表达式结果类型。

这类结果仍然是 candidate，不是类型系统结论。

## Hover 与 Signature Help

Hover 的目标是让用户快速看到“这个名字有哪些可信候选”。它会展示：

- 候选类型和路径。
- stored signature。
- `tier`、`confidence`、`reason`。
- 紧贴候选前面的 Doxygen 或普通注释，能恢复就渲染为 Markdown。

Signature Help 也遵循同样原则：在简单函数调用中按 exact-name 找函数声明 / 定义候选，并根据 include reachability 排序。它不会做 overload resolution、参数类型匹配、模板推导、命名空间解析、函数式宏展开或函数指针目标推断。

如果签名太复杂，参数无法安全切分，则显示整体签名，不伪造参数标签。

## Find All References

FossilSense 的引用查找是 **text candidate search**，不是语义引用解析。

它先对光标下的 identifier 做区分大小写的 whole-word 搜索，再对命中位置做 best-effort 语法角色分类：

- definition
- declaration
- call
- write
- type
- read

普通 VS Code References 面板只显示位置，不显示角色。若要看角色分组，使用 `FossilSense: Find References (Grouped by Role)`。

结果默认最多返回 2000 条。解析失败的文件会把角色降级为 `read`，而不是中断整个引用搜索。

## 语义着色

FossilSense 的语义着色范围故意很窄，只处理 TextMate 最容易分错、而索引又相对能判断的几类名字：

- 宏。
- typedef / struct / enum / union / class 类型名。
- 枚举常量。

函数、变量、参数、局部变量、struct / union 字段都不着色，交给编辑器原有语法高亮。

如果一个名字在索引中有多种含义，FossilSense 会选择最常见的可着色 kind；完全平票时不着色。这意味着某些 wrapper macro 或同名符号可能会被误着色，这是可接受的 best-effort 取舍。

## 与其它 C/C++ 工具共存

FossilSense 可以和 clangd、Microsoft C/C++、CMake Tools 同时安装，但一个工作区最好只有一个主要 C/C++ language provider。

`fossilsense.mode` 控制整体 server：

| 值 | 行为 |
|---|---|
| `"auto"` | 默认。启动 FossilSense，并在检测到 clangd / cpptools / ccls 时给一次互斥提示。 |
| `"on"` | 启动 FossilSense，但不显示互斥提示。 |
| `"off"` | 不启动 FossilSense。 |

如果 clangd 或 IntelliSense 已经能正确建立项目模型，它们仍然是更精确的工具。FossilSense 更适合无法稳定建立编译模型的工作区。

## 构建与检查

仓库根目录：

```powershell
cargo build
cargo test
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/release-check-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/release-check-mini.sqlite
```

VS Code 扩展：

```powershell
cd extensions/vscode
pnpm install
pnpm compile
```

运行扩展开发宿主时，打开本仓库，按 `F5`，再在 Extension Development Host 中打开 `samples/mini-c`，执行 `FossilSense: Start Server`。

## 打包 VSIX

每个对外发布版本都必须产出可直接安装的 `.vsix`。这是 FossilSense 的硬性交付物。

```powershell
cd extensions/vscode
pnpm install
pnpm run package
```

`pnpm run package` 会执行以下动作：

1. `cargo build --release -p fossilsense`
2. 复制 `target/release/fossilsense(.exe)` 到 `extensions/vscode/bin/`
3. 编译扩展入口 `out/extension.js`
4. 通过 `vsce package --no-dependencies` 生成 VSIX

产物位于仓库根目录 `dist/`：

```text
dist/fossilsense-vscode-<version>_BUILD<YYYYMMDD_HHMMSS>.vsix
```

安装方式：

```powershell
code --install-extension dist/<name>.vsix
```

或在 VS Code 扩展面板中选择 `... -> Install from VSIX`。

## 当前不做

这些限制不是遗漏，而是当前产品边界：

- 不自写 C/C++ 解析器。
- 不捆绑 GPL 的 ctags。
- 不在 VS Code 扩展宿主里跑索引。
- 不把 best-effort 名字候选伪装成精确语义绑定。
- 不实现完整 C++ 语义，包括继承、重载、模板、命名空间、访问控制等。

本质上，FossilSense 选择的是另一条工程路线：在编译模型缺失时，先提供稳定、可解释、可降级的导航能力。它不追求“像编译器一样正确”，而是追求在复杂旧工程里打开就能开始工作，并且让用户知道每个结果为什么排在这里。
