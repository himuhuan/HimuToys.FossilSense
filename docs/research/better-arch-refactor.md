# 总体判断

基于你提供的 FossilSense 架构材料，我认为它的**产品架构方向是正确的，但已经接近复杂度拐点**：薄 VS Code 扩展、单 Rust 进程、SQLite 持久索引、内存读模型、容错解析，以及把“不确定性”显式建模为 `ScopeTier`、`ResolutionConfidence`、`ResolutionReason`、`ReachScope`，这些都是很有价值的基础。当前真正的风险不是功能不足，而是**状态生命周期、缓存一致性和策略归属尚未成为一等架构概念**。

我的核心建议是：

> 保持单进程和模块化单体，不做微服务化；把 FossilSense 演进为一个“原子快照驱动、读写职责分离、候选证据统一、功能策略可插拔”的语言分析引擎。

以下判断属于架构级审查。由于材料没有包含完整源码依赖图、锁竞争数据和性能剖析，部分耦合判断仍需通过源码和运行数据验证；但缓存生命周期、`server`、`FileSemanticIndex` 和 completion 排名体系的风险已经较为明确。

Parnas 的经典模块化原则强调，模块应隐藏“容易变化或难以理解的设计决策”，而不应仅按照系统执行步骤拆分。FossilSense 当前的部分模块，特别是 include、server 和 completion，已经开始按处理流程横跨多个变化原因，因此需要从“流程模块”进一步转向“决策边界模块”。([ACM Digital Library][1])

---

# 一、当前架构的真实数据流

从材料看，FossilSense 实际存在两条主数据流。

```text
写入流

文件系统 / VS Code 文件事件
          │
          ▼
 server 调度、去抖、配置解析
          │
          ▼
 indexer → parser → FileSemanticIndex
          │
          ▼
 SQLite 事务写入
          │
          ├── include edge 更新或重建
          └── NameTable / ReachGraph / IncludeTable 等缓存刷新
```

```text
读取流

LSP 请求
   │
   ▼
server / language_server
   │
   ├── 当前文档 overlay
   ├── local word cache
   ├── NameTable
   ├── ReachGraph
   ├── include table
   ├── SQLite hydration
   └── completion history
          │
          ▼
query / resolver / completion / coloring / references
          │
          ▼
LSP adapter → VS Code
```

问题在于，这两条数据流都在 `server` 汇合。`server` 不仅是协议适配层，还逐渐成为：

* 缓存所有者；
* 索引生命周期管理者；
* 并发调度器；
* 配置解释器；
* 工作区状态机；
* 功能编排器；
* 降级状态提供者。

这会形成典型的**时间耦合**：每个对象单独看都正确，但必须以特定顺序更新，系统才正确。

---

# 二、当前值得保留的稳定边界

以下边界是健康的，不建议在优化中破坏。

**VS Code 扩展与 Rust 引擎边界。** TypeScript 只承担生命周期、配置、命令、状态和 UI，重计算留在 Rust 进程中。这一边界清晰，且符合扩展宿主的资源约束。

**最佳努力候选模型。** “结果是候选，不是编译器级 binding”是 FossilSense 最重要的领域约束。未来增加 namespace、macro、模板或局部类型推断时，也必须继续以候选证据的形式接入，而不能绕过这一模型。

**单一解析入口。** `parse()` 作为统一解析入口是好设计。需要拆分的是输出契约，而不是重新为 completion、coloring、references 创建独立解析器。

**`model` 与 `resolver` 的领域地位。** `ScopeTier`、置信度和原因应该继续作为跨功能共享语言。但共享的应是“证据与语义”，不是强迫所有功能使用一个总分。

**SQLite 与内存读模型的组合。** 这个方向本身正确。需要明确的是：SQLite 是耐久事实存储，内存结构是请求服务快照，二者不能在同一请求中随意跨 generation 混合读取。

---

# 三、高耦合热点与风险排序

| 优先级    | 热点                                      | 主要耦合                                                                                           | 风险与建议                                                                                           |
| ------ | --------------------------------------- | ---------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| **P0** | `server.rs`、`server/language_server.rs` | 协议、调度、缓存、配置、功能编排、状态通知                                                                          | 容易形成 God Object。拆为 `WorkspaceRuntime`、`IndexCoordinator`、`RequestContextFactory` 和纯 LSP adapter |
| **P0** | 多个工作区缓存                                 | `NameTable`、`ReachGraph`、include table、文件列表各自刷新                                                | 可能产生混合 generation。引入不可变 `EngineSnapshot` 并一次性原子发布                                               |
| **P0** | `completion.rs`                         | 候选生成、去重、证据合并、排名、history、截断、指标                                                                  | 新证据源会修改整条流水线。拆为 provider、normalizer、policy、budgeter、explainer                                   |
| **P1** | `FileSemanticIndex`                     | 持久事实、请求时事实、诊断、fallback 状态共存                                                                    | 解析字段变化会扩散至 store 和多个功能。保留单次 parse，但输出分型                                                         |
| **P1** | `resolver` 与 feature-specific ranking   | scope 语义共享，但 completion 又有独立评分逻辑                                                               | 易发生概念漂移。统一证据模型，保留 feature-specific policy                                                       |
| **P1** | include 子系统                             | `includes`、`store/includes`、`indexer/include_edges`、`reachability`、`server/include_completion` | 领域逻辑、持久化和 UI 功能交叉。建立独立 Include Domain                                                           |
| **P1** | `store` 与功能查询                           | schema、migration、写入和 feature-specific reads 混杂                                                 | 上层可能逐渐依赖 SQL 形状。通过 repository/read-model port 隔离                                                |
| **P1** | `tokio`、`rayon`、SQLite                  | async 请求、CPU 解析、阻塞 I/O、单写者约束                                                                   | 可能出现优先级反转和请求取消不及时。建立统一工作调度器                                                                     |
| **P2** | `query` 中的 LSP kind mapping             | 协议无关层含 LSP 概念                                                                                  | 将映射移动至 `server/lsp_adapters`                                                                    |
| **P2** | VS Code `extension.ts`                  | 生命周期、配置、状态、命令继续增长                                                                              | 目前不是高风险；增长后拆为 lifecycle、config bridge、presenter                                                 |

这些热点与材料中列出的 `server`、`completion`、`include_completion`、`coloring`、`query` 和 `store` 等高密度区域基本一致。

---

# 四、建议的目标架构

目标不是增加更多层，而是让每层只承担一种变化原因。

```text
┌────────────────────────────────────────────┐
│              外部适配器                     │
│ VS Code / LSP / CLI / File Watcher / SQLite│
└────────────────────┬───────────────────────┘
                     │
┌────────────────────▼───────────────────────┐
│             Application Runtime            │
│ Request Router                             │
│ Priority Scheduler                         │
│ Index Coordinator                          │
│ Snapshot Publisher                         │
│ Capability Health                          │
└────────────────────┬───────────────────────┘
                     │
┌────────────────────▼───────────────────────┐
│               Feature Use Cases            │
│ Completion / Definition / Hover            │
│ References / Coloring / Signature          │
│ Include Completion / Member Completion     │
└────────────────────┬───────────────────────┘
                     │
┌────────────────────▼───────────────────────┐
│                 Domain Kernel              │
│ Facts / IDs / Candidate / Evidence          │
│ Uncertainty / Scope / Ranking Policies      │
│ ReachScope / Include Semantics              │
└───────────────┬──────────────────┬─────────┘
                │                  │
┌───────────────▼────────┐ ┌──────▼──────────┐
│ Syntax & Fact Producer │ │ Immutable       │
│ lexical + tree-sitter  │ │ Read Models     │
└───────────────┬────────┘ └──────┬──────────┘
                │                  │
                └────────┬─────────┘
                         ▼
                  Storage Ports
                         │
                  SQLite Adapter
```

这是一种轻量的 ports-and-adapters 加读写分离设计，但仍然是一个 Rust 二进制，不引入网络边界和部署复杂度。

---

# 五、最关键的设计：原子工作区快照

当前所有请求都应从同一个不可变快照读取，而不是分别访问多个可变缓存。

```rust
struct RequestContext {
    document: DocumentSnapshot,
    engine: Arc<EngineSnapshot>,
    deadline: Deadline,
    cancellation: CancellationToken,
}

struct EngineSnapshot {
    epoch: EngineEpoch,
    config_revision: ConfigRevision,
    index_generation: IndexGeneration,
    graph_generation: GraphGeneration,

    files: Arc<FileCatalog>,
    names: Arc<NameIndex>,
    symbols: Arc<SymbolDetailsIndex>,
    includes: Arc<IncludeCatalog>,
    reachability: Arc<ReachabilityIndex>,
}
```

一次 LSP 请求开始时只捕获一次 `EngineSnapshot`。之后无论后台索引是否提交，该请求都继续在原快照上执行。新请求可以看到新快照，但单个请求不能同时读取新 `NameTable` 和旧 `ReachGraph`。

索引提交过程应变为：

```text
SQLite 事务提交 generation N
        │
        ▼
构建或增量更新所有读模型
        │
        ▼
验证读模型都属于 generation N
        │
        ▼
一次原子交换发布 EngineSnapshot N
        │
        ▼
发送 ready / degraded 状态
```

这里的关键不是使用哪一种原子指针库，而是建立不变量：

> 一个可见的 `EngineSnapshot` 必须是完整的；不允许先发布 `NameIndex`，再更新 `ReachabilityIndex`。

对于交互热路径，建议内存读模型自足，至少包含 completion、definition、hover 等常用操作所需的紧凑字段。若请求拿到旧快照后还要使用 row ID 去当前 SQLite 中 hydrate，新旧 generation 仍可能混合。冷路径必须满足以下二者之一：

1. SQL 查询显式携带 generation，并使用版本化记录；
2. 使用稳定业务 ID，读取后校验 generation，不匹配则重试新快照。

---

# 六、重构 `FileSemanticIndex`，但不要拆散解析过程

当前 `FileSemanticIndex` 同时包含持久索引事实、文档局部事实和诊断信息。建议保留一次解析、一次 tree-sitter 遍历，但把结果分为三个明确的数据产品：

```rust
struct ParseArtifact {
    persistent: PersistentFileFacts,
    document: Option<DocumentFacts>,
    quality: ParseQuality,
}

struct PersistentFileFacts {
    symbols: Vec<SymbolFact>,
    includes: Vec<IncludeFact>,
    records: Vec<RecordFact>,
    members: Vec<MemberFact>,
    aliases: Vec<AliasFact>,
}

struct DocumentFacts {
    occurrences: Vec<OccurrenceFact>,
    local_declarations: Vec<LocalDeclaration>,
    local_bindings: Vec<LocalBinding>,
}

struct ParseQuality {
    parser_mode: ParserMode,      // AST / lexical / mixed
    diagnostics: Vec<ParseDiagnostic>,
    completeness: FactCompleteness,
}
```

`ParseFacts` 可以继续作为输入计划，但不同消费者只能看到相应类型：

* `IndexWriter` 只接受 `PersistentFileFacts`；
* completion 和 coloring 可读取 `DocumentFacts`；
* 状态和解释层读取 `ParseQuality`；
* SQLite adapter 不应知道 request-time local binding。

这样可以减少“解析器增加一个局部字段，数据库层也被迫感知”的变更扩散。

对于打开的文档，可以缓存 tree-sitter tree，并按编辑增量更新；Tree-sitter 本身就是增量解析库，适合将开放文档的交互解析与后台磁盘索引区分开。([Tree-sitter][2])

---

# 七、把候选系统重构为“证据模型 + 功能策略”

不建议让 `resolver::pack_score` 扩展成一个越来越大的万能分数，也不建议完全统一 completion、definition 和 coloring 的排序语义。

应统一的是候选事实和证据：

```rust
struct Candidate<T> {
    value: T,
    evidence: SmallVec<[Evidence; 4]>,
    uncertainty: UncertaintyVector,
    provenance: Provenance,
}

enum Evidence {
    CurrentDocument,
    LocalBinding,
    ExactName,
    PrefixName,
    ReachableInclude,
    ExternalHeader,
    IndexedFact,
    AstFact,
    LexicalFallback,
    CompletionHistory,
}

struct UncertaintyVector {
    parse_quality: ParseQualityLevel,
    scope_closure: ScopeClosure,
    binding_strength: BindingStrength,
    freshness: Freshness,
}
```

然后由功能策略决定过滤和排序：

```rust
trait RankingPolicy<T> {
    fn eligibility(
        &self,
        candidate: &Candidate<T>,
        ctx: &QueryContext,
    ) -> Eligibility;

    fn score(
        &self,
        candidate: &Candidate<T>,
        ctx: &QueryContext,
    ) -> ScoreVector;

    fn explain(
        &self,
        candidate: &Candidate<T>,
        ctx: &QueryContext,
    ) -> CandidateExplanation;
}
```

建议使用可比较的 `ScoreVector`，而不是把所有维度提前压入一个整数：

```text
[eligibility,
 semantic_evidence,
 scope,
 source_reliability,
 text_match,
 locality,
 history,
 deterministic_tiebreaker]
```

不同功能采用不同策略：

| 功能         | 策略                                    |
| ---------- | ------------------------------------- |
| Definition | scope 和 binding strength 可以形成强优先级     |
| Completion | local/semantic evidence 优先，scope 是软先验 |
| Coloring   | 保守阈值，宁可不着色也不制造错误确定性                   |
| Hover      | 返回最佳候选，并在低置信度时提示存在其他候选                |
| References | 优先召回，分类和置信度后置                         |

用户可见的 `ResolutionReason` 应从证据和不确定性推导，而不是从最终排名分数反推。否则调一个 history 权重，可能意外改变用户看到的“置信度”。

---

# 八、Completion 应成为显式的多阶段流水线

将当前 completion 拆为以下阶段：

```text
Context Extraction
        │
        ▼
Candidate Providers
  ├── LocalBindingProvider
  ├── CurrentDocumentProvider
  ├── NameIndexProvider
  ├── MemberProvider
  ├── TextFallbackProvider
  └── HistoryProvider
        │
        ▼
Normalize + Stable Dedup
        │
        ▼
Feature Ranking Policy
        │
        ▼
Top-K Enrichment
        │
        ▼
Budgeted Truncation
        │
        ▼
LSP Conversion
```

每个 provider 应有独立配额和时间预算：

```rust
trait CandidateProvider<Q, T> {
    fn recall(
        &self,
        query: &Q,
        ctx: &RequestContext,
        budget: &mut QueryBudget,
    ) -> CandidateBatch<T>;
}
```

这样增加 namespace provider、macro provider 或轻量类型推断时，只需增加证据源，不需要在 `completion.rs` 中增加更多分支。

召回算法可以继续保持可解释和确定性：

* exact 和 prefix：排序字符串表上的二分范围检索；
* camel/subsequence：只对前置召回集计算；
* 大符号表 fuzzy：先用首字符、长度区间或 trigram 做粗筛，再对有限集合做精排；
* 只对 top-K hydrate hover、签名和文档等昂贵信息；
* 使用稳定 ID 和稳定 tie-breaker，避免用户继续输入时候选上下跳动。

除了准确率，应增加一个 UX 指标：**Top-K churn**，即连续两个输入前缀之间前 K 个候选的变化率。一个排序略低但稳定的候选列表，通常比频繁跳动的列表更易使用。

当前保持 `isIncomplete` 的设计是合理的：LSP 明确定义，当 completion list 标记为 incomplete 时，继续输入会触发重新计算，而不是仅由客户端过滤旧列表。([GitHub上微软][3])

---

# 九、增量计算：先引入概念，不要立即整体迁移到 Salsa

FossilSense 可以借鉴 Salsa 和 rust-analyzer，但不建议把“引入 Salsa”作为第一步。当前首要问题是状态边界和纯度边界不够清晰；直接引入依赖追踪框架，可能只是把现有耦合隐藏起来。

## 1. 分层文件指纹

不要只记录完整内容 hash。建议增加：

```rust
struct FileSemanticFingerprint {
    content_hash: Hash,
    outline_hash: Hash,
    include_hash: Hash,
    local_facts_hash: Hash,
}
```

含义：

* `outline_hash`：全局 symbols、records、members、aliases；
* `include_hash`：include 指令及解析上下文；
* `local_facts_hash`：occurrences、local bindings；
* `content_hash`：完整内容。

若只修改函数体局部表达式：

* 不重建 `NameIndex`；
* 不更新 include graph；
* 不使工作区级符号查询失效；
* 只更新该文档的局部事实和语义着色。

rust-analyzer 当前明确把“函数体编辑不能使全局派生数据失效”“语法树按文件构建”和“请求上下文重建”作为架构不变量，这些原则很适合 FossilSense 借鉴。([GitHub][4])

## 2. Revision Vector

不要只维护一个全局 generation，可以逐步引入：

```rust
struct RevisionVector {
    document: u64,
    workspace_index: u64,
    include_graph: u64,
    external_headers: u64,
    config: u64,
}
```

每个读模型声明自己依赖哪些 revision。例如：

* `NameIndex` 依赖 `workspace_index` 和 `external_headers`；
* `ReachabilityIndex` 依赖 `include_graph` 和 `config`；
* local completion 依赖 `document`；
* include completion catalog 依赖 `config`、`workspace_index` 和 `external_headers`。

Salsa 的核心思路也是将输入变化与确定性派生计算分开，通过 revision 和依赖关系复用未变化结果；其 durable incrementality 进一步区分高频变化输入和稳定输入，避免局部编辑触发稳定依赖的重新验证。([Salsa][5])

## 3. Include Graph

建议把 include 相关能力组织为一个领域：

```text
IncludeParser
IncludeResolver
IncludeEdgeRepository
IncludeGraphBuilder
ReachabilityService
IncludeCompletionCatalog
```

归属规则：

* `ReachScope`、`OpenReason`：领域模型；
* 图遍历算法：analysis/domain service；
* 图缓存与 generation：runtime/read-model；
* SQL 表和查询：SQLite adapter；
* LSP completion item：LSP adapter。

算法上建议：

* 使用稳定、稠密的 `FileId: u32`；
* 同时维护 outgoing 和 reverse adjacency；
* 文件变化时只更新该文件的 outgoing edges；
* reachability cache 以 `(graph_generation, root_file, policy)` 为 key；
* reachable files 使用 bitset 或压缩 bitmap；
* include 环较多且遍历成本明显时，再考虑 Tarjan SCC 压缩为 DAG；
* include path 配置变化才触发大范围解析重算，而普通文件变化不应默认全图重建。

---

# 十、用户体验应进入架构契约

建议建立如下初始性能目标。这些是项目目标，不是通用行业定律，需要结合真实工作区数据调整。

| 请求                 | 建议目标                                           |
| ------------------ | ---------------------------------------------- |
| Completion         | P50 小于 30ms，P95 小于 100ms，150ms 后停止低价值 provider |
| Hover / Definition | P95 小于 150ms                                   |
| References         | 首批结果小于 300ms，后续分批返回                            |
| 当前文档解析             | 用户输入后优先完成局部语法和 local facts                     |
| Cancellation       | 新请求到达后，旧交互请求尽快停止 CPU 工作                        |
| 后台索引               | 不得长期占满所有 CPU worker，必须为交互请求预留容量                |

调度优先级建议为：

```text
P0  completion / hover / definition / 当前文件 semantic tokens
P1  打开文档重新解析
P2  references / workspace symbols
P3  工作区增量索引
P4  外部头文件扫描、压缩、全量维护
```

对于同一文档的 completion、hover 和 semantic token 请求，应采用 **latest request wins**：新 document revision 到来后，旧请求即使最终完成也不能覆盖新结果。

LSP 原生支持请求取消、work-done progress 和 partial result progress；references、workspace symbols 和索引状态应尽量使用这些标准机制，而不是全部通过自定义状态通知表达。([GitHub上微软][3])

降级状态也不应只有一个全局 “degraded”。建议按能力表达：

```rust
enum CapabilityState {
    Ready,
    Updating { progress: Progress },
    Partial { reasons: Vec<DegradationReason> },
    Unavailable { reason: FailureReason },
}
```

例如：

```text
Completion: Ready
Definition: Partial — include graph contains unresolved edges
Coloring: Ready for current document
References: Updating index
```

这样用户可以理解“什么还能用”，而不是看到一个模糊的全局警告。

---

# 十一、SQLite 与并发模型

建议采用：

```text
一个索引写入协调器
+
有限的短生命周期读取
+
不可变内存快照服务交互请求
```

SQLite WAL 模式允许读者在写入提交期间继续读取，但仍然只有一个 writer；长读事务还可能阻碍 checkpoint。因此不要把长生命周期 SQLite read transaction 绑定到编辑器会话。([sqlite.org][6])

如果准备启用多连接 WAL，需要额外检查 bundled SQLite 版本。SQLite 官方文档在 2026 年披露了一个罕见的 WAL-reset 并发问题，修复版本包括 3.51.3，以及部分旧分支的 3.50.7 和 3.44.6。由于 FossilSense 使用 bundled SQLite，这应成为构建和发布检查项。([sqlite.org][6])

Rust 并发方面，需要明确资源所有权：

* Tokio 负责异步协议和协调；
* CPU 解析统一进入一个有优先级或至少有配额的 CPU executor；
* SQLite 写入只由 `IndexWriter` 串行协调；
* 禁止在 Tokio 核心 worker 上直接执行大范围同步解析或 SQL；
* 请求取消必须传入 provider 和遍历算法，而不只是 LSP handler 最外层；
* 索引事务取消前不能发布任何新 generation。

---

# 十二、用架构适应性测试防止再次退化

建议在 CI 中建立以下“架构不变量测试”。

| 不变量                         | 验证方法                                      |
| --------------------------- | ----------------------------------------- |
| 增量结果等价于全量重建                 | 随机文件修改序列后比较数据库事实和查询结果                     |
| 请求不混用 generation            | 在索引提交和查询并发时注入调度点                          |
| 函数体局部编辑不使全局符号失效             | 比较 `outline_hash` 和 read-model rebuild 数量 |
| 未解析 include 不会提高置信度         | property-based ranking test               |
| 无关候选加入不会改变前列候选顺序            | metamorphic ranking test                  |
| 排名完全确定                      | 同输入多次执行及不同线程调度结果一致                        |
| 取消不会产生半提交状态                 | 在各 pipeline stage 注入 cancellation         |
| SQLite 冷启动与内存增量结果一致         | 重启恢复后执行 golden queries                    |
| LSP 类型不能进入 domain           | crate/module dependency CI 检查             |
| `rusqlite` 类型不能进入 feature 层 | 编译边界或依赖扫描                                 |

还应增加以下工程指标：

```text
每次变更的 invalidation fanout
每百万 symbol 的内存占用
EngineSnapshot 构建和发布时间
completion 各 provider 耗时和召回数
Top-K churn
fallback / open-scope 结果比例
取消请求停止延迟
全量 include graph rebuild 次数
增量索引与全量结果差异数
```

这些指标可以只用于本地 benchmark 和 CI，不需要违反当前“不做远程 telemetry”的产品边界。

---

# 十三、建议的渐进迁移顺序

## 阶段 0：先建立观测和不变量

增加 request trace、generation 日志、provider 耗时、缓存刷新统计，以及“增量等价于全量”的测试。此阶段不改变用户行为。

退出条件：能回答一次 completion 使用了哪个 document revision、哪个 index generation、哪些 provider 和哪种 fallback。

## 阶段 1：解决状态一致性

引入：

* `DocumentRevision`；
* `IndexGeneration`；
* `GraphGeneration`；
* `EngineSnapshot`；
* `RequestContext`；
* 原子 snapshot publish。

把 server 中分散的缓存读取统一收敛到 `RequestContext`。

退出条件：任意请求只使用一个快照；不存在逐个缓存发布。

## 阶段 2：拆分数据产品与存储端口

将 `FileSemanticIndex` 分为 persistent、document 和 quality；引入 `IndexWriter`、`ProjectionLoader`、`SymbolRepository`、`GraphRepository` 等端口；把 rusqlite 类型限制在 adapter 内。

退出条件：feature 层不直接依赖 schema 和 SQL row。

## 阶段 3：重构 completion 与 ranking

先保持现有排序结果，通过 golden tests 锁定行为，再逐步提取 provider、证据、去重、policy 和 explanation。

退出条件：增加一种候选证据源，不需要修改其他 provider，也不需要修改 LSP handler。

## 阶段 4：细粒度增量和调度

增加分层 semantic fingerprint、revision vector、include edge 增量更新、请求取消和优先级调度。

退出条件：局部函数体编辑不触发 NameIndex 和 ReachGraph 重建，交互请求不被后台索引显著拖慢。

## 阶段 5：再评估 crate 拆分或 Salsa

边界稳定后，可以把模块升级为内部 crates，例如：

```text
fossilsense-domain
fossilsense-syntax
fossilsense-index
fossilsense-engine
fossilsense-storage-sqlite
fossilsense-app
```

这些 crate 最终仍链接为一个二进制。只有当手工 revision/dependency 管理已经成为主要复杂度来源时，再对 Salsa 做隔离原型。不要在领域纯度和快照边界尚未稳定时先引入它。

---

# 最终评估

FossilSense 目前不是需要推倒重来的架构。它已经拥有几个难得且正确的核心：明确的产品能力边界、薄编辑器扩展、容错解析、持久索引、读模型和显式不确定性。

当前最大的三项风险依次是：

1. **缓存和索引状态缺少统一的原子快照语义；**
2. **completion 与 server 正在吸收越来越多变化原因；**
3. **持久事实、请求时事实、排名证据和用户解释之间的契约尚未完全分离。**

先解决这三项，未来增加 C++ namespace、macro、轻量类型推断、更多成员信息或新的候选来源时，就可以表现为“增加 provider 和 policy”，而不是继续扩大 `server`、`completion` 和 `FileSemanticIndex`。这会显著提高可扩展性，同时降低缓存失效、并发一致性、排序漂移和用户体验退化的风险。

[1]: https://dl.acm.org/doi/10.1145/361598.361623?utm_source=chatgpt.com "On the criteria to be used in decomposing systems into ..."
[2]: https://tree-sitter.github.io/?utm_source=chatgpt.com "Tree-sitter: Introduction"
[3]: https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/ "Specification"
[4]: https://github.com/rust-lang/rust-analyzer/blob/master/CLAUDE.md "rust-analyzer/CLAUDE.md at master · rust-lang/rust-analyzer · GitHub"
[5]: https://salsa-rs.github.io/salsa/overview.html "Overview - Salsa"
[6]: https://sqlite.org/wal.html "Write-Ahead Logging"
