> **Status: superseded** (2026-07-10)
>
> 权威事实以仓库根目录 CLAUDE.md 与当前代码为准。本文是历史过程/评估文档，只保留决策痕迹，不得当作 backlog、实现规范或自动复活的愿景来源。
## 总体判断

我会把 FossilSense 视为一个**“不依赖可靠编译环境的 C/C++ 候选语义系统”**，而不是传统意义上的编译器前端、clangd 替代品或纯 LSP 包装器。它的核心价值不在于“给出唯一正确绑定”，而在于：在大规模、Windows-oriented、缺失 `compile_commands.json`、宏展开不可靠、IntelliSense/clangd 不可用或成本过高的环境下，持续产出带置信度、作用域、降级原因和候选来源的导航/补全/引用/着色结果。这个产品定位已经比较清晰，而且当前架构中 `model.rs`、`resolver.rs`、`ScopeTier`、`ResolutionConfidence`、`ResolutionReason`、`ReachScope`、`OpenReason` 等抽象，是最值得保护的“架构资产”。
我的核心建议是：**不要优先把系统拆成更多进程或更多 crate；优先把“候选语义域、读模型、缓存代际、LSP 适配、持久化适配、解析事实”这些边界显式化。** 当前最大风险不是 Rust 单体 binary，而是若干模块正在成为高 fan-in/fan-out 的“责任汇聚点”，尤其是 `server`、`completion`、`FileSemanticIndex`、`query`、`store` 与缓存失效逻辑。

这与 Parnas 的经典模块化思想一致：模块划分不应只按执行流程拆，而应隐藏最可能变化、最困难的设计决策。Parnas 的论文明确将模块化与灵活性、可理解性和开发效率关联起来，并强调划分标准会决定模块化效果。([ACM Digital Library][1]) FossilSense 最可能变化的不是“LSP 请求流程”，而是解析事实来源、候选证据、排名策略、作用域可达性、缓存一致性和未来 C++ 智能能力。

---

## 一、当前架构的优秀部分

### 1. 产品边界是正确的

FossilSense 没有承诺 compiler-grade semantic binding，而是把输出定义为 candidates，并通过 `ScopeTier`、`ResolutionConfidence`、`ResolutionReason`、`ReachScope`、`OpenReason` 暴露不确定性。这个边界非常重要，因为它避免了“半吊子编译器前端”的架构陷阱。

这类工具的用户体验关键不是“永远正确”，而是：

```text
能用 > 静默失败
可解释的不确定 > 伪装成确定
渐进增强 > 依赖完整构建环境
```

因此，候选模型、置信度、fallback reason、scope tier 应该被视为核心领域模型，而不是若干 feature 的辅助字段。

### 2. VS Code extension 保持薄层是正确的

当前 TypeScript extension 主要负责激活、二进制路径解析、LSP client、状态栏、命令、配置转发、冲突提示和引用 UI，重计算都在 Rust binary 内。这是合理的：VS Code extension host 不应承担大型 workspace 的扫描、解析、索引与排序压力。

### 3. SQLite + 内存读模型的方向合理

SQLite 作为 durable index，`NameTable`、`ReachGraph`、include table、local word cache 作为热路径读模型，是适合“大仓库 + 增量索引 + 快速查询”的组合。关键问题不是是否使用 SQLite，而是这些读模型的**契约、构建来源、代际一致性、失效边界**是否足够显式。

### 4. `resolver.rs` 是一个关键抗漂移边界

`resolver::scope_tier`、`resolver::pack_score`、`resolver::confidence_reason_for` 把 goto definition、workspace symbols、completion、coloring 等功能共享的候选语义集中起来，这是正确方向。没有这个中心，后续每个 feature 都会私自定义“当前文件优先”“include 可达优先”“external 降权”“unknown/open scope 如何解释”，最终用户看到的置信度和排序会互相矛盾。

---

## 二、我看到的主要架构风险

下面按风险优先级排序。

| 风险点                                       | 当前表现                                                                                          | 架构风险                                                 | 建议优先级 |
| ----------------------------------------- | --------------------------------------------------------------------------------------------- | ---------------------------------------------------- | ----- |
| `server.rs` / `server/language_server.rs` | LSP 编排、缓存、请求分发、设置、索引调度集中                                                                      | 容易变成 God Object；所有 feature 都绕不开它                     | 最高    |
| `completion.rs`                           | 证据源、dedupe、ranking、history boost、metrics、guard、truncation 集中                                  | 排名策略、召回策略、表现层策略混杂                                    | 最高    |
| `FileSemanticIndex`                       | persistent facts 与 request-time facts 共存                                                      | DTO 变成“语义杂物箱”，新增事实会持续膨胀                              | 最高    |
| 缓存失效                                      | `NameTable`、`ReachGraph`、include table、indexed file list、local word cache、history snapshot 多源 | stale cache、跨 generation 查询、局部刷新错误难审计                | 最高    |
| `store` / `store/*`                       | schema、writes、queries、includes、feature-specific query surfaces                                | persistence DTO 与 domain candidate 容易互相污染            | 高     |
| `query.rs` / `query/*`                    | read model、文本定位工具、feature ranking、re-export 混合                                                | “方便复用”最终演变成横向依赖中心                                    | 高     |
| include/reachability 分散                   | `includes`、`store/includes`、`indexer/include_edges`、`reachability`、`resolver` 多处参与            | include 语义、open scope、edge rebuild、scope policy 可能漂移 | 中高    |
| TS extension                              | 目前薄，但 config/status/restart/command 继续增长                                                      | 未来 UI state 与 engine state 纠缠                        | 中     |
| pathing/source identity                   | workspace 相对路径、external absolute path、cache hash、include root                                 | 一旦路径规范化分散，会制造难复现 bug                                 | 中高    |

高耦合核心模块通常会带来长期维护成本。关于架构技术债，MacCormack 与 Sturtevant 的研究指出，高耦合的 core/central components 往往比 loosely-coupled peripheral components 维护成本更高；他们也使用 Design Structure Matrix 这类结构分析方法来评估系统耦合。([Harvard Business School][2]) 近年的架构异味研究也指出，循环依赖、hub-like dependencies、tangled multi-hubs 会随系统演进累积复杂度。([科学直接][3])

---

## 三、建议的目标架构：保留单 binary，重塑内部边界

我建议 FossilSense 的目标架构不是“微服务化”，而是一个**单进程、多上下文、端口适配器风格的模块化核心**。

Hexagonal Architecture / Ports and Adapters 的重点不是一定要引入复杂接口层，而是把核心应用逻辑与 UI、数据库、外部 API 等技术细节隔离；Alistair Cockburn 将 port 描述为一种有目的的交互边界，外部 adapter 可以替换而不污染内部模型。([Alistair Cockburn][4]) AWS 对该模式的解释也强调：应用组件应可独立测试，不依赖 UI 或数据存储，从而减少技术栈变更对业务逻辑的影响。([AWS 文档][5])

对 FossilSense 来说，可以这样分：

```text
┌──────────────────────────────────────────────┐
│ VS Code Extension                             │
│ lifecycle / config / status / command / UI    │
└─────────────────────┬────────────────────────┘
                      │ LSP stdio
┌─────────────────────▼────────────────────────┐
│ LSP Adapter Layer                             │
│ tower-lsp types, URI/Position conversion      │
│ no ranking policy, no persistence policy      │
└─────────────────────┬────────────────────────┘
                      │ Engine requests
┌─────────────────────▼────────────────────────┐
│ Application Orchestration                     │
│ WorkspaceSession / Scheduler / CacheLedger    │
│ DocumentStore / FeatureService dispatch       │
└─────────────────────┬────────────────────────┘
                      │ Domain services
┌─────────────────────▼────────────────────────┐
│ Candidate Domain Core                         │
│ model / resolver / scope / confidence / rank  │
│ best-effort candidate semantics               │
└───────┬─────────────┬──────────────┬─────────┘
        │             │              │
┌───────▼──────┐ ┌────▼───────┐ ┌────▼────────┐
│ Semantic     │ │ Read       │ │ Feature     │
│ Facts        │ │ Models     │ │ Pipelines   │
│ parser facts │ │ NameTable  │ │ definition  │
│ diagnostics  │ │ ReachGraph │ │ completion  │
└───────┬──────┘ └────┬───────┘ │ hover etc.  │
        │             │         └────┬────────┘
┌───────▼─────────────▼──────────────▼─────────┐
│ Persistence Adapter                           │
│ SQLite schema / migration / query / writes    │
└──────────────────────────────────────────────┘
```

这里的关键是：**server 层只适配协议，application 层只编排生命周期，domain 层表达候选语义，persistence 层只处理 SQLite。**

---

## 四、模块边界重构建议

### 1. 把“候选语义域”提升为核心 domain

建议将 `model.rs`、`resolver.rs` 以及与 scope/confidence/reason/ranking 相关的类型收敛成一个更明确的领域子模块，例如：

```text
src/domain/
  mod.rs
  candidate.rs
  scope.rs
  confidence.rs
  reason.rs
  ranking.rs
  source.rs
```

核心规则：

```text
domain 不依赖 tower-lsp
domain 不依赖 rusqlite
domain 不依赖 VS Code 概念
domain 不读取文件系统
domain 不知道缓存实现
```

该层应该回答：

```text
一个候选是什么？
一个候选为什么可信？
一个候选为什么不确定？
一个候选属于 current/reachable/external/unknown/global 哪一层？
open scope 如何影响 confidence？
ranking 中哪些信号是硬约束，哪些是软信号？
```

这样做的收益是：未来添加 namespaces、templates、inheritance、macro hints、optional clang evidence 时，不会让每个 feature 各自发明一套置信度语言。

### 2. 拆分 `FileSemanticIndex`

当前 `FileSemanticIndex` 同时承载：

```text
symbols
includes
occurrences
records
fields
members
aliases
local_declarations
local_bindings
diagnostics
```

它既是持久化事实的输入，又是 request-time features 的事实载体。这种 DTO 短期很方便，长期会导致三类问题：

1. 新增 feature 时自然往里面塞字段；
2. 调用方无法区分“未请求”“请求但不可得”“fallback 后为空”；
3. persistent schema 与 request-time parse 需求互相牵制。

建议拆成：

```rust
struct ParsedFile {
    identity: SourceIdentity,
    facts: FileFacts,
    diagnostics: ParseDiagnostics,
    provenance: ParseProvenance,
}

struct FileFacts {
    persistent: PersistentFacts,
    request: RequestFacts,
}

struct PersistentFacts {
    symbols: Vec<SymbolFact>,
    includes: Vec<IncludeFact>,
    records: Vec<RecordFact>,
    members: Vec<MemberFact>,
    aliases: Vec<TypeAliasFact>,
}

struct RequestFacts {
    occurrences: FactState<Vec<OccurrenceFact>>,
    local_declarations: FactState<Vec<LocalDeclarationFact>>,
    local_bindings: FactState<Vec<LocalBindingFact>>,
}

enum FactState<T> {
    NotRequested,
    Available(T),
    Unavailable { reason: ParseFallbackReason },
}
```

重点不是具体 Rust 代码，而是引入一个明确语义：**空列表不再同时表示“没有事实”“没有请求”“fallback 失败”“解析器不支持”。**

这会直接提升用户体验，因为 hover、references、semantic tokens、member completion 可以向上层返回更准确的 degradation reason。

### 3. 为 `NameTable`、`ReachGraph`、include table 建立显式读模型契约

当前 `NameTable` 是热路径读模型，`ReachGraph` 是 include graph 读模型，include completion table 是另一个读模型。它们都很合理，但需要被制度化。

建议引入统一概念：

```rust
struct ReadModelSnapshot<T> {
    workspace_id: WorkspaceId,
    index_generation: IndexGeneration,
    settings_generation: SettingsGeneration,
    built_at: Instant,
    data: Arc<T>,
}
```

并把缓存分为四类：

| 缓存类型               | 示例                                                          | 失效条件                                      |
| ------------------ | ----------------------------------------------------------- | ----------------------------------------- |
| index-derived      | `NameTable`, `ReachGraph`, include table, indexed file list | full index 或 dirty-file index 成功提交        |
| settings-derived   | include paths, include scoping mode, semantic coloring mode | LSP initialization/settings generation 变化 |
| document-derived   | local word cache, current-file overlay                      | open document version 变化                  |
| local-user-derived | completion history                                          | history mutation 或 clear command          |

所有 feature 请求都应该拿到一个不可变的 `WorkspaceSnapshot`：

```rust
struct WorkspaceSnapshot {
    index_generation: IndexGeneration,
    settings_generation: SettingsGeneration,
    name_table: Arc<NameTable>,
    reach_graph: Arc<ReachGraph>,
    include_table: Arc<IncludeCompletionTable>,
}
```

这样可以避免 handler 在一次请求中混用不同 generation 的缓存。

### 4. 重构 `server` 为 WorkspaceSession + FeatureService

`server/language_server.rs` 不应继续承载“所有事情的中枢”。建议拆为：

```text
server/
  language_server.rs       # tower-lsp trait implementation only
  adapters.rs              # LSP <-> engine DTO
  workspace_session.rs     # workspace state facade
  document_store.rs        # open documents and versions
  scheduler.rs             # index debounce, rebuild, refresh
  cache_ledger.rs          # cache generations and snapshots
  handlers/
    definition.rs
    completion.rs
    hover.rs
    references.rs
    semantic_tokens.rs
    signature_help.rs
    workspace_symbols.rs
```

LSP handler 的理想形态：

```text
LSP request
-> adapter converts URI/Position/params
-> WorkspaceSession builds QueryContext
-> FeatureService executes protocol-agnostic use case
-> adapter converts EngineResult to LSP response
```

边界规则：

```text
tower_lsp types 只允许出现在 server/lsp_adapters 和 handlers 外壳
feature modules 不接触 Client、Url、Position、CompletionItem
server 不直接拼 ranking 细节
server 不直接拼 SQL query
```

### 5. 把 completion 变成显式 pipeline

`completion.rs` 是高风险点，因为补全天然容易吸纳所有策略：召回、证据、去重、排序、历史、fallback、UX guard、metrics、truncation。建议拆成管线：

```text
completion/
  mod.rs
  context.rs          # trigger/context extraction
  recall.rs           # recall orchestration
  sources/
    indexed.rs
    locals.rs
    current_file.rs
    words.rs
    history.rs
    members.rs
  evidence.rs         # evidence normalization
  identity.rs         # dedupe key
  rank.rs             # ranking policy
  truncate.rs         # quota/truncation
  explain.rs          # user-visible provenance/confidence
  metrics.rs
```

管线阶段：

```text
1. TriggerGuard
2. CompletionContext extraction
3. Recall from channels
4. Evidence normalization
5. Candidate identity + dedupe
6. Scope signal enrichment
7. Ranking policy
8. Quota/truncation
9. Presentation mapping
```

建议把候选拆成三层：

```rust
struct CompletionEvidence {
    source: EvidenceSource,
    scope_signal: ScopeSignal,
    text_match: TextMatchSignal,
    locality: LocalitySignal,
    history: HistorySignal,
    provenance: EvidenceProvenance,
}

struct CompletionCandidate {
    identity: CandidateIdentity,
    label: String,
    kind: CandidateKind,
    evidence: Vec<CompletionEvidence>,
}

struct PresentedCompletion {
    candidate: CompletionCandidate,
    sort_key: SortKey,
    detail: Option<String>,
    documentation: Option<String>,
}
```

这样可以避免“为了 LSP 展示方便”污染候选证据，也能让 ranking policy 可测试、可版本化。

### 6. 明确 `resolver` 与 completion ranking 的关系

当前设计里 `resolver::pack_score` 是严格 tier-dominant 排名，而 ordinary completion 使用 evidence-aware ranking，把 `ScopeTier` 当作软先验。这是合理的，但需要文档化为架构规则：

```text
Definition-like features:
  tier 是强策略，Current > Reachable > External > Unknown > Global

Completion-like features:
  tier 是软信号，不能单独淘汰候选
  evidence merge 后综合排序
```

建议用两个类型避免概念漂移：

```rust
enum ScopeTier {
    Current,
    Reachable,
    External,
    Unknown,
    Global,
}

struct ScopeSignal {
    tier: ScopeTier,
    strength: SignalStrength,   // Hard / Soft / Weak
    reason: ResolutionReason,
}
```

这样 ordinary completion 不必“违背” resolver，而是显式声明自己使用 soft scope signal。

### 7. 把 SQLite 变成 persistence adapter，而不是 domain API

当前 `store` 已经有 schema/writes/queries/includes 拆分，这是好基础。但建议再推进一步：让 feature 模块依赖“查询端口”，而不是依赖 SQLite shaped records。

可以定义较小的 reader traits：

```rust
trait SymbolReadStore {
    fn load_name_table_rows(&self, workspace: WorkspaceId) -> Result<Vec<NameRow>>;
    fn hydrate_symbols(&self, ids: &[SymbolId]) -> Result<Vec<SymbolRecord>>;
}

trait IncludeGraphStore {
    fn load_include_edges(&self, workspace: WorkspaceId) -> Result<Vec<IncludeEdge>>;
}

trait MemberReadStore {
    fn find_members(&self, query: MemberQuery) -> Result<Vec<MemberRecord>>;
}
```

但不要过度抽象成一个巨大 `IndexStore` trait。更好的方式是按 read model builder 或 use case 切接口：

```text
NameTableBuilder -> SymbolReadStore
ReachGraphBuilder -> IncludeGraphStore
MemberCompletionService -> MemberReadStore
```

SQL 结果应先转成 persistence DTO，再映射成 domain model。避免 `rusqlite::Row`、SQL column naming、schema migration 细节渗透到 query/completion/resolver。

### 8. include/reachability 应成为独立领域

include 相关逻辑现在横跨：

```text
includes
store/includes
indexer/include_edges
reachability
resolver
completion
coloring
```

这不是一定错误，因为 include 语义天然跨越扫描、存储、查询和 UX。但需要一个明确的领域边界：

```text
include_domain/
  directive.rs        # raw include directive
  roots.rs            # workspace/external include roots
  resolution.rs       # include target resolution + ambiguity
  edge.rs             # persisted include edge domain shape
  graph.rs            # ReachGraph
  reach_scope.rs      # ReachScope/OpenReason
  policy.rs           # feature-specific effect matrix
```

尤其要把“open scope 对不同 feature 的影响”写成一张 policy matrix：

| Feature           | closed reachability           | open reachability                   |
| ----------------- | ----------------------------- | ----------------------------------- |
| semantic coloring | 可 hard-gate reachable/current | 降级为宽松策略，并显示/记录 open reason          |
| definition        | tier-dominant ranking         | unknown/external 保留候选但降低 confidence |
| completion        | reachable/current 加权          | 不因不可达直接删除候选                         |
| references        | textual recall 保留             | role/confidence 降级                  |
| hover             | 优先 current/reachable          | 保留 fallback explanation             |

这样避免每个 feature 私自解释 `OpenReason`。

Microsoft 对 Anti-Corruption Layer 的定义是：当两个子系统语义不同，通过 facade/adapter 层翻译请求，避免外部语义限制内部设计。([微软学习][6]) 在 FossilSense 内部，LSP、SQLite、tree-sitter、VS Code 配置、include root、future clang-like provider 都可以被看作外部语义源；它们应通过 adapter 翻译成 FossilSense 自己的 candidate/fact/reachability 语言。

---

## 五、推荐的目录演进

不建议一次性大搬家。可以先建立新的边界，再逐步迁移。

目标形态可参考：

```text
crates/fossilsense/src/
  app/
    workspace_session.rs
    scheduler.rs
    cache_ledger.rs
    document_store.rs

  domain/
    candidate.rs
    scope.rs
    confidence.rs
    ranking.rs
    reason.rs
    source.rs

  workspace/
    config.rs
    pathing.rs
    scanner.rs
    include_roots.rs

  facts/
    mod.rs
    parsed_file.rs
    persistent.rs
    request.rs
    diagnostics.rs

  parser/
    mod.rs
    lexical.rs
    ast.rs
    tree_sitter.rs

  indexing/
    pipeline.rs
    dirty.rs
    planner.rs
    writer.rs
    include_edges.rs
    progress.rs

  persistence/
    mod.rs
    ports.rs
    sqlite/
      schema.rs
      migrations.rs
      writes.rs
      queries.rs
      includes.rs

  read_models/
    name_table.rs
    reach_graph.rs
    include_table.rs
    local_words.rs
    builders.rs

  include_domain/
    directive.rs
    resolution.rs
    graph.rs
    reach_scope.rs
    policy.rs

  features/
    definitions.rs
    hover.rs
    references.rs
    signatures.rs
    coloring.rs
    completion/
      context.rs
      recall.rs
      evidence.rs
      rank.rs
      truncate.rs
      sources/
        indexed.rs
        locals.rs
        current_file.rs
        words.rs
        history.rs
    member_completion/
      context.rs
      resolve.rs
      rank.rs
    include_completion/
      context.rs
      table.rs
      rank.rs

  server/
    language_server.rs
    lsp_adapters.rs
    handlers/
      completion.rs
      definition.rs
      hover.rs
      references.rs
      semantic_tokens.rs
      signature_help.rs
      workspace_symbols.rs
```

这个目录不是要求马上落地，而是表达一个边界原则：

```text
server 负责协议
app 负责生命周期和并发
domain 负责候选语义
facts 负责解析事实结构
parser 负责事实提取
indexing 负责构建 durable index
persistence 负责 SQLite
read_models 负责热路径查询模型
features 负责功能管线
include_domain 负责 include/reachability 语义
```

C4 模型适合把这个目标架构文档化。C4 的官方介绍强调从 system context、container、component 到 code 多层 zoom-in，用于沟通、onboarding、架构评审、风险识别和威胁建模。([C4 model][7]) 对 FossilSense 来说，建议至少维护三张图：Context 图、Rust binary 内部 Component 图、Indexing/Query/Completion 三条关键序列图。

---

## 六、数据流应重新文档化为 5 条主链路

当前 packet 已经描述了 startup、indexing、parsing、storage、query hot path，但为了后续扩展，建议把数据流固化为以下 5 条“架构主链路”。

### 1. Startup / configuration flow

```text
VS Code settings
-> extension config normalization
-> LSP initialization options
-> server options parsing
-> WorkspaceSession settings_generation
-> index scheduler / feature flags / read model policy
```

设计重点：

```text
配置只在一个地方归一化
影响初始化的配置显式 restart
可热更新的配置显式 settings_generation
每个 feature 能知道自己使用的是哪一代配置
```

### 2. Full indexing flow

```text
WorkspaceConfig
-> scanner discovers workspace candidates
-> include roots resolved
-> external include candidates capped
-> fingerprint check
-> parse changed files
-> PersistentFacts
-> SQLite writes
-> missing file cleanup
-> include edge rebuild
-> index_generation++
-> read model rebuild/publish
```

设计重点：

```text
parse product 与 persisted product 分离
index commit 是 generation 边界
read model rebuild 不应被 request handler 临时触发到混乱状态
```

### 3. Dirty-file flow

```text
file watcher / document save / change notification
-> DirtyFileChange
-> dirty index planner
-> parse selected files
-> transactional write/delete
-> affected include edge update
-> index_generation++
-> dependent read models invalidated/refreshed
```

设计重点：

```text
dirty update 必须能说明影响了哪些 read model
delete/upsert 对 include graph 的影响要可测试
open document overlay 与 persisted index 不应混淆
```

### 4. Query flow

```text
LSP request
-> LSP adapter converts params
-> QueryContext built from WorkspaceSnapshot + DocumentSnapshot
-> feature pipeline
-> domain candidates with confidence/reason
-> LSP adapter presentation
```

设计重点：

```text
feature 不读 tower-lsp 类型
feature 不直接读 SQLite
feature 使用同一套 candidate semantics
```

### 5. Completion flow

```text
trigger context
-> recall indexed/local/current-file/text/history/member/include sources
-> normalize evidence
-> merge candidate identity
-> enrich with scope/reachability
-> rank
-> truncate
-> explain/present
```

设计重点：

```text
召回源独立
证据归一化
ranking 可测试
presentation 最后发生
```

---

## 七、架构治理：用“fitness functions”防止边界回退

SEI 将 architectural tactic 定义为影响质量属性的设计决策，并把 modifiability 与 coupling、cohesion、cost motivations 联系起来。([卡内基梅隆大学软件工程研究所][8]) 对 FossilSense，建议把“可维护性”落成可自动检查的 architecture fitness functions。

### 建议加入 CI 检查

```text
1. 禁止 tower_lsp 出现在 server/ 之外
2. 禁止 rusqlite 出现在 persistence/store adapter 之外
3. 禁止 vscode-specific 概念进入 Rust domain/features
4. 禁止 feature 模块直接依赖 server
5. 禁止 parser 依赖 store
6. 禁止 domain 依赖 parser/indexer/persistence/server
7. 禁止路径规范化逻辑散落在 pathing/workspace 之外
8. 检查 Rust module dependency cycle
9. 检查 TS extension import cycle
10. 检查大型模块阈值：行数、函数数、fan-in/fan-out
```

### 建议度量

| 指标                               | 用途                    |
| -------------------------------- | --------------------- |
| module fan-in / fan-out          | 找出 central components |
| cycle count                      | 找架构异味                 |
| change coupling                  | 从 git 历史看哪些文件经常一起改    |
| defect/change hotspot            | 找“高变更 + 高复杂度”区域       |
| public type count per module     | 找抽象泄漏                 |
| request path allocations/latency | 防止 UX 退化              |
| cache generation mismatch count  | 找 stale cache 风险      |
| fallback reason distribution     | 看用户体验是否真实降级           |

尤其推荐做一个简单 DSM，即 Design Structure Matrix，横轴/纵轴是模块，矩阵格子表示 import dependency 或 co-change dependency。这个方法对识别 core/periphery、循环依赖和隐性耦合很有效，和前述架构技术债研究的分析方向一致。([Harvard Business School][2])

---

## 八、用户体验导向的架构要求

FossilSense 的 UX 不应该只靠 UI 层修补，而要由核心架构保证。

### 1. 所有 feature 返回“带降级信息的结果”

建议定义统一结果形态：

```rust
struct EngineResult<T> {
    items: T,
    confidence: ResultConfidence,
    reasons: Vec<DegradationReason>,
    provenance: Vec<EvidenceProvenance>,
    metrics: Option<QueryMetrics>,
}
```

对用户来说，最差体验不是“结果不完美”，而是“不知道为什么跳错/补错/少了”。当前已有 `ResolutionConfidence`、`ResolutionReason`、`OpenReason` 等基础，应继续放大它们的作用。

### 2. 降级原因应标准化

建议将 degradation reason 统一成有限枚举：

```text
NoIndexYet
IndexStale
OpenScopeDueToUnresolvedInclude
OpenScopeDueToAmbiguousInclude
ReachDepthCapped
ReachNodeCapped
ParserFallbackLexicalOnly
RequestFactsUnavailable
ExternalIncludeCapped
TextFallbackUsed
HistoryOnlyCandidate
```

这样 VS Code status、hover detail、completion detail、debug log、tests 都能复用同一语言。

### 3. 用户可见标签必须来自 domain，而不是 feature 临时拼接

例如：

```text
High confidence · current file
Medium confidence · reachable include
Low confidence · external header
Fallback · lexical parse only
Open scope · unresolved include
```

这些标签应来自 `domain/confidence.rs` 或 `domain/reason.rs`，而不是散落在 hover/completion/definition/coloring 中。

---

## 九、在添加更多 C++ intelligence 前必须先处理的风险

FossilSense 未来若添加 namespaces、templates、inheritance、overload hints、macro heuristics、optional compile database、optional clang provider，最容易破坏现有边界。

我建议先引入一个统一的“证据模型”：

```rust
enum EvidenceSource {
    LexicalParser,
    TreeSitterAst,
    SQLiteIndex,
    IncludeReachability,
    CurrentFileOverlay,
    LocalBinding,
    TextSearch,
    CompletionHistory,
    FutureCppHeuristic,
    FutureCompileDatabase,
    FutureClangProvider,
}

struct Evidence {
    source: EvidenceSource,
    confidence: EvidenceConfidence,
    scope: Option<ScopeSignal>,
    range: Option<CandidateRange>,
    symbol: Option<SymbolFact>,
    reason: EvidenceReason,
}
```

未来的 C++ intelligence 不应该直接产出“最终定义”或“最终补全”，而应产出 evidence，再由现有 candidate/resolver/ranking 层合并。这能保持产品原则：**新智能是渐进增强，不是推翻 best-effort candidate model。**

---

## 十、分阶段落地路线

### Phase 0：冻结架构语言，补齐文档

目标：不重构，先降低认知风险。

产出：

```text
1. ARCHITECTURE.md
2. C4 Context / Container / Component diagrams
3. ADR: best-effort candidate model
4. ADR: SQLite durable index + read models
5. ADR: scope/confidence/reason canonical source
6. ADR: cache generation model
7. module dependency graph
8. architecture risk register
```

### Phase 1：建立边界检查

目标：防止继续恶化。

动作：

```text
1. CI 检查 tower_lsp 使用范围
2. CI 检查 rusqlite 使用范围
3. CI 检查 module cycles
4. 给 server/completion/query/store 设置模块复杂度阈值
5. 加入 cache generation debug assertion
```

### Phase 2：拆 `FileSemanticIndex`

目标：解决事实 DTO 膨胀。

动作：

```text
1. 引入 PersistentFacts / RequestFacts / ParseDiagnostics
2. 引入 FactState：NotRequested / Available / Unavailable
3. indexer 只消费 PersistentFacts
4. coloring/references/member completion 消费 RequestFacts
5. 测试 fallback provenance
```

### Phase 3：重构 server orchestration

目标：降低 God Object 风险。

动作：

```text
1. 新增 WorkspaceSession
2. 新增 DocumentStore
3. 新增 CacheLedger
4. handler 只做 LSP adapter + use case call
5. index scheduler 从 language_server 中剥离
```

### Phase 4：重构 completion pipeline

目标：让补全可演进、可测试、可解释。

动作：

```text
1. 拆 context / recall / evidence / identity / rank / truncate / present
2. 每个 recall source 单测
3. ranking policy 单测与 golden tests
4. 输出 evidence explanation
5. completion metrics 与 fallback reason 标准化
```

### Phase 5：明确 persistence/read-model 契约

目标：让 SQLite 和 read model 各司其职。

动作：

```text
1. 定义 read store ports
2. NameTableBuilder / ReachGraphBuilder / IncludeTableBuilder 显式化
3. SQL DTO 与 domain candidate 分离
4. 所有 read model snapshot 带 generation
```

### Phase 6：为高级 C++ intelligence 预留 evidence provider

目标：未来功能不破坏核心模型。

动作：

```text
1. 引入 EvidenceSource/Evidence
2. C++ heuristic 只作为 provider
3. 不让 namespace/template/inheritance 逻辑散进 resolver/completion/server
4. 给新 provider 设置 capability flags 和 fallback reason
```

---

## 最终建议

FossilSense 当前架构的方向是好的：薄 VS Code extension、Rust engine、SQLite durable index、内存读模型、中心化 candidate semantics、明确 best-effort 边界，这些都值得保留。真正需要优化的是**内部职责重心**：

```text
从“按功能流程组织”
转为
“按变化原因与语义边界组织”
```

我会把最高优先级放在四件事上：

```text
1. 把 domain candidate / scope / confidence / reason 固化为核心领域层
2. 把 FileSemanticIndex 拆成 persistent facts 与 request-time facts
3. 把 server 改成 LSP adapter + WorkspaceSession，而不是全局调度中心
4. 把 completion 拆成 evidence pipeline，避免继续成为策略黑洞
```

这样做之后，FossilSense 才能安全地继续增加 C++ 特定智能，而不会把“无可靠编译环境下的可解释候选系统”演变成一组互相冲突的启发式功能集合。

[1]: https://dl.acm.org/doi/10.1145/361598.361623?utm_source=chatgpt.com "On the criteria to be used in decomposing systems into ..."
[2]: https://www.hbs.edu/ris/Publication%20Files/2016-JSS%20Technical%20Debt_d793c712-5160-4aa9-8761-781b444cc75f.pdf "Technical debt and system architecture: The impact of coupling on defect-related activity"
[3]: https://www.sciencedirect.com/science/article/pii/S0164121224002152 "Evolution patterns of software-architecture smells: An empirical study of intra- and inter-version smells - ScienceDirect"
[4]: https://alistair.cockburn.us/hexagonal-architecture "hexagonal-architecture"
[5]: https://docs.aws.amazon.com/prescriptive-guidance/latest/cloud-design-patterns/hexagonal-architecture.html "Hexagonal architecture pattern - AWS Prescriptive Guidance"
[6]: https://learn.microsoft.com/en-us/azure/architecture/patterns/anti-corruption-layer "Anti-Corruption Layer Pattern - Azure Architecture Center | Microsoft Learn"
[7]: https://c4model.com/introduction "Introduction | C4 model"
[8]: https://www.sei.cmu.edu/library/modifiability-tactics/ "Modifiability Tactics | CMU Software Engineering Institute"
