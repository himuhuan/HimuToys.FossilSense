# FossilSense v1.3.0 Call Hierarchy 架构评审材料

## 0. 材料边界

本文面向 FossilSense 下一个版本 v1.3.0 的外部架构优化评审，评审主题是 SourceInsight 风格的 Call Hierarchy / Relation Window 能力：用户在 VS Code 中右键触发 `Analyse Call Hierarchy`，生成可点击的调用关系窗口。

按当前需求，本文保留项目、模块、类型、命令和配置项的原始名称，未做名称去敏。本文只总结现有架构、目录组织、抽象层和解析器 facts 能力，不给出实现方案。

本文刻意不包含以下内容：密钥、凭据、私有端点、生产日志、用户数据、配置值、大段源码、可复用的内部路径映射、实现补丁。

## 1. 评审目标

### 1.1 需要外部专家理解的问题域

FossilSense 是一个面向大型 Windows C/C++ 工作区的 best-effort 代码导航与分析工具。项目默认假设用户没有可靠编译环境：没有完整 `compile_commands.json`、没有稳定 clangd/IntelliSense、仓库巨大、宏和条件编译复杂、构建链不可直接复用。

现有能力强调“候选不是绑定”：跳转定义、补全、引用、着色、hover、signature help 都提供 ranked candidates，并暴露 `tier` / `confidence` / `reason` / fallback / ambiguity 等信息，避免把文本或启发式匹配伪装成编译级语义绑定。

v1.3.0 的 Call Hierarchy 目标需要在这个约束下提供类似 SourceInsight 的可点击关系视图。外部评审应基于以下事实评估架构边界、事实模型、持久化模型和 UI/服务协议是否足以支撑这一类关系分析。

### 1.2 当前不是目标的内容

本文不讨论完整 C++ 语义绑定、模板实例化、重载解析、继承解析、命名空间解析、宏展开、预处理条件求值、编译数据库接入、clangd 集成或精确 AST/CFG/Call Graph。

## 2. 目录组织

### 2.1 顶层目录

| 路径 | 职责 |
| --- | --- |
| `crates/fossilsense` | Rust 核心引擎，构建单一原生二进制 `fossilsense`。 |
| `extensions/vscode` | VS Code 扩展，负责进程管理、命令入口、配置桥接、状态栏、QuickPick/UI。 |
| `samples/mini-c` | 小型 C 样例，用于 smoke / CI 级验证。 |
| `scripts` | 架构 fitness、release hardening、CI smoke 等脚本。 |
| `tests/architecture_fitness` | 架构 fitness 的 fixtures 和 golden 输出。 |
| `docs/research` | 历史研究和架构演进材料。 |

### 2.2 Rust crate 组织

核心源码位于 `crates/fossilsense/src`。

| 模块 | 当前职责 |
| --- | --- |
| `main.rs` | clap CLI 入口：`lsp`、`index`、`scan`、`query symbol/def/refs`。 |
| `scanner.rs` | 工作区遍历，尊重 `.gitignore` 和 FossilSense scope 配置。 |
| `config.rs` / `config/` | `fossilsense.json` 和客户端配置归一化。 |
| `pathing.rs` | Windows 路径规范化、workspace hash、默认索引/历史缓存路径。 |
| `parser.rs` / `parser/` | tree-sitter C/C++ + 词法 fallback 的统一解析入口和 facts 模型。 |
| `indexer.rs` / `indexer/` | 全量/增量索引、候选发现、并行解析、SQLite 写入、include edge 重建。 |
| `store.rs` / `store/` | SQLite schema、迁移、写入、兼容查询 wrapper、typed read views。 |
| `model.rs` | 跨功能共享的候选语义模型：`DefinitionCandidate`、`ScopeTier`、`ResolutionConfidence` 等。 |
| `resolver.rs` | 协议无关的 scope tier 判定、排名 packing、confidence/reason 投影、候选去重。 |
| `reachability.rs` | include graph 到 `ReachScope` 的有限 BFS，可达集缓存与 open reason。 |
| `includes.rs` | `#include` 文本解析、form-aware 路径解析、include completion context。 |
| `query.rs` / `query/` | 协议无关查询逻辑：`NameTable`、定义候选排序、hover、signature、文本上下文等。 |
| `references.rs` / `references/` | ripgrep whole-word 引用搜索，以及基于解析器 `Occurrence` 的角色分类。 |
| `completion.rs` / `completion/` | ordinary completion 证据合并、ranker、intent、history-aware ranking。 |
| `completion_history.rs` | 本地补全接受历史，存匿名 candidate hash / kind / intent / prefix bucket。 |
| `completion_words.rs` | 当前文档词表提取。 |
| `language_builtins.rs` | 静态 C/C++ fallback 关键词/内建类型/常量候选。 |
| `coloring.rs` | 语义着色分类和 LSP semantic tokens 编码。 |
| `server.rs` / `server/` | tower-lsp 语言服务、请求编排、workspace session、cache ledger、LSP adapters。 |
| `progress.rs` | CLI / LSP 共用索引状态与耗时统计。 |

### 2.3 VS Code 扩展组织

| 路径 | 当前职责 |
| --- | --- |
| `extensions/vscode/src/extension.ts` | 扩展激活、语言客户端启动/停止、命令注册、文件 watcher、状态栏、配置重启、grouped references QuickPick。 |
| `extensions/vscode/src/referencesView.ts` | role-labeled references 的 QuickPick rows 构建。 |
| `extensions/vscode/src/serverPath.ts` | server binary 查找：配置路径、扩展内 `bin/`、仓库 `target/release|debug`。 |
| `extensions/vscode/src/config.ts` | `auto/on/off` 等配置值归一化。 |
| `extensions/vscode/src/conflicts.ts` | clangd / cpptools / ccls 互斥提示文案。 |
| `extensions/vscode/src/status.ts` | 状态栏 tooltip 和 degraded capability warning。 |
| `extensions/vscode/src/completionHistory.ts` | 清除本地补全历史的命令请求封装。 |

当前 `package.json` 已贡献命令：Start/Stop Server、Refresh Index、Full Rebuild Index、Find References (Grouped by Role)、Clear Completion History。尚未贡献 `Analyse Call Hierarchy` 命令、右键菜单项、Relation Window / tree view / webview 等 UI surface。

## 3. 运行时分层

### 3.1 容器级结构

```text
VS Code Extension (TypeScript)
  - process management
  - command and configuration bridge
  - status bar / QuickPick UI
  - file watcher forwarding

        LSP over stdio

fossilsense native binary (Rust)
  - CLI scan/index/query
  - LSP server
  - scanner / parser / indexer
  - SQLite persistence
  - query and ranking
  - in-memory read models and caches
```

Rust 二进制是唯一重活执行位置。VS Code 扩展不在 Node.js 进程里跑索引或解析。

### 3.2 请求和索引的主路径

全量索引主路径：

1. VS Code 扩展启动 `fossilsense lsp`。
2. LSP `initialized` 后 `Backend::spawn_index_roots` 开始索引。
3. `indexer::index_workspace` 读取 `WorkspaceConfig`，发现 workspace files 和 external include files。
4. 增量检查先比较 size / mtime；需要更新时再读文件并计算 hash。
5. `parse_pipeline::parse_and_write_changed` 用 bounded Rayon pool 并行解析，索引期使用 `ParseFacts::INDEX`。
6. `store::IndexStore` 批量写入 `files`、`symbols`、`includes`、`record_defs`、`members`、`type_aliases`。
7. `indexer::include_edges::rebuild_include_edges` 根据持久化 include facts 重建 `include_edges`、unresolved/ambiguous counts、`directly_included`。
8. `CacheLedger::publish_full_index` 重建内存 read models：`NameTable`、`ReachGraph`、`IncludeCompletionTable`、indexed workspace file list，并刷新 workspace generation。

增量 dirty update 路径：

1. VS Code file watcher 触发 `did_change_watched_files`。
2. `watched_change_in_scope` 做 scope/config 判断并合并 dirty changes。
3. `indexer::index_dirty_files` 只 upsert/delete 受影响文件。
4. include edges 只针对直接变更源和可能受 normalized include target 影响的源文件重建。
5. `CacheLedger::publish_dirty_index` 局部更新 `NameTable`，增量刷新 `ReachGraph`，重建 include table 和 indexed file list，刷新 generation。

### 3.3 查询请求的主路径

| 功能 | 当前事实来源 | 主要路径 |
| --- | --- | --- |
| Go to Definition | 当前打开文档 live parse + SQLite symbols + ReachScope | `language_server.rs` -> `store.symbol_read_view().symbols_by_name` -> `query::rank_definitions_into_candidates_with_scope` |
| Workspace Symbol | 内存 `NameTable` + SQLite symbol readback | `NameTable::search_ranked` -> `store.symbol_read_view().symbols_by_ids` |
| Document Symbol | 打开文档 live parse | `get_or_parse_document` -> `persistent_facts().symbols` |
| Find References | indexed file list 或 workspace walk + ripgrep + per-file role parse | `references::search_references_with_result_cache_and_files` |
| Grouped References | 与 standard references 同源，额外返回 role-labeled items | `fossilsense.lsp.groupedReferences` -> `GroupedReferenceItem` -> VS Code QuickPick |
| Completion | `NameTable`、current-file overlay、local bindings、local words、language builtins、history | `completion::ordinary_service::complete_ordinary_identifier` |
| Signature Help | lexical call context + exact-name function symbols + ReachScope | `query::call_context_at` -> `rank_function_signature_candidates` |
| Semantic Tokens | live parse occurrences + `NameTable` colorable kind counts | `server/semantic_tokens.rs` + `coloring.rs` |
| Member Completion | live parse local declarations / member access chain + `MemberStoreView` | `server/member_completion.rs` -> `store.member_view()` |

## 4. 架构设计要点

### 4.1 Best-effort 是第一等设计原则

FossilSense 不依赖编译数据库，也不把名字匹配称为语义绑定。当前代码中多个模型都显式体现这一点：

| 概念 | 用途 |
| --- | --- |
| `DefinitionCandidate` | 跳转候选，携带 indexed facts、range、source、tier、base_match、confidence、reason。 |
| `ScopeTier` | `Current` / `Reachable` / `External` / `Unknown` / `Global`。 |
| `ResolutionConfidence` | `Exact` / `Reachable` / `Heuristic` / `Ambiguous` / `Fallback`。 |
| `ResolutionReason` | `CurrentFile` / `ReachableInclude` / `ExternalFirstLayer` / `GlobalFallback`。 |
| `ReachScope` | include 有界可达集，携带 `open` 和 `OpenReason`。 |
| `ReferenceHit` | 文本级引用命中，附带句法角色，不代表绑定引用。 |
| `RecordCandidate` / `MemberCandidate` | record/member evidence 候选，不代表完整 C++ 类型绑定。 |

### 4.2 协议边界和持久化边界

当前架构 fitness 脚本约束如下：

| 边界 | 约束 |
| --- | --- |
| LSP 边界 | `tower_lsp` 使用限制在 `server` 和 LSP adapter 边界。 |
| SQLite 边界 | `rusqlite` 使用限制在 `store` / persistence 模块。 |
| core dependency direction | `parser` 不依赖 `store/server/indexer`；`resolver` 不依赖 `parser/store/server/indexer`；`model` 不依赖 `store/server/indexer`；`store` 不依赖 `server`。 |
| ordinary completion service | `completion/ordinary_service.rs` 必须保持协议无关，不能 import `tower_lsp`。 |

当前 fitness 输出：无失败项；12 个 large-source-file warning；`query/lsp_kinds.rs` 有一个已 allowlist 的 transitional LSP-kind adapter。

### 4.3 Unified fact boundary, separate domain pipelines

当前 steady-state 合同是统一 fact 边界，而不是合并所有业务管线：

```text
parser/store facts
  |-- symbol/query pipeline --> resolver/candidates/presentation
  |-- reference pipeline ----> text hits/role labels/presentation
```

symbol/query pipeline consumes parser/store facts through projections, read views, typed rows, and resolver-backed candidate APIs. 这一路径服务 definition、hover、signature、ordinary completion、semantic coloring 的 symbol gates、member completion 和 read-model rebuild；它可以使用 `ScopeTier`、`ResolutionConfidence`、`ResolutionReason`、`ReachScope` 和 resolver ranking，但仍然只表达 best-effort candidates。

reference pipeline stays whole-word text hits plus syntactic roles from request facts. References 使用 ripgrep whole-word discovery，按需用 `ParseFacts::COLOR_REF`、`request_facts()` 和 `FactAvailability` 取得 occurrence role；它不使用 resolver ranking，也不把 `ReferenceHit` 包装成 semantic binding。

reference discovery keeps the historical path/line/column truncation cap before role presentation. Standard references 和 grouped references 在 retained hits 上共享 role / path / line / column 展示顺序；这保持 `REFERENCES_LIMIT` 的既有截断集合，同时让两种展示入口的分组顺序一致。

legacy broad IndexStore query wrappers are test-only parity helpers. 生产 durable reads 必须通过 `store::views`、typed rows 或 store 边界内的 domain loaders；旧 broad wrapper 只允许作为 `#[cfg(test)]` parity oracle 保留。

### 4.4 共享 resolver

`resolver.rs` 是当前候选排序和 scope projection 的共享原语：

| 原语 | 作用 |
| --- | --- |
| `ResolveContext` | 当前文件路径 + 可选 `ReachScope`。 |
| `scope_tier` | 从候选路径、external/source 标记、directly_included 和 reach scope 得到 `ScopeTier`。 |
| `pack_score` | 用 `TIER_STRIDE` 打包 `(tier, base_match, locality)`，保证 tier 严格主导。 |
| `confidence_reason_for` | 从 tier + exact_name + open_reason 投影 confidence/reason。 |
| `dedup_keep_higher` | 同名候选保留更高 `(tier, confidence)`。 |

定义、补全、workspace symbol、语义着色等路径都复用这个 scope/ranking 词汇。引用搜索当前不携带 `ScopeTier`，只做文本命中和句法 role 排序。

### 4.5 Workspace session 和缓存层

`server/workspace.rs` 把 live document 与 read models/cache 收束成 `WorkspaceSession`：

| 结构 | 内容 |
| --- | --- |
| `DocumentStore` | open documents、live parse cache、local word cache。 |
| `CacheLedger` | `NameTable`、`ReachGraph`、`IncludeCompletionTable`、indexed file list、workspace generations、read model snapshots、reference caches、completion memo。 |
| `WorkspaceSnapshot` | 请求期一致快照，携带 root、generation、settings、name table、reach graph、include table、indexed files。 |
| `WorkspaceGeneration` | 由 root 和 read model Arc 指针组成的 hash，用于 completion memo / reference cache invalidation。 |

这一层现在服务于补全、引用、着色、include completion、signature help 等请求。

## 5. 持久化模型

SQLite schema 当前版本为 `SCHEMA_VERSION = 10`。主表如下：

| 表 | 主要内容 |
| --- | --- |
| `files` | path、extension、size、mtime、hash、indexed_at、status、source、directly_included、unresolved/ambiguous include counts。 |
| `symbols` | file_id、name、kind、role、range、signature、guard、container。 |
| `includes` | file_id、line、target_text、target_form、target_normalized、target_basename。 |
| `include_edges` | src_file_id、dst_file_id、resolution。 |
| `record_defs` | record display/tag/typedef name、kind、range、signature、confidence。 |
| `members` | record_id、name、kind、confidence、range、signature、type_name。 |
| `type_aliases` | alias、range、target_record_id / target_name / target_kind、confidence。 |

当前没有持久化以下事实：

| 未持久化事实 | 当前替代路径 |
| --- | --- |
| identifier occurrences | 打开文档 live parse，或 references 请求时按命中文件临时 parse。 |
| call sites | `Occurrence` 可标 `Call`，但索引期 `ParseFacts::INDEX` 不收集 occurrences。 |
| caller/callee edges | 无持久化表。 |
| scope-local bindings | 只在 live/request parse 中使用，不写 SQLite。 |
| reference relation graph | references 使用 ripgrep whole-word search，不写关系索引。 |

跨模块 durable reads 主要通过 `store::views`：

| Read view | 用途 |
| --- | --- |
| `NameTableStoreView` | 构建内存 `NameTable`。 |
| `ReachGraphStoreView` | 构建/刷新 `ReachGraph`。 |
| `IncludeTableStoreView` | include completion table、workspace include path 查找。 |
| `SymbolReadView` | 按 ids/name 读取 `SymbolRecord`。 |
| `ReferenceFileStoreView` | references 请求的 indexed workspace file list。 |
| `MemberStoreView` | record/member/alias 查询和 member completion fallback。 |

## 6. Include 与可达性

`includes.rs` 提供 include target 解析与 form-aware resolution：

| Include form | 解析优先级 |
| --- | --- |
| quote `"..."` | current file directory -> workspace exact -> external roots -> workspace suffix。 |
| angle `<...>` | external roots -> workspace exact -> workspace suffix。 |

`IncludeResolution` 分为：

| 结果 | 意义 |
| --- | --- |
| `Edge { dst, kind }` | 产生一个可达边，kind 为 `RelativeExact` / `WorkspaceExact` / `ExternalExact` / `SuffixMatch`。 |
| `Ambiguous { dsts }` | 多个候选且没有 exact-tier winner，不建立 proven edge。 |
| `Unresolved` | 无法解析，增加 open count。 |

`ReachGraph` 从 `include_edges` 和 unresolved/ambiguous file rows 构建 `ReachScope`。BFS 限制为 `MAX_REACH_DEPTH = 32`、`MAX_REACH_NODES = 4096`。任一可达文件存在 unresolved/ambiguous include 或触达遍历上限，scope 标记为 open，并记录首个 `OpenReason`。

当前可达性主要用于定义/补全/着色排序或 gating，不提供函数级调用关系。

## 7. Parser facts 能力

### 7.1 统一入口

解析器入口：

| API | 用途 |
| --- | --- |
| `parse(path, source) -> FileSemanticIndex` | 兼容入口，默认 `ParseFacts::ALL`。 |
| `parse_with_handle(path, source, handle, facts)` | 可复用 parser handle，按 fact mask 收集。 |
| `parse_thread_local_with_facts(path, source, facts)` | 索引并行路径，每个 Rayon worker 复用 thread-local `ParserHandle`。 |

解析过程固定包含一遍 line-based lexical pass；tree-sitter 成功时再做一遍 iterative AST DFS。普通 parse error 不返回 `Err`，tree-sitter 没有 usable tree 时走 lexical fallback。

### 7.2 `FileSemanticIndex`

`FileSemanticIndex` 当前字段：

| 字段 | 来源 | 是否持久化 | 用途 |
| --- | --- | --- | --- |
| `symbols` | lexical + AST enum constants | 是 | symbols、document outline、definition、workspace symbol、hover、completion。 |
| `includes` | lexical | 是 | include table、include edges、include completion、jump-to-header。 |
| `occurrences` | AST | 否 | semantic tokens role gate、references role classification。 |
| `records` | AST | 是 | record/type evidence、member completion。 |
| `fields` | AST | 兼容/间接 | field evidence，当前主要通过 members/read view 消费。 |
| `members` | AST | 是 | fields、C++ method/static method evidence。 |
| `aliases` | AST | 是 | typedef/alias 到 record 的候选解析。 |
| `local_declarations` | AST | 否 | member completion receiver 推断。 |
| `local_bindings` | AST | 否 | ordinary completion 的当前函数参数/局部变量。 |
| `diagnostics` | metadata | 否 | parse error count、fallback、fact provenance、requested facts。 |

投影接口：

| 投影 | 内容 |
| --- | --- |
| `persistent_facts()` | symbols、includes、records、fields、members、aliases。 |
| `request_facts()` | occurrences、local_declarations、local_bindings。 |
| `fact_availability(group)` | 区分 `Available`、`NotRequested`、`Unavailable(LexicalFallback)`。 |

### 7.3 `ParseFacts`

| Mask | 当前用途 |
| --- | --- |
| `INDEX` | 索引期 facts：symbols、includes、records、fields、aliases；跳过 occurrences/local declarations/local bindings。 |
| `COLOR_REF` | 语义着色和引用角色所需：symbols、includes、occurrences。 |
| `MEMBER` | 成员补全所需：local declarations/bindings、records、fields、aliases。 |
| `ALL` | 兼容默认，全部 facts。 |

对 v1.3.0 Call Hierarchy 评审很重要的一点：当前 bulk index 路径使用 `ParseFacts::INDEX`，不会收集或持久化 `occurrences`。因此当前数据库没有 call occurrence 或 caller/callee edge。

### 7.4 词法 pass 可获取的信息

`parser/lexical.rs` 是 line-based extraction，目标是不可失败：

| 信息 | 说明 |
| --- | --- |
| `#include` | 存 raw target text 和 line。 |
| macro definition | `#define` 名称、signature、guard。 |
| function declaration/definition | top-level statement 正则/brace 状态识别，区分 declaration / definition。 |
| typedef type | 支持普通 typedef 和多行 record typedef alias。 |
| tag type | `struct` / `union` / `enum` / `class` tag name。 |
| global variable | 简单 top-level global var。 |
| guard | 维护简单 preprocessor guard stack。 |

词法 pass 会跳过 leading comments，处理 preprocessor continuation，并通过 brace depth 粗略识别 top-level statement。

### 7.5 AST pass 可获取的信息

`parser/ast.rs` 用 tree-sitter C/C++ 做一次 iterative DFS。当前可获取：

| 信息 | 说明 |
| --- | --- |
| `Occurrence` | `identifier` / `type_identifier` 的 name、byte、UTF-16 line/col、length、`SyntacticRole`。 |
| occurrence role | `Definition`、`Declaration`、`Call`、`Write`、`TypeUse`、`Read`。无法识别时降级为 `Read`。 |
| enum constants | AST enumerator name 合并进 `symbols`，kind 为 `EnumConstant`。 |
| record defs | `struct_specifier` / `union_specifier` / `class_specifier`，记录 tag / typedef / display name / kind / range / signature / confidence。 |
| fields | record body 中 field declaration 的 name、range、signature。 |
| members | field、method、static method、简单 out-of-class `Owner::method` evidence、type_name。 |
| anonymous nested record | 对匿名嵌套 `struct/union/class` 生成 synthetic nested record evidence。 |
| type aliases | `typedef` 到 `RecordKey` / named record / unresolved type name。 |
| local declarations | record-typed local/parameter declaration，用于 member receiver 推断。 |
| local bindings | 当前函数参数和局部变量，包含 name、kind、type_text、decl_start_byte、function body byte range。 |

当前 `SyntacticRole::Call` 的判定仅来自 AST 中 `call_expression` 的 function field。它能够告诉某个 identifier token 处在调用位置，但不提供已解析的 callee symbol id，也不把 caller function 和 callee candidate 建成边。

### 7.6 Signature Help 的额外 call context

`query/signatures.rs` 有独立的 lexical scanner：

| 能力 | 说明 |
| --- | --- |
| `call_context_at` | 从当前文本和光标位置找最近函数调用上下文，返回 call name 和 active argument。 |
| 参数拆分 | 从 stored signature 拆 parameter spans，支持嵌套括号/数组/简单指针函数形态。 |
| 候选排名 | exact-name function symbols 经 `rank_function_signature_candidates`，复用 definition ranking 和 ReachScope。 |

这一路径服务 signature help，不持久化，也不产生调用关系。

## 8. 当前引用能力与 Relation Window 相关现状

### 8.1 Standard references

`references.rs` 当前执行 case-sensitive whole-word text search。流程：

1. 从光标下取 identifier。
2. 使用 indexed workspace file list；不可用时 fallback 到 workspace walk。
3. 使用 ripgrep kernel 搜索 `\bidentifier\b`。
4. 对命中文件按 fingerprint 使用 `ReferenceRoleCache` 缓存 parsed occurrence roles。
5. 用 `Occurrence` 的位置映射把 hits 标注为 definition/declaration/call/write/type/read。
6. `sort_hits_by_role` 将 definition/declaration 排前，之后 call/write/type/read。
7. 结果 capped at `REFERENCES_LIMIT = 2000`。

标准 `textDocument/references` 返回 LSP `Location`，不能携带 role label。

### 8.2 Grouped references command

已有命令链：

```text
VS Code command: fossilsense.findReferencesGrouped
LSP executeCommand: fossilsense.lsp.groupedReferences
server result: GroupedReferenceItem[]
client UI: QuickPick grouped by role
```

这是现有“服务端返回富信息，扩展侧展示可点击列表”的先例。它不是 Relation Window，也不是树/图结构；仅是 role-grouped flat QuickPick。

### 8.3 和 Call Hierarchy 的现状差距

当前 references 能识别 call-site role，但有以下事实限制：

| 事实限制 | 当前表现 |
| --- | --- |
| 没有持久化 call sites | 每次 references 通过文本搜索和按需 parse 得到 role。 |
| 没有 caller function ownership | `Occurrence` 记录 token 位置和 role，不记录它位于哪个 enclosing function。 |
| 没有 callee binding | `Call` role 只说明 token 在调用位置，不说明它绑定到哪个函数定义/声明。 |
| 没有 call edge table | SQLite schema 不包含 caller -> callee relation。 |
| 没有 relation read model | `CacheLedger` 当前没有 CallGraph / RelationGraph 类 read model。 |
| 没有 dedicated relation UI | VS Code 扩展目前只有 QuickPick grouped references，没有 tree/webview/relation window。 |

## 9. 当前 UI / LSP surface

### 9.1 VS Code extension

扩展职责保持轻量：

| 职责 | 当前实现 |
| --- | --- |
| server 启停 | `LanguageClient` 启动 `fossilsense lsp`。 |
| 配置桥接 | 初始化选项传 completion、history、semantic coloring、include scoping、include paths、debug/perf。 |
| 文件监听 | C/C++ 文件和 `fossilsense.json` watcher，同步给 LSP。 |
| 状态反馈 | `fossilsense/indexStatus` notification 更新 status bar 和 output channel。 |
| 命令 | Refresh/Rebuild/Grouped References/Clear Completion History。 |
| 冲突提示 | 检测 clangd / cpptools / ccls，auto 模式一次性提示。 |

### 9.2 Rust LSP server

`Backend` 目前暴露：

| LSP capability | 当前用途 |
| --- | --- |
| `definitionProvider` | symbols 和 include jump。 |
| `referencesProvider` | standard references。 |
| `workspaceSymbolProvider` | workspace symbol。 |
| `documentSymbolProvider` | document outline。 |
| `hoverProvider` | ranked exact-name candidates + signature/comment。 |
| `completionProvider` | ordinary/include/member completion。 |
| `signatureHelpProvider` | function signature help。 |
| `semanticTokensProvider` | macro/type/enum/current locals tokens。 |
| `executeCommandProvider` | refresh/rebuild/groupedReferences/completionAccepted/clearCompletionHistory。 |

当前没有注册 LSP Call Hierarchy 标准 capability，也没有自定义 call hierarchy executeCommand。

## 10. 架构健康现状

### 10.1 已有健康边界

| 方面 | 现状 |
| --- | --- |
| 协议无关核心 | parser/resolver/query/store views 多数不依赖 LSP 类型。 |
| 持久化边界 | SQL 和 `rusqlite` 基本被限制在 `store`。 |
| 索引/请求 facts 分离 | `ParseFacts`、`PersistentFacts`、`RequestFacts`、`FactAvailability` 已存在。 |
| 大仓库默认 | 增量 mtime gate、bounded parser pool、SQLite batch write、in-memory hot read models。 |
| fallback 可见 | parse diagnostics、open scope reason、candidate confidence/reason、degraded capability。 |
| UI 先例 | grouped references 已有 server rich result -> client clickable QuickPick 的路径。 |

### 10.2 当前大文件和复杂度信号

当前 `scripts/architecture_fitness.js --format text` 无失败项，但报告 12 个 large-source-file warning：

| 文件 | 行数信号 |
| --- | --- |
| `crates/fossilsense/src/coloring.rs` | 1023 |
| `crates/fossilsense/src/completion.rs` | 1736 |
| `crates/fossilsense/src/completion/ordinary_service.rs` | 889 |
| `crates/fossilsense/src/parser/ast.rs` | 976 |
| `crates/fossilsense/src/parser/lexical.rs` | 902 |
| `crates/fossilsense/src/parser/tests.rs` | 1214 |
| `crates/fossilsense/src/query.rs` | 924 |
| `crates/fossilsense/src/query/tests.rs` | 816 |
| `crates/fossilsense/src/resolver.rs` | 851 |
| `crates/fossilsense/src/server/include_completion.rs` | 1345 |
| `crates/fossilsense/src/server/language_server.rs` | 1019 |
| `crates/fossilsense/src/server/tests.rs` | 2054 |

一个 allowlisted 架构例外：

| 文件 | 说明 |
| --- | --- |
| `crates/fossilsense/src/query/lsp_kinds.rs` | transitional LSP-kind adapter；当前 allowlist 表示未来可迁移到 server/LSP adapter 边界。 |

## 11. Call Hierarchy 相关可复用事实

以下是现有系统中与调用层级最接近的事实：

| 现有事实 | 来源 | 当前用途 | 与 Call Hierarchy 的关系 |
| --- | --- | --- | --- |
| function symbols | lexical `symbols` | 定义、workspace symbol、hover、signature help | 可作为函数节点候选。 |
| symbol range/signature/role | `symbols` table | 跳转和显示 | 可用于可点击节点和标签。 |
| `SyntacticRole::Call` | AST `Occurrence` | references role grouping | 可识别调用位置，但未持久化。 |
| call name + active arg | `query::call_context_at` | signature help | 能识别光标附近调用，不是全文件调用抽取。 |
| `ReachScope` | include graph | 排序/着色 gating | 可作为候选置信度上下文。 |
| `DefinitionCandidate` ranking | resolver/query | 跳转、signature help | 可解释 callee candidate 排序，但不是绑定。 |
| `ReferenceHit` role | references | grouped references | 可找“某名字被调用的位置”，但不拥有 caller/callee edge。 |
| live parse cache | `DocumentStore` | 当前打开文档功能 | 可覆盖未保存编辑的当前文件事实。 |
| indexed file list | `CacheLedger` | references 搜索范围 | 可避免每次 workspace walk。 |

## 12. 需要外部专家重点评审的现状问题

以下问题仅用于界定评审重点，不包含解决方案。

1. 当前 `ParseFacts::INDEX` 不收集 `occurrences`，所以索引数据库没有 call occurrences；而 references 请求的 role 事实是按需、临时、文本搜索驱动的。

2. 当前 `Occurrence` 只记录 token、位置和句法角色，不记录 enclosing function、callee textual shape、member call receiver、macro call/function-like macro、qualified name 或调用表达式范围。

3. 当前 definition ranking 可以对 exact-name symbols 排序并给出 `ScopeTier` / `confidence` / `reason`，但没有一个表示“call edge candidate”的公共模型。

4. 当前 `references.rs` 和 `query::rank_definitions...` 是两条事实消费路径：前者从名字找文本命中，后者从名字找定义候选；二者未组合成关系图。

5. 当前 SQLite schema 已有 symbols/records/members/includes/include_edges，但没有 relation/call graph 表，也没有相关 read view。

6. 当前 `WorkspaceSession` / `CacheLedger` 可以承载 read model snapshot，但还没有 relation read model、relation cache、relation generation 或 relation invalidation 规则。

7. 当前 VS Code 侧有 QuickPick 可点击列表先例，但没有 Relation Window 类型的持久可交互视图，也没有右键菜单命令贡献。

8. 当前 architecture fitness 已限制 `tower_lsp` 和 `rusqlite` 边界；新增关系功能若跨 parser/query/store/server/extension 多层，会触碰现有边界设计。

9. 当前 large-source-file warning 显示 `parser/ast.rs`、`query.rs`、`server/language_server.rs`、`completion.rs` 等模块已经较大；任何关系分析能力都可能增加这些热点文件复杂度。

10. 当前产品原则要求“不伪装精确”。Call Hierarchy 的 UI 文案、数据模型和交互如果展示树状调用关系，需要继续表达候选、fallback、ambiguity 和 truncation。

## 13. 外部评审建议问题清单

以下问题是给外部专家的评审入口，不预设答案：

1. 在无编译环境、无完整预处理、无 C++ 语义绑定前提下，Call Hierarchy 的事实模型应如何表达“不确定调用关系”才不会误导用户？

2. 当前 `FileSemanticIndex` 的 persistent/request facts 分离是否适合扩展到调用关系 facts？哪些 facts 应该持久化，哪些应保持请求期计算？

3. 当前 `Occurrence` 事实粒度是否足以作为 Call Hierarchy 的基础？若不足，最小必要的新增事实边界是什么？

4. 当前 SQLite schema 和 `store::views` 的边界是否适合加入关系读取视图？这种视图与 `NameTable` / `ReachGraph` / `ReferenceFileStoreView` 应如何协作？

5. 当前 references text search 路径与 definition candidate ranking 路径是否应在关系分析中合流？合流时如何保留 best-effort 的 confidence/reason？

6. 当前 `WorkspaceSession` / `CacheLedger` / generation 模型是否足以表达 relation read model 的缓存一致性？

7. 当前 VS Code 扩展的 QuickPick 先例是否足以承载 Relation Window 用户体验，还是需要新的 tree/webview/custom editor surface？

8. 当前架构 fitness 规则是否足以约束 v1.3.0 的关系分析变更，还是需要新增边界规则来防止 LSP/UI/persistence/parser/query 概念漂移？

9. 在千万行级工作区下，Call Hierarchy 的索引成本、增量更新成本、请求延迟、内存 read model 规模应如何被评估和约束？

10. 对 SourceInsight 风格用户而言，Relation Window 中哪些 confidence/fallback/ambiguity 信号必须显性展示，哪些可以只在详情或 debug 中展示？

## 14. 当前事实摘要

FossilSense 当前具备以下基础：

- 可索引函数、宏、类型、枚举常量、全局变量、record/member/alias、include edges。
- 可按 include reachability 对定义/补全/着色进行分层排序或 gating。
- 可通过 request-time AST facts 标注 identifier occurrence 的 `Call` / `Read` / `Write` / `TypeUse` 等句法角色。
- 可用 ripgrep whole-word search 找到某个名字的所有文本命中，并按句法角色分组。
- 可在扩展侧用 executeCommand 返回富 JSON，并用 QuickPick 做可点击跳转。

FossilSense 当前尚不具备以下关系分析基础：

- 持久化 call sites。
- caller function ownership。
- callee candidate edge 模型。
- caller -> callee relation table / read view。
- relation graph cache/generation。
- `Analyse Call Hierarchy` 命令、右键菜单贡献、Relation Window UI。
- LSP Call Hierarchy capability 或自定义 relation protocol。
