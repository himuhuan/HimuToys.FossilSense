# CLAUDE.md

## 1. 项目定位

FossilSense 是面向大型 Windows 工作区的 C/C++ 容错代码导航与分析工具，对标 VS Code 中的 SourceInsight。

默认假设：没有可靠编译环境。无 clangd、完整 IntelliSense、`compile_commands.json`，仓库巨大、构建复杂、宏很多，都不是降级场景，而是基本盘。

核心原则：

| 原则 | 要求 |
|---|---|
| 开箱即用 | 一个自包含 VSIX 即可工作，无外部工具链依赖 |
| Best-effort | 给排序候选和启发式结果，但必须标注 confidence / fallback / ambiguity |
| 不伪装精确 | 不把名字匹配写成编译级语义绑定 |
| 容错优先 | 解析、include、配置缺口应降级，不应崩溃 |
| 大仓库优先 | 设计默认面向千万行级工作区 |

典型用户场景：嵌入式、固件、内核、老代码、vendored SDK，以及用户需要立刻导航而不是先修构建系统的代码库。

## 2. 架构

```text
VS Code 扩展 (TypeScript, extensions/vscode)
        |
        | LSP over stdio
        v
fossilsense 单一 Rust 原生二进制 (crates/fossilsense)
  - CLI: scan / index
  - LSP: lsp
```

职责：

| 层 | 职责 |
|---|---|
| VS Code 扩展 | 进程管理、状态栏、配置桥接、命令入口 |
| Rust 二进制 | 扫描、解析、索引、查询、LSP 服务 |
| CLI | 脚本化、无编辑器调试、索引验证 |

二进制查找顺序：`fossilsense.serverPath` -> 扩展内 `bin/` -> 仓库 `target/release` 或 `target/debug`。

## 3. 模块地图

源码：`crates/fossilsense/src`。

| 模块 | 职责 |
|---|---|
| `main.rs` | clap CLI：`scan` / `index` / `lsp` |
| `scanner` | 文件遍历，尊重 `.gitignore` 和默认排除 |
| `indexer` | 扫描、增量判定、解析、SQLite 写入、进度事件 |
| `model` | 候选语义层与规范导出名 |
| `call_model` | 调用关系领域值对象、稳定 locator、关系质量与覆盖合同；协议中立，不依赖 parser/store/server |
| `call_catalog` | 从 active call facts 构建不可变 callable 目录；事实与关系负载只存一份，incoming/outgoing 使用紧凑 ID 邻接索引，请求按页物化歧义/未解析证据 |
| `parser` | tree-sitter C/C++ 容错解析；唯一入口 `parse()`；提供 persistent/request facts 投影与 fact availability |
| `semantic_model` | parser/store 中立的共享语义事实；不得依赖 parser/store/server/indexer |
| `store` | SQLite schema、迁移、事务、清理、写入，以及 durable read views / typed rows |
| `store_parser_adapter` | `FileSemanticIndex` 到 store ingestion port 的唯一适配层；store 本体不得依赖 parser |
| `pathing` | Windows 路径规范化、仓库相对路径、workspace hash |
| `progress` | CLI / LSP 共用索引状态 |
| `project_context` | 构建标记发现、规范化 `ProjectKey`、最近祖先项目推断与协议中立 DTO；项目只是补全 evidence，不是绑定 |
| `coloring` | 语义着色分类与 LSP 相对编码 |
| `includes` | include 解析、规范化、补全上下文 |
| `reachability` | include 图、可达集、有界闭包、开放原因 |
| `resolver` | 共享作用域与排名原语，无 `tower-lsp`，可单测 |
| `completion` | 普通补全 intent、candidate evidence、确定性 pipeline/ranking，以及 ordinary service/provider 转换 |
| `query` | 定义/补全/Hover 等协议中立查询；`query/comments` 负责 Hover、补全和 Signature Help 共用的注释归属、清洗、Doxygen/XML 容错解析与 Markdown 渲染 |
| `server` | tower-lsp 适配、`fossilsense/indexStatus` 通知、不可变 `EngineSnapshot` 原子发布与请求上下文 |

必须复用的模型名：

| 模型 | 用途 |
|---|---|
| `DefinitionCandidate` | 跳转定义候选 |
| `ScopeTier` | `Current` / `Reachable` / `External` / `Unknown` / `Global` |
| `ResolutionConfidence` | `Exact` / `Reachable` / `Heuristic` / `Ambiguous` / `Fallback` |
| `ResolutionReason` | `CurrentFile` / `ReachableInclude` / `ExternalFirstLayer` / `GlobalFallback` |
| `Occurrence` | 标识符出现及句法角色 |
| `ReferenceHit` | 引用查询结果 |
| `ReachScope` | include 可达范围，携带 open 与 reason |
| `OpenReason` | `UnresolvedInclude`、`AmbiguousInclude`、遍历上限等 |
| `RecordCandidate` | struct / class / union record 候选 |
| `CallableAnchor` / `CallableEntity` | 调用关系中的具体源码锚点与保守逻辑实体 |
| `CallSiteFact` / `CallRelation` | 调用表达式事实与按调用点聚合的候选关系 |
| `RelationConfidence` / `EvidenceLedger` | 与定义置信度分离的关系质量及可解释证据 |

新增能力不得绕开这些模型另起 `smart` / `semantic` 等平行概念。

## 4. 存储与路径

默认索引库：

```text
<user-cache-dir>/FossilSense/indexes/<workspace-hash>/index.sqlite
```

规则：

| 项 | 规则 |
|---|---|
| CLI `--db` | 仅供测试和调试 |
| 工作区内文件 | 存仓库相对路径，分隔符统一为 `/` |
| IO 边界 | 单点转换为平台路径 |
| 外部 include 头 | 存规范化绝对路径 |
| `files.source` | 标记 `workspace` / `external` |

Store read-view 规则：

| 项 | 规则 |
|---|---|
| read views | 跨模块 durable reads 通过 `store::views` 的窄视图：name table、reach graph、include table、symbol/reference file、member、call facts |
| typed rows | read-model builder 输入使用 typed row / DTO，不依赖 SQL tuple column order |
| SQL ownership | `rusqlite` 与 SQL-to-domain 转换留在 `store` / persistence 边界 |
| compatibility | 旧 `IndexStore` query wrapper 可作为兼容/测试 oracle 保留，但应委托 read views 或共享 typed loader |
| bundled SQLite | 必须包含 WAL-reset 并发损坏修复（当前基线 SQLite 3.51.3+）；`store` resilience test 防止版本回退 |

统一语义代际规则：

| 项 | 规则 |
|---|---|
| immutable facts | 每次解析生成新的 `file_revisions` 与 fact rows；索引期间不得覆盖 active facts |
| active views | `files`、`symbols`、`includes`、`record_defs`、`members`、`type_aliases` 只暴露 `active_file_revisions` 指向的事实 |
| staging | 全量和 dirty 索引都先写 `index_builds` / `pending_file_revisions`，失败或废弃 build 不得改变生产查询结果 |
| publication | 文件 manifest、include edges/open counts、first-layer external 标记与持久化 `SemanticGeneration` 在一个短事务中切换 |
| SQLite snapshot | `SemanticReadGuard` 在事务开始时捕获并可校验 generation；旧 WAL reader 在发布后继续看到完整旧代 |
| cleanup | inactive/orphan revisions 在发布后清理；清理失败不得回滚已发布 generation |

Runtime snapshot 规则：

| 项 | 规则 |
|---|---|
| `EngineSnapshot` | 每工作区一个完整不可变读模型，统一携带 name table、reach graph、include table、reference file list、project context、relation catalog、degraded state |
| publication | 所有下一代读模型在后台构建完成后，只通过一次 map 交换发布；构建期间旧快照继续服务请求 |
| `EngineEpoch` | 每次成功发布分配显式单调 epoch；`0` 只表示尚未发布索引读模型 |
| `SemanticGeneration` | SQLite active manifest 的持久化单调代际；marker-only 等纯派生刷新不得推进它 |
| `RequestContext` | 请求开始时捕获一个 `Arc<EngineSnapshot>` 和 request settings；请求期间不得重新逐项读取缓存 |
| open document | LSP 使用 incremental sync；文本快照用 `Arc<str>` 共享；live parse 按事实掩码 singleflight，版本推进取消旧解析且 latest revision wins |
| saved overlay | 不能用全局 generation 推断单文件已发布；只在 active revision 的同路径内容哈希匹配时清除 |
| dirty reach graph | 增量 include edge 更新生成新的 `ReachGraph`，不得原地修改旧快照持有的图 |
| publisher | snapshot publisher 串行协调；发布失败不得暴露半更新状态 |

## 5. 当前能力

| 能力 | 当前范围 |
|---|---|
| 索引 | 打开工作区后台索引；文件事件增量；`Refresh Index` 增量；`Full Rebuild Index` 全量 |
| 状态 | `discovering -> checking -> parsing -> indexing -> finalizing -> ready`，失败为 `failed` |
| 符号 | 工作区符号、文档大纲、跳转定义候选 |
| 引用 | 符号级查找，按句法角色分类；提供 grouped references 命令 |
| 补全 | 标识符、include 路径、有限成员补全；结构化候选通过 `completionItem/resolve` 延迟挂载对应源码注释，不在每键热路径读盘 |
| 着色 | 宏、类型、枚举量、当前函数参数和局部变量；角色门控；成员字段不着色 |
| include | 有限解析、跳转、可达性收窄 |
| 外部头 | 通过 `fossilsense.includePaths` 索引和补全 |
| 冲突扩展 | 检测 clangd / cpptools / ccls 并提示二选一 |
| 调用关系 | 标准 LSP Call Hierarchy 与 `fossilsense.lsp.callRelations` 富结果共享同一一跳关系服务；当前正式绑定范围为 C/C++ 自由函数 |
| Rich Hover | 索引符号的签名 + tier/confidence/reason；best-effort 挂载 leading / inline-leading / trailing 注释；Doxygen/XML 参数与返回值结构化渲染，未知 tag 走 `### Tag` fallback；源码不可读、超限或 malformed 时降级为 signature-only |
| Signature Help | 简单调用的 exact-name 函数候选；显示对应函数注释，并将 Doxygen/XML `param` 描述挂载到匹配的参数 popup；不做重载或参数类型绑定 |
| `.h/.c` 配对 | 仅在双方都有同一 `ProjectKey` 且名称、种类、规范化签名兼容时启用：Hover、补全与 Signature Help 优先使用头文件声明文档；普通调用点跳转和调用关系保持源文件定义优先；在声明/定义锚点上跳转则优先去对侧，避免原地跳转 |

## 6. 补全规则

主标识符补全：

| 规则 | 要求 |
|---|---|
| `isIncomplete` | 成功、空结果、截断结果都必须为 `true` |
| 截断 | top-N 始终针对当前完整前缀重算 |
| 增量 | 前缀延长时结果收窄；空结果不能黏住后续输入 |
| 热路径 | 每键扫描内存 `NameTable` / `RankedNameHit`，不做磁盘 IO |
| 召回 | exact / prefix 档通过 sorted-by-lower 前缀索引二分 |
| 排序 | 可叠加目录局部性偏移，但绝不过滤 |

Smart Completion 当前约定：

| 项 | 规则 |
|---|---|
| pipeline | 普通标识符补全候选必须经过 `completion` 核心模块合并 evidence、去重、排序和截断 |
| module boundary | `completion/intent.rs` 负责 intent；`completion/pipeline.rs` 负责 evidence、merge/dedup、policy、metrics；`completion/ordinary_service/providers.rs` 负责候选源到统一 evidence 的转换 |
| 排序 | 普通标识符补全使用 deterministic evidence-aware ranker；`ScopeTier` 是 soft prior，并通过 guard band 防止低置信 global/text 噪音反超 |
| strict policy | `resolver::pack_score` 仍可用于跳转、着色、workspace symbol、`NameTable` recall 和兼容测试；不再作为普通补全最终 displayed ranking |
| evidence merge | 同名候选合并 indexed、local binding、current-file overlay、language builtin、local word evidence，优先保留更结构化的 LSP kind/detail |
| project context | 普通标识符补全可使用构建标记推断的同项目召回与有界排序 evidence；不得新增 `ScopeTier`、过滤跨项目候选或影响其它语言功能 |
| strict opt-out | `Unspecified`、`projectContext.mode=off`、无祖先标记或 project model unavailable 时，召回预算、顺序、kind/detail/documentation 与无此能力的基线一致 |
| current overlay | 当前 open document 的宏、typedef/using alias、枚举常量、函数声明/定义、record/type 定义和附近 identifier 使用可作为普通补全 evidence；raw text fallback 仍标为 `text` |
| language builtin | 静态 C/C++ 关键词、内置类型和常量可作为低置信 fallback 补全 evidence；显示为 `keyword` / `builtin type` / `builtin constant`，不写入索引，不参与跳转、workspace symbol 或着色 |
| intent | 普通补全使用轻量规则式 intent ranking，覆盖 type、expression、call、macro preprocessor、declaration-name；intent 只是排序证据，不做类型推断或绑定，不硬过滤 |
| recall | 普通补全 indexed recall 使用 bounded multi-channel quotas，在 current/local、reachable、external、unknown/open-scope、global、可选 same-project、text evidence 间保留有限代表性后再统一 rerank |
| include ranking | include path completion 保留 quote/angle source prior，并增加 same-directory、sibling/component edge、recent include、basename frequency、path depth 二级 evidence |
| metrics | verbose/perf 日志只输出分阶段耗时、候选来源/返回计数（含 language_builtin/project 聚合计数）、intent bucket、recall channel counts、guard 摘要、shadow rank 摘要和 include ranking 计数 |
| 隐私 | 默认 debug/perf summary 不输出候选名、源码片段或用户代码内容 |
| shadow | shadow ranking 只作 ranker 对比和回归观测；不得改变返回内容 |
| v1.2.1 Phase 7-8 | member evidence 覆盖字段和第一版 C++ 方法；ordinary completion 可使用本地 accepted-completion history 作为有界排序证据 |
| v1.2.2 Phase A-D/H | 行为保持型架构健康发布；新增架构基线、fitness functions、WorkspaceSession/CacheLedger/DocumentStore 边界、ordinary completion service 边界和 release hardening 门禁 |
| v1.2.3 | 解析与成员补全体验修复版；多行 typedef struct 容错、匿名嵌套 record evidence、数组下标/括号/解引用的简单成员链补全，以及链解析失败后的全局 member fallback |
| v1.3.0 | 架构健康与补全 evidence 版本；收敛 parser/store/server 边界，并加入有界 language builtin 与 project context 普通补全证据 |
| v1.3.1 | 项目上下文补全版本；加入构建标记发现、最近祖先项目归属和同项目普通补全排序证据，并保持严格 opt-out parity |
| v1.3.2 | 解析类型符号卫生修复；注释/字面量不再污染 type symbols，AST 精确类型名优先，关键词不可跳转，schema 11 强制重建 |
| v1.3.3 | 有证据的一跳调用关系版本；统一代际 callable/call-site facts、标准 LSP、富协议、workspace open-document overlay 与原生双视图；schema 14 |
| v1.3.4 | 注释美化渲染版本；Hover / 补全文档 / Signature Help 共用注释归属与 Doxygen/XML Markdown 渲染，补全经 `completionItem/resolve` 延迟挂载，并支持严格同项目 `.h/.c` 文档配对 |
| 后置能力 | auto include insertion、ML ranker、telemetry、cloud sync、完整 C++ 语义仍不属于当前版本 |

项目上下文约定：

| 项 | 规则 |
|---|---|
| 默认标记 | `Makefile` / `GNUmakefile`、`CMakeLists.txt`、QMake `*.pro`、Ninja `build.ninja`、`*.sln` / `*.vcxproj` / `*.vcproj`、`meson.build`、Bazel `BUILD` / `WORKSPACE` 主文件；Windows 大小写不敏感 |
| 排除 | 不把任意 `*.mk` / `*.pri` / `*.ninja`、`compile_commands.json` 或 CMake cache 当项目根；发现尊重 `.gitignore`、默认 `build/out/target` 等排除和 `fossilsense.json` scope |
| 自动归属 | 请求 URI 所在的最具体 workspace root 内，最近祖先 marker 目录获胜；同目录 marker 合并，嵌套项目保持独立 |
| 选择 | 状态栏提供 `Current Project (Auto)`、所有发现路径和 `Unspecified`；显式选择只存 VS Code `workspaceState` |
| 配置 | `fossilsense.projectContext.mode = auto / promptOnAmbiguous / off`；prompt 只在有可选项目且活动本地 C/C++ 文件无法归属时每 URI/会话提示一次 |
| snapshot | `ProjectContextIndex` 与带 `ProjectKey` 的 `NameTable` 同代原子发布；marker 变化只重建派生读模型，不重解析未变源码 |
| memo | engine epoch、selection epoch 和 effective project 都参与 completion memo generation；marker/选择变化不得复用旧池 |
| fallback | project discovery 失败标记 `projectContext` degraded，ordinary completion 继续走基线；热路径只查内存，不遍历文件系统 |
| 边界 | 项目上下文的召回/排序 boost 仍仅由 ordinary identifier completion 消费；definition、Hover、completion documentation 与 Signature Help 只读取 `ProjectKey` 作为严格 `.h/.c` 配对门槛，不改变跨项目召回或基础候选排名；references、coloring、workspace symbol、member/include completion 不消费 |

短前缀：

| 前缀长度 | 规则 |
|---|---|
| `< 3` | 只接受 exact、prefix、词边界子串；score >= 650；丢弃普通子串和子序列长尾 |
| `>= 3` | 保留全档位，包括 camelCase initials 子序列 |

成员补全：

| 场景 | 规则 |
|---|---|
| C | 用当前文件简单声明猜 receiver 的 record 类型，再查字段 |
| 跨文件前向声明 | 可通过 record 索引补字段 |
| 链式访问 | 支持简单字段链、数组下标、括号和 `*`/`&` lvalue 形态，例如 `a.mem1[n].`、`arr[i].`、`(*ptr).inner.` |
| 匿名嵌套 record | 对匿名嵌套 `struct/union` 成员生成 best-effort record evidence，可继续补内层字段 |
| 链解析失败 | 调用结果、复杂表达式等解析失败时，不做表达式类型推断，按 prefix 走全局 member fallback |
| 猜不到 receiver | 回退全库 member 名前缀候选 |
| C++ | 复用 record/member evidence 补 class/struct 字段和第一版方法 |
| weak receiver | 只做明确声明和唯一名字相关的窄范围推断，并通过 confidence/fallback 标注 |
| 不支持 | 函数调用结果、复杂 cast、宏展开、继承、重载、模板、命名空间、访问控制、完整表达式类型推断 |

成员 fallback 必须满足：

- 只给 prefix 命中的 member 候选，不做完整表达式类型推断。
- 前缀长度 >= 2。
- 只取前缀命中。
- 数量受 `COMPLETION_LIMIT` 控制。
- 前缀长度 < 2 时返回空 incomplete。

本地补全历史：

| 项 | 规则 |
|---|---|
| 范围 | 只作用于 ordinary identifier completion；不改变 include/member routing |
| 信号 | 只记录 completion item command 触发的 positive accept evidence |
| 存储 | workspace-local cache，bounded JSON；不写入源码仓库，不进主 symbol index |
| 内容 | candidate hash、kind、intent、prefix bucket、时间；不存 raw label、源码片段或路径 |
| 控制 | `fossilsense.completionHistory.mode = auto/on/off`；`FossilSense: Clear Completion History` 清除 |
| 排序 | 小幅 capped boost；不得压过 high-confidence current/local evidence |
| 禁用 | `off` 或清除后，ordinary completion 回到 deterministic evidence-aware ranker |

## 7. Include 与可达性

配置：

| 配置 | 默认 | 作用 |
|---|---|---|
| `fossilsense.includeScoping` | `auto` | 用 include 可达集收窄作用域；可设 `off` |
| `fossilsense.includePaths` | 空 | 外部参考头目录 |

解析顺序：

| 形式 | 顺序 |
|---|---|
| `"..."` | 当前文件目录 -> 工作区 -> `includePaths` |
| `<...>` | `includePaths` -> 工作区 |

解析结果：

| 结果 | 规则 |
|---|---|
| `RelativeExact` | 当前文件相对路径精确命中 |
| `WorkspaceExact` | 工作区精确命中 |
| `ExternalExact` | 外部 include 路径精确命中 |
| `SuffixMatch` | suffix tier 唯一命中 |
| `Ambiguous` | suffix tier 多命中；不建立边 |
| unresolved | 零命中；记录未解析 include |

可达性：

- `ReachGraph` 每工作区一份；每次索引用新 `Arc` 替换以失效缓存。
- 当前文件做有界 BFS：`MAX_REACH_DEPTH = 32`，`MAX_REACH_NODES = 4096`。
- 闭包内任一文件有 unresolved、ambiguous 或触顶上限，则 scope 为 open。
- `OpenReason` 优先级：`UnresolvedInclude` -> `AmbiguousInclude` -> 遍历上限。

着色与补全：

| 场景 | 着色 | 补全 |
|---|---|---|
| 可达集确定 | 只着当前文件和可达文件内定义 | 当前 / 可达 / 其余分层排序 |
| open scope | 回退旧全局行为 | 关闭非可达降权，继续给候选 |
| ambiguous include | 不把歧义孪生着色为 proven 可达 | 歧义孪生降为 `Unknown` tier，可导航 |
| includeScoping off | 回退全局行为 | 回退全局行为 |

设计底线：着色宁可不着色；补全只软排序，不硬过滤到符号消失。

外部头：

- 外部目录单独遍历，不套 `.gitignore`。
- 按扩展名、文件数、体积上限过滤。
- 超限退回“仅路径解析，不入符号”。
- 按 mtime 增量。
- 外部符号可搜索、可补全，但排在工作区符号之后。
- 语义着色只纳入被工作区文件以 `ExternalExact` 直接 include 的第一层外部头。
- 传递包含的外部符号只可导航，不参与着色。
- 目录缺失、非目录、重复条目只 warning 后跳过。
- FossilSense 不编译外部头，错平台头不应触发编译类报错。

## 8. Parser

唯一解析入口：

```rust
parse(path, source) -> FileSemanticIndex
```

`FileSemanticIndex`：

| 字段 | 说明 |
|---|---|
| `symbols` / `includes` | 词法 pass |
| `occurrences` | AST walk，含句法角色 |
| `records` / `members` / `aliases` | record、字段/方法 member、type alias 候选 |
| `callable_anchors` / `call_sites` | AST 同遍历产生的可调用锚点与调用表达式事实；不代表编译级绑定 |
| `local_declarations` | 请求期 receiver 推断，不持久化 |
| `ParseDiagnostics` | parse error、fallback、provenance |

Parser facts 合同：

| 项 | 规则 |
|---|---|
| `ParseFacts::INDEX` | 索引期事实；包含 call relations，跳过 occurrences、local declarations、local bindings |
| `ParseFacts::CALL_RELATIONS` | callable anchors 与 call sites；与既有 symbol 投影并行，不能替换文档符号语义 |
| `ParseFacts::COLOR_REF` | 着色和引用角色所需 occurrences + lexical facts |
| `ParseFacts::MEMBER` | 成员补全 receiver 推断所需 local declarations / bindings + record/member/alias facts |
| `PersistentFacts` | `FileSemanticIndex::persistent_facts()` 返回 symbols、includes、records、fields、members、aliases、callable anchors、call sites 的借用投影 |
| `RequestFacts` | `FileSemanticIndex::request_facts()` 返回 occurrences、local declarations、local bindings 的借用投影 |
| `FactAvailability` | `Available` / `NotRequested` / `Unavailable(LexicalFallback)`；空向量不能单独代表 skipped 或 fallback |

降级：

- 普通解析问题不返回 `Err`。
- 词法 pass 永远执行。
- tree-sitter 给不出可用 tree 时走 `lexical_fallback`。
- 带 `ERROR` node 的 tree 仍视为可用 tree。
- fallback 只保留词法 `symbols` / `includes`，AST 向量为空。

调用事实首版边界：

- C/C++ 自由函数是关系解析的正式候选；record method、member call、函数指针和 callable object 仍持久化为显式事实，但不得伪装为已绑定关系。
- 外部头只贡献声明锚点，不索引函数体调用点。
- 全局初始化表达式使用 synthetic global initializer 作为 caller；lambda 内调用暂不错误归属给外层函数。
- schema 14 为 callable anchors / call sites 建立独立 active views 和查询索引，并完整保存 declaration/body UTF-16 ranges，与统一语义代际一起发布。

调用关系查询合同：

- `RelationCatalog` 随 `EngineSnapshot` 原子发布；callable、call site 与逻辑 relation 负载各只存一份，incoming/outgoing 只保存共享 relation ID，协议 DTO 必须在分页和 call-site 上限之后物化；请求不得直接查询 SQLite 或深拷贝完整 catalog。
- dirty index 可以重建完整关系目录，因为任一声明变化都可能改变跨文件候选；失败只标记 `callRelations` degraded，不得暴露半代目录。
- open document overlay 只纳入内容与磁盘不同，或已保存但尚未发布更新 `SemanticGeneration` 的文档；普通 clean open document 直接复用基础 catalog。overlay 以有效文档版本集合和 engine epoch 缓存，`didOpen` / `didChange` / `didSave` / `didClose` 与索引发布必须主动释放旧项。
- overlay generation 覆盖同一 workspace 的全部有效未同步文档，因此查询 callee incoming 时必须看到其它未保存 buffer 的调用点；缓存与请求只共享 `Arc<RelationCatalog>`，不得为读取或存储缓存深拷贝完整 catalog。
- 标准 hierarchy item 携带 `CallableLocator`；entity key 失效后按 path、signature digest 与最近旧锚点保守重定位，不得只依赖瞬时数据库行号。
- 名字、显式限定名、arity、internal linkage、same-file 与 include reachability 都只是证据；reachability 不得作为 hard filter。
- 标准 LSP 只返回可表示的已解析候选；富协议同时返回 confidence、evidence、ambiguity、unresolved、revision、coverage 与 budget state。

旧入口已移除：`FileIndex`、`ColoringTargets`、`collect_coloring_targets`、`occurrences_with_roles`。

## 9. 引用与角色

句法角色：definition、declaration、call、write、type use、read。无法更精确判断时用 `read`。

引用查找：

- ripgrep 发现候选文件。
- 只解析命中文件。
- 按文件指纹做 LRU 缓存。
- 解析失败时角色降级为 `Read`。
- 返回前按角色分组，定义和声明在前。
- 保持 `REFERENCES_LIMIT` 截断与汇报规则。

标准 `textDocument/references` 仍返回 `Location`。带角色入口：

```text
executeCommand: fossilsense.lsp.groupedReferences
command: FossilSense: Find References (Grouped by Role)
```

## 10. Resolver 与候选语义层

跳转、补全、工作区符号、着色读路径都必须走共享 `resolver`。

| 原语 | 规则 |
|---|---|
| `scope_tier` | 从文件和可达上下文判定 `ScopeTier` |
| `pack_score` | `TIER_STRIDE` packing；tier 严格主导 base match + locality |
| `confidence_reason_for` | 从 tier、exact name、open reason 投影 confidence / reason |
| `dedup_keep_higher` | 同名候选保留更高 `(tier, confidence)` |

禁止恢复旧 magic score：`CURRENT_FILE_BONUS`、`REACHABLE_BONUS`、`UNREACHABLE_PENALTY`、`EXTERNAL_SCORE_PENALTY`、`LOCALITY_PER_SEGMENT`、`definition_score`。

排序规则：

- tier 严格主导，locality 或 match quality 不能反超更可信 tier。
- `External > Global`：直接 include 的外部头有可达性证据，优先于无可达路径的全局工作区符号。
- ambiguity 是作用域信号，通过 `OpenReason::AmbiguousInclude` 暴露。
- 候选不是语义绑定。

## 11. Record / Member / Alias

当前模型：

| 数据 | 规则 |
|---|---|
| `record_defs` | 第一类 record 定义 |
| `members` | 字段、第一版方法、static method 等 member evidence 带 `record_id` 外键；member 不进普通 `symbols` |
| `type_aliases` | 作用域感知 alias 候选 |
| `RecordCandidate` | record 候选 |

成员补全必须使用：`resolve_record_candidates`、`members_for_records`、`fallback_member_candidates`。`fields_for_records` 只作为兼容 wrapper 保留。

成员 durable reads 必须通过 `store::views::MemberStoreView` 或兼容 wrapper，保持 `RecordCandidate` / `MemberCandidate`、`ScopeTier`、alias recursion、prefix filtering、fallback caps 与排序语义不变。

禁止恢复：

- 依赖 `symbols.container` 做成员补全。
- 全局 `resolve_alias`。
- `fields_by_record[_scoped]` 这类 field-only 查询面。

alias 规则：递归解析必须防环；不收敛成单一全局赢家；同 tier 候选全保留，只按 record id 去重。

## 12. 用户可见标注

| 能力 | 标注 |
|---|---|
| 补全 | 非 `Current` 项在 `detail` 显示 `reachable` / `external` / `global` / `ambiguous` |
| 补全文档 | 显示完整 `tier`、`confidence`、`reason` |
| 跳转定义调试 | `fossilsense.debug.candidateReasons = true` 时输出候选理由 |
| 引用 | grouped references 命令显示 role |
| 项目上下文 | 独立状态栏显示 Auto / manual / Unspecified / Off / unavailable；tooltip 显示 workspace-relative path、marker 和“只作补全排序”限制 |

降噪：

- `Current` 补全项不标注。
- debug candidate reasons 默认关闭；关闭时定位结果和顺序不变。
- 无可用标签时不要强行显示。
- `Unknown` tier 且 open reason 为 `AmbiguousInclude` 时，confidence 为 `Ambiguous`；其他 open reason 为 `Fallback`。

## 13. VS Code 扩展互斥

冲突扩展：clangd、Microsoft C/C++ cpptools、ccls。

`fossilsense.mode = auto` 时检测冲突并弹一次性通知，提示二选一，提供停止 FossilSense 或打开设置动作。不再按 completion / semanticColoring 单项静默让位。整体停用走 `fossilsense.mode = off` 或 `FossilSense: Stop Server`。

## 14. 命令

仓库根：

```bash
cargo build
cargo test
cargo run -p fossilsense -- scan samples/mini-c
cargo run -p fossilsense -- index samples/mini-c --db target/mini.sqlite --force
```

`extensions/vscode`，包管理器用 pnpm：

```bash
pnpm install
pnpm run compile
pnpm run package
```

调用关系 UI：

- `FossilSense: Analyse Call Hierarchy` 从活动 C/C++ 光标默认加载 incoming；Relation Panel 中 `FossilSense Call Relations` 可切 incoming/outgoing。
- 选择关系必须同时导航目标并刷新 `FossilSense Call Sites & Evidence`，后者展示 coverage、confidence/evidence 和每个可跳转 call site。
- 扩展只渲染 `fossilsense.lsp.callRelations` 富协议，不得在 TypeScript 侧重新解析或绑定源码。

Release hardening gate:

```bash
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify_release_hardening.ps1
```

## 15. 打包与发布硬约定

每个对外发布版本，包括中间可演示阶段成果，都必须产出可直接安装的 `.vsix`。这是硬性交付物。

发布收尾必须：

1. 运行 `pnpm run package`。
2. 确认 VSIX 落在仓库根 `dist/`。
3. 交付说明写清版本、能力范围、能做什么、还不能做什么。
4. 确认 VSIX 自包含原生二进制。

安装：

```bash
code --install-extension dist/<name>.vsix
```

或 VS Code：`Extensions -> ... -> Install from VSIX`。

`pnpm run package` 流程：

1. `cargo build --release -p fossilsense`
2. 复制 `target/release/fossilsense(.exe)` 到 `extensions/vscode/bin/`
3. esbuild 输出 `out/extension.js`
4. `vsce package --no-dependencies`

VSIX 文件名：

```text
dist/fossilsense-vscode-<version>_BUILD<YYYYMMDD_HHMMSS>.vsix
```

## 16. 代码与文档规则

核心风险：局部补丁看似完整，却造成概念漂移。后续修改优先保持概念稳定、行为可解释、测试有杀伤力、文档同步当前实现。

文档分层（详见 `docs/README.md`）：

| 层 | 位置 | 规则 |
|---|---|---|
| 权威 | 本文件、README、扩展 README、代码/测试 | 当前事实只认这里；冲突时改文档对齐实现 |
| 活笔记 | `docs/architecture/` | 只保留仍指导当前理解的短文；不堆施工过程 |
| 施工单 | `openspec/changes/<name>/` | 仅未完成变更；完成后必须迁入 `openspec/changes/archive/` |
| 归档 | `docs/archive/`、`openspec/changes/archive/` | 标 `archived` / `superseded`；可查痕迹，不当 backlog，不得自动复活愿景 |

| 规则 | 要求 |
|---|---|
| 当前事实优先 | `CLAUDE.md`、README、扩展 README、`package.json` 描述、CLI about 必须同步实现 |
| 历史文档标状态 | `current` / `implemented` / `archived` / `superseded`；不得自动复活历史愿景 |
| 过程文档不平行 | research / OpenSpec design 落地后归档或抽成短笔记；禁止与本文件抢权威 |
| 先稳定概念 | 新能力先说明使用 `Definition`、`Declaration`、`Occurrence`、`ReferenceHit`、`RecordDef`、`FieldDef`、`TypeAlias`、`IncludeEdge`、`ReachScope` 中哪个概念 |
| 候选不是绑定 | 没有编译级证据时，跳转、补全、引用、着色都是 best-effort candidate |
| 文档必须写清 | confidence、fallback、ambiguity、open scope、truncation、cache invalidation、parser fact availability、store read-view contracts |
| 少叠 magic score | 排序必须分清 match quality、scope tier、external penalty、locality、fallback confidence；继续走共享 `resolver` |
| 注释讲不变量 | 避免 `always` / `never` / `complete` / `zero-cost` / `non-empty`，除非代码和测试支撑 |
| 错误处理可见 | 解析和配置缺口可降级；索引失败、DB 损坏、`NameTable` / `ReachGraph` 构建失败不能伪装 ready |
| 依赖准入 | 新依赖必须说明替代方案、运行时/打包影响、平台约束、版本约束 |
| 大文件拆边界 | `server.rs`、`store.rs`、`query.rs`、`parser.rs`、`indexer.rs` 新增大功能前，先抽纯逻辑模块并单测 |
| 用户可见能力 | 修改补全、引用、include、着色、跳转定义时，同步 can / cannot / fallback |
| 重构默认可破坏 | 用户要求重构时先问是否需要兼容；未明确要求兼容则默认破坏性移除老设计 |

测试必须验证质量：

| 能力 | 必测 |
|---|---|
| 补全 | 排序、`isIncomplete`、截断、前缀收窄 |
| 引用 | 角色、降级、缓存 |
| include | ambiguous、unresolved、可达性开放 |
| 增量 | `NameTable` / `ReachGraph` 失效 |
| watcher/debounce | 合并和二次调度 |
| project context | marker family/exclusion、nested/multi-root、原子发布、marker/selection 失效、同项目召回/排序、重复 label 展示、严格 opt-out parity、无热路径 IO |

Architecture fitness 的 large-source-file 只统计生产代码：专用测试文件和 Rust 内联 `#[cfg(test)] mod tests` 体积不产生大文件警告；测试代码仍受依赖方向、LSP/SQLite 边界和热路径 IO 规则约束。

## 17. 明确不做

- 不自写 C/C++ 解析器。
- 不捆绑 GPL 的 ctags。
- 不在扩展宿主内跑索引。
- 不把 best-effort 名字候选伪装成精确语义绑定。
- 不实现完整 C++ 语义：继承、重载、模板、命名空间、访问控制、表达式类型推断等。
- 不上传 completion history，不做匿名 telemetry、cloud sync、ML ranker 或自动 include 插入。
- 不解析 Make/CMake/QMake/Ninja/Visual Studio/Meson/Bazel 内容来推断 target、编译参数、宏或链接关系；构建文件只作 best-effort 项目标记。

## 18. 验收样本

| 样本 | 用途 |
|---|---|
| `example/HimuOS` | 正确性验收样本，本地、git-ignored、不入库 |
| `samples/mini-c` | CI 级自动测试样本 |

## 19. 维护检查清单

改动前检查：

- 是否仍符合“无编译环境优先”。
- 是否把启发式候选误写成语义绑定。
- 是否复用现有 model / resolver 概念。
- 是否同步用户可见 can / cannot / fallback。
- 是否新增 magic score 或平行模型。
- 是否考虑大仓库热路径和磁盘 IO。
- 是否测试排序、降级、歧义、缓存失效。
- 发布或演示时，是否产出可安装 VSIX。
