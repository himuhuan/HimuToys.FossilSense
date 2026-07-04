# Smart Completion 开发评估报告

Status: dev-eval
Date: 2026-07-05
Input: `docs/research/smart-completion.md`
Output target: `research/smart-completion-dev-eval.md`

## 1. 结论摘要

专家报告的总体方向值得采纳：FossilSense 下一阶段的补全优化，不应该走“更多 fuzzy 匹配”或“伪装 clangd 语义”的路线，而应该走证据聚合、意图感知、低延迟、可解释的排序系统。

但结合当前代码看，专家报告有一个重要前提已经过时：FossilSense 现在已经具备第一版 current-function local overlay。代码中已有 `LocalBinding` / `LocalCompletionCandidate`，普通标识符补全会从打开文档中提取当前函数参数和光标前局部变量，并以 `CompletionCandidateSource::LocalBinding` 注入候选池。这意味着“把当前文件 fallback 升级成 ephemeral symbols”不是从零开始，而是应该扩展为更完整的 evidence overlay。

最需要谨慎的是“strict scope tier 改成 soft scope prior”。当前 `CLAUDE.md` 和 `resolver::pack_score` 都明确要求 `ScopeTier` 严格压倒文本质量、locality 等信号，且跳转、补全、符号搜索、着色共用 `resolver`。直接把 completion 排序改成软加权，会破坏现有设计契约和测试不变量。可行推进方式是：先做可观测性、shadow ranking、同 tier 内意图排序、evidence merge；只有在离线指标证明收益后，才考虑把“completion-only soft prior”作为一次显式设计变更。

推荐下一大版本目标收敛为：

> Smart Completion Foundation: 在保持候选诚实和热路径安全的前提下，建立补全证据层、调试观测、当前文档 overlay 扩展、同名证据合并、轻量意图排序和列表稳定性。成员补全 v2 与本地历史可作为独立增量，不建议把 ML/LLM 纳入下一版本主线。

建议工作量：

| 范围 | 估算 | 可行性 | 建议 |
|---|---:|---|---|
| Foundation only: 可观测性、overlay 扩展、evidence merge、同 tier 意图排序、rank stability | 18-30 人日 | 高 | 下一大版本主线 |
| Foundation + include ranking + 多通道召回 | 28-45 人日 | 中高 | 可作为 stretch |
| Foundation + 成员补全 v2 的 field/method 基础版 | 35-60 人日 | 中 | 单独立项，避免拖垮主线 |
| 本地选择历史与个性化 | 8-14 人日额外 | 中 | 先验证 VS Code accept hook |
| 匿名日志 + ML reranker | 25-45+ 人日额外 | 低到中 | 暂缓，需产品/隐私/评估体系成熟 |

## 2. 当前代码基线

### 2.1 普通标识符补全

入口在 `crates/fossilsense/src/server/language_server.rs` 的 `completion` handler。

当前顺序是：

1. `#include` 上下文先走 include-path completion。
2. `.` / `->` 上下文先走 member completion。
3. 普通 identifier completion 读取当前 open document snapshot。
4. 通过 `get_or_parse_document` 使用 live parse cache。
5. 从 `index.local_bindings` 生成当前函数参数/局部变量候选。
6. 从每个 workspace 的 in-memory `NameTable` 搜索索引符号。
7. 加入 current-file raw local words fallback。
8. 同名 dedup 后按 score 排序，返回 `CompletionList { is_incomplete: true }`。

关键代码事实：

- `query::NameTable::search_ranked_scoped_pooled` 已支持 scoped ranking 和 prefix-extension pool memo。
- `query::COMPLETION_LIMIT = 100`。
- `query::SHORT_PREFIX_MIN_LEN = 3`，短前缀只保留 exact、prefix、词边界子串。
- `server::state::CompletionMemo` 只缓存每个 workspace table 的候选池，不缓存最终 rank / session history。
- `completion_words::extract_words` 只做打开文档的 raw identifier word fallback，跳过注释和字符串。

### 2.2 当前函数局部补全已经实现第一版

相关文件：

- `crates/fossilsense/src/parser.rs`
- `crates/fossilsense/src/parser/ast.rs`
- `crates/fossilsense/src/query/local_completion.rs`
- `crates/fossilsense/src/server.rs`

当前能力：

- `LocalBindingKind::{Parameter, LocalVariable}`。
- `LocalBinding { name, kind, type_text, decl_start_byte, function_start_byte, function_end_byte }`。
- `query::local_completion_candidates` 只返回当前函数 body 范围内、声明早于光标、匹配当前 prefix 的绑定。
- Local binding 使用 `resolver::pack_score(ScopeTier::Current, base_match, 0)`。
- `completion_items_for_local_bindings` 同时把 local binding 的 confidence 标成 `Heuristic`，这是一个重要信号：它有很强的当前位置证据，但仍不声称编译级绑定。
- Same-name dedup 的 source priority 是 `LocalBinding > Indexed > LocalWord`。

这已经覆盖专家报告中“当前文件 fallback 太弱”的一部分问题。剩余差距不是“有没有 overlay”，而是 overlay 还只覆盖参数和局部变量，没有统一纳入 open-document 中未保存的宏、typedef/using、enum 常量、函数、类型、最近字段访问词等证据。

后续 evidence merge 需要保留这种区分：`ScopeTier::Current` 表示位置和来源证据，`ResolutionConfidence` / detail 文案表示 best-effort 置信度，不能把 local binding 展示成精确语义绑定。

### 2.3 当前排序是 strict tier

相关文件：

- `crates/fossilsense/src/resolver.rs`
- `crates/fossilsense/src/model.rs`
- `crates/fossilsense/src/query.rs`

当前不变量：

- `ScopeTier` 顺序：`Current > Reachable > External > Unknown > Global`。
- `resolver::TIER_STRIDE = 2048`，大于 `MAX_BASE_MATCH + MAX_LOCALITY`。
- `pack_score(tier, base_match, locality)` 是严格字典序打包，而不是软加权。
- `resolver` 注释和测试都明确要求 tier 不能被文本匹配或 locality 反超。

这使得专家建议的 soft scope prior 有真实价值，但不是低风险修改。它会影响普通补全、workspace symbol、goto definition、coloring 共享概念的稳定性。即使只改 completion，也必须显式说明：`ScopeTier` 仍来自共享 resolver，但 completion ranking 从“tier 主导”变成“tier 是强 evidence feature”。这需要更新文档和测试，而不能作为局部补丁混入。

### 2.4 成员补全仍是 field-only

相关文件：

- `crates/fossilsense/src/server/member_completion.rs`
- `crates/fossilsense/src/store/queries.rs`
- `crates/fossilsense/src/store/schema.rs`
- `crates/fossilsense/src/parser/ast.rs`

当前能力：

- `member_receiver_name` 只接受 bare identifier receiver。
- `parser::infer_receiver_record` 只基于 `local_declarations` 找最近 record-typed local/parameter declaration。
- receiver 可解析时，通过 `resolve_record_candidates` 找 record，再 `fields_for_records` 取字段。
- receiver 不可解析且 prefix 长度 >= 2 时，走 `fallback_field_candidates`。
- LSP item kind 固定为 `FIELD`。

当前边界：

- 不返回 method / static method / enum member / nested type。
- 不处理 call result、chain receiver、index receiver。
- 不处理继承、模板、重载、命名空间、访问控制。
- 成员 fallback 只做 SQL `LIKE 'prefix%'`，不做 camel initials 或 fuzzy。

专家关于成员补全 recall 窄的判断成立。但 field+method 不是小改：需要扩展 parser fact、schema、store query、LSP kind、排序证据和测试。

### 2.5 Include completion 有基础排序，但没有使用习惯证据

相关文件：

- `crates/fossilsense/src/server/include_completion.rs`
- `crates/fossilsense/src/server/indexing/cache.rs`
- `crates/fossilsense/src/store/includes.rs`

当前能力：

- quote include 和 angle include 有不同 base score。
- workspace include path 有 `IncludeCompletionTable`，避免每次打开 SQLite。
- external include dir 有 mtime-based directory cache。
- 仍会在当前目录、workspace、include roots 之间做候选合并。

缺口：

- 没有 sibling file include 习惯。
- 没有最近 include。
- 没有 basename frequency。
- 没有 current-file already included / same-component include 统计。
- 没有 `requires include` 类 evidence label 或 `additionalTextEdits`。

Include ranking 是中等工作量、风险较低的优化点，但应先把统计表预计算到内存，避免每键查库。

### 2.6 观测能力不足以支撑大排序改动

当前已有：

- `fossilsense.debug.candidateReasons` 主要用于 goto definition。
- `perf_logging_enabled` 能记录 completion 总耗时和 memo hit kind。
- 单元测试覆盖 short-prefix、dedup、local completion、member fallback、scope tier。

当前缺少：

- completion candidate feature dump。
- old/new ranking shadow comparison。
- accepted candidate rank。
- rank churn/list stability 指标。
- 分桶离线评估 harness。
- 用户选择历史。

因此，不建议先改默认 ranker。应先补 observability，否则后续调参会靠感觉。

## 3. 专家建议采纳评估

| 专家建议 | 本仓库适配结论 | 采纳方式 |
|---|---|---|
| Evidence-Aware Intent Ranking | 方向正确 | 采纳，但分阶段；先做 evidence struct 和 debug dump |
| Current-file ephemeral symbol overlay | 已部分实现 | 扩展现有 `LocalBinding` / open-document parse，不新建平行 semantic 模型 |
| Strict tier 改 soft scope prior | 有价值但冲突大 | 暂不默认启用；先做同 tier intent ranking 与 shadow soft-ranker |
| 多通道召回 + 配额合并 | 可行 | 需要先把 internal pool 和最终 limit 分离，注意大仓库内存与延迟 |
| Evidence merge dedup | 高性价比 | 优先做；替代当前 same-name 只保留一个来源的逻辑 |
| 成员补全 field + method + weak inference | 高价值但大 | 单独立项；先做 method index 基础版，继承/链式推断后置 |
| Include 近期/惯用头排序 | 中高性价比 | 采纳，基于 store 中 includes 数据预构建内存统计 |
| 本地选择历史 | 有价值 | 先做 accept hook 可行性 spike，再持久化本地 stats |
| 匿名日志 + ML | 暂不适合下一版 | 需产品、隐私、评估与 kill switch；先积累 deterministic features |
| LLM | 不建议 | 与 FossilSense 自包含、低延迟、无外部依赖定位不符 |

## 4. 推荐路线图

### Phase 0: Completion observability and shadow ranking

目标：在不改变用户可见排序的前提下，让后续排序改动可测、可解释、可回滚。

建议任务：

- 增加 `CompletionEvidence` / `CompletionFeatureDump`，但只作为 debug 输出，不改变排序。
- 给每次 completion 请求生成 `completion_session_id` 或等效的 URI+prefix session key。
- 记录每个候选的 source、scope tier、confidence、reason、base match、locality、kind、是否 local binding、是否 raw word、最终 rank。
- 增加 debug setting，例如 `fossilsense.debug.completionRanking`。
- 增加 shadow ranker：后台计算新 rank，默认仍返回旧 rank。
- 增加开发日志：accepted rank 先不做，先记录 old/new rank movement、latency。

估算：3-5 人日。

可行性：高。

风险：低。主要风险是日志噪声，需要默认关闭。

验收：

- `cargo test -p fossilsense`。
- completion 默认输出顺序不变。
- 开启 debug 后能解释 Top-N 候选的主要证据。
- perf log 能区分 retrieval / evidence / ranking / render 粗粒度耗时。

### Phase 1: Safe evidence foundation

目标：在不破坏 strict tier 契约的前提下，提高当前文档候选和同名候选的质量。

建议任务：

- 扩展 open-document overlay：
  - 参数、局部变量沿用现有 `LocalBinding`。
  - 纳入当前打开文件中未保存的 top-level function/type/macro/enum constant。
  - 纳入 local typedef/using/enum constant，能保守识别就给 evidence，不能识别就 fallback。
  - raw local word 继续保持 fallback，不整体升为 `Current`。
- 引入 evidence merge：
  - same label/name 不再简单丢弃来源。
  - 合并 Indexed、LocalBinding、OpenDocumentSymbol、LocalWord、history 等 evidence。
  - insertion payload 选择最可靠来源，score 使用聚合证据。
  - UI detail 使用少量稳定标签，如 `local`、`reachable`、`external`、`global`、`text`。
- 在同一 `ScopeTier` 内加入 kind/intent/locality 微调，不允许低 tier 反超高 tier。

估算：7-12 人日。

可行性：高。

风险：中。主要是候选 dedup 逻辑会从“选一个赢家”变成“合并证据”，测试要覆盖 same-name 多来源。

验收：

- 当前已实现的 local binding 行为不回退。
- raw local word 不会突然超过 reachable indexed candidate。
- 未保存的当前文件 function/type/macro 能以当前文档 evidence 出现。
- 同名候选 detail 不互相矛盾。

### Phase 2: Intent classifier and deterministic reranker

目标：让排序知道“这里更像类型位置、调用位置、表达式位置还是声明名字位置”，但仍保持 best-effort。

建议先支持这些 intent：

- `IncludePath`: 继续由 include completion 处理。
- `MemberAccess`: 继续由 member completion 处理。
- `CallTarget`: identifier 后面紧邻 `(` 或上下文显示正在调用。
- `TypeName`: 声明、cast、`new`、template 参数附近。
- `ExpressionValue`: RHS、return、argument、condition。
- `MacroPreprocessor`: `#if` / `#ifdef` / `#define` 附近。
- `DeclarationName`: 类型后声明新变量的位置，降低已有全局符号。

实现建议：

- 新建协议无关 helper，例如 `query::completion_context`。
- 输入只使用当前 line、open document parse、byte offset、已有 `Occurrence` / tree-sitter shape。
- 先输出 `IntentKind`、`intent_confidence`、expected/forbidden symbol kinds。
- 第一版只在同 tier 内调整 score，不改变 tier 主导顺序。
- 对低置信 intent 不施加惩罚，只加小正向 prior。

估算：5-8 人日。

可行性：中高。

风险：中。错误 intent 会造成排序惊扰，所以第一版应该弱化并可 debug。

验收：

- 类型上下文中 type/typedef/enum 排名在同 tier 内上升。
- 表达式上下文中 variable/function/enum constant 排名在同 tier 内上升。
- 调用上下文中 function-like 候选在同 tier 内上升。
- 低置信或 parse fallback 时回到旧排序。

### Phase 3: Rank stability and preselect guard

目标：减少 `isIncomplete = true` 带来的列表抖动。

建议任务：

- 扩展 `CompletionMemo`，存储上一轮 Top-N label/name、score bucket、source/evidence key。
- 当前 prefix 延长且候选仍匹配时，小分差保留相对顺序。
- Top 3 掉出需要 hysteresis threshold。
- 增加 preselect guard：
  - top1/top2 margin 足够。
  - top1 不是 plain text fallback。
  - top1 不是 ambiguous/global-only。
  - intent confidence 足够。

估算：3-5 人日。

可行性：中高。

风险：中。稳定性策略可能让新证据不能及时体现，需要阈值测试。

验收：

- prefix 从 `co` 到 `cou` 时，仍匹配候选的相对顺序不会因小分差剧烈变化。
- 强证据新候选仍能进入 Top-N。
- 空结果仍为 incomplete，不粘住。

### Phase 4: Retrieval improvements

目标：提高 Recall@5 / Recall@10，尤其避免 prefix 候选填满窗口后强相关候选进不了 rerank。

建议任务：

- 把 `NameTable` 的 retrieval limit 和 LSP output limit 分离：
  - internal pool 可先设 300-500，不必一开始到 2000。
  - 最终仍输出 `COMPLETION_LIMIT = 100`。
- 对 channel 做配额：
  - open document overlay。
  - indexed exact/prefix。
  - indexed boundary substring / camel initials。
  - reachable / external / unknown / global backoff。
  - local raw words。
- 增加可选 camel-boundary initials index 或 token-boundary inverted index。

估算：6-10 人日。

可行性：中。

风险：中高。`NameTable` 是热路径，任何额外索引和 internal pool 扩大都要做 latency 分桶。

验收：

- p95 completion latency 不明显恶化。
- prefix 延长仍可复用 pool。
- 短前缀噪声规则保持。
- Top-N 中各来源不会被单一通道挤满。

### Phase 5: Include ranking improvement

目标：让 include-path completion 更贴近项目习惯。

建议任务：

- 在索引 ready 后构建 `IncludeUsageTable`：
  - basename frequency。
  - directory frequency。
  - file -> included basenames。
  - sibling files include stats。
  - same directory / same component include stats。
- completion 热路径只读内存表。
- quote include 优先 current dir、sibling include habit、workspace headers。
- angle include 优先 include roots、external/standard-like headers、workspace public headers。

估算：4-7 人日。

可行性：中高。

风险：低到中。主要是避免每次 completion 打开 SQLite 或扫描目录。

验收：

- 现有 include completion tests 保持。
- sibling include 统计能改变同分候选顺序。
- external dir cache 仍按 mtime 失效。

### Phase 6: Member completion v2

目标：提升 `.` / `->` 后的用户感知质量。

建议拆成两个阶段，不建议一次做完专家报告中的全部弱推断。

#### Phase 6A: Receiver inference expansion

建议任务：

- 扩展 `LocalDeclaration`，记录 pointer/reference/typedef alias 更丰富的 type text。
- 支持 `auto x = makeFoo()` 的极窄高置信模式：只有 initializer 是直接 call 且函数名/return type 可从 index 高置信关联时才加入。
- 支持 `Foo x(...)` 构造表达式。
- 支持简单 assignment source `x = getFoo()`，但只作为低置信 fallback。
- 继续拒绝复杂 chain/call/index receiver，或只做一跳受控推断。

估算：6-10 人日。

可行性：中。

风险：中高。容易产生“像语义绑定”的错觉，UI 必须标注 confidence/fallback。

#### Phase 6B: Method/member-function index

建议任务：

- 新增 `MemberEntry` 或扩展 `fields` 为 `members`。
- parser AST pass 收集 class/struct body 内 method declaration/definition。
- schema migration，存储 `kind = Field | Method | StaticMethod | EnumMember | NestedType`。
- `fields_for_records` 升级为 `members_for_records`。
- member completion LSP kind 区分 `FIELD` / `METHOD` / `FUNCTION` / `ENUM_MEMBER` / `STRUCT`。

估算：10-18 人日。

可行性：中。

风险：高。涉及 schema、parser、store、query、LSP、tests 多层。建议单独需求文档和实现计划。

不建议下一版纳入：

- 继承展开。
- 模板实例化。
- overload resolution。
- namespace/static member 完整语义。
- access control。

这些会把 FossilSense 推向没有 compile database 的 clangd，和项目定位冲突。

### Phase 7: Local history and personalization

目标：使用本地历史改善排序，不上传源码。

前置 spike：

- 验证 VS Code 是否能稳定发出 completion accept 信号。
- 可能路径：
  - LSP `CompletionItem.command` 触发 client command。
  - VS Code extension 注册 command 后通知 server。
  - 或只在 extension 侧本地存储并通过 initialization/workspace config 下发。

建议统计：

- stable symbol key。
- workspace hash。
- accepted count EWMA。
- last accepted decay。
- prefix-symbol pair。
- intent-symbol pair。
- visible but skipped 的弱负反馈只在 Top 1/3 可见时记录。

估算：8-14 人日，不含 ML。

可行性：中，取决于 accept hook。

风险：中。错误负反馈会伤排序；隐私和可解释性要写清。

### Phase 8: ML reranker

不建议进入下一大版本主线。

原因：

- 当前没有 feature dump、accept logging、offline benchmark。
- FossilSense 的优势是自包含、低延迟、可解释，ML 会引入部署、回退和隐私复杂度。
- 没有足够数据前，CatBoost/LambdaMART 只是增加系统复杂度。

可作为后续研究：

- deterministic ranker 稳定后，做 opt-in structured telemetry。
- 不上传源码、不上传候选原文，只上传枚举/数值特征和行为结果。
- 模型只做 rerank，不做 recall。
- 必须有 kill switch 和 heuristic fallback。

## 5. 推荐下一版本范围

建议把下一大版本拆成“必做”和“可选 stretch”。

### 必做范围

- Phase 0: completion observability and shadow ranking。
- Phase 1: open-document overlay 扩展和 evidence merge。
- Phase 2: 同 tier 内 intent-aware deterministic reranking。
- Phase 3: rank stability and preselect guard。

这部分预计 18-30 人日，适合形成一个 coherent 的 Smart Completion Foundation。

### 可选 stretch

- Phase 5: include ranking improvement。
- Phase 4 的轻量版：internal pool 300-500 + channel quota，不做复杂 3-gram。

这部分预计额外 8-15 人日。

### 单独立项

- Phase 6 member completion v2。
- Phase 7 local history。
- Phase 8 ML reranker。

原因是它们各自都有独立数据模型、UX 风险和验证方式，塞进同一个版本会让范围失控。

## 6. 关键设计建议

### 6.1 不要废弃 `ScopeTier`

即使将来做 soft prior，也应该继续用现有 `resolver::scope_tier` 产出 canonical scope evidence。变化点只能是 completion ranker 如何消费 `ScopeTier`，而不是新建一套 `SmartScope` / `SemanticScope`。

建议模型：

```text
resolver::scope_tier -> ScopeTier evidence
query::completion_context -> IntentKind evidence
completion evidence assembler -> merged candidate evidence
completion ranker -> score / sortText / labels
```

这能遵守 `CLAUDE.md` “候选不是绑定”和“不得另起 smart/semantic 平行概念”的约束。

### 6.2 soft scope prior 必须分两步

第一步：同 tier 内软排序。

```text
Current candidates still outrank Reachable.
Reachable still outranks External.
External still outranks Unknown.
Unknown still outranks Global.
Intent/kind/locality/history only break ties inside same tier.
```

第二步：completion-only guarded soft prior。

只有在 shadow ranking 指标证明收益后，才允许强本地 evidence 反超较高 tier，并且必须有 guard band：

```text
plain text word 不得轻易反超 indexed symbol
global fallback 不得轻易反超 reachable prefix match
ambiguous/global-only 不得 preselect
low confidence intent 不做负向惩罚
```

这一步需要更新 `CLAUDE.md` 的补全规则、`resolver`/completion tests 和用户可见文档。

### 6.3 evidence merge 比 soft ranker 更优先

当前 dedup 是“同名只保留一个来源”。这会丢失重要信号：同一个名字可能同时来自 index、open document、local binding、raw word、history。

建议先做 merge：

```text
same label/name
  -> merge evidence
  -> choose best insertion payload
  -> score from aggregated evidence
  -> render one item with concise labels
```

这比直接改全局排序风险低，而且能为后续 ML/heuristic ranker 提供统一特征。

### 6.4 completion 解释文本应继续克制

不要显示“resolved to”或“bound to”。推荐用：

- `local`
- `reachable`
- `external`
- `recent`
- `ambiguous`
- `global`
- `text`

文档可写：

```text
Evidence suggests this candidate is local/reachable/recent.
```

不要写：

```text
This resolves to the variable/function/type.
```

## 7. 测试与验收建议

### 7.1 单元测试

应覆盖：

- local binding 仍只在函数体内启用。
- open-document symbol overlay 不依赖 SQLite。
- same-name evidence merge。
- intent classifier 在类型/表达式/调用/声明位置的分类。
- low-confidence / parse fallback 回退。
- rank stability 对 prefix extension 生效。
- short-prefix noise gate 不被绕过。
- include/member completion 继续优先短路。

### 7.2 集成验证

建议基础命令：

```bash
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-mini.sqlite
cd extensions/vscode
pnpm run compile
```

如果是对外演示或发布，还必须：

```bash
cd extensions/vscode
pnpm run package
```

### 7.3 离线评估指标

在没有真实 accept telemetry 前，可以先做 replay-style benchmark：

- 从样本代码中抹掉 identifier 后缀，模拟 prefix。
- 用真实 identifier 作为 expected candidate。
- 分桶统计 Top1 / Top3 / Top5 / Top10。
- 统计 MRR。
- 统计 wrong-kind rate。
- 统计 member fallback owner uncertainty。
- 统计 list churn。
- 统计 p50 / p95 / p99 latency。

重点分桶：

- prefix length 1 / 2 / 3+。
- Current local binding。
- Open-document unsaved symbol。
- Reachable workspace。
- External first-layer。
- Unknown open include graph。
- Global fallback。
- Member resolved。
- Member fallback。
- Include quote / angle。

## 8. 风险清单

| 风险 | 影响 | 可能性 | 缓解 |
|---|---|---:|---|
| soft scope prior 破坏当前 resolver 契约 | 排序概念漂移、测试大量失效 | 高 | 先 shadow，同 tier 内排序，显式设计变更后再默认启用 |
| 热路径变慢 | 每键补全卡顿 | 中 | internal pool 分阶段扩大，所有统计预构建到内存，perf 分桶 |
| 当前文件 raw word 被过度提升 | 噪声冲到 Top1 | 中 | raw text 继续低 source rank，只有 structured evidence 才加权 |
| intent classifier 误判 | 列表排序惊扰 | 中 | 低置信不惩罚，debug dump，分桶测试 |
| 成员补全 v2 伪装语义 | 用户误信错误候选 | 中高 | confidence/detail 标注，method/inheritance 分阶段 |
| schema migration 影响索引稳定性 | 旧库兼容、全量 rebuild 风险 | 中 | member schema 单独版本，migration tests |
| local history accept hook 不稳定 | 数据不准，个性化反向优化 | 中 | 先 spike，不记录不可见候选负反馈 |
| ML 过早进入 | 延迟、隐私、部署复杂度 | 高 | deterministic ranker 稳定后再研究 |

## 9. 建议立即创建的开发任务

1. Smart completion observability requirements
   - 目标：completion feature dump、shadow rank、latency breakdown。
   - 产出：不改变默认排序的 debug 设施。

2. Completion evidence model requirements
   - 目标：把 Indexed / LocalBinding / OpenDocumentSymbol / LocalWord 合成统一 evidence。
   - 产出：same-name evidence merge，仍使用 `ScopeTier` / `ResolutionConfidence` / `ResolutionReason`。

3. Completion intent classifier requirements
   - 目标：TypeName / ExpressionValue / CallTarget / MacroPreprocessor / DeclarationName。
   - 产出：同 tier 内 deterministic reranking。

4. Completion rank stability requirements
   - 目标：减少 `isIncomplete = true` session 中列表抖动。
   - 产出：prefix-extension hysteresis 与 preselect guard。

5. Member completion v2 research spike
   - 目标：确认 method/member schema、parser 采集范围、receiver inference 边界。
   - 产出：是否进入下一版本的独立决策。

## 10. 最终建议

可以推进 Smart Completion，但不要按专家报告里的终局架构一次性重写。

下一步最稳的路线是：

1. 先补观测和 shadow ranking。
2. 扩展现有 current-function local overlay，而不是新造 semantic overlay。
3. 做 evidence merge，减少当前 dedup 丢信号的问题。
4. 做同 tier 内 intent-aware deterministic ranking。
5. 做 rank stability。
6. 用离线指标决定是否进入 completion-only soft scope prior。

这条路线既吸收了专家报告的核心价值，也符合 FossilSense 当前的工程底线：无编译环境优先、best-effort、候选不是绑定、热路径不做磁盘 IO、用户可见不确定性。
