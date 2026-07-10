> **Status: superseded** (2026-07-10)
>
> 权威事实以仓库根目录 CLAUDE.md 与当前代码为准。本文是历史过程/评估文档，只保留决策痕迹，不得当作 backlog、实现规范或自动复活的愿景来源。
# Healthy FossilSense 架构重构开发评估报告

Status: draft
Date: 2026-07-06
Input: `docs/research/healthy-fossilsense-architecture-refactoring.md`
Scope: 结合当前代码树评估 v1.2.2 以代码质量、架构健康度、可维护性为目标的重构路线、工作量、可行性与用户功能保护策略。

## 0. 结论摘要

外部评估的主判断是正确的：FossilSense 不应拆成多进程或微服务，也不应为了“架构感”新增平行概念；它最值得保护的是现有的 best-effort candidate 语义、`ScopeTier`、`ResolutionConfidence`、`ResolutionReason`、`ReachScope`、`OpenReason`、SQLite durable index 和内存读模型。

但外部评估没有看到全部代码，因此有两处需要修正：

1. 当前代码已经不是“无边界单体”。`model.rs` / `resolver.rs` 已经是候选语义中心，`parser` 已有 `ParseFacts`，`store` 已拆出 schema/writes/queries/includes，`server/indexing/cache.rs` 已经有 NameTable / ReachGraph / IncludeTable / indexed file list 的 rebuild 与 generation 机制，`completion.rs` 已经有 evidence-aware pipeline。
2. 当前最大问题不是“没有抽象”，而是若干抽象仍停留在文件内或局部门面层，缺少显式契约、快照边界、模块依赖规则和行为保持型回归测试。

因此 v1.2.2 最合理的目标应是：

> 做一次行为保持型的架构健康版本：用户可见能力、排序策略、fallback 语义、配置、VSIX 交付方式不主动改变；内部则把 server/cache/completion/store/parser facts 的边界显式化，让后续功能版本更容易推进。

推荐范围：

| 范围 | 工作量 | 推荐度 | 说明 |
|---|---:|---|---|
| 保守 MVP：架构文档 + 依赖边界检查 + server/cache/ordinary completion 的行为保持型拆分 | 40-60 人日 | 高 | 最符合 v1.2.2 “工程质量版本”定位 |
| 推荐增强：MVP + `FileSemanticIndex` facts 语义过渡层 + `IndexStore` 小型 read/write facade | 60-90 人日 | 中高 | 能解决更深层结构债，但要严格分阶段 |
| 完整外部目标架构：domain/facts/app/persistence/read_models/features 全面迁移 | 95-140 人日 | 中 | 技术上可行，但不建议压进一个 minor 版本 |

如果只能选一条主线，建议优先做：

```text
1. 冻结用户可见行为和架构语言
2. 建立依赖/模块边界检查
3. 把 server 的 workspace state、cache generation、document store 抽成 WorkspaceSession/CacheLedger
4. 把 ordinary completion 从 LSP handler 中抽成协议无关服务，先保证排序完全兼容
```

不建议 v1.2.2 做的事：

- 不做补全排序策略改变，除非作为 shadow/兼容测试，不改变返回顺序。
- 不做新 C++ 智能能力、ML、telemetry、auto include insertion。
- 不做全仓目录大搬家式的“看起来干净”的重构。
- 不引入大型 trait 层或过度 hexagonal 抽象。
- 不改变 VSIX 自包含发布承诺。

## 1. 当前代码事实

### 1.1 代码规模与热点

当前版本是 `1.2.1`，Rust crate 和 VS Code extension 版本一致。代码树中 Rust 源文件约 68 个，TypeScript 源文件约 13 个。通过简单行数统计，主要热点如下：

| 文件 | 行数 | 观察 |
|---|---:|---|
| `crates/fossilsense/src/completion.rs` | 1583 | evidence、intent、rank、history、metrics、shadow rank 都在一个文件内 |
| `crates/fossilsense/src/server/include_completion.rs` | 1239 | include completion 已独立，但承担召回、排序、缓存、render |
| `crates/fossilsense/src/server/language_server.rs` | 1006 | LSP trait、请求编排、ordinary completion、references、commands 混合 |
| `crates/fossilsense/src/coloring.rs` | 932 | coloring 逻辑和大量测试在同文件 |
| `crates/fossilsense/src/server.rs` | 837 | server 状态、LSP 类型、completion adapter、缓存字段集中 |
| `crates/fossilsense/src/query.rs` | 821 | NameTable、recall、query re-export、文本工具混合 |
| `crates/fossilsense/src/resolver.rs` | 800 | 核心语义集中，测试丰富，值得保留为中心边界 |
| `crates/fossilsense/src/parser/ast.rs` | 750 | AST facts 收集在单 pass 中，性能方向正确但扩展压力大 |
| `crates/fossilsense/src/server/indexing.rs` | 682 | full/dirty index 调度、状态通知、cache rebuild 编排集中 |
| `crates/fossilsense/src/store/queries.rs` | 681 | store 查询面已拆出，但 `IndexStore` 仍是单一大门面 |

测试保护网不薄：Rust `#[test]` / `#[tokio::test]` 约 484 个，TS test 中约 31 行断言。测试覆盖了 parser、resolver、query、completion、include、reachability、store、indexer、server、LSP smoke 和 extension 配置/状态/冲突/引用 UI 等关键路径。

这意味着 v1.2.2 可以重构，但必须走“先锁行为，再迁移边界”的路线。当前测试已经足以支撑很多内部拆分，但还缺少架构边界测试和跨 feature 的 golden behavior tests。

### 1.2 已有架构资产

外部评估建议提升为核心 domain 的部分，代码中已经有基础：

| 资产 | 当前位置 | 价值 |
|---|---|---|
| 候选语义模型 | `model.rs` | `DefinitionCandidate`、`ScopeTier`、`ResolutionConfidence`、`ResolutionReason` 是跨功能语言 |
| 共享 resolver | `resolver.rs` | `scope_tier`、`pack_score`、`confidence_reason_for` 防止各功能私造排序语义 |
| 容错 parse product | `parser.rs` | `parse()` 是唯一入口，`ParseDiagnostics` 标注 fallback 和 provenance |
| parse facts mask | `ParseFacts` | index-time 已用 `INDEX` 跳过 request-time facts，说明 facts 拆分有落点 |
| read model | `NameTable`、`ReachGraph`、`IncludeCompletionTable` | 符合大仓库热路径要求，补全每键不查 SQLite |
| cache generation 雏形 | `server/state.rs`、`server/indexing/cache.rs` | generation 已由各读模型 `Arc` 指针组合而来，可升级为显式 snapshot |
| store 分层雏形 | `store/schema.rs`、`writes.rs`、`queries.rs`、`includes.rs` | 已按职责拆文件，但 API 仍从 `IndexStore` 暴露 |
| VS Code 薄层 | `extensions/vscode/src/*` | extension 主要做启动、配置、状态、命令、冲突提示，不承担重计算 |

这些资产应保留，不应被 v1.2.2 重构推翻。

### 1.3 当前主要结构风险

| 风险 | 当前表现 | 用户风险 |
|---|---|---|
| `server` 高 fan-in/fan-out | `server.rs` 同时持有 LSP 类型、缓存、completion adapter、history、references cache、index schedule | 重构时容易改变状态栏、索引调度、缓存失效、completion session 行为 |
| ordinary completion handler 过重 | `language_server.rs` 中 completion 分支同时处理上下文、history、live parse、local words、NameTable、memo、spawn_blocking、render | 任何“纯重构”都可能改变排序、`isIncomplete`、本地词 fallback 或性能 |
| `FileSemanticIndex` 语义混合 | persistent facts 与 request-time facts 同 struct；skipped facts 和 fallback 都表现为空向量，只靠 diagnostics 补充解释 | coloring/references/member completion 可能误判“没有事实”和“未请求/不可用” |
| cache generation 仍偏实现细节 | generation 当前基于各 `Arc` 指针组合，不是显式 `IndexGeneration` / `SettingsGeneration` / `DocumentGeneration` | 一次请求可能混用不同代 read model，难定位 stale cache |
| `IndexStore` 是单一门面 | store 文件已拆，但 feature/read-model builder 仍直接调用大量 `IndexStore` 方法 | SQL schema shape 容易渗透到 query/completion/member/include |
| include/reachability 跨多模块 | `includes`、`store/includes`、`indexer/include_edges`、`reachability`、`resolver`、completion/coloring 都参与 | open scope、ambiguous include、external first-layer 策略可能漂移 |
| 架构边界缺少自动检查 | 目前没有 CI 规则限制 `tower_lsp`、`rusqlite`、server 依赖方向 | 后续功能容易把技术细节继续带入核心逻辑 |

## 2. 必须保护的用户已有功能

v1.2.2 的目标不是功能优化，因此每个重构都应默认“用户可见行为不变”。这里的“不变”不只是命令还能跑，而是包含排序、降级、标注、性能、隐私和发布形态。

| 用户能力 | 必须保护的不变量 | 建议保护方式 |
|---|---|---|
| 索引与状态栏 | full/dirty index 阶段、warning、ready/failed 语义、增量跳过逻辑不变 | indexer tests + server status tests + CLI index smoke |
| Go to Definition | include line 跳头文件；普通标识符按 current/reachable/external/unknown/global 排序与 candidate reason 不变 | resolver/query/server tests + candidate reason golden |
| 普通补全 | `isIncomplete=true` 恒定；短前缀降噪；每键热路径不查 SQLite；history boost 上限；raw text fallback 不伪装成语义 | completion pipeline compatibility tests |
| include completion | quote/angle search order、external dir cache、same-directory/recent/sibling/basename/depth 既有排序不变 | include completion tests |
| member completion | `.` / `->` routing、resolved receiver 优先、weak receiver 窄范围、fallback 前缀长度 >= 2、数量上限不变 | member completion/store tests |
| references | 全词文本搜索、角色分组、LRU/cache、limit/truncated log 不变 | references tests + LSP smoke |
| semantic coloring | 宏/类型/枚举/current local bindings；open scope fallback；不着字段；解析失败降级不崩 | coloring tests + semantic token server tests |
| hover/signature | exact-name candidate view、comment recovery、fallback 到 signature-only、signature help limit 不变 | query hover/signature tests |
| 配置与冲突提示 | `fossilsense.mode`、includePaths、completionHistory、includeScoping、candidateReasons、trace 的启动/重启语义不变 | extension TS tests |
| 隐私 | perf/debug 默认不输出候选名、源码片段、accepted label、include path | log assertion 或 review checklist |
| 发布 | `pnpm run package` 必须产出自包含 VSIX 到 `dist/` | package smoke，发布前硬门禁 |

重构期可以大幅调整内部文件和模块，但不应把“用户能感知到的排序或结果差异”作为顺带副作用带入 v1.2.2。若某处必须改变行为，应单独立功能 change，而不是混进质量版本。

## 3. 对外部建议的采纳评估

| 外部建议 | 当前代码现实 | v1.2.2 采纳判断 | 可行性 | 用户风险 |
|---|---|---|---|---|
| 保留单 binary，重塑内部边界 | 与现状一致 | 强采纳 | 高 | 低 |
| 建立 domain candidate / scope / confidence / reason 层 | 已有 `model.rs` / `resolver.rs` | 采纳，但先做文档和依赖边界，不急于搬目录 | 高 | 低 |
| 拆 `FileSemanticIndex` 为 PersistentFacts / RequestFacts / FactState | `ParseFacts::INDEX` 已是基础，但 store/parser/query 依赖广 | 选择性采纳，建议后半程做兼容过渡层 | 中 | 中高 |
| 为 NameTable / ReachGraph / IncludeTable 建立 snapshot 契约 | 已有 generation，但仍是 server 内实现细节 | 强采纳 | 高 | 中 |
| 重构 server 为 WorkspaceSession + FeatureService | 当前 server 是最大编排中心 | 强采纳，但先抽状态和缓存，再抽 handler | 中高 | 中 |
| completion 显式 pipeline | 已有 pipeline，但 ordinary handler 仍拼接候选 | 强采纳，必须行为兼容 | 高 | 中高 |
| SQLite 作为 persistence adapter，通过 read ports 访问 | store 已拆文件但门面大 | 采纳小接口，不做巨大 `IndexStore` trait | 中 | 中 |
| include/reachability 成为独立领域 | reachability 已纯净，include policy 分散 | 采纳文档/policy matrix，目录迁移后置 | 中高 | 中 |
| 统一 `EngineResult<T>` 与 degradation reason | 方向好，但触达所有 feature 和 UI | 暂缓，先标准化 enum 和 debug/log 语言 | 中 | 中高 |
| architecture fitness functions | 当前缺少自动边界检查 | 强采纳 | 高 | 低 |
| C4/ADR/architecture docs | 当前 `CLAUDE.md` 很强，但缺少独立架构图和 ADR | 强采纳 | 高 | 低 |
| 为未来 C++ intelligence 预留 evidence provider | v1.2.2 不做新功能 | 只写设计约束，不实现 provider | 高 | 低 |
| 大规模目录演进到 app/domain/facts/persistence/features | 现有模块已局部拆分，直接搬迁会制造 churn | 暂缓，不作为 v1.2.2 首要目标 | 中 | 高 |

核心判断：外部建议应被“内化为边界契约”，而不是“照目录树重写”。v1.2.2 成功的标志不是文件夹变多，而是每个 hot path 的变化原因更清楚、测试更能防止行为漂移。

## 4. 推荐目标架构

建议 v1.2.2 采用一个保守目标架构，不强求一次到位：

```text
VS Code Extension
  - process/config/status/commands/conflict UI
        |
        | LSP stdio
        v
server::language_server
  - tower-lsp trait shell only
  - LSP params/result adapter
        |
        v
app::WorkspaceSession
  - workspace roots
  - settings snapshot
  - document store
  - cache ledger
  - index scheduler facade
        |
        v
features
  - definition / references / hover / signature
  - completion ordinary/member/include
  - semantic tokens / workspace symbols
        |
        v
domain + read models
  - model / resolver / reachability policy
  - NameTable / ReachGraph / IncludeCompletionTable
        |
        v
persistence + parser + indexing
  - IndexStore sqlite adapter
  - parser facts extraction
  - full/dirty index pipelines
```

v1.2.2 的边界规则建议写进文档和自动检查：

```text
1. `tower_lsp` 只能出现在 server 边界和 LSP adapter。
2. `rusqlite` 只能出现在 store/persistence 模块。
3. parser 不依赖 store/server/query。
4. resolver/model 不依赖 parser/indexer/store/server。
5. feature service 不直接持有 Client，不发送 LSP log，不管理 locks。
6. ordinary completion 的最终排序只由 completion pipeline 决定，server 只收集输入和 render。
7. read model snapshot 在一次请求内不可变。
8. 用户可见 fallback/confidence/reason 文案来自统一枚举或 helper，不在各 feature 临时拼接。
```

注意：这些规则可以先作为 CI script 或 `xtask` 风格脚本落地，不一定要立即重排整个目录。

## 5. 分阶段计划与工作量

### Phase A：架构基线与行为冻结

目标：让 v1.2.2 重构有明确“不能破坏什么”的边界。

内容：

- 新增 `docs/ARCHITECTURE.md` 或 `docs/architecture/`，写清 Context / Component / Key flows。
- 写 ADR：
  - best-effort candidate model。
  - SQLite durable index + in-memory read models。
  - scope/confidence/reason canonical source。
  - cache generation and snapshot model。
  - v1.2.2 refactor is behavior-preserving。
- 增加 architecture risk register。
- 把当前用户可见能力整理成 regression checklist。
- 生成模块依赖图或至少做 import inventory。

工作量：6-10 人日。

可行性：高。

用户风险：低。

验收：

- 文档能解释 startup、full index、dirty index、query、completion 五条主链路。
- 文档明确哪些行为 v1.2.2 不改变。
- 后续任务可直接引用这些 ADR。

### Phase B：架构 fitness functions

目标：防止重构期间和之后边界继续回退。

内容：

- 增加脚本检查 `tower_lsp` 使用范围。
- 增加脚本检查 `rusqlite` 使用范围。
- 检查 `server` 之外是否直接依赖 LSP types。
- 检查 parser/store/server 之间的禁止依赖。
- 检查大文件阈值，先设 warning，不必一开始 fail CI。
- 增加 `cargo test -p fossilsense`、extension tests、CLI smoke 的本地门禁说明。

工作量：6-9 人日。

可行性：高。

用户风险：低。

验收：

- 边界脚本能在 CI 或本地一条命令运行。
- 失败信息指向具体文件和规则。
- 现有代码可以先用 allowlist 过渡，避免为了检查本身做大搬迁。

### Phase C：WorkspaceSession / CacheLedger / DocumentStore

目标：降低 `server.rs` / `language_server.rs` 的状态中心风险。

内容：

- 抽 `DocumentStore`：open docs、document version、live parse cache、local word cache 的统一入口。
- 抽 `CacheLedger`：NameTable、ReachGraph、IncludeTable、indexed file list、completion memo、reference caches 的 generation 和失效。
- 抽 `WorkspaceSnapshot`：

```rust
struct WorkspaceSnapshot {
    root: PathBuf,
    generation: WorkspaceGeneration,
    name_table: Option<Arc<NameTable>>,
    reach_graph: Option<Arc<RwLock<ReachGraph>>>,
    include_table: Option<Arc<IncludeCompletionTable>>,
    indexed_files: Option<Arc<Vec<(String, PathBuf)>>>,
}
```

- `language_server.rs` handler 通过 snapshot 取数据，不直接拼多个 lock。
- 保留现有 `WorkspaceGeneration` 算法，先只是封装。

工作量：12-20 人日。

可行性：中高。

用户风险：中。主要风险是 cache 失效、dirty update、completion memo、references cache 清理行为被改坏。

验收：

- did_change/did_close 后 live parse/local word/completion memo/reference cache 清理行为不变。
- full index 和 dirty index 后 generation 更新行为不变。
- completion prefix pool 仍按 generation + prefix 判断 hot/pool/cold。
- server tests 和 LSP smoke 通过。

### Phase D：ordinary completion 行为保持型拆分

目标：把普通标识符补全从 LSP handler 中抽出，但不改变排序。

内容：

- 新增协议无关 `completion::ordinary` 或 `features::completion::ordinary`。
- 将 handler 中的普通补全拆成：
  - context collection。
  - indexed recall。
  - local binding/current file overlay/local word recall。
  - evidence merge/rank。
  - LSP presentation。
- 保留 `run_evidence_aware_pipeline_with_context` 的结果顺序。
- `server` 只负责 include/member/ordinary routing、取 snapshot、attach accept command、返回 LSP list。
- 增加 compatibility fixture：给定同一组 candidates/context，拆分前后 labels/order/detail/sortText 一致。

工作量：12-18 人日。

可行性：高。

用户风险：中高。completion 是最敏感 UX 路径，任何 rank/order/detail 变化都容易被用户感知。

验收：

- 普通补全现有测试全过。
- `isIncomplete` 仍恒为 true。
- 短前缀、history boost、guard band、shadow rank metrics、local word fallback 行为不变。
- 每键热路径不新增 SQLite 查询，不新增 workspace scan。

### Phase E：IndexStore 小型 facade 与 read model builder 契约

目标：让 store 更像 persistence adapter，而不是 feature 直接依赖 SQL-shaped API。

内容：

- 不引入巨大 trait。按使用方拆小 facade：
  - `NameTableStoreView`
  - `ReachGraphStoreView`
  - `IncludeTableStoreView`
  - `DefinitionStoreView`
  - `MemberStoreView`
  - `ReferenceFileStoreView`
- 先用 `impl IndexStore` 内部方法或 wrapper struct 实现，不急于 trait object。
- read model builder 明确输入/输出：
  - `NameTable::build_with_paths(rows)`
  - `ReachGraph::new(edges, unresolved, ambiguous)`
  - `IncludeCompletionTable::build_with_edges(paths, edges)`
- SQL DTO 和 domain candidate 的转换集中到 store/query 边界。

工作量：10-16 人日。

可行性：中高。

用户风险：中。主要风险是 SQL query 返回列、external/directly_included、member alias recursion 细节被改错。

验收：

- store tests 全过，尤其 schema migration、sources/includes、members、query_scoping。
- 不改变 schema version，除非另立 migration 任务。
- `IndexStore::open_readonly` 和 WAL 读取行为不变。

### Phase F：Parser facts 语义过渡

目标：解决 `FileSemanticIndex` 空向量歧义，但不一次性改穿所有调用方。

内容：

- 保留 `FileSemanticIndex` 对外结构一段时间，新增内部 projection：

```text
PersistentFacts: symbols/includes/records/members/aliases
RequestFacts: occurrences/local_declarations/local_bindings
FactAvailability: NotRequested / Available / Unavailable(reason)
```

- indexer parse pipeline 消费 `PersistentFacts`。
- live parse path 仍可拿完整 `FileSemanticIndex`，通过 adapter 投影 request facts。
- `ParseDiagnostics` 增加或补充 skipped facts / fallback reason 的表达。
- 不改变 parser 事实收集算法，不改变 tree-sitter 使用。

工作量：14-24 人日。

可行性：中。

用户风险：中高。parser facts 被 indexer、store、document symbols、coloring、references、member completion、local completion 共同使用。

验收：

- index-time `ParseFacts::INDEX` 仍不收集 occurrences/local bindings。
- lexical fallback 仍保留 symbols/includes。
- skipped facts 不再和 fallback facts 混淆。
- parser tests、indexer tests、coloring/references/member completion tests 全过。

### Phase G：include/reachability policy 收敛

目标：让 open scope、ambiguous include、external first-layer 在各 feature 中的解释一致。

内容：

- 写 policy matrix：
  - definition。
  - ordinary completion。
  - member completion。
  - include completion。
  - semantic coloring。
  - references。
  - hover/signature。
- 把 `OpenReason` 到 confidence/detail/documentation 的投影集中。
- 审查 `includes`、`store/includes`、`indexer/include_edges`、`reachability`、`resolver` 的术语一致性。
- 目录迁移可后置，先做命名和文档。

工作量：6-10 人日。

可行性：高。

用户风险：中。主要风险是“不小心把 open scope 从软排序改成硬过滤”。

验收：

- ambiguous include 不被误标为 proven reachable。
- open scope 下 completion 继续给候选，不硬过滤。
- coloring 在不确定时宁可回退，不做误导着色。

### Phase H：发布硬化

目标：确保质量版本仍是可安装、可回退、可说明的产品交付。

内容：

- 跑完整 Rust tests。
- 跑 extension compile/test。
- 跑 CLI scan/index/query smoke。
- 跑 LSP smoke。
- `pnpm run package` 产出 VSIX。
- 更新 README / extension README / CLAUDE.md 中关于架构和行为保持的描述。
- release note 写清：v1.2.2 是内部质量版本，用户可见能力不主动改变。

工作量：5-8 人日。

可行性：高。

用户风险：低。

验收：

- `dist/fossilsense-vscode-1.2.2_BUILD*.vsix` 生成。
- VSIX 内包含 release binary。
- 用户安装后原有配置与命令可用。

## 6. 推荐 v1.2.2 范围

### 6.1 推荐必做

建议 v1.2.2 至少包含：

```text
Phase A: 架构基线与行为冻结
Phase B: 架构 fitness functions
Phase C: WorkspaceSession / CacheLedger / DocumentStore
Phase D: ordinary completion 行为保持型拆分
Phase H: 发布硬化
```

预计工作量：40-60 人日。

这是性价比最高的范围：它正面处理最大 hot path，同时不主动改变用户功能。完成后，后续版本做 parser facts、store ports、include policy、更多 C++ intelligence 都会稳很多。

### 6.2 可选增强

如果 v1.2.2 可以承载更大的重构，可增加：

```text
Phase E: IndexStore 小型 facade 与 read model builder 契约
Phase G: include/reachability policy 收敛
```

预计增量：16-26 人日。

总计约 56-86 人日。

### 6.3 建议拆到后续版本

建议把 `FileSemanticIndex` 深拆作为 v1.2.3 或 v1.2.2 stretch，除非团队愿意接受更长周期：

```text
Phase F: Parser facts 语义过渡
```

预计增量：14-24 人日。

原因：它确实重要，但牵动面比 server/cache/ordinary completion 更广，而且对用户没有直接可见收益。适合在前面边界和测试门禁到位后再做。

## 7. 用户功能保护策略

### 7.1 先建立兼容测试，再移动代码

推荐给以下路径补黄金样例或 compatibility tests：

| 路径 | 样例 |
|---|---|
| ordinary completion | indexed + local binding + overlay + local word + history + open scope 混合排序 |
| include completion | quote/angle、same dir、recent、sibling、external dir cache |
| member completion | resolved receiver、weak receiver、fallback prefix >= 2、prefix < 2 empty incomplete |
| definition | current/reachable/external/unknown/global 的 exact-name candidate order |
| dirty index | upsert/delete 后 NameTable、ReachGraph、IncludeTable、indexed file list 同步 |
| parser fallback | tree-sitter fallback、skipped facts、diagnostics |
| extension | start/stop/restart、config restart、conflict warning、history clear |

测试重点不是追求覆盖率数字，而是固定用户已经依赖的行为。

### 7.2 兼容层优先于立即改类型

重构不要一开始就把所有调用方改到新类型。建议模式：

```text
old public shape
    -> adapter/projection
    -> new internal service
    -> old observable output
```

例如 `FileSemanticIndex` 可以先保留字段，同时新增 `persistent_facts()` / `request_facts()` projection。`IndexStore` 可以先保留原方法，同时让新 read model builder 调小 facade。

### 7.3 一次只改变一个风险轴

不要在同一个 PR/阶段同时做：

```text
目录搬迁 + 类型迁移 + 排序策略变化 + cache generation 改写
```

推荐顺序是：

```text
1. 抽函数/抽模块，行为不变
2. 加测试固定行为
3. 引入新类型或 facade
4. 迁移调用方
5. 删除旧兼容层
```

### 7.4 性能门禁

v1.2.2 需要特别保护补全和索引性能：

- ordinary completion 每键路径不能新增 SQLite 查询。
- completion memo 的 hot/pool/cold 行为不应劣化。
- dirty index 后 read model refresh 不应退化成总是 full rebuild，除非明确 fallback。
- references cache 清理不能过度频繁，也不能 stale。
- parser thread pool、stack size、write batch size 不应在重构中顺手调整。

建议至少记录：

```text
cargo test -p fossilsense
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/healthy-v122-mini.sqlite --force
cargo test -p fossilsense --test lsp_smoke
cd extensions/vscode && pnpm run compile && pnpm run test
cd extensions/vscode && pnpm run package
```

## 8. 风险清单

| 风险 | 影响 | 可能性 | 缓解 |
|---|---|---:|---|
| ordinary completion 顺序被纯重构改变 | 用户直接感知，可能误接受候选 | 高 | compatibility fixture，先拆服务不改 rank |
| cache generation 封装时漏清理 | stale completion/references/coloring | 中高 | CacheLedger 单测 did_change/did_close/full/dirty |
| WorkspaceSnapshot 引入锁顺序问题 | LSP request hang 或死锁 | 中 | 统一 snapshot 构建顺序，避免持锁进入 spawn_blocking |
| `FileSemanticIndex` facts 迁移误伤 parser fallback | coloring/references/member completion 降级错误 | 中高 | 过渡 projection，保留旧字段到后期 |
| `IndexStore` facade 过度抽象 | 代码更绕，收益不明显 | 中 | 小接口，按 builder/use case 拆，不做大 trait |
| include policy 收敛时改变 open scope 语义 | 候选减少或误着色 | 中 | policy matrix + open/ambiguous tests |
| 文档和实现继续漂移 | 后续维护者按旧约定改错 | 中高 | ADR + CLAUDE/README 同步 + architecture check |
| 大规模目录迁移造成 review 困难 | 难发现行为变化 | 中 | 目录迁移后置，先抽内部模块和 tests |
| 发布质量版本未产 VSIX | 违反项目硬约定 | 低 | Phase H 作为 release gate |