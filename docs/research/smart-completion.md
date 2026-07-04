## 总体判断

FossilSense 现在已经不是一个“简单前缀补全器”，而是一个**基于索引、include 可达性和模糊召回的作用域感知补全器**。真正的优化方向不应是盲目增加模糊匹配，而是把它升级成一个**证据聚合式、意图感知的低延迟排序系统**。

我建议把目标定义为：

> 在不引入完整 clangd 级语义绑定的前提下，让用户最可能想选的候选稳定出现在 Top 1/3/5，同时保留 FossilSense 的 best-effort 特性：不消失、不假装确定、可解释、低延迟。

这意味着下一代算法不应追求“精确 C++ 语义”，而应追求“多弱信号融合后的高置信推荐”。

---

## 1. 当前设计的关键问题

### 1.1 作用域层级“严格压倒”文本质量，过于刚性

当前排序中，scope tier 严格压倒 text match。这个设计能防止全局噪声污染 Top-N，但副作用是：一个用户刚刚在当前函数、当前文件、当前编辑上下文里使用过的候选，如果只是“文本回退词”或“未被索引为当前文件符号”，可能被可达头文件里的弱相关符号压下去。

更合理的方式是把 scope 从“硬字典序主导”改成**强先验分数**：默认可达符号更高，但允许“强意图证据”反超。例如：当前函数附近出现过、刚被用户选择过、与当前语法上下文匹配、前缀精确匹配的本地候选，应当能够超过一个 merely reachable 的普通子串候选。

### 1.2 当前文件词回退太弱

当前文件单词只是文本 fallback，而不是“临时符号”。但在真实编码体验里，用户最常补的是刚定义的局部变量、参数、局部 typedef、局部宏、附近函数名和刚写过的 helper。它们不应该只获得普通文本项待遇。

我建议引入一个 **ephemeral symbol overlay**：每个打开文档实时抽取当前文件的局部声明、函数、宏、typedef/using、enum 常量、字段访问词、局部变量名，把它们作为“当前文件临时符号”参与统一排序，而不是只作为 fallback word。

### 1.3 成员补全的 recall 太窄

成员补全目前仅返回字段，不包括方法、继承成员、嵌套类型、枚举成员，也不能处理链式接收者、调用结果接收者、索引表达式。这会让用户在 `obj.` / `ptr->` 后看到的列表显得“不像 IDE”。

即使没有完整语义，也可以做 best-effort 改进：从 record index 中引入 method/member-function；用 receiver 变量名、局部声明、赋值来源、构造表达式、`auto` 初始化器做弱类型推断；把 base class 的字段/方法用低置信度并入；链式接收者只支持一跳或两跳的保守推断。

### 1.4 缺少历史与近期性

现有系统完全不使用选择历史、近期性、项目惯用 API、用户偏好。历史信号对代码补全有效已有长期研究支持：早期研究就提出用程序历史改进补全排序，并指出传统 alphabetic/pattern-only 排序会让用户找候选变慢；后续 IDE 实践也证明匿名使用日志可以训练补全排序模型并通过 A/B 验证提升用户行为指标。([Springer][1])

---

## 2. 参考成熟方案后，对 FossilSense 最有价值的借鉴

LSP 层面，服务器可以通过 `sortText` / `filterText` 影响客户端排序和过滤；`CompletionList.isIncomplete = true` 表示继续输入时需要重新计算列表；同时 `sortText`、`filterText`、`insertText`、`textEdit` 这类字段通常必须在初始 completion 响应里给出，不能等 `completionItem/resolve` 再改变。FossilSense 现在持续返回 incomplete 的策略是合理的，但下一步要控制重排抖动。([Microsoft GitHub][2])

clangd 的公开实现可以作为排序特征设计参考：它区分 symbol quality 与 relevance，质量侧包含引用数、deprecated、reserved name、符号类别等；相关性侧包含 name match、context words、file proximity、scope proximity、preferred type、main-file references 等。clangd 还保留了 DecisionForest 排序接口，并把 name match 与模型预测组合成最终分数。([代码浏览器][3])

JetBrains 的经验也很适合 FossilSense：他们把候选和上下文特征在客户端计算，匿名收集结构化日志，不收集源码本身，然后训练 CatBoost 排序模型，并用 A/B test 验证显式选择率和所需输入动作数的变化。这个路径比直接引入大模型更适合 FossilSense 的隐私、延迟和部署约束。([arXiv][4])

代码补全研究上有三条可借鉴但不应照搬的线索：Bruch 等人的 repository-example learning 证明“从已有代码库学习”能显著提升补全相关性；Raychev 等人的统计语言模型说明 API 调用序列本身有可学习模式；Aroma 的结构化代码搜索说明基于 AST/parse-tree 特征的召回能在大代码库中找到惯用结构。([Lancaster University research directory][5])

我不建议第一阶段直接上 LLM。ICSE 2024 的真实 IDE 补全评估指出，在线/离线评估并不总是一致，模型失败既来自模型能力，也来自开发上下文中的不合适使用；这更支持 FossilSense 先做“索引 + 结构信号 + 本地历史 + 轻量学习排序”的混合方案。([arXiv][6])

---

## 3. 推荐的新架构：Evidence-Aware Intent Ranking

建议把补全系统拆成五层：

```text
Context Classifier
    ↓
Candidate Retrieval
    ↓
Evidence Assembly
    ↓
Intent-aware Re-ranker
    ↓
UX Renderer + Feedback Loop
```

### 3.1 Context Classifier：轻量意图分类器

在每次补全请求时，先用 tree-sitter + lexical fallback 生成一个 `CompletionContextEvidence`：

```rust
struct CompletionContextEvidence {
    intent: IntentKind,
    intent_confidence: f32,

    prefix: String,
    trigger: TriggerKind,

    expected_kinds: SmallBitSet<SymbolKind>,
    forbidden_kinds: SmallBitSet<SymbolKind>,

    current_scope_hint: ScopeHint,
    namespace_hint: Option<String>,

    receiver: Option<ReceiverEvidence>,
    expected_type_hint: Option<TypeHint>,

    nearby_tokens_hash: u64,
    context_words: SmallVec<String>,
    in_preprocessor: bool,
    in_include: bool,
    in_type_position: bool,
    in_expression_position: bool,
    in_call_argument: bool,
}
```

`IntentKind` 不需要完整 C++ 语义，先做粗粒度即可：

| intent                 | 触发场景                          | 排序倾向                            |
| ---------------------- | ----------------------------- | ------------------------------- |
| `IncludePath`          | `#include "..."` / `<...>`    | header path、目录、近期 include       |
| `MemberAccess`         | `.` / `->`                    | receiver record 的 field/method  |
| `TypeName`             | 声明、cast、`new`、template 参数附近   | class/struct/typedef/enum       |
| `ExpressionValue`      | RHS、return、argument、condition | variable/function/constant/enum |
| `CallTarget`           | identifier 后即将 `(`            | function/macro/function-like    |
| `MacroPreprocessor`    | `#if/#ifdef/#define`          | macro                           |
| `NamespaceOrQualifier` | `::` 前后                       | namespace/type/static member    |
| `DeclarationName`      | 类型后声明新变量                      | 降低已有全局符号，提升本地命名模式/文本候选          |

这个分类器的价值很大：它能在不做完整语义绑定的情况下，让排序从“名字像不像”变成“此处想要什么”。

---

## 4. 召回层优化

### 4.1 常规标识符：从单一 Top-N 改成多通道召回 + 配额合并

当前召回已经有 exact/prefix/substr/subsequence。建议保留，但内部候选池从 100 扩大到例如 500–2000，然后最终 Top 100 输出。关键是使用**多通道召回配额**，防止某一类候选把窗口挤满：

```text
internal_pool:
  current_file_ephemeral_symbols:  up to 200
  reachable_index_symbols:         up to 500
  direct_external_symbols:          up to 200
  unknown_scope_symbols:            up to 200
  global_backoff_symbols:           up to 200
  current_file_text_words:           up to 100
```

最终再统一 rerank。这样比“前缀候选填满就跳过模糊扫描”更稳，因为用户输入较短时，Top-N 很容易被高频前缀符号占据，导致后续强相关候选无法进入重排阶段。

### 4.2 当前文件 overlay：把 fallback word 升级成临时符号

每个打开文档维护一个 `EphemeralSymbolTable`：

```rust
struct EphemeralSymbol {
    name: String,
    kind_guess: SymbolKind,
    range: TextRange,
    scope_depth: u16,
    enclosing_function: Option<SymbolId>,
    declaration_confidence: f32,
    last_seen_distance: u32,
    occurrence_count: u16,
    source: EphemeralSource,
}
```

来源包括：

```text
local variable declaration
function parameter
function definition/declaration
macro definition
typedef / using
enum constant
struct/class name
field-like member access token
plain identifier fallback
```

排序时，`local variable declaration`、`parameter`、`typedef/using` 应明显高于 plain identifier fallback。这样能修复“当前文件词不如可达头文件符号”的体验问题。

### 4.3 字符串匹配索引：保留现有热路径，但增加轻量倒排

建议增加三类内存结构：

```text
lowercase prefix trie / sorted array       -> exact/prefix
camel-boundary initials index              -> PB => PushBack / parseBuffer
3-gram or token-boundary inverted index    -> substring/fuzzy narrowing
```

不要每次都全表 fuzzy scan。对 3+ 前缀，可以先用 3-gram/边界 token 缩小候选集合，再做精确 fuzzy scoring。

### 4.4 成员补全：从 field-only 改成 member evidence

成员索引建议扩展为：

```rust
struct RecordIndexEntry {
    record_id: RecordId,
    record_name: String,
    aliases: Vec<String>,
    scope_tier: ScopeTier,
    file_path: PathId,
    bases: Vec<RecordId>,
    members: Vec<MemberEntry>,
}

struct MemberEntry {
    name: String,
    kind: MemberKind, // Field, Method, StaticMethod, EnumMember, NestedType
    owner: RecordId,
    access: Option<AccessKind>,
    type_hint: Option<TypeId>,
    frequency: u32,
    declaration_order: u16,
}
```

接收者解析仍然 best-effort，但要多走几条弱证据路径：

```text
1. 当前函数参数/局部变量声明：Foo x; x.^
2. 指针/引用声明：Foo* p; p->^
3. typedef/using alias：Alias a; a.^
4. auto 初始化：auto x = makeFoo(); x.^
5. 构造表达式：Foo x(...); x.^
6. 赋值来源：x = getFoo(); x.^
7. 命名暗示：cfg/settings/options/client/request/ctx 等 receiver name 与 record name 共现
8. 一跳链式：a.b.^ 若 b 在 a 的候选 record 中是唯一高置信字段
```

成员排序也不应只按 owner scope tier。建议改成：

```text
receiver_type_confidence
+ owner_scope_prior
+ member_name_match
+ member_kind_prior
+ member_frequency
+ current_file_member_usage
+ receiver_name_owner_correlation
+ inherited_member_penalty
+ access_uncertainty_penalty
```

对 fallback member，前缀长度 2 的要求可以保留，但 3+ 时应允许 camel initials 和边界子序列；否则 `pb` 找不到 `push_back` 这类体验会很差。

### 4.5 include-path 补全：加入近期与项目惯用头文件

include 补全现在的 quote/angle 策略合理，但排序可以加入：

```text
recently_included_in_this_file
included_by_sibling_files
included_in_same_directory_files
header_basename_frequency
header_exports_symbol_used_nearby
path_depth_penalty
private/internal/test path penalty
```

对 `#include <vec`，`vector` 应由配置 include root + 近期/标准库频率共同支撑；对 `#include "foo"`，当前目录和 sibling include 习惯应很强。

另一个高价值能力是**普通符号补全附带“需要 include”提示**。clangd 的补全可以跨代码库建议并插入 include；但 clangd 依赖完整 C++ parser 和 compile flags，FossilSense 不应强行模仿，只应在 header metadata 高置信时把它作为可见 evidence，比如标签显示 `requires include`，并用 `additionalTextEdits` 可选插入。clangd 官方说明中也强调其精确补全来自完整 C++ parser 和构建参数配置，这正是 FossilSense 需要谨慎降级的原因。([Visual Studio Marketplace][7])

---

## 5. 排序层：从 packed strict tier 改成可校准多信号模型

### 5.1 建议的候选特征

```rust
struct CandidateFeatures {
    // text
    text_match_tier: u8,
    fuzzy_score: f32,
    prefix_len: u8,
    case_match: bool,
    camel_match: bool,
    edit_distance_small: bool,

    // scope / reachability
    scope_tier: u8,
    include_distance: u8,
    include_graph_open: bool,
    direct_external: bool,
    path_locality: f32,

    // intent
    intent_kind: IntentKind,
    kind_matches_intent: bool,
    kind_prior: f32,
    expected_type_match: Option<f32>,

    // locality
    current_file_symbol: bool,
    current_function_symbol: bool,
    nearby_occurrence_count: u16,
    distance_to_cursor_bucket: u8,
    same_directory: bool,
    sibling_file_usage: u16,

    // quality
    symbol_reference_count_bucket: u8,
    deprecated: bool,
    internal_or_reserved: bool,
    generated_file: bool,

    // personalization
    accepted_count_decay: f32,
    last_accepted_decay: f32,
    accepted_in_this_workspace: bool,
    recently_rejected_decay: f32,

    // member-specific
    receiver_type_confidence: f32,
    inherited_member: bool,
    receiver_name_owner_correlation: f32,

    // stability
    previous_session_rank: Option<u8>,
}
```

这些特征大多可以在本地计算，不需要上传源码，也不需要完整语义。

### 5.2 第一阶段：确定性加权排序

先不要直接上 ML。建议先把 strict tier 改成 soft score：

```text
score =
  3.00 * intent_score
+ 2.50 * scope_prior
+ 2.00 * text_score
+ 1.50 * locality_score
+ 1.20 * usage_score
+ 0.80 * quality_score
+ 0.50 * path_score
- penalties
```

其中 `scope_prior` 不再是绝对压倒，而是一个强先验：

```text
current function / current file semantic symbol: +3.0
current file ephemeral declaration:             +2.6
include reachable workspace:                    +2.2
direct external include:                         +1.8
unknown due to open include graph:               +1.2
global fallback:                                 +0.4
plain current-file text word:                    +0.8 ~ +1.8, depending on locality
```

这样既保留现有“可达优先”的优势，又允许强本地意图反超。

### 5.3 加入“置信护栏”，避免低质量候选乱冲 Top 1

可以用 guard band，而不是硬 tier：

```rust
fn can_outrank(a: &Candidate, b: &Candidate) -> bool {
    if a.is_low_confidence_global() && b.is_reachable_prefix_match() {
        return a.score > b.score + 1.5;
    }
    if a.is_plain_text_word() && b.is_index_symbol() {
        return a.has_strong_locality_or_history() && a.score > b.score + 0.5;
    }
    true
}
```

这能减少“奇怪文本词突然冲到第一”的 UX 风险。

### 5.4 去重从“保留一个”改成“证据合并”

当前同名去重会丢失 evidence。建议改成：

```text
same label/name candidates
    ↓
merge evidence
    ↓
choose best insertion payload
    ↓
score uses aggregated evidence
```

例如同一个 `Foo` 同时来自 index、当前文件 overlay、近期选择历史，那么最终候选显示一个 `Foo`，但分数获得三份证据加成。UI 标签显示最有解释力的 evidence，例如 `local · reachable`，而不是只保留 index 或 fallback 其中一方。

---

## 6. 学习排序与个性化

### 6.1 默认本地个性化：不上传、不训练全局模型

先做 local-only 的轻量历史库：

```rust
struct LocalCompletionStats {
    symbol_key: StableSymbolKey,
    workspace_id_hash: u64,
    accepted_count_ewma: f32,
    last_accepted_time: Timestamp,
    accepted_prefix_buckets: PrefixBucketStats,
    accepted_intent_buckets: IntentBucketStats,
    ignored_when_top_count_ewma: f32,
}
```

核心策略：

```text
正反馈：
  用户显式选择候选
  用户 Tab/Enter 接受候选
  用户接受后没有立即 undo/delete

弱负反馈：
  候选在 Top 1/Top 3 可见，但用户选择了其他候选
  用户继续输入到完整名称仍未选择该候选

不作为负反馈：
  候选在列表下方不可见
  用户关闭补全
  自动触发但用户继续正常输入
```

建议使用指数衰减：

```text
recency_score = exp(-age_hours / half_life_hours)
```

不同粒度分开：

```text
symbol-level:       用户常用某符号
prefix-symbol:      输入 str 时常选 string_view
context-symbol:     在 include / member / type context 中常选
workspace-level:    当前项目惯用
global-user-level:  用户跨项目偏好
```

### 6.2 第二阶段：匿名日志 + GBDT / LambdaMART / CatBoost 重排

当 deterministic ranker 稳定后，再做 opt-in 匿名训练。训练数据不需要源码，只需要数值/枚举特征、候选 rank、是否被接受、输入动作数等。JetBrains 的论文和工程博客都说明这种“客户端计算特征、匿名日志、服务端训练、本地部署模型”的路线可行；他们还强调目标指标选择很关键，因为补全通常是 winner-takes-all，Top 1 错误的代价很高。([The JetBrains Blog][8])

模型建议优先级：

```text
Phase 1: hand-tuned heuristic scorer
Phase 2: GBDT / CatBoost pointwise classifier
Phase 3: LambdaMART / pairwise/listwise ranker
Phase 4: optional local small model for personalization
```

CatBoost/GBDT 适合这里，因为特征里有大量类别特征，例如 kind、scope tier、intent、trigger、source category。CatBoost 本身就是面向类别特征的梯度提升方法，并提供快速 CPU 推理能力。([arXiv][9])

如果进入真正 Learning-to-Rank，LambdaMART 是成熟选择；它是 LambdaRank 的 boosted tree 版本，在真实排序问题中长期被使用。([Microsoft][10])

### 6.3 模型只做 re-rank，不做候选生成

不要让 ML 控制召回。召回仍由索引、tree-sitter、文本匹配、include graph 决定。模型只对内部 Top 500/1000 做重排。这样有三个好处：

```text
1. 候选不会因为模型误判而消失
2. 模型延迟可控
3. 出问题时可快速回退到 heuristic
```

---

## 7. UX 设计：减少惊扰、解释不确定性

### 7.1 标签要表达 evidence，而不是假装语义绑定

现有标签方向是对的。建议进一步收敛为少量稳定标签：

```text
local
reachable
direct include
recent
requires include
ambiguous
global
text
```

hover/detail 中再展开：

```text
Ranked higher because:
- used in current file
- from directly included header
- matches TypeName context
- selected recently in this workspace
```

这对 best-effort 工具很重要：不要说“resolved to Foo”，而应说“evidence suggests Foo”。

### 7.2 控制列表抖动

由于 `isIncomplete = true` 会让编辑器每次输入都重新请求，FossilSense 必须主动做 rank stability。建议在 completion session 内保存上一轮 rank：

```text
如果候选仍匹配，且新旧分数差距小于阈值，则保留相对顺序
如果候选从 Top 3 掉出，需要分数差距超过 hysteresis threshold
用户刚移动选择项后，不要因为下一字符输入把选中项剧烈移动
```

这比单纯提高准确率更影响 UX。用户不是只看 Top 1，也依赖“肌肉记忆”和列表稳定性。

### 7.3 谨慎使用 preselect

只有当满足以下条件才 preselect：

```text
score_margin(top1, top2) >= threshold
top1 intent_confidence high
top1 source not plain text fallback
top1 not ambiguous/global-only
```

否则宁可不预选，避免用户 Enter/Tab 误接受。

### 7.4 LSP 输出层建议

`sortText` 应编码最终服务端排序，格式可用：

```text
{inverted_score_bucket}:{stable_name}:{source_tiebreak}
```

同时设置 `filterText` 为可匹配文本，`textEdit` 明确替换范围，避免客户端猜 word boundary。文档、复杂 detail、昂贵 include 解释可以延迟到 `completionItem/resolve`；LSP 明确支持 completion resolve 用于延迟填充 detail/documentation，但排序和过滤相关字段不能等 resolve 再变。([Microsoft GitHub][2])

对于相同 edit range、commit characters、insertTextMode 的大量结果，可以利用 LSP 3.17 的 `CompletionList.itemDefaults` 减少 payload。([Microsoft GitHub][2])

---

## 8. 三个分支的具体优化方案

## 8.1 常规标识符补全

### 立即改动

```text
1. 当前文件 overlay 临时符号表
2. strict scope tier -> soft scope prior
3. 多通道召回配额
4. 同名 evidence merge
5. intent kind prior
```

### 排序示例

在表达式上下文：

```cpp
foo = co|
```

候选：

```text
count        current function local variable
Config       reachable type
connect      global function
condition    current-file plain word
```

旧策略可能让 reachable indexed symbol 压过 current-file fallback。新策略应排序为：

```text
count        local declaration + prefix + expression value
condition    nearby current-file text/ephemeral + prefix
connect      function + prefix
Config       type, but expression context penalty
```

在类型上下文：

```cpp
std::vector<Co|
```

则 `Config` / `Codec` / `ConstIterator` 这类 type kind 应上升，局部变量 `count` 应下降。

## 8.2 成员补全

### 已解析 receiver

```cpp
Foo f;
f.ba|
```

排序：

```text
1. Foo::bar       direct field/method + prefix
2. Foo::baz       direct member + prefix
3. Base::balance  inherited member + prefix + inherited penalty
4. Other::bar     fallback only，除非用户继续输入或 Foo 置信度低
```

### 未解析 receiver

```cpp
client.se|
```

fallback 不应只按字段频率。应加入 receiver name correlation：

```text
client.send       owner names historically associated with client
client.setHeader  same owner / sibling files used
server.seed       name prefix same但 receiver correlation 弱，降低
```

### 是否加入方法

建议加入。用户在 `.` / `->` 后通常期待 fields + methods。只返回字段会让 FossilSense 在核心路径上显得残缺。若担心误导，可用 kind icon 与标签区分：

```text
field
method
static method
inherited
ambiguous owner
```

## 8.3 include-path 补全

### 召回

保留 segment prefix 为主，但 3+ 字符后加入：

```text
segment initials
basename substring
minor typo tolerance
recent include basename
sibling include basename
```

### 排序

```text
quote include:
  current directory
  sibling file includes
  workspace headers
  configured include roots
  external

angle include:
  configured include roots
  standard/external headers
  workspace public headers
  local headers
```

在 quote include 中，`src/foo/bar.h` 和当前文件同目录/同组件的证据应比全局 header 强；在 angle include 中，标准库/外部 include root 仍应优先。

---

## 9. 评估体系

### 9.1 离线指标

```text
MRR
Recall@1 / @3 / @5 / @10
Top-1 accepted rate
Top-3 accepted rate
Characters saved
Keystrokes before accept
Wrong-kind rate
Wrong-member-owner rate
List churn / rank instability
p50 / p95 / p99 latency
truncation miss rate
```

特别要分桶看：

```text
prefix length 1
prefix length 2
prefix length 3+
member resolved
member fallback
include quote
include angle
current-file symbol
reachable workspace
external header
unknown include graph
```

### 9.2 在线指标

核心不是“用户选了多少次”，而是：

```text
用户是否更快找到正确候选
是否减少继续输入字符数
是否减少列表滚动/搜索
是否降低误接受
Top 1 是否更稳定
```

Bibaev/JetBrains 的工作使用离线 held-out 与在线 A/B 两类评估，并观察 explicit selection sessions 与 typing actions 的变化；FossilSense 也应走这个路径。([arXiv][4])

### 9.3 Shadow ranking

上线前可以 shadow mode：

```text
用户仍看到旧排序
后台计算新排序
记录 accepted item 在新旧排序中的位置差异
```

这样能低风险判断新 ranker 是否真的提升。

---

## 10. 分阶段落地计划

### Phase 0：可观测性与调试面板

先实现：

```text
completion_session_id
candidate source breakdown
per-candidate feature dump
old_score / new_score
rank movement
accepted candidate rank
latency breakdown
```

同时在开发版 hover/detail 中显示：

```text
score = 8.42
intent = TypeName, confidence = 0.78
scope = reachable workspace
text = prefix
locality = same directory
history = selected recently
```

没有这个，后续 tuning 会非常慢。

### Phase 1：无 ML 的排序重构

优先做四件事：

```text
1. current-file ephemeral symbol overlay
2. evidence merge dedup
3. soft scope prior
4. intent kind prior
```

这是性价比最高的一步，能明显改善 UX，风险低。

### Phase 2：召回增强

```text
1. member method index
2. receiver weak inference
3. inherited member with penalty
4. 多通道候选配额
5. include recent/sibling ranking
6. 3+ prefix camel/subsequence narrowing
```

这一阶段主要提高 Recall@5 和减少“明明应该有但没有”的情况。

### Phase 3：本地个性化

```text
1. local accepted history
2. EWMA recency/frequency
3. prefix-symbol pair stats
4. context-symbol stats
5. negative feedback only for visible strong cases
```

默认本地保存，不上传。

### Phase 4：匿名日志与 ML re-ranker

```text
1. opt-in structured telemetry
2. feature-only，不上传源码/候选原文
3. train GBDT/CatBoost/LambdaMART
4. deploy as Rust-embedded tree ensemble
5. A/B test
6. kill switch + heuristic fallback
```

---

## 11. 我建议优先实施的前三个改动

### 第一优先级：把当前文件 fallback 升级成 ephemeral symbols

这是最直接改善用户体验的改动。当前文件、当前函数、附近代码对用户意图的解释力很强，不应只是文本 fallback。

### 第二优先级：strict scope tier 改成 soft scope prior + guard band

保留 include reachability 的优势，但允许强本地证据、强 intent evidence、近期选择历史反超。这样 FossilSense 会从“作用域主导”升级为“证据主导”。

### 第三优先级：member completion 从 field-only 扩展到 field + method + weak receiver inference

成员补全是用户感知最强的路径。即使整体语义仍是 best-effort，只要 `obj.` 后的候选更像真实 IDE，体验提升会非常明显。

---

## 最终设计结论

FossilSense 不应试图变成没有 compile database 的 clangd。更合适的路线是：

```text
Best-effort parser/index
+ include reachability
+ current-file ephemeral semantic overlay
+ intent classifier
+ evidence-merged candidates
+ soft calibrated ranking
+ local recency/frequency personalization
+ optional privacy-preserving ML re-ranker
```

这个方案能保持 FossilSense 的核心优势：轻量、快、不依赖完整编译模型、不会让候选轻易消失；同时补上当前最缺的三类能力：**当前编辑意图、用户/项目使用习惯、弱类型/成员上下文**。

[1]: https://link.springer.com/article/10.1007/s10515-010-0064-x "Improving code completion with program history | Automated Software Engineering | Springer Nature Link"
[2]: https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/ "Specification"
[3]: https://codebrowser.dev/llvm/clang-tools-extra/clangd/Quality.h.html "Quality.h source code [clang-tools-extra/clangd/Quality.h] - Codebrowser "
[4]: https://arxiv.org/pdf/2205.10692 "All You Need Is Logs: Improving Code Completion  by Learning from Anonymous IDE Usage Logs"
[5]: https://research.lancaster-university.uk/en/publications/learning-from-examples-to-improve-code-completion-systems/ "
        Learning from examples to improve code completion systems
      \-  Lancaster University research directory"
[6]: https://arxiv.org/abs/2402.16197 "[2402.16197] Language Models for Code Completion: A Practical Evaluation"
[7]: https://marketplace.visualstudio.com/items?itemName=llvm-vs-code-extensions.vscode-clangd "
        clangd - Visual Studio Marketplace
    "
[8]: https://blog.jetbrains.com/blog/2021/08/20/code-completion-episode-4-model-training/ "Code Completion, Episode 4: Model Training - The JetBrains Blog"
[9]: https://arxiv.org/abs/1810.11363 "[1810.11363] CatBoost: gradient boosting with categorical features support"
[10]: https://www.microsoft.com/en-us/research/publication/from-ranknet-to-lambdarank-to-lambdamart-an-overview/ "From RankNet to LambdaRank to LambdaMART: An Overview - Microsoft Research"
