> **Status: superseded** (2026-07-10)
>
> 权威事实以仓库根目录 CLAUDE.md 与当前代码为准。本文是历史过程/评估文档，只保留决策痕迹，不得当作 backlog、实现规范或自动复活的愿景来源。
# Smart Completion 开发评估报告

Status: draft
Date: 2026-07-05
Input: `docs/research/smart-completion.md`
Scope: 结合当前代码树评估下一大版本补全优化的工作量、可行性、用户体验收益和维护成本。

## 0. 结论摘要

外部报告提出的方向总体正确：FossilSense 下一阶段不应追求 clangd 式精确语义，而应把现有“索引 + include 可达性 + 模糊召回”升级为“多证据聚合 + 意图感知 + 稳定低延迟排序”。但报告中的若干伪代码没有看到当前实现，直接照搬会踩到两个现有约束：

1. 当前代码已经把 `ScopeTier` 作为跨跳转、补全、着色、workspace symbol 的统一排序/解释原语，并且在 `resolver::pack_score` 中写死了“scope tier 严格压倒文本质量”的不变量。
2. 当前补全热路径依赖内存 `NameTable`、open document live parse cache、completion prefix pool，不适合在 LSP handler 里继续堆 ad hoc scoring 或磁盘查询。

因此推荐路线不是“在现有 score 上继续加权”，而是做一次有边界的补全内核重构：

```text
CompletionContext
  -> 多通道召回 CandidateSource
  -> CandidateEvidence 聚合
  -> DeterministicCompletionRanker
  -> LSP Renderer + Debug/Shadow Metrics
```

下一大版本建议目标：

> 在保持 best-effort、不隐藏不确定性、不让候选消失的前提下，让普通标识符、成员、include 三类补全的 Top 1/3/5 更贴近当前编辑意图，并降低列表抖动。

推荐优先级：

1. **必须先做：补全观测与评估基线**，否则 soft ranking、intent、history 的收益不可调。
2. **高性价比：补全候选/evidence/ranker 模块化重构**，把 strict tier 改为补全专用的 soft scope prior，但保留 guard band。
3. **高性价比：扩展 current-file ephemeral overlay**，当前函数参数/局部变量已经完成一部分，下一步补齐宏、typedef、enum、当前文件函数/类型、附近词等证据。
4. **中高收益：轻量 intent classifier**，先识别 TypeName / ExpressionValue / CallTarget / MacroPreprocessor / DeclarationName / IncludePath / MemberAccess。
5. **中等收益：多通道召回与配额合并**，避免单一 top-N 在短前缀时截断强相关候选。
6. **高用户感知但高成本：成员补全从 field-only 扩展到 member evidence**，建议放在第二阶段，不要和 ranker 重构塞进同一里程碑。
7. **暂不建议进入下一大版本核心范围：匿名 telemetry、ML ranker、自动 include 插入、继承/模板/重载级 C++ 语义。**

按单人熟悉 Rust/LSP/本仓库估算，推荐下一大版本 MVP 约 **35-55 人日**；如果包含成员方法索引和弱 receiver inference，约 **55-85 人日**；如果再包含本地历史个性化，约 **70-110 人日**。ML/匿名训练是独立产品线级工作，保守估计 **45+ 人日** 且依赖隐私、版本分发和线上评估体系，不建议现在承诺。

本评估只基于当前代码树和 `docs/research/smart-completion.md`。工作区中已有一个被删除的 `docs/research/smart-completion-dev-eval.md`，按用户要求忽略旧方案，不从 git 历史恢复或复用。

## 1. 当前代码现状

### 1.1 普通标识符补全

当前普通补全入口在 `crates/fossilsense/src/server/language_server.rs`：

- `#include` 上下文先短路到 include path completion。
- `.` / `->` 上下文先短路到 member completion。
- 之后基于当前行 prefix 执行普通 identifier completion。
- prefix 长度小于 `MIN_PREFIX_LEN` 时返回 empty incomplete list。
- open document 会通过 `get_or_parse_document` 进入 live parse cache。
- 当前函数参数和局部变量通过 `query::local_completion_candidates` 注入候选池。
- 全局/工作区符号来自每个 workspace root 的内存 `NameTable`。
- 当前文件 raw words 仍作为 `CompletionCandidateSource::LocalWord` fallback。
- 返回 `CompletionList { is_incomplete: true }`，符合项目约定。

当前已有三个候选来源：

| Source | 当前含义 | 排序/去重特点 |
|---|---|---|
| `Indexed` | SQLite 索引加载到内存 `NameTable` 的符号，不含 field | 由 `NameTable` 按 `ScopeTier + base_match + locality` 排序 |
| `LocalBinding` | 当前函数参数/局部变量，来自 open document live parse | same-name dedup 优先级最高 |
| `LocalWord` | 当前文件词表 fallback | 不当作当前文件定义，避免压过 reachable/external indexed symbol |

这说明外部报告的“当前文件 overlay”方向已经部分落地：当前函数参数和声明早于光标的局部变量已经是结构化候选，不再只是 raw text word。差距在于 overlay 范围仍窄：当前文件宏、typedef、enum 常量、当前未保存函数/类型、field-like token、附近使用频次、声明距离等证据还没有统一进入候选模型。

### 1.2 NameTable 与召回

`crates/fossilsense/src/query.rs` 中 `NameTable` 目前承担召回与初步排序：

- `sorted` lower-name 数组支持 exact/prefix 二分。
- `score_match` 支持 exact / prefix / boundary substring / plain substring / subsequence。
- 短前缀 `< 3` 只允许分数不低于 650 的匹配，抑制长尾噪音。
- `search_ranked_scoped_pooled` 支持 prefix 逐字符延长时复用上一轮 pool。
- 每次每个 root 直接取 `COMPLETION_LIMIT = 100` 的 ranked hits。

这条路径已经比较成熟，但它把“召回”和“最终排序”绑得太紧。外部报告建议的多通道召回、内部候选池扩大、evidence rerank，在当前结构里会受限，因为强相关候选可能在 `NameTable` top 100 外就被截断。

### 1.3 ScopeTier 与 strict ranking

`crates/fossilsense/src/resolver.rs` 目前是排序核心：

```text
ScopeTier::Current > Reachable > External > Unknown > Global
pack_score = tier.rank() * TIER_STRIDE + base_match + locality
```

`TIER_STRIDE` 被设计成严格大于 `MAX_BASE_MATCH + MAX_LOCALITY`，并有测试固定“tier 必须压倒文本质量”。这和外部报告“scope 从硬字典序主导改成强先验分数”的建议正面冲突。

这个冲突不是坏事，而是提醒我们：如果要采纳 soft scope prior，必须作为一次明确的设计迁移完成，不能在 `server/language_server.rs` 局部绕开 `resolver` 加 magic score。否则会造成 `CLAUDE.md` 反复警告的“概念漂移”。

建议：

- `scope_tier` 仍保留为唯一 scope 证据来源。
- `pack_score` 可以保留给 goto/coloring 或严格策略。
- 新增补全专用 `CompletionRanker`，输入是 `CandidateEvidence`，输出 final score、guard reason、presentation label。
- 文档同步更新：补全从 strict tier 迁移为 evidence-aware soft ranking；跳转定义是否跟进另行决策。

### 1.4 成员补全

当前 member completion 在 `crates/fossilsense/src/server/member_completion.rs`：

- 只处理 `.` / `->`。
- 从当前文件 AST 的 `local_declarations` 推断 receiver record。
- 通过 `store.resolve_record_candidates` 支持 record/typedef alias 候选。
- 命中 record 后只查 `fields_for_records`。
- receiver 推断失败时走 `fallback_field_candidates(prefix)`。
- UI kind 固定是 `CompletionItemKind::FIELD`。

代码注释明确写了“Fields only”。外部报告建议加入 method、static method、nested type、inherited member、链式 receiver，这不是排序小改，而是 parser/schema/indexer/store/LSP 的完整扩展：

```text
Parser: 提取 class/struct 内 method declaration/definition、访问级别、owner
Schema: 新增 members 或扩展 fields 表
Indexer: 写入 member entries，增量删除/重建
Store: owner-scoped member query、fallback member query
Resolver: owner tier + receiver confidence + member evidence
Server: member item kind/detail/documentation/render
Tests: C/C++ class method、typedef owner、fallback、ambiguous owner、性能
```

这条线用户感知很强，但工作量和回归风险也最高，建议排在 ranker/overlay 稳定之后。

### 1.5 Include path completion

`crates/fossilsense/src/server/include_completion.rs` 当前能力：

- quote include: current dir -> workspace -> includePaths。
- angle include: includePaths -> workspace -> current dir。
- workspace include table 在索引后构建，避免每键查 SQLite。
- external include root 有目录 listing cache。
- 过滤 header-like 文件和目录。
- 排序主要由来源 base score + prefix/exact 提升决定。

外部建议中的 recent include、sibling include、basename frequency、path depth penalty 都还没有。这一部分不需要改变核心语义，属于中等工作量、较低风险的 UX 提升。

### 1.6 可观测性

当前已有：

- `fossilsense.debug.candidateReasons`：只覆盖 goto-definition 候选理由。
- `fossilsense.trace.server=verbose` 或 `RUST_LOG`：输出 completion/reference/index/semantic token 等 perf 日志。
- `completion_memo`：记录 prefix pool，但不记录 session rank 稳定性。

当前缺少：

- completion candidate feature dump。
- old rank / new rank shadow comparison。
- accepted candidate rank。
- rank churn/list instability。
- per-source candidate count。
- truncation miss 观测。
- 本地离线 benchmark 数据集和指标。

没有这些，soft ranker 调参会变成主观体验拉扯。

## 2. 对外部建议的采纳判断

| 外部建议 | 采纳判断 | 原因 |
|---|---|---|
| Evidence-Aware Intent Ranking 总架构 | 强采纳 | 符合 best-effort 定位，能统一解释排序原因 |
| Context Classifier | 采纳，但先做规则版 | tree-sitter + lexical context 足够支持粗粒度 intent，不需要 ML |
| current-file ephemeral overlay | 强采纳，但注意当前已有 LocalBinding | 需要从“函数局部”扩展到“当前文件结构化临时符号” |
| strict tier -> soft scope prior | 采纳，但必须设计迁移 | 直接改 `pack_score` 会破坏现有不变量和测试 |
| 多通道召回 + 配额 | 采纳 | 当前每 root top 100 可能截断强相关候选 |
| evidence merge dedup | 强采纳 | 当前 same-name dedup 是“选赢家”，会丢证据 |
| rank stability / hysteresis | 采纳 | `isIncomplete=true` 每键重算，列表稳定性是 UX 关键 |
| include recent/sibling ranking | 采纳 | 低风险、体验收益明确 |
| member methods / weak receiver inference | 选择性采纳，第二阶段 | 需要 schema/parser/indexer 大改 |
| local history personalization | 后置采纳 | 有价值，但需要隐私、持久化和负反馈定义 |
| 匿名日志 + CatBoost/LambdaMART | 暂不采纳为近期目标 | 需要产品/隐私/分发/AB 体系，当前基础不够 |
| LLM completion | 不建议 | 与离线、低延迟、自包含、best-effort 导航定位不匹配 |
| 自动插入 include | 暂不采纳 | 缺少高置信 header ownership，误编辑风险高 |

## 3. 推荐目标架构

### 3.1 模块拆分

建议新增或重组为以下协议无关模块，避免把复杂逻辑塞进 LSP handler：

```text
crates/fossilsense/src/completion/
  mod.rs
  context.rs       // CompletionContext, IntentKind, TriggerKind
  evidence.rs      // CandidateEvidence, MatchEvidence, ScopeEvidence, LocalityEvidence
  recall.rs        // 多通道召回入口，组合 NameTable / overlay / include / member
  ranker.rs        // DeterministicCompletionRanker
  render.rs        // 转 LSP 前的 display/detail/documentation 数据
  stability.rs     // session rank hysteresis
  debug.rs         // feature dump / shadow ranking 输出
```

也可以先放在 `query/completion_*` 下，关键不是目录名，而是边界：

- `server/language_server.rs` 只判断 LSP 上下文、拿 snapshot、调用 completion service、渲染返回。
- `query::NameTable` 只负责高效召回和 raw match evidence，不直接决定最终 rank。
- `resolver` 继续提供 `scope_tier`、reachability reason、confidence projection，但补全最终排序由 completion ranker 负责。
- 当前 `CompletionCandidateSource` 升级为更完整的 `CandidateSource` / `CandidateEvidence`。

### 3.2 核心数据模型

建议的最小模型：

```rust
pub struct CompletionContext {
    pub prefix: String,
    pub trigger: TriggerKind,
    pub intent: IntentKind,
    pub intent_confidence: f32,
    pub current_path: Option<String>,
    pub cursor: CursorPoint,
    pub reach: Option<ReachScope>,
}

pub struct CompletionCandidate {
    pub key: CompletionCandidateKey,
    pub label: String,
    pub insert_text: Option<String>,
    pub kind: CompletionKind,
    pub evidence: CandidateEvidence,
}

pub struct CandidateEvidence {
    pub sources: SmallVec<[CandidateSource; 4]>,
    pub scope_tier: ScopeTier,
    pub confidence: ResolutionConfidence,
    pub reason: ResolutionReason,
    pub text_match: TextMatchEvidence,
    pub intent_match: IntentMatchEvidence,
    pub locality: LocalityEvidence,
    pub quality: QualityEvidence,
    pub history: Option<HistoryEvidence>,
}
```

`CandidateEvidence` 的重点是“证据合并”，不是一次性把所有信号压成整数。这样同名候选可以合并：

```text
Foo from index reachable
Foo from current-file overlay
Foo recently accepted
=> one Foo item, evidence = reachable + local + recent
```

UI 可以显示最有用的短标签，如 `local`、`reachable`、`recent`、`global`，detail/documentation 再展开完整证据。

### 3.3 排序策略

第一阶段不要上 ML。推荐确定性加权 + guard band：

```text
final_score =
  intent_score
+ scope_prior
+ text_score
+ locality_score
+ quality_score
+ usage_score
- penalties
```

但必须保留 UX 护栏：

- plain text word 不能仅靠模糊匹配冲到高置信 indexed symbol 之前。
- global fallback 要超过 reachable prefix match，必须有强本地/历史/intent 证据。
- ambiguous/open-scope 候选可以上浮，但必须继续显示 ambiguity/fallback。
- 类型上下文降低变量，表达式上下文降低类型。
- DeclarationName 上下文应降低已有全局符号，避免补全把“我要声明新变量”误导成“复用旧符号”。

这比完全放弃 tier 更稳：用户能获得更贴近意图的 Top-N，同时不会看到 random text word 突然占据第一。

### 3.4 与现有 resolver 的关系

推荐迁移方式：

1. 保留 `resolver::scope_tier` 为唯一 scope 判断。
2. 保留 `confidence_reason_for`，但允许 completion ranker 附加更多 explanation。
3. `pack_score` 不再作为普通补全唯一排序入口，改为 strict policy 的实现细节。
4. `CLAUDE.md`、README、测试同步更新“补全 soft ranking，goto/coloring strict/hard gate”的新约定。
5. 禁止在 LSP handler 内直接写 magic number；所有权重进入 `CompletionRankerConfig`，有测试固定。

## 4. 分阶段计划与工作量

估算假设：

- 1 人熟悉 Rust、tower-lsp、tree-sitter、SQLite 和本仓库。
- 包含单元测试、集成测试、文档同步。
- 不包含对外发布打包，若发布追加 VSIX 验证。
- 人日为有效开发日，未包含长时间人工 dogfood。

### Phase 0：补全观测与评估基线

目标：能回答“新排序是否更好、慢在哪里、抖在哪里”。

内容：

- completion debug dump：每个候选 source、tier、match、intent、score、最终 rank。
- perf breakdown：context、recall、merge、rank、render。
- shadow ranking 框架：旧排序展示，新排序后台算 rank delta。
- 离线 fixture：mini-c、slop-cases、ambiguity、FFmpeg 小采样。
- 指标脚本：MRR、Recall@3/5、wrong-kind rate、list churn、p95 latency。

工作量：**5-8 人日**

可行性：高。

风险：debug 输出过多影响性能或泄漏源码。缓解：默认关闭，开发模式只输出结构化摘要，必要时 hash 候选名。

验收：

- `fossilsense.trace.server=verbose` 能看到 completion 分阶段耗时。
- 测试可构造一组 old/new rank fixture。
- Debug dump 不改变排序和返回内容。

### Phase 1：补全核心模块化重构

目标：把普通补全从 LSP handler 内的候选拼接，迁移为协议无关 completion pipeline。

内容：

- 新建 `CompletionContext` / `CandidateEvidence` / `CompletionRanker`。
- `NameTable` 返回 recall hits + raw evidence，不直接决定最终排序。
- 当前 `CompletionCandidateSource::{Indexed, LocalBinding, LocalWord}` 迁移为 evidence sources。
- same-name dedup 改成 evidence merge。
- LSP 渲染保留 `sortText`、`detail`、`documentation` 行为。
- 旧排序先通过兼容 ranker 复现，确保重构不改变行为。

工作量：**8-14 人日**

可行性：高，但需要小步提交。

风险：大文件 `server.rs` / `language_server.rs` 继续膨胀。缓解：先抽纯逻辑，server 只做 IO 和 LSP。

验收：

- 现有 `cargo test -p fossilsense` 通过。
- 重构后旧排序 fixture 完全一致。
- 无新增每键 SQLite 查询或 workspace scan。

### Phase 2：确定性 evidence-aware ranker

目标：实现 soft scope prior + guard band + intent/kind/locality 基础权重。

内容：

- `ScopeTier` 从 strict packing 改为补全 rank feature。
- ranker weights 集中定义并单测。
- guard band 防止 low-confidence text/global 候选乱冲。
- current function `LocalBinding` 保持高优先级。
- raw local word 根据距离/出现频次获得有限上浮，但不伪装成 current semantic symbol。
- 输出 short detail：`local` / `reachable` / `external` / `ambiguous` / `global` / `text`。

工作量：**7-12 人日**

可行性：中高。

风险：破坏当前“reachable 永远压过 global”的可解释性。缓解：只在 completion feature 中迁移，并用 debug reason 明示反超原因。

验收：

- 当前函数局部变量可以压过不同名 reachable 弱匹配。
- reachable prefix match 仍稳定压过普通 global fuzzy。
- short prefix 噪音不回潮。
- `CompletionList.isIncomplete` 仍恒为 true。

### Phase 3：current-file ephemeral overlay 扩展

目标：把当前 open document 中更多强意图信号结构化，而不是只靠 raw words。

当前已完成：

- 当前函数参数。
- 当前函数局部变量。
- 语义着色也复用了 local bindings。

建议新增：

- 当前 open document 的宏定义。
- typedef/using/type alias。
- enum constants。
- 当前文件函数声明/定义。
- 当前文件 record/type 定义。
- 当前函数附近 identifier 使用频次和距离。
- plain word fallback 保持最低层。

工作量：**6-10 人日**

可行性：高。

风险：overlay 与 indexed same-name 候选重复。缓解：必须走 evidence merge，不能再“保留一个赢家”。

验收：

- 未保存的新 typedef/macro 能参与普通补全。
- 同名 indexed + overlay 合并为一个 item，detail 不冲突。
- parse fallback 时退回 raw words/indexed candidates。

### Phase 4：轻量 intent classifier

目标：让排序知道“此处想要什么”，但不做完整 C++ 语义。

首批 intent：

| Intent | 场景 | 排序影响 |
|---|---|---|
| `IncludePath` | `#include "..."` / `<...>` | 走 include completion |
| `MemberAccess` | `.` / `->` | 走 member completion |
| `TypeName` | 声明、cast、`new`、template-like 位置 | type/typedef/record 上浮 |
| `ExpressionValue` | RHS、return、condition、argument | variable/function/constant 上浮 |
| `CallTarget` | identifier 后接/即将接 `(` | function/function-like macro 上浮 |
| `MacroPreprocessor` | `#if/#ifdef/#define` | macro 上浮 |
| `DeclarationName` | 类型后声明新变量 | 降低复用已有全局符号，提升本地命名模式 |

工作量：**6-10 人日**

可行性：中高。

风险：误判上下文比不判断更烦。缓解：intent confidence 低时只给小权重，不硬过滤。

验收：

- 类型位置变量下降，类型上升。
- 表达式位置类型下降，变量/函数/常量上升。
- preprocessor 行宏上升。
- malformed code 不崩，intent 低置信降级。

### Phase 5：多通道召回与配额

目标：召回阶段不被单一 top-N 填满，为 rerank 保留候选多样性。

建议通道：

```text
current_file_ephemeral: 150-250
current_function_locals: 100
reachable_index_symbols: 300-500
direct_external_symbols: 150-250
unknown_open_scope_symbols: 150-250
global_backoff_symbols: 100-200
current_file_text_words: 80-120
```

实现方式：

- `NameTable` 支持按 source/tier 产出 raw pool，而不是只返回最终 top 100。
- scoped recall 可以先按 match gate 取较大 pool，再交 ranker。
- completion memo 需要记录各通道 pool generation。
- 对 3+ prefix 可考虑 token-boundary/camel index，避免全表 fuzzy scan。

工作量：**8-14 人日**

可行性：中。

风险：候选池扩大导致延迟上升。缓解：分桶 cap、复用 pool、perf p95 门禁。

验收：

- 短前缀 p95 不显著劣化。
- reachable/global/current overlay 在内部池中都有配额。
- 长 prefix 下原本被 top-N 截断的候选能重新进入 Top 100。

### Phase 6：include completion 排序增强

目标：include path 补全更符合项目习惯。

内容：

- 当前文件最近 include。
- sibling 文件 include 模式。
- 同目录 header 优先。
- basename frequency。
- path depth penalty。
- quote/angle 继续保留不同 base prior。

工作量：**5-9 人日**

可行性：高。

风险：需要从 indexed includes 构建额外内存表。缓解：和 `IncludeCompletionTable` 同步构建、同 generation 失效。

验收：

- quote include 中同目录/sibling 常用 header 上升。
- angle include 中 includePaths/外部 root 仍优先。
- external dir cache 行为不退化。

### Phase 7：成员补全扩展到 member evidence

目标：`obj.` / `ptr->` 后不再只像字段列表，而是更接近 IDE 的 fields + methods。

建议拆两步：

第一步：schema 与 parser 支持 method member。

- 新增 `members` 表，或把 `fields` 重构为 `members`。
- `MemberKind::{Field, Method, StaticMethod, EnumMember, NestedType}`。
- C++ class/struct 内 method declaration。
- out-of-class `Foo::bar` 先做低置信 owner 关联，能做再做。
- LSP kind 区分 FIELD/METHOD/ENUM_MEMBER/CLASS。

第二步：weak receiver inference。

- 当前已有 `Foo x; x.^` 和 typedef alias 基础。
- 增加 pointer/reference 更明确区分。
- `auto x = makeFoo()`、`x = getFoo()`、一跳 `a.b.^` 作为低置信扩展。
- receiver name 和 owner name correlation 只作为 fallback 弱信号。

工作量：**18-30 人日**，复杂 C++ owner 关联另加。

可行性：中。

风险：最容易越过 FossilSense “不伪装精确”的边界。缓解：所有 method/weak receiver 必须显示低置信/ambiguous owner，不做访问控制/重载承诺。

验收：

- resolved receiver 下 field/method 同时出现。
- fallback 不因方法加入而大量污染 Top-N。
- `fields` 不泄漏进普通补全的现有约束仍成立，除非明确设计允许某些 method 作为普通符号。

### Phase 8：本地历史个性化

目标：利用用户在本机/本 workspace 的选择习惯提升排序。

内容：

- local-only SQLite/JSON stats。
- accepted count EWMA。
- prefix-symbol stats。
- intent-symbol stats。
- last accepted decay。
- 可清除、可关闭。
- 不上传，不收集源码。

工作量：**10-18 人日**

可行性：中。

风险：LSP 是否能可靠知道 completion accept。VS Code extension 侧可能需要参与。缓解：先只在 extension 能可靠捕获时做本地 opt-in；否则延后。

验收：

- 最近接受候选在相同 prefix/intent 下适度上升。
- undo/delete 不轻易当强正反馈。
- 禁用后排序回到 deterministic ranker。

### Phase 9：匿名日志与 ML reranker

目标：训练全局排序模型。

判断：不建议进入下一大版本承诺。

阻塞项：

- 隐私政策和用户授权。
- 特征脱敏协议。
- 日志 schema 版本化。
- 数据采集、训练、评估、模型分发。
- kill switch 和 deterministic fallback。
- A/B 或至少 shadow deployment。

工作量：**45+ 人日**，且不是纯工程任务。

## 5. 推荐下一大版本范围

建议把下一大版本拆成“可发布 MVP”和“增强包”。

### 5.1 MVP 必做

目标：不碰 ML，不碰完整 C++ 语义，先把补全体验从 strict tier top-N 升级到 deterministic evidence-aware ranking。

范围：

1. Phase 0 补全观测与评估。
2. Phase 1 completion pipeline 模块化。
3. Phase 2 deterministic ranker。
4. Phase 3 current-file overlay 扩展。
5. Phase 4 轻量 intent classifier 的前四类：TypeName / ExpressionValue / CallTarget / MacroPreprocessor。
6. 文档同步：can / cannot / fallback / confidence / rank explanation。

预计工作量：**35-55 人日**。

推荐发布口径：

```text
FossilSense 1.x smart completion:
- 更理解当前编辑上下文的普通标识符补全
- 当前文件未保存声明、局部符号和 include 可达候选会合并证据排序
- 类型/表达式/调用/宏上下文下候选种类更贴近意图
- 补全详情可解释候选为何上浮
- 仍是 best-effort，不做完整 C++ 语义绑定
```

### 5.2 增强包

范围：

1. Phase 5 多通道召回。
2. Phase 6 include ranking。
3. Phase 7 成员 method/member evidence 第一阶段。

预计增量：**20-35 人日**。

这部分可作为同一大版本后续 minor 发布，不建议压进首个 smart completion MVP。

### 5.3 暂缓

暂缓内容：

- 本地历史个性化。
- 匿名 telemetry。
- ML ranker。
- auto include insertion。
- C++ inheritance/template/overload/name namespace 语义。

暂缓原因不是价值低，而是它们依赖前面的 evidence/ranker/observability 基础。先做这些会让后续调参和回归成本失控。

## 6. 用户体验评估

### 6.1 预期正收益

普通补全：

- 当前函数/当前文件强相关候选能进入 Top 1/3/5。
- 类型位置和表达式位置的 wrong-kind 候选减少。
- same-name 候选不再丢证据，用户看到的标签更稳定。
- 未保存编辑中的新声明更快进入补全。

成员补全：

- 第二阶段加入 method 后，`obj.` 体验会明显更像 IDE。
- weak receiver inference 能减少 fallback field 噪音。

Include：

- 大仓库里 quote include 更贴近同组件习惯。
- angle include 对外部 root/标准头仍保持直觉排序。

### 6.2 UX 风险

列表抖动：

- `isIncomplete=true` 每键重算，soft ranking 会增加 rank movement。
- 需要 session hysteresis：分数差距小于阈值时保留相对顺序。

误接受：

- Top1 上浮如果置信不够，用户按 Enter/Tab 容易误选。
- preselect 必须保守：低置信 global/text/ambiguous 不应默认预选。

解释噪音：

- detail 不能塞满 `tier/confidence/reason/score`。
- 建议短标签只显示 `local/reachable/external/recent/ambiguous/global/text`，完整解释进 documentation 或 debug。

过度承诺：

- “intent-aware” 容易被用户理解成语义理解。
- 文档必须继续写清：这是 evidence，不是 binding。

## 7. 开发维护评估

### 7.1 为什么需要模块化重构

如果继续在 `server/language_server.rs` 里堆逻辑，会出现三个问题：

1. LSP IO、缓存、召回、排序、UI 渲染混在一起，测试只能走重型 server tests。
2. 权重分散在 `NameTable`、`resolver`、server dedup、member completion 中，难以解释和调参。
3. 后续本地历史、shadow ranking、ML fallback 都没有插入点。

建议把补全逻辑抽成 protocol-agnostic 层，server 只负责：

- 取 document snapshot。
- 判断 include/member/ordinary surface。
- 取 workspace root / reach scope / name tables。
- 调 completion service。
- 转 LSP item。

### 7.2 需要同步更新的项目约定

如果采纳 soft ranking，必须同步这些文档/测试：

- `CLAUDE.md` 第 6 节补全规则。
- `CLAUDE.md` 第 10 节 Resolver 与候选语义层。
- `README.md` 当前能力。
- `extensions/vscode/package.json` 配置描述。
- `resolver::tier_packing_invariant_holds` 相关测试，或明确限定 strict packing 只用于非补全策略。
- `server::tests::local_word_does_not_outrank_reachable_indexed_candidate` 等严格 tier 断言。

不要留下“文档要求 strict tier，代码却 soft rank”的状态。

### 7.3 测试策略

必须新增四类测试：

1. **Ranker 单元测试**：给定 evidence，断言顺序和 guard band。
2. **Context classifier 测试**：类型/表达式/调用/宏/声明名上下文。
3. **Pipeline fixture 测试**：NameTable + overlay + reach scope + ranker 端到端。
4. **Stability 测试**：同一 session prefix 延长时，低差距候选保持相对稳定。

建议保留现有：

- parser local binding tests。
- store member tests。
- include completion tests。
- query scoping tests。
- index dirty/full cache invalidation tests。

新增评估脚本可以先不进 CI，但应该能本地运行：

```text
cargo run -p fossilsense -- index samples/mini-c --db target/smart-completion-mini.sqlite --force
cargo test -p fossilsense completion::ranker
cargo test -p fossilsense completion::context
cargo test -p fossilsense query::tests
```

## 8. 关键技术风险与缓解

| 风险 | 影响 | 可能性 | 缓解 |
|---|---|---|---|
| soft ranker 破坏当前可解释性 | 用户不理解为什么 global/text 上浮 | 中 | evidence documentation + guard band + debug dump |
| 候选池扩大导致延迟上升 | 每键补全卡顿 | 中 | channel cap、pool memo、p95 门禁、先 shadow |
| intent classifier 误判 | wrong-kind 上浮 | 中 | intent confidence，低置信只给小权重 |
| evidence merge 改变 same-name 行为 | 原有测试大量变化 | 中 | 先兼容重构，再启用新 ranker |
| member method schema 迁移复杂 | 索引/增量/查询回归 | 高 | 与 ranker 分期，单独 schema version |
| debug/telemetry 泄漏源码 | 隐私风险 | 中 | 默认关闭，只输出枚举/分数/可选 hash，不上传 |
| 文档和实现不一致 | 维护者误改 | 高 | 同步 `CLAUDE.md` 并用测试固定新不变量 |

## 9. 建议实施顺序

```text
Milestone A: 可观测性与兼容重构
  A1 completion debug/metric dump
  A2 completion pipeline 抽纯逻辑
  A3 旧排序兼容 ranker

Milestone B: Evidence-aware deterministic ranking
  B1 CandidateEvidence + evidence merge
  B2 soft scope prior + guard band
  B3 current-file overlay 扩展
  B4 TypeName / ExpressionValue / CallTarget / MacroPreprocessor intent
  B5 rank stability

Milestone C: 召回与 include
  C1 多通道召回配额
  C2 include sibling/recent/frequency 排序
  C3 p95/p99 latency tuning

Milestone D: member completion 下一阶段
  D1 member schema/parser/store
  D2 method completion
  D3 weak receiver inference
```

若只能选一个最小切入点：

> 先做 Milestone A + B1/B2。也就是先让同名 evidence merge 和 soft scope prior 可解释地跑起来。它能验证外部报告最核心的判断，同时不会立即陷入 C++ method/receiver 的复杂坑。

## 10. 最终建议

外部报告值得采纳，但要把它工程化为 FossilSense 自己的路线：

- 不做“智能补全”平行系统。
- 不绕开 `ScopeTier` / `ResolutionConfidence` / `ResolutionReason`。
- 不在 LSP handler 局部加 magic score。
- 不把 ML 或完整 C++ 语义作为下一大版本卖点。
- 先做 deterministic、可解释、可回退的 evidence-aware ranker。

下一大版本最合理的成功标准不是“支持更多语义”，而是：

```text
用户在真实大仓库中输入 1-3 个字符后，
更少滚动、更少继续输入、更少误接受，
并且能看懂 FossilSense 为什么把某个 best-effort 候选排在前面。
```

这与 FossilSense 的定位一致：轻量、开箱即用、在没有可靠编译环境时仍能给出有解释的高质量候选。
