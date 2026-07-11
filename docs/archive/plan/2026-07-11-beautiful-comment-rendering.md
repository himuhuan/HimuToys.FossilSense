# 美观注释提取与渲染实现计划

> 状态：implemented
>
> 适用分支：`feature/beautiful-comment-render`
>
> 本文已落地并归档。当前用户可见能力以 `CLAUDE.md` 与 `extensions/vscode/README.md` 为准。

## 目标

FossilSense 当前的 Rich Hover 已经能够从候选源码附近恢复一部分注释，但现有实现把**注释归属、注释清洗、Doxygen/XML 识别和 Markdown 渲染**集中在 `query/hover.rs` 中。tag 目前主要被处理为行内代码，同行尾注释也无法可靠挂载，因此继续叠加字符串规则会让容错行为越来越难解释。

本次工作的目标是建立一条独立的注释处理链：

```text
源码 + 符号锚点
    -> CommentExtractor
    -> RawComment
    -> CommentParser
    -> CommentDocument
    -> CommentMarkdownRenderer
    -> Hover Markdown
```

这条处理链需要满足以下结果：

- 一般注释可以来自符号上一行、符号前的同行块注释，或者声明后的同行注释。
- `/** ... */` 的结构性星号不会泄漏到正文中。
- Doxygen 和 XML 注释被解析为协议无关的中间结构，不直接在解析阶段拼接 Markdown。
- 参数、返回值由专用 handler 渲染；未知 tag 由统一 fallback handler 渲染。
- 普通文本、tag 正文和参数描述在最终 Hover 中保持原有换行。
- 不完整或不规范注释只降低结构化程度，不应让整个 Hover 消失。
- 现有候选排序、signature、`tier/confidence/reason` 和源码读取预算保持不变。

## 本次范围

### 纳入范围

- 紧邻符号的连续 `//`、`///`、`//!` 注释。
- 紧邻符号的 `/* ... */`、`/** ... */`、`/*! ... */` 注释。
- 声明前的同行块注释，例如 `/** docs */ bool ready;`。
- 声明后的同行 `//` 和 `/* ... */` 注释。
- Doxygen 的 `@param`、`\param`、容错形式 `/param`，以及参数方向 `[in]`、`[out]`、`[in,out]`。
- Doxygen 的 `@return`、`\return`，并容错接受 `@returns`。
- XML 的 `<param name="...">...</param>`。
- Doxygen/XML 未知 tag 的 `### Tag` 通用回退渲染。
- 普通文本、空行、tag 正文和参数描述的换行保持。
- malformed Doxygen/XML、超长注释和不可信 Markdown 内容的安全回退。
- 当前未保存文档与磁盘候选源码两条 Hover 路径。

### 不纳入范围

- 不增加新的 Hover 符号种类，不顺带实现局部变量、字段、任意表达式或完整类型语义 Hover。
- 不把注释持久化到 SQLite，不修改 schema，也不要求重建索引。
- 不实现完整 Doxygen/XML 标准，不引入完整 XML parser。
- 不实现 `@see` 跳转、符号链接、URL 激活、继承文档或声明/定义文档合并。
- 不校验 `@param` 名字是否真实存在于函数签名。
- 不改变 Hover 候选排名、候选数量、置信度模型和签名展示。
- 不使用 Markdown table、复杂 HTML/CSS 或新的用户配置项。
- 不新增第三方依赖。

> **关于 `<summary>`**
>
> 本计划按当前需求把 `<summary>` 视为普通的非特殊 tag，因此通过 fallback 渲染为 `### Summary` 和正文。以后如果希望 `<summary>` 只解包成普通文本，可以新增一个 handler，不需要修改提取器和 parser。

## 行为合同

### 注释归属优先级

当一个符号附近同时存在多种注释时，按以下顺序选择一个最可信的注释块：

1. 声明后的同行注释。
2. 声明前的同行块注释。
3. 与声明无空行间隔的上一行连续注释块。
4. signature 中残留的前置注释，仅作为兼容回退。

同行尾注释不能只依赖 `CandidateRange.end_col`。当前 lexical symbol range 可能覆盖整行，而且列使用 UTF-16 语义，直接当字节下标会在非 ASCII 源码中产生错误。因此提取器应进行轻量词法扫描，并把 range 列只当作辅助信息。

同行尾注释至少需要满足：

- comment delimiter 位于字符串和字符字面量之外。
- 注释之前存在当前目标标识符。
- 目标标识符与注释之间存在声明终止证据，例如 `;`。
- 一行存在多个声明且无法证明注释属于当前符号时，不挂载该注释。

空行仍然阻断上一行注释的挂载。文件头、copyright、license、`@file` 等现有排除规则继续保留，但普通注释不再因为超过六行就整段丢弃；达到预算后应执行可见截断。

### 清洗合同

清洗阶段只去掉注释语法，不理解 tag：

- 去掉 `//`、`///`、`//!`。
- 去掉 `/*`、`/**`、`/*!` 和对应的 `*/`。
- 多行块注释每行只去掉一个 margin decoration `*`。
- 正文自身的第二个 `*` 必须保留，因此 Markdown 列表不能被误删。
- 只清除首尾由注释语法产生的空行，正文内部空行保持原位。
- CRLF 与 LF 可以统一为 `\n`，但渲染后的视觉行边界必须一致。

例如：

```cpp
size_t db_size; /** cache size in database */
```

清洗后必须是：

```text
cache size in database
```

而不是：

```text
* cache size in database
```

### 解析合同

`CommentParser` 输出 `CommentDocument`，而不是 Markdown。建议模型如下：

```rust
pub struct CommentDocument {
    pub blocks: Vec<CommentBlock>,
    pub diagnostics: CommentDiagnostics,
}

pub enum CommentBlock {
    Text(TextBlock),
    Tag(TagBlock),
}

pub struct TagBlock {
    pub canonical_name: String,
    pub raw_name: String,
    pub syntax: TagSyntax,
    pub attributes: Vec<TagAttribute>,
    pub lines: Vec<String>,
    pub raw: String,
}
```

`CommentDiagnostics` 至少记录：

- 是否使用 malformed fallback。
- 是否因行数或字符数预算被截断。
- 是否遇到未闭合 XML tag。

这些诊断首版不要求展示给用户，但必须可单测，并为以后 debug 日志或 UI 标注保留接口。

结构化 Doxygen command 只在一行的首个非空白位置识别，避免把邮箱、路径和正文中的 `@name` 误认为 tag。tag 的正文持续到下一个同级 tag，期间的空行和换行全部保留。

XML 只解析注释渲染需要的容错子集：

- 支持单行和多行 paired tag。
- 支持读取 `<param name="...">` 的 `name` attribute。
- 未闭合 tag 尽量消费后续相邻正文。
- 无法恢复结构时保留 raw text，不能静默丢弃。
- 嵌套和复杂 XML 不做完整语义解析，不能识别的部分作为转义后的正文。

### 渲染合同

`CommentMarkdownRenderer` 使用 handler chain：

```text
ParameterTagRenderer
    -> ReturnTagRenderer
    -> FallbackTagRenderer
```

接口需要允许以后插入 `SeeTagRenderer` 等 handler。fallback handler 必须始终存在，未知 tag 不得被丢弃。

参数使用紧凑列表，不使用 table：

```md
### Parameters

- `db_size` — cache size in database
- `flush` *(in)* — whether cached data should be flushed
```

返回值渲染为：

```md
### Returns

The current cache size.
```

未知 tag 渲染为：

```md
### Warning

The cache is not synchronized.
```

连续参数可以聚合到一个 `### Parameters` 下，但不能为了聚合而改变它们与其它正文/tag 的源顺序。非连续参数块可以分别渲染，优先保证顺序可解释。

正文不能只使用普通 `\n` 期待 Markdown renderer 保留换行。统一的 `MarkdownWriter` 应：

- 对普通连续文本行输出 Markdown hard break。
- 对源空行输出段落边界。
- 对列表项的多行描述输出缩进续行或显式 hard break。
- 对用户正文中的 Markdown 控制字符执行转义。
- 不允许正文中的 ````、`# heading` 或 raw HTML 破坏外层 Hover 结构。
- 在 block 边界截断并追加明确的省略标记，不能从生成后的 Markdown 中间直接切字符。

## 目标代码结构

建议将注释能力从 `query/hover.rs` 拆出：

```text
crates/fossilsense/src/query/
├── hover.rs
└── comments/
    ├── mod.rs
    ├── extract.rs
    ├── model.rs
    ├── parse.rs
    ├── markdown.rs
    └── tests.rs
```

模块职责：

| 模块 | 职责 |
|---|---|
| `extract.rs` | 注释定位、归属判断、轻量词法扫描、原始 span 和 placement |
| `model.rs` | `CommentAnchor`、`RawComment`、`CommentDocument`、tag 和 diagnostics |
| `parse.rs` | marker 清洗、Doxygen/XML 容错解析、纯文本回退 |
| `markdown.rs` | handler chain、fallback、MarkdownWriter、安全转义和预算截断 |
| `tests.rs` | 提取/解析/渲染的表驱动和 golden tests |
| `hover.rs` | 候选排名、signature/evidence 外壳和 comments 模块调用 |

建议对外暴露一个窄入口：

```rust
pub fn comment_markdown_for_symbol(
    source: &str,
    anchor: &CommentAnchor,
    options: &CommentRenderOptions,
) -> Option<RenderedComment>;
```

`RenderedComment` 可以携带 Markdown 和 diagnostics。LSP 层只消费 Markdown，不依赖 parser 内部 block。

## 分步实施与验收

## Step 0：固定基线和新增失败用例

### 实施

在改动生产代码前，先为现有必须保留的行为建立 characterization tests，并为新需求增加当前会失败的测试。

需要固定的旧行为：

- 上一行 `///` 和 `/** */` 能挂载到符号。
- 注释与符号之间存在空行时不挂载。
- 上一条声明后的块注释不能挂载到下一符号。
- 文件头和 `@file` 注释不挂载到第一个符号。
- 当前 open document 使用未保存文本恢复注释。
- 读不到候选文件或文件过大时仍显示 signature。
- signature、header guard、candidate evidence 展示保持不变。

新增失败用例至少覆盖：

- `size_t db_size; // cache size in database`
- `size_t db_size; /* cache size in database */`
- `size_t db_size; /** cache size in database */`
- XML `summary`。
- Doxygen/XML 参数。
- 返回值。
- 未知 tag fallback。
- 两行普通正文的视觉换行。

### 验收

运行：

```powershell
cargo test -p fossilsense query::hover
cargo test -p fossilsense server::hover
```

验收结果：

- 旧行为测试全部通过。
- 新需求测试以明确断言失败，而不是 panic、hang 或依赖本地环境失败。
- 每个失败测试只描述一个缺失能力，后续实现可以逐项转绿。

## Step 1：建立 comments 模块和中间模型

### 实施

创建 `query/comments`，定义：

- `CommentAnchor`
- `CommentPlacement`
- `CommentStyle`
- `RawComment`
- `CommentDocument`
- `CommentBlock`
- `TagBlock`
- `CommentDiagnostics`
- `RenderedComment`

先把当前上一行注释提取和普通文本渲染迁入新模块，暂时不改变用户可见结果。`query/hover.rs` 只保留候选排名和 Hover section 组装。

旧的 `leading_comment_markdown(source, symbol_start_line)` 不作为长期兼容 API 保留。如果测试迁移需要短期 wrapper，完成本步骤前应删除 wrapper，避免新旧入口并存。

### 验收

运行：

```powershell
cargo test -p fossilsense query::comments
cargo test -p fossilsense query::hover
cargo test -p fossilsense server::hover
```

检查：

- 旧基线测试全部通过。
- `server/hover.rs` 不包含注释格式解析。
- `query/hover.rs` 不再包含 Doxygen/XML token 扫描函数。
- comments 模块不依赖 `tower-lsp`、`rusqlite` 或 server/store 类型。
- 没有新增第三方依赖。

## Step 2：实现可靠的注释归属和同行尾注释

### 实施

在 `extract.rs` 中实现：

- 上一行 line comment group。
- 上一行 block comment group。
- 符号前同行 block comment。
- 声明后同行 line/block comment。
- string/char literal 屏蔽。
- UTF-16 column 到 source byte 边界的安全处理，或者完全避免用列直接切片。
- 多声明同行和不确定终止位置的保守拒绝。
- placement 优先级。

提取器只返回 raw comment 和 source span，不清洗 marker，也不识别 tag。

### 验收

表驱动测试至少覆盖：

| 输入 | 预期 |
|---|---|
| `// docs\nsize_t db_size;` | 提取 leading line comment |
| `size_t db_size; // docs` | 提取 trailing line comment |
| `size_t db_size; /* docs */` | 提取 trailing block comment |
| `/** docs */ size_t db_size;` | 提取 inline-leading block comment |
| `const char *url = "http://x";` | 不提取字符串中的 `//` |
| `char c = '/';` | 不把字符字面量当注释 |
| `int old; /* old */\nint current;` | 不把 old 的注释挂到 current |
| `// docs\n\nint current;` | 空行阻断 |
| 一行多个声明且归属不明确 | 返回 None |
| 中文前缀后出现目标声明 | 不因 UTF-16/byte 混用越界 |

运行：

```powershell
cargo test -p fossilsense query::comments::tests::extract
```

验收结果：

- 所有位置类型都有明确的 `CommentPlacement`。
- malformed block comment 不 panic。
- 提取器不会扫描整工作区或执行磁盘 IO。

## Step 3：实现 marker 清洗和容错结构解析

### 实施

在 `parse.rs` 中先完成 marker-aware 清洗，再解析 block：

- 普通文本形成 `TextBlock`。
- 行首 Doxygen command 形成 `TagBlock`。
- XML paired tag 形成 `TagBlock`。
- `<param name="">` 保存 name attribute。
- tag continuation lines 保持顺序和空行。
- malformed tag 设置 diagnostics，并保留正文。

删除旧的 `highlight_doc_tags`、`next_doc_tag`、`xml_tag_end` 等直接拼接 Markdown 的实现。

### 验收

golden tests 至少验证：

```cpp
/** cache size in database */
```

解析正文为 `cache size in database`，不能有前导 `*`。

```cpp
/**
 * * first
 * * second
 */
```

解析正文仍保留两个 Markdown 列表星号。

```cpp
/// <summary>
/// cache size in database
/// </summary>
```

解析为一个 canonical name 为 `summary` 的 `TagBlock`，正文只有 `cache size in database`。

还需要覆盖：

- `@param[in] src source bytes`
- `\param dst destination bytes`
- `/param size cache size`
- `@return current size`
- `<param name="size">cache size</param>`
- 多行 XML param。
- 未闭合 XML param。
- 正文中的邮箱 `owner@example.com` 不形成 tag。
- 正文中的 `foo @warning bar` 不形成结构 tag。
- 未知 `@custom` 保留完整正文。

运行：

```powershell
cargo test -p fossilsense query::comments::tests::parse
```

验收结果：

- parser 测试只比较中间模型，不依赖 Markdown。
- malformed 输入始终能得到 Text/Tag fallback 或 None，不 panic。
- 原始行顺序和内部空行没有被压缩。

## Step 4：实现 handler chain 和美观 Markdown

### 实施

在 `markdown.rs` 中实现：

- `ParameterTagRenderer`
- `ReturnTagRenderer`
- `FallbackTagRenderer`
- `MarkdownWriter`
- Markdown text escaping。
- hard line break。
- block-aware truncation。

参数 handler 识别 Doxygen 与 XML param，并统一形成紧凑列表。返回值 handler 使用固定 `### Returns`。fallback 根据 canonical tag name 生成 `### Tag`。

### 验收

以下 Markdown 必须进行精确或 golden 对比。

普通两行文本：

```text
first line
second line
```

输出必须包含可被 VS Code Markdown 识别的 hard break，不能只依赖 soft newline。

参数：

```md
### Parameters

- `src` *(in)* — source bytes
- `dst` *(out)* — destination bytes
```

XML 参数：

```md
### Parameters

- `size` — cache size in database
```

返回值：

```md
### Returns

cache size in database
```

未知 tag：

```md
### Warning

cache is not synchronized
```

XML summary：

```md
### Summary

cache size in database
```

安全与格式测试：

- 正文中的 `# fake heading` 不生成外层标题。
- 正文中的 triple backticks 不关闭 Hover 的 signature code fence。
- raw HTML 被当作正文，不改变 Hover DOM。
- 多行参数描述不会和下一个参数挤在一起。
- 空行保持段落边界。
- 截断不会留下未闭合 code span、列表或标题。
- 未知 tag handler 一定落到 fallback。

运行：

```powershell
cargo test -p fossilsense query::comments::tests::markdown
```

## Step 5：接入 Hover 并删除旧实现

### 实施

将 comments 模块接入：

- 当前 open document candidate。
- workspace 磁盘 candidate。
- external header candidate。
- signature comment compatibility fallback。

`server/hover.rs` 继续遵守单候选源码文件 `256 KiB` 上限。读不到源码、文件过大或注释无法恢复时，继续显示 signature 和 candidate evidence。

删除：

- 旧 `leading_comment_markdown` 入口。
- 旧 tag 高亮器和 XML token 扫描器。
- 只为旧实现服务的测试。
- 普通注释六行以上整段拒绝的策略。

保留：

- `HOVER_CANDIDATE_LIMIT`。
- candidate ranking。
- signature sanitation。
- header guard 展示规则。
- 多候选之间的分隔。
- `tier/confidence/reason`。

### 验收

运行：

```powershell
cargo test -p fossilsense query::hover
cargo test -p fossilsense server::hover
cargo test -p fossilsense --test lsp_smoke
```

增加或更新 LSP smoke，至少验证：

- server 声明 `hoverProvider`。
- 当前文档的同行尾注释能够出现在 Hover Markdown。
- XML param 或 Doxygen param 的结构化结果能够经过 LSP 返回。
- source unavailable 时仍返回 signature-only Hover。

代码检查：

```powershell
rg -n "highlight_doc_tags|next_doc_tag|leading_comment_markdown" crates/fossilsense/src
```

验收结果应为没有旧生产入口残留；如果测试名中存在旧名称，也应同步改名。

## Step 6：预算、性能和降级验收

### 实施

保持请求期 top-K source hydration 模型，不增加索引期注释解析。预算建议沿用当前基线：

- 单候选源码文件最大 `256 KiB`。
- 注释最多读取 `48` 行。
- 正文预算约 `2,000` 字符。

预算应在 raw/document block 边界执行。超过预算时：

- 保留已经完成的 block。
- 对当前 block 做安全截断。
- 设置 `diagnostics.truncated`。
- 输出可见省略标记。

### 验收

测试：

- 30,000 行候选文件不会读取注释，但 signature 保留。
- 超过 48 行的普通注释不会整段消失。
- 超过字符预算的参数或未知 tag 不会破坏 Markdown。
- 四个 Hover candidates 的注释处理仍受各自预算约束。
- malformed 输入不会导致明显的超线性扫描。

运行：

```powershell
cargo test -p fossilsense
cargo fmt --all -- --check
```

如果项目当前 clippy 基线允许，再运行：

```powershell
cargo clippy -p fossilsense --all-targets -- -D warnings
```

验收结果：

- 没有请求期工作区遍历。
- 没有新增 SQLite read。
- 没有新增 schema migration。
- 没有新增全局可变 cache。
- 注释失败只降级为无注释或纯文本，不影响 signature Hover。

## Step 7：用户文档和发布门禁

### 实施

同步：

- `CLAUDE.md` 当前能力和 can/cannot/fallback。
- `extensions/vscode/README.md` 的 Rich Hover 描述。
- 必要的 release note。

文档需要明确：

- 这是 best-effort comment attachment，不是编译级文档绑定。
- 支持 leading、inline-leading 和 trailing comments。
- 支持参数、返回值和未知 tag fallback。
- malformed 或不可读源码会降级。
- 不支持 `@see` 跳转、完整 XML/Doxygen 和新增语义 Hover。

### 验收

完整门禁：

```powershell
cargo test
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_release_hardening.ps1
```

如果本次作为可安装演示版本或对外版本交付，还必须运行：

```powershell
Set-Location extensions/vscode
pnpm run package
```

并确认：

- VSIX 生成在仓库根 `dist/`。
- VSIX 内包含 release 版 `fossilsense.exe`。
- 安装后对普通、Doxygen、XML、malformed 四组样例进行手工 Hover 验收。
- 交付说明写清版本、能力范围和明确不支持项。

完成后将本文标为 implemented，并移入 `docs/archive/`，避免 `docs/plan` 成为长期 backlog。

## 端到端验收样例

### 一般注释

```cpp
// cache size in database
size_t db_size;

size_t db_size; // cache size in database

size_t db_size; /* cache size in database */
```

三种形式的注释正文都应显示为：

```text
cache size in database
```

### Doxygen 单行块注释

```cpp
size_t db_size; /** cache size in database */
```

正文不能包含结构性 `*`。

### XML summary

```cpp
/// <summary>
/// cache size in database
/// second line remains separate
/// </summary>
size_t db_size;
```

渲染结构：

```md
### Summary

cache size in database  
second line remains separate
```

### Doxygen 参数和返回值

```cpp
/**
 * @param[in] key database key
 *                encoded as UTF-8
 * @param[out] size cache size
 * @return true when the cache entry exists
 */
bool query(const char *key, size_t *size);
```

要求：

- 只出现一个连续参数区域。
- `key` 和 `size` 分别显示方向。
- `key` 的第二行描述保持为该参数的续行。
- return 形成独立的 `### Returns`。
- 参数和 return 的顺序与源码一致。

### XML 参数

```cpp
/// <param name="key">
/// database key
/// encoded as UTF-8
/// </param>
bool query(const char *key);
```

要求：

- 参数名显示为 `key`。
- 两行描述不能挤成一行。
- XML marker 不出现在最终正文。

### 未知 tag 回退

```cpp
/**
 * @warning cache access is not synchronized
 * caller must hold the database lock
 */
size_t cache_size(void);
```

要求：

- 渲染 `### Warning`。
- 两行正文都保留。
- 不要求 warning 专用样式。
- parser 不认识 warning 的附加语义也不能丢失内容。

### malformed 回退

```cpp
/// <param name="size">
/// cache size in database
size_t query_size(void);
```

要求：

- Hover 仍然返回。
- 能恢复参数时按参数显示；不能恢复时按普通 tag 或纯文本显示。
- diagnostics 标记 malformed fallback。
- signature 和 candidate evidence 不受影响。

## 完成定义

只有同时满足以下条件，本次功能才算完成：

- 提取、解析、渲染已经形成独立模块边界。
- 参数和返回值通过专用 handler，未知 tag 通过 fallback handler。
- 一般注释、Doxygen、XML 和 malformed 样例全部通过自动测试。
- 最终 Markdown 在 VS Code 中实际保持原始换行。
- 同行尾注释不会因为字符串字面量或明显歧义产生错误挂载。
- 超长、不可读或无法解析的注释能够可解释地降级。
- 旧字符串 tag 高亮实现已删除，没有新旧两条渲染路径。
- 完整 Rust 测试和 release hardening gate 通过。
- 用户可见文档已同步。
- 如果对外演示或发布，已生成并手工验证自包含 VSIX。
