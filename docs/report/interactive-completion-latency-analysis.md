# 超大工作区连续输入与普通补全迟滞分析

> **文档状态**：架构分析与 1.5.1 实施台账；只有“实施状态”中标记完成并附验证结果的能力代表已经实现。
>
> **存放说明**：本文按本次明确要求存放在 `docs/report/`，属于对仓库常规 Markdown 边界的单次显式例外；若进入正式提交或发布流程，资产所有者仍需决定是否保留该位置或把结论转入 issue/PR。
>
> **分析基线**：FossilSense `1.5.0`，源码提交 `3fbe93d`，2026-07-31。
>
> **证据边界**：结论来自当前源码、测试和脚本。当前已经有完整 U-Boot engine 上直接调用生产 `LanguageServer::completion` 的 64 请求 release replay，以及 context/parse/local words/overlay/worker/render 分段和稳定 P95；仍没有 VS Code renderer 的 key-to-paint、stdio transport trace 或真实用户机器 CPU profile，因此服务端门禁不能冒充 UI 端到端结论。

# 1.5.1 实施状态

> **执行分支**：`perf/1.5.1-interactive-completion`。
>
> **更新规则**：本节记录已经落到当前分支并通过验证的事实。尚未运行的性能 case 不填写推测数字；后续阶段继续以源码、测试和脚本输出为准。

| 阶段 | 状态 | 当前成果 | 尚未完成 |
|---|---|---|---|
| Phase 0A：latest-request-wins 与前台 admission | 已通过阶段验证 | completion token 在 RPC 首个 `await` 前按请求顺序注册；`didOpen`、`didChange`、`didSave`、`didClose` 在异步 mutation 前后 supersede 竞态窗口内的旧 token；memo 只允许 current token 提交，最终 publish check 后不再挂起；前台 CPU admission 保留 1–2 个 permit，排队旧请求可提前退出；生产 pooled recall 的 scan 与精确 top-K selection 均每 256 个 entry 协作检查取消；stale partial hits/pool 不会进入返回或 memo | overlay 构建、live parse 和其它 feature 的 blocking closure 尚未接入同一 token；专用 CPU pool 与 background credit 尚未实现 |
| Phase 0B：生产路径观测 | 已通过阶段验证 | `[perf] completion` 新增互不重叠的 context/parse/local words/overlay/admission wait/worker/render 时间，以及 scan/selection/cancel 指标；取消路径累计 superseded、queue cancellation、worker cancellation、worker failure 与 stale inspected entries；production pooled-recall component 与 64 请求生产 handler replay 均已接入 U-Boot 自动门禁 | 并发突发 replay、端到端 stdio replay 与 VS Code key-to-paint trace 尚未实现 |
| Phase 0C：full-build 门禁恢复 | 已通过阶段验证 | disposable full build 在事实写入期间延迟全部非唯一 lookup index，完成后批量恢复；在线/增量路径、quick/foreign-key check、generation lease 与原子发布不变 | Wine 发布候选门禁留到最终阶段 |
| Phase 1：identity / persistent overlay | 最终独立复审无 blocker，正式门禁通过 | completion-only `RecallUniverseId` 由实际投影内容计算；当前文档 body-only 编辑复用同一 immutable overlay Arc 与 memo generation；请求级 reach graph 现在只保存 dirty source/edge、open/Go/direct-external override，并通过 `Arc` 共享 captured base；发布代维护 immutable `IncludePathIndex`，请求只叠加 dirty path delta；compact `NameTable` 继续共享发布代 base/delta；path-only external include 使用有界异步 probe cache，前台不做文件系统 metadata IO；完整 hover/navigation overlay 与 resolve 仍保持 exact-version/epoch 校验 | 当前文档 line map/local words/syntax facts 增量化与剩余 blocking work 统一取消留到 Phase 3；stdio/VS Code UI 延迟不属于当前门禁 |
| Phase 2：bounded recall v2 | 最终独立复审无 blocker，正式门禁通过 | base/delta 使用惰性 k-way bounded prefix cursor；所有 root 共用 `16,384` candidate budget；C/Go 在消费预算前隔离；mixed segment 通过 active path 与 selected-project compact prefix posting 恢复，scope/project/recovery 的 source、metadata probe 和 candidate pop 各自保留份额，总和仍受原 1/8 priority budget 与 `4,096` metadata cap 约束；跨源同一 declaration 只评分一次；项目/语言族 presence 与 segment-local project ID 均由发布时 O(1) map 提供；direct-external 请求投影封顶 `4,096`；完整 matcher 仍消费全部 query 字符；truncated pool 不再用于 memo narrowing；当前 U-Boot 生产 handler replay P95 `23.284 ms`、最大 inspected `16,384`、payload SQL reads `0` | 200 分任意子序列长尾仍是显式截断的均匀 fallback；stdio/VS Code UI 延迟不属于当前门禁 |
| Phase 3：增量文档与 parse | 后续版本，非 1.5.1 发布阻断 | 本次不扩大实现范围 | line map/local words、incremental tree/facts 与客户端完整字符策略验证 |

当前已通过的 TDD/结构性验证：

- `query::tests::production_completion_recall_reports_work_and_observes_cooperative_cancellation`：冷态生产召回报告完整扫描量；scan 中的受控旧请求最多再检查一个 256-entry block，且不泄漏 partial hits/pool。
- `query::tests::production_completion_recall_checks_cancellation_during_post_scan_selection`：token 在 scan 完成后、精确 top-K selection 的首个 block 内翻转；最多再检查 256 个源 entries，每个命中只进行 `O(log K)` 的固定容量 heap 更新，最终只排序 `K` 项，结果与 pool 全部丢弃。
- `query::tests::production_completion_recall_counts_and_cancels_sparse_channel_filtering`：即使 Reachable 等通道零命中，被 predicate 排除的源 entries 也计入 256-entry cancellation budget，不能隐藏一次 `O(D)` 稀疏过滤扫描。
- `server::tests::completion_runtime_supersedes_queued_request_before_cpu_admission`：单 permit 被占用时，同 URI 新请求使旧排队请求先于 permit 释放退出。
- `server::tests::document_change_supersedes_active_completion_token`：文档 revision 变化立即使活动 token 失效。
- `server::tests::completion_runtime_never_allows_older_work_to_replace_a_post_change_request` 与 `cancelled_request_cannot_repopulate_a_memo_after_waiting_for_its_lock`：覆盖旧 snapshot 晚完成和 memo mutex 排队交错，旧请求既不能反向取消新请求，也不能在 change/close 后复活 memo 或返回结果。
- `store::tests::maintenance::full_build_defers_secondary_indexes_until_facts_are_complete`：full build 构建中不维护代表性查询 lookup index，finalize 后全部恢复；构建期间仅保留发布清理明确依赖的窄 `idx_file_revisions_file_id` 维护索引。
- `query::tests::bounded_completion_recall_has_a_hard_budget_and_keeps_full_query_matching` 及相邻 differential cases：候选生成共享 `16,384` 硬预算，语言家族在消费预算前隔离，最终 matcher 使用完整输入；重复名称 expansion、delta/tombstone 与 fuzzy posting 都有回归覆盖。
- `server::tests::completion_overlay_reuses_recall_universe_across_current_body_edits`、`completion_overlay_invalidates_when_another_dirty_declaration_changes` 与 `current_completion_projection_tombstones_renamed_indexed_declaration`：分别证明 body-only 复用、语义投影变化失效和当前 dirty path 不泄漏 durable 声明。
- `server::tests::completion_memo_generation_tracks_recall_universe_not_body_revision` 与 `overlay_completion_resolve_rejects_a_newer_overlay_epoch`：base memo 使用稳定召回身份，completion resolve 仍以 exact epoch 拒绝旧结果。
- `bounded_*_recall_skips_tombstoned_sibling_path_inside_{one_delta,base}` 六个用例：base 与多路径 delta 中的一个 path 后续 tombstone 后，另一个仍存活 path 不会被同 segment 内超过预算的 stale rows 饿死；prefix、single-character 与 fuzzy 三种入口均保持 `entries_inspected <= budget`。
- `selected_project_recall_skips_tombstoned_sibling_path_inside_base`：selected-project posting 的 head 属于已删除 base path 时，仍能在 priority budget 内召回同项目 active sibling。
- `priority_source_setup_deduplicates_scope_and_mixed_segment_recovery` 与 `priority_source_setup_observes_cancellation_before_scope_fanout`：scope/recovery 的同一 path cursor 只初始化一次；100-path scope 在 candidate budget 为 32 时最多尝试 4 个 source，且已取消请求在第一次 source 前退出。
- `selected_project_recall_keeps_a_source_after_unmatched_scope_fanout`、`reachable_recall_keeps_a_source_after_selected_project_fanout` 与 `selected_project_recall_keeps_candidate_budget_after_long_scope_cursor`：source probe、source slot 与 candidate pop 三层均按 scope/project/recovery 分区；无匹配 fanout、四个 project cursor 和长 reachable cursor 都不能跨通道吃掉显式 quota，未使用份额才在第二轮回收。
- `priority_recovery_counts_inactive_path_probes_and_cancels_cooperatively` 与 `priority_scope_counts_missing_path_probes_and_cancels_cooperatively`：inactive prefix-path pair 和不存在的 scope path 也消费 metadata probe；分别在 32 probes 截断、256 probes 协作取消，不再隐藏 `O(paths)` 前置扫描。
- `priority_candidate_deferral_checks_cancellation_before_heap_fanout`：257-row 非整块 share 在 defer 前取消，256-row 整块 share 不会对每个 deferred cursor 重复执行原子取消读取；独立 deferred counter 同时封住漏检查和过度检查。
- `bounded_recall_scores_each_index_once_across_priority_and_global_sources`：同一 declaration 同时由 project、scope 和 global prefix 召回时只进入 matcher/pool 一次，不能在最终去重前挤占 top-K 或 channel quota。
- `project_posting_carries_its_segment_id_for_constant_time_prefix_recovery`：256 个项目中位于末尾的 selected project 直接从现有 `by_project` value 取得 segment-local ID；project prefix positions 只保存 `u32` pair 位置，HashMap bucket 与各 Vec capacity 全部进入 recall core 计账。
- `selected_project_presence_is_language_partitioned_without_copying_indices`：除语言隔离外，还验证 499 个 path tombstone 后 active count 从 500 变为 1、最后一个 path 从 Go 切换到 C 后两个 family count 同步为 0/1，并显式覆盖 `compacted()` 与 marker `with_project_context()` 重建计数；production presence 只做一次 HashMap lookup。
- `request_direct_external_projection_is_bounded_by_the_reach_node_cap`：单源超过上限的 external fanout 只投影词典序前 `4,096` 个；超过上限的 workspace edge 噪声不能把排在输入末尾的 direct external exact edge 挤掉。
- `request_suffix_lookup_bounds_duplicate_basename_scan_and_candidates`：10,000 个同名 basename 的 request suffix fallback 最多检查 `4,096` 项、保留 `256` 个候选；被截断的结果不能证明唯一，reachability 保持 open。
- `path_only_external_include_is_probed_off_request_and_cache_tracks_probe_epoch`、`deferred_external_probe_reaches_root_after_expired_negative_prefix` 与相邻 probe/cache 用例：前台只读取有界缓存，单 view 最多排队 `16` 次，后段 root 由同一后台 worker 公平推进；pending/expired probe 不会污染 overlay cache 或 completion memo。
- `unrelated_c_request_overlay_borrows_large_go_package_membership`：无关 C overlay 不再克隆大型 Go package membership；受影响 package 的 base 扫描也受剩余 reach-node budget 约束。
- `completion_overlay_cache_metrics_measure_real_hits_and_misses`：production replay 的 forced miss 指标来自实际 overlay-cache lookup 计数；正式样本区间强制断言 `hits = 0`、`misses = 64`，不再用请求数代替测量值。
- 当前完整复验为 `cargo fmt --all -- --check`、`cargo clippy -p fossilsense --all-targets -- -D warnings`、`cargo test -p fossilsense` 全部通过；Rust 主套件统计 `1111 passed; 0 failed; 8 ignored`，CLI `1/1`、LSP integration `2/2`，VS Code `pnpm run compile` 与 `pnpm run test` 通过。`scripts/verify.ps1 -SkipInstall` 总门禁通过；最终 reviewer 明确结论为 Phase 1 的 suffix、probe fairness、Go membership 与 replay 实测四项阻断均已关闭，可以进入 U-Boot release 实测。正式组合门禁及组件数据见下文。

## 2026-08-01 U-Boot 阶段数据

机器为 Windows 11 build 26220、Intel Core i5-12500H（12 核/16 线程）、约 24 GiB 物理内存。样本提交 `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c`，本地另有 `boot/scene.c` 与 `boot/vbe_abrec.c` 两处未提交小改动；结果不得与不同样本状态混写。

| Case | 指标 | 优化前诊断 | lookup index 延迟构建后 |
|---|---:|---:|---:|
| U-Boot full index | engine `elapsed_ms` | 89,114 | 30,182 |
| U-Boot full index | outer elapsed | 89,402 ms | 31,182 ms |
| U-Boot full index | `write_ms` | 48,540 | 10,890 |
| U-Boot full index | `include_edge_ms` | 7,832 | 1,120 |
| U-Boot full index | `secondary_index_ms` | 8,667 | 6,271 |
| U-Boot full index | `publication_ms` | 10,864 | 4,219 |
| U-Boot full index | peak Private | 160,505,856 B | 135,487,488 B |
| U-Boot full index | database | 363,282,432 B | 355,368,960 B |
| U-Boot engine hydration | declarations / files | — | 654,890 / 13,244 |
| U-Boot engine hydration | recall core | — | 89.4 MiB |
| U-Boot engine hydration | single generation | — | 174.98 MiB |
| U-Boot engine hydration | two-generation peak | — | 332.51 MiB |
| U-Boot engine hydration | first / second build | — | 3,666 / 3,579 ms |

正式 full-index 门禁按本次明确决策调整为 `elapsed_ms <= 120,000`；本次结果同时低于旧 60 秒阈值。full-index 原始报告位于 ignored 的 `target/benchmark/large-workspace-20260801_001740.{json,md}`，hydration 原始报告位于 `target/benchmark/large-workspace-20260801_002508.{json,md}`。单代与双代分别低于 384 MiB 和 512 MiB 门禁。

production pooled-recall component 基线直接调用 `search_completion_recall_pooled_with_project_for_family`。初始 40 次 cold samples 为 p50 `98,465 µs`、p95 `161,579 µs`；改用可取消的固定容量精确 top-K heap 后，同库复验为 p50 `89,840 µs`、p95 `99,940 µs`，分别改善约 8.76% 与 38.13%，SQLite payload reads 仍为 `0`。但两次每个样本都检查全部 `654,890` entries，因此该数据只证明普通补全使用的召回组件仍为冷态 `O(D)` 且没有因取消机制产生性能回退；admission 与 stale cancellation 的正确性由并发结构测试证明，尚不能据此推断端到端延迟。Phase 2 在将 inspected entries 收敛到固定预算、并补齐并发 LSP replay 前不能宣称交互性能完成。

## 2026-08-01 Phase 2 bounded recall 阶段数据

本节数据仍来自同一 U-Boot 数据库与机器，测量入口是 ordinary completion 使用的 production pooled-recall component；它包含 compact candidate generation、完整 `score_match`、scope/project channel selection 与精确 top-K，但不包含 LSP JSON、overlay 构建、payload hydration、最终渲染和 VS Code suggest widget，因此只用于算法组件门禁。

| 指标 | Phase 0 全扫描 | Phase 2 当前结果 |
|---|---:|---:|
| cold p50 | 89,840 µs | 10,786 µs |
| cold p95 | 99,940 µs | 19,040 µs |
| candidate entries inspected p50 / max | 654,890 / 654,890 | 16,384 / 16,384 |
| selection entries inspected max | 受全扫描命中量驱动 | 66,925；结构上不超过 candidate budget 的 6 倍 |
| SQLite payload reads | 0 | 0 |
| candidate coverage | 隐式完整全扫 | 达到预算时 `truncated=true`；prefix/posting/uniform sample 分别计数 |

代表性 exact/prefix 质量用同一次 full-scan 作为 oracle：`i/in/init/d/de/dev/c/cmd` 八组的 Top-100 均为 `100/100`。初版 lexicographic cursor 的单字符结果曾只有 `63/51/38`，因此增加了每声明至多一个 `u32` 的 static-quality head posting；这是由失败质量对照驱动的修复，而不是事后只记录成功样本。

非前缀 fuzzy 进一步区分可索引的高质量层与任意子序列长尾。修复原 matcher 的贪心边界误判后，continuous substring 与 camel/underscore boundary-subsequence posting 对 `dbdtn/ugdbn/ogn/bif` 的 `base_match >= 400` oracle 覆盖分别为 `2/2`、`2/2`、`54/54`、`100/100`；前三个目标 `device_bind_driver_to_node`、`uclass_get_device_by_name`、`ofnode_get_name` 在 bounded 原始列表中分别位于第 `1/1/19`。`board_init_f` 因大量重复声明连 legacy full-scan 的 300 条原始声明配额也未进入，bounded 与 oracle 的存在性一致，不能把该旧有去重前配额问题归因于 bounded recall。

`base_match = 200` 的任意字符子序列不能由有限连续/boundary trigram 证明完整；它继续使用剩余预算的确定性 workspace-wide sample，并明确设置 `truncated=true`。因此当前保证是：预算覆盖全集时与 full scan 完全一致；大型表的 exact/prefix 与已索引 fuzzy tier 通过上述 differential gate；低质量任意子序列是诚实的有界近似，而不是伪装成完整结果。U-Boot 单代/双代 hydration、120 秒 full-index、生产 handler replay 与全量回归均已通过，Phase 2 已由独立复审收口。

### 稳定召回身份的 RED → GREEN 证据

第一次把完整 U-Boot engine 接入生产 `LanguageServer::completion` 时，bounded recall 已经生效，但每次 body edit 仍用新的 `overlay_epoch` 重建 completion overlay。该 RED 运行的 P50/P95/max 为 `68.789/83.843/102.105 ms`，分段 P95 为 context `51 ms`、overlay `50 ms`、worker `34 ms`；说明剩余主因不是 recall 又退回全扫描，而是请求 identity 把无关 body revision 扩散到工作区 projection。

现在 completion-only overlay 先从 dirty 文件提取有界投影并计算 BLAKE3 `RecallUniverseId`。当前文件的 durable path 始终 tombstone，但其 declaration/fallback 不进入稳定 workspace 投影，由同请求精确 parse 单独提供；其它 dirty 文件的声明、fallback、include/import、package 与 facts availability 进入投影。相同 universe 复用原 `Arc<CandidateOverlaySnapshot>`，不同 universe 才刷新 reach graph。完整 semantic overlay 仍按 exact epoch 构建，completion item/resolve 也继续携带并校验实际 `overlay_epoch`，因此该复用不放宽新鲜度边界。

正式运行见下表。stable universe 首次把 P95 从 RED 的 `83.843 ms` 降至 `37.143 ms`；首轮审查修复加入 active-delta 优先和 scope/project compact posting，P95 为 `25.296 ms`；第二轮再关闭 mixed-delta、项目 presence 与 direct-external fanout 后为 `25.497 ms`；第三轮补齐 base sibling 与 source 初始化预算后为 `28.409 ms`；末轮把 source/probe/candidate 三层配额、inactive metadata、跨源去重与 selected-project path posting 全部收口后，当前正式 P95 为 `26.264 ms`，context/overlay P95 为 `4/4 ms`，worker P95 为 `22 ms`。这既保留了 identity 修复的因果证据，也避免只展示最后一次成功数字。

| 生产 handler replay | RED：epoch identity | stable universe 首轮 | 首轮审查修复后 | 第二轮审查修复后 | 第三轮审查修复后 | 最终复审修复后 |
|---|---:|---:|---:|---:|---:|---:|
| requests / declarations | 64 / 654,890 | 64 / 654,890 | 64 / 654,890 | 64 / 654,890 | 64 / 654,890 | 64 / 654,890 |
| P50 | 68.789 ms | 27.495 ms | 17.520 ms | 18.343 ms | 20.291 ms | 19.212 ms |
| P95 | 83.843 ms | 37.143 ms | 25.296 ms | 25.497 ms | 28.409 ms | 26.264 ms |
| max | 102.105 ms | 62.640 ms | 26.109 ms | 26.085 ms | 30.819 ms | 28.451 ms |
| context P95 | 51 ms | 2 ms | 2 ms | 2 ms | 4 ms | 4 ms |
| overlay P95 | 50 ms | 2 ms | 1 ms | 2 ms | 4 ms | 4 ms |
| worker P95 | 34 ms | 35 ms | 23 ms | 22 ms | 24 ms | 22 ms |
| inspected max / payload SQL reads | 16,384 / 0 | 16,384 / 0 | 16,384 / 0 | 16,384 / 0 | 16,384 / 0 | 16,384 / 0 |

P95 的硬门禁是 `<= 50 ms`，不是用平均值掩盖尾部；各阶段 max 也保留在表中。replay 在同一进程中先做 2 轮 warm-up，再对 `i/in/init/d/de/dev/c/cmd` 各测 8 次，共 64 个样本。审查修复后不再只断言“response 非空”：每个请求还必须实际返回 indexed candidate、看到至少 500,000 active entries、使用精确 `16,384` candidate budget、`entries_inspected` 位于 `1..=16,384` 且在大表上标记 `truncated=true`；source attempt、source/name/declaration metadata probe 也分别执行 `2,048/4,096` 上限检查。当前 64/64 请求均满足，indexed returned 最小值为 `350`，source probe/attempt/initialized 最大值为 `2/1/1`，fuzzy name/declaration probe 最大值为 `0/0`，payload SQL reads 为 `0`。它覆盖 production handler、dirty edit、parse/overlay/recall/rank/render，但不覆盖 stdio、扩展宿主或 VS Code suggest widget。

### 首轮独立审查与修复

首轮 reviewer 没有发现 stable `RecallUniverseId`、当前文档声明排除、exact overlay epoch resolve 或 incomplete-pool narrowing 的直接 stale 泄漏，但发现两项候选正确性问题和三项门禁/隐藏工作问题。修复均先加入必红测试，再转绿：

| 审查问题 | 修复与验证 |
|---|---|
| 大量 shadowed base rows 可先耗尽 prefix/single/fuzzy budget，使唯一 active delta 消失 | prefix/single heap 先比较 active 状态，fuzzy posting 先消费最新 delta segment；新增超过预算的 stale base + 唯一 active replacement 三类回归，仍断言总 inspected `<= budget` |
| lexical 全局截断发生在 reach/project tier 计算前，reachable 或 selected-project 唯一候选可能被挤掉 | `NameSegment` 新增按 path、project 和 semantic family 分区的 compact posting；在同一总预算内保留 1/8 priority prefix channel，最后仍由完整 matcher 与统一 resolver 评分；超过预算 global 噪声的 reachable/project 用例转绿 |
| selected-project 通过 `project_indices()` 最多构造三次全项目 `Vec<usize>`，且混入另一语言家族 | production path 改为 family-partitioned project posting 的无分配 presence/membership；same-project top-K 直接检查 compact entry 的 `ProjectKey`；Go-only selected project 不再为 C completion 开启项目 quota |
| production replay 只要求 response 非空，禁用 indexed recall 也可能假绿 | 新增纯 gate 测试拒绝 64 个全零 fast metrics；真实 replay 逐请求断言 indexed/active/budget/inspected/truncated，并将聚合最小值写入 JSON/Markdown |
| 120 秒 full-index 只靠调用方传 timeout，未校验完成后的 outer/engine elapsed | runner 内部引入独立 gate helper、case-specific timeout 和双时钟事后检查；入口 fixture 覆盖两个 `120,001 ms` 失败分支 |

另外补充 completion overlay cache 竞态测试：不同 universe 中 epoch 2 先发布后，epoch 1 晚完成不能覆盖；随后新 engine publication 的双侧 cache revision invalidation 必须让旧 universe lookup miss。

### 第二轮独立审查与修复

第二轮 reviewer 证明“每个 segment 的当前 heap head 优先 active”还不足以处理一次增量同时写入多个 path、随后只替换其中一部分的布局，并继续检查了 candidate budget 之前的隐藏工作。四项代码问题已经用新的失败用例固定并转绿：

| 审查问题 | 修复与验证 |
|---|---|
| 同一 delta 中 path A 有大量 stale rows，path B 仍 active，但单一 segment cursor 看不到 A 后面的 B | `NameTable` 持久维护每个 delta 的 active path 列表；只要 delta 变为 mixed，就从仍存活 path 的 family-partitioned CSR posting 建独立 cursor，最多准备 `4,096` 个 path source，pop 仍消费原总 candidate budget 的 1/8，没有额外候选预算；prefix/single/fuzzy 三类 mixed-delta + subset-tombstone RED 用例全部转绿 |
| selected-project presence 在 candidate budget 分配前扫描 project posting，最坏仍为 O(D) | base publication 建立 `ProjectKey -> [C count, Go count]`，每次 path replacement 在发布阶段按旧/新 path posting 增减 active count；request 只做 O(1) lookup。测试覆盖 500→1 tombstone、Go→C family switch 和跨语言 quota 隔离 |
| 单个源文件的 direct-external include 数无上限，请求会克隆、排序并建任意数量 cursor | reach graph 构建和 source refresh 将 direct external exact edge 稳定排在其它 edge 前；request projection 只检查并克隆前 `MAX_REACH_NODES = 4,096` 条。测试同时覆盖 4,160 个 external edge 的确定性截断，以及 4,160 个 workspace 噪声后仍保留输入末尾 external edge |
| single-character heap 的 `Eq`/`Ord` 对重复稳定声明不一致；engine 发布后旧 builder 的最终交错没有显式测试 | `ShortPrefixHeapEntry::cmp` 增加全局虚拟 index tie-break；专用 heap contract 测试保证不同 slot 不比较为 Equal。cache 竞态测试现在真的在 engine publication 后用旧 revision 再次 publish，并断言旧 engine key 仍为空 |

第二轮结束时，正式 U-Boot handler P95 从 `25.296 ms` 变为 `25.497 ms`，仍保留约 24.5 ms 的门禁余量；recall core 增加到 `164.28 MiB`，单代与双代 Private Bytes 仍分别低于 384/512 MiB。第三轮继续处理的遗漏见下节。

### 第三轮独立审查与修复

第三轮 reviewer 将 mixed-delta 结论推广到 base segment，构造出“base path A 有超过预算的 stale rows、base path B 仍 active，随后只 tombstone A”的遗漏布局；同时证明 priority channel 虽然只 pop 4 个 row，却可能在 pop 前建立数千个 cursor。两项 blocker 均先以失败测试复现，再修改实现：

| 审查问题 | 修复与验证 |
|---|---|
| `active_delta_paths` 只能恢复 mixed delta；base 中已删除 path 的排序头仍能遮蔽 active sibling，prefix/single/fuzzy 与 selected-project 均受影响 | `NameTable` 同步维护排序后的 `active_base_paths`；仅当 base/delta 处于 mixed 状态且当前 prefix/project/fuzzy posting head 确实 inactive 时，才开启 active path CSR cursor。base prefix/single/fuzzy 三个 RED 用例及 selected-project base 用例全部转绿，且不在 clean segment 增加恢复 source |
| candidate budget 为 32、priority budget 为 4 时，mixed/scope/project 循环仍可预建约 8,192 个 heap source，初始化工作没有计数且不能取消 | source key 统一为 segment/family/path-or-project/mode，scope 与 recovery 跨通道去重；source 尝试总数取 `min(priority budget, 4,096)`，第一次尝试前及之后每 256 次检查取消；`priority_source_attempts` 与 `priority_sources_initialized` 同时进入 recall 指标和 `[perf] completion`。100-path scope 明确断言只尝试/初始化 4 个，取消场景在 0 次尝试、0 个 inspected row 时退出 |

第三轮还补充了 `active_project_family_counts` 在 `compacted()`、移除 marker ownership 以及重新应用 `with_project_context()` 后的显式不变量。reviewer 当时同时确认一个 Phase 1 残余：include/import 变化导致 stable-universe cache miss 时，代码仍会复制完整 workspace path/basename lookup、克隆 reach graph 主结构并扫描全局 direct-external evidence；最终请求 projection 虽已封顶 `4,096`，但 miss 前置工作还不是 workspace-size independent。该问题随后由 persistent reach/path view 阶段关闭，见下文。

### 最终独立复审与配额闭环

最终 reviewer 继续从“已初始化 source 是否真的能得到 candidate 份额”和“指标是否覆盖所有前置工作”构造反例。下列问题均先由确定性 RED 复现，再转绿；最终复审结论为未发现新的 Phase 2 可执行 blocker：

| 审查问题 | 修复与验证 |
|---|---|
| scope 先耗尽 probe 会饿死 project，简单交换顺序又会产生对称饥饿；一个长 scope cursor 还可吃掉全部 priority candidate pop | source attempt、metadata probe、candidate pop 都按 Scope/Project/Recovery 分区，第一轮保留份额，只有未使用份额才回收；两个对称 source fanout 用例和 long-scope cursor 用例分别覆盖三层配额 |
| inactive recovery pair、missing scope path 和 project-pass mismatch 在计数/取消前 `continue`，实际可扫描大量路径但 metrics 近零 | 每个 raw path/pair examination 先消费 channel metadata probe；32-probe 截断和 256-probe 取消用例转绿。selected-project 使用 project-partitioned prefix positions，避免为找项目候选重新扫描全 prefix path range |
| selected-project segment ID 通过 `Vec<ProjectKey>::position` 线性查找 | 现有 `by_project` value 扩展为 `CompactProjectPostings { project_id, by_family }`，不复制第二份 key；256-project 尾部 selected key 直接 O(1) 取 ID |
| priority/project/scope/global 多个 source 可把同一 index 重复送入 matcher，最终去重前挤占 top-K | priority 与普通 prefix 均只在 `seen.insert(index)` 首次成功时评分；跨三种 source 的 pool 长度等于 unique index 数 |
| saturated channel 的 deferred heap head 搬移既可能连续 2,048 次不检查取消，也可能在 share 恰为 256 倍数时逐 cursor 原子读取 | deferred 使用独立 counter 每 256 次检查；entries-based check 只在实际消费 candidate 前执行。257-row 取消用例从旧实现的 512 inspected 收敛到 257，256-row share 的完整请求 cancellation checks 保持低于 64 |

`project_positions` 只保存指向已有 `(token, path_id)` pair 的 `u32` 位置；HashMap bucket、每个 positions Vec capacity 和 `CompactProjectPostings` value 已全部进入 `NameTable::accounted_bytes()`。这项新增常驻内存使旧双代数据作废，因此以下正式门禁始终使用当前源码重新执行 hydration 与 completion replay，而不是沿用第三轮报告。

### Phase 1 persistent reach/path 与非阻塞 path probe

发布代现在一次性构建 immutable `IncludePathIndex`，保存 active path 的排序表与 compact basename positions；请求级 `IncludePathView` 只持有 base `Arc` 和 dirty path delta。`ReachGraph` 同样改为 base `Arc` 加稀疏 `ReachGraphOverlay`，overlay 只保存 dirty source replacement、dirty edge、open source、Go package/import/build-guard override 和 direct-external count override。未受影响的 C/C++ graph、file/package map 与 Go package membership 直接借用 captured generation；只有受影响 package 才按剩余 `MAX_REACH_NODES` budget 物化 delta。candidate context 仅在 engine epoch、semantic generation 和 indexed-files identity 全部一致时复用这些发布组件，因此请求期间的新发布不会混入旧 snapshot。

path-only external include 的兼容能力保留，但请求线程不再调用 `Path::is_file` 或等待磁盘。进程级 exact-path probe cache 由单个后台 worker 更新：缓存最多 `1,024` 项、即时队列最多 `64` 项、单 view 最多提交 `16` 次；positive/negative TTL 分别为 30 秒/2 秒，超出当次额度的候选进入同容量约束的 deferred 队列，避免第 17 个 root 被前缀 negative cache 长期饿死。pending、expired、队列饱和或 suffix 截断都返回 open/incomplete 证据；依赖这些未知 probe 的 overlay 不进入共享 cache，旧 completion memo 也会清除。basename suffix fallback 共用每请求 `4,096` scan 与 `256` candidate 上限，被截断的单候选不能被提升为唯一绑定。

最终 reviewer 复核了四个发布阻断反例：10,000 个重复 basename 的后缀扫描、17 个 path-only root 的 probe 公平性、无关 C overlay 对大型 Go package membership 的借用，以及 production replay 的真实 cache hit/miss 计数。四项均有 RED → GREEN 测试并被确认关闭，未发现新的 Phase 1 blocker。

### 正式 U-Boot 组合门禁

2026-08-01 Phase 1 最终复审后当前源码的组合报告为 `target/benchmark/large-workspace-20260801_100424.{json,md}`；三项 case 使用同一次新建数据库，样本与机器信息同上。

| 门禁 | 正式结果 | 限制 | 结论 |
|---|---:|---:|---|
| full-index engine / outer elapsed | 33,238 / 34,161.629 ms | 各自 `<= 120,000 ms` | 通过 |
| full-index write / secondary / publication | 10,695 / 6,134 / 4,823 ms | 诊断保留 | — |
| full-index peak Private | 133,922,816 B（127.72 MiB） | 诊断保留 | — |
| hydration declarations / files | 654,890 / 13,244 | `>= 500,000 / 10,000` | 通过 |
| recall core | 175,450,934 B（167.32 MiB） | 计入常驻 compact core | — |
| single generation Private | 268,288,000 B（255.86 MiB） | `<= 384 MiB` | 通过 |
| two-generation peak Private | 518,066,176 B（494.07 MiB） | `<= 512 MiB` | 通过；余量约 17.93 MiB，作为发布观察项保留 |
| first / second hydration build | 6,778 / 7,287 ms | 诊断保留 | — |
| completion requests / 实测 overlay misses | 64 / 64 | `= 64 / = 64`，且采样期 hits `= 0` | 通过 |
| completion P50 / P95 / max | 17.365 / 23.284 / 28.991 ms | P95 `<= 50 ms` | 通过 |
| completion context / overlay / worker P95 | 3 / 3 / 19 ms | 分段诊断保留 | — |
| completion indexed min / truncated requests | 350 / 64 | `> 0 / = 64` | 通过 |
| completion active min / budget min-max | 654,890 / 16,384–16,384 | `>= 500,000 / = 16,384` | 通过 |
| completion inspected min-max / payload SQL reads | 16,384–16,384 / 0 | `1..=16,384 / = 0` | 通过 |
| priority source probe / attempt / initialized max | 2 / 1 / 1 | `<= 4,096 / <= 2,048 / = attempts` | 通过 |
| priority fuzzy name / declaration probe max | 0 / 0 | 各自 `<= 4,096` | 通过 |

自动化入口使用 `-IncludeCompletionReplay` 与 `u-boot-completion-replay` 同 full-index/hydration 组合运行；`scripts/test_benchmark_entrypoints.ps1` 验证默认 case 不会意外包含昂贵 replay、显式开关只执行指定三项。replay 在两个 warm-up pass 后重置真实 overlay-cache 计数器，逐请求拒绝超过 budget、metadata probe 上限、attempt/initialized 不一致或未返回 indexed candidate 的假绿结果，最后强制 `hits = 0`、`misses = 64`。full-index runner 对外层进程耗时与引擎输出 `elapsed_ms` 分别执行 `<= 120,000` 的事后硬检查；索引门禁放宽没有传播到补全门禁。

# 结论

> **硬约束：全字符参与匹配。**当前补全上下文中的每个输入字符都必须进入最新 prefix，并参与最终候选校验和评分。允许取消过期请求、合并队列工作和使用倒排索引缩小候选集，但不允许删减 trigger coverage、采样 query 字符或忽略尾字符来换取性能。

本节到“分阶段交付与回滚”之前保留的是 1.5.0 分析基线与当时源码因果链，用于解释 1.5.1 改动为何发生；它不是对当前分支仍未实现能力的重复声明。当前实施事实、复验数字与残余风险以上方“1.5.1 实施状态”为准。

1.5.0 的困境并不是 SQLite 查询慢，也不是 `EngineSnapshot` 的不可变发布模型本身有问题。更准确地说，系统已经在**持久化代一致性**和**旧快照持续服务**上做对了主要选择，但交互热路径把一次很小的字符编辑放大成了多组与工作区规模相关的工作：

`didChange → overlay_epoch 变化 → overlay cache miss + CompletionMemo generation 变化 → 请求级图/路径/NameTable 派生 → 普通补全全表候选评分 → isIncomplete=true → 下一按键再次请求`

与此同时，旧请求的若干 `spawn_blocking` 工作不能被 Tokio 强制中止，后台索引、内存模型发布、语义着色和补全又共享 CPU、内存带宽及 Tokio blocking pool。其结果是：单个函数的微基准可以很快，但连续输入时会出现**请求放大、失效放大和排队放大**。字符回显本身不应该等待 LSP 响应，因此如果字符也明显迟滞，优先怀疑的是 CPU 饱和、旧阻塞任务残留、扩展宿主/renderer 调度和建议列表 UI churn，而不能只看服务端某个 `total_ms`。

该分析基线最核心的五个问题是：

1. **身份域耦合**：`overlay_epoch` 既参与 overlay cache，又参与 completion generation。每个成功的 `didChange` 都递增 epoch，因此 `a → ab → abc` 通常无法复用上一前缀候选池。
2. **生产召回仍是冷态 `O(D)`**：普通补全使用 scoped/project pooled recall；没有 prior pool 时遍历所有 active entries。现有 hot benchmark 测的是另一个 prefix-index 快路径。
3. **请求级 overlay 复制了工作区级状态**：dirty overlay miss 会构造全工作区路径/basename lookup、复制 `ReachGraph` 主要 maps，并复制 `NameTable` 的 path override、delta 和 all-workspace reach 集合。
4. **当前文档工作仍接近 `O(L)`**：增量编辑先复制完整字符串并从头换算 UTF-16 位置；补全按版本重新扫描全文 local words；同一版本追加 facts mask 时会重新执行 union parse。`line_text` 查找也从文档开头遍历。
5. **没有 latest-request-wins 的完整执行语义**：parser 有版本级取消，但普通召回、排序、overlay 派生和若干其它 blocking closure 没有统一的请求令牌与协作检查。旧 Future 被取消，不等于已经开始的 CPU 工作停止。

所以，优化目标不能只是“让某个函数再快一点”。需要把热路径改成下面的复杂度形状：

- 稳定索引候选生成：`O(log D + B)` 或“受硬预算约束的 posting traversal”，而不是每键 `O(D)`。
- dirty overlay：`O(O + ΔV + ΔE)`，而不是每个 epoch `O(F + E)`。
- 当前文档：接近 `O(edit + changed syntax/facts)`，而不是稳定地 `O(L)`。
- 调度：旧请求消耗的 CPU 有明确上界，最新文档版本始终优先。

其中 `B` 是候选生成预算，`ΔV/ΔE` 是 dirty 文档实际改变的节点和边。返回条数上限只限制输出，**候选生成预算**才限制计算。

# 源码确认的因果链

## `overlay_epoch` 让候选池复用在正常打字中失效

`DocumentStore::apply_document_changes()` 会复制当前字符串、应用编辑、把文档标记为 `Unsaved`，随后递增 `overlay_epoch` 并清除 live parse/local-word 状态。对应实现见 [`workspace.rs`](../../crates/fossilsense/src/server/workspace.rs#L151)。

普通补全把所有 root 的 `EngineEpoch`、项目选择状态和 `overlay_epoch` 一起传给 `combine_completion_generation()`，见 [`state.rs`](../../crates/fossilsense/src/server/state.rs#L37) 和 [`language_server.rs`](../../crates/fossilsense/src/server/language_server.rs#L619)。`CompletionMemo` 只有在 generation 完全相同、table 数量相同且新 prefix 扩展旧 prefix 时才返回 prior pools。

这里需要修正一个容易误解的说法：`didChange` **没有直接删除** `CompletionMemo`；它是通过改变 generation，让旧 memo 在下一次查询中不可用。这种区别不影响性能结果，但会影响改造方式——需要拆分 identity，而不是只改一个 `clear()` 调用。

`CandidateOverlayCacheKey` 同样包含 `(root, semantic_generation, overlay_epoch)`，调用位置见 [`candidate_context.rs`](../../crates/fossilsense/src/server/candidate_context.rs#L61)。因此只要存在 active dirty overlay，连续输入通常会同时导致：

- 上一 prefix 的 `CompletionMemo` 无法 narrowing；
- 上一请求的 `CandidateOverlaySnapshot` 无法复用；
- 每个 root 重新准备 dirty 文档、解析 overlay、刷新 reach graph 和派生 effective table。

这说明 `overlay_epoch` 同时承担了两个不同职责：

- **请求新鲜度**：本次结果必须对应准确的打开文档版本。
- **候选宇宙身份**：稳定 base index、shadow path 集合、overlay declarations 和 reach evidence 是否真的改变。

前者每个按键都应该变化，后者不应该因为函数体里多输入一个普通字符就全部变化。

## 现有 completion hot benchmark 测到的不是生产算法

`NameTable::search_ranked()` 在 unscoped exact/prefix candidates 已经填满 limit 时会使用 sorted prefix index 跳过全表扫描，见 [`name_search.rs`](../../crates/fossilsense/src/query/name_search.rs#L183)。现有 [`benchmark_large_declaration_index_completion_hot_path`](../../crates/fossilsense/src/declaration_index.rs#L346) 正是通过这个入口测 p50/p95，并断言零 SQLite payload reads。

生产 ordinary completion 则由 [`ordinary_service.rs`](../../crates/fossilsense/src/completion/ordinary_service.rs#L430) 调用 `search_completion_recall_pooled_with_project_for_family()`。这个路径为了同时产生 global、reachable、external、unknown 和 project channels，会进入 `scored_pool_for_query()`；当 prior pool 缺失时，它遍历 `active_indices()` 并逐项执行名称匹配与 scope evidence 计算，见 [`name_search.rs`](../../crates/fossilsense/src/query/name_search.rs#L528)。

因此当前两个结论可以同时成立：

- hot microbenchmark 很快，而且没有 SQLite IO；
- 真实连续输入仍然在每个 prefix 扫描几十万甚至更多 active entries。

这不是基准造假，而是**基准与生产调用链不等价**。现有基准只能证明 compact recall core 的某个 fast path 健康，不能证明普通补全端到端健康。

## 请求级 overlay 仍在复制 `O(F+E)` 状态

`CandidateOverlaySnapshot::refresh_reach_graph()` 会先把 indexed workspace paths materialize 成 `HashSet<String>`，再建立 basename lookup。它还会在 include root 中调用 `Path::is_file()` 验证候选路径，见 [`candidate_service.rs`](../../crates/fossilsense/src/candidate_service.rs#L465)。在 Windows、杀毒软件扫描目录或网络映射盘环境中，请求热路径上的 metadata IO 可能产生明显尾延迟。

dirty source edges 计算完后，`ReachGraph::with_refreshed_sources_with_kinds()` 会复制：

- C/C++ edges 与 open maps；
- Go package/file maps；
- package edges 与 open package maps。

对应实现见 [`reachability.rs`](../../crates/fossilsense/src/reachability.rs#L345)。Go overlay 的后续刷新还会再构造一代完整 maps。

`NameTable` 本身已经是 immutable base + delta segments，这一方向是正确的。但请求级 `with_updated_entries()` 仍会复制 delta list、offset list、`path_overrides`，并克隆 `all_workspace_reach` 的文件集合，见 [`name_updates.rs`](../../crates/fossilsense/src/query/name_updates.rs#L175)。这意味着 segmented payload 避免了复制所有 declaration，却没有完全避免复制伴随集合。

因此当前 overlay 成本不是一个单独的“大对象 clone”，而是多个中等规模结构在每个 root、每个 epoch 上重复 materialize。多 root 会进一步放大这个过程。

## 当前文档工作会阻塞交互路径

当前增量同步虽然是 LSP incremental text sync，但服务端 `apply_document_changes()` 先执行 `document.text.to_string()`，然后每个 range edit 都从字符串开头扫描到目标行并换算 UTF-16 位置，见 [`workspace.rs`](../../crates/fossilsense/src/server/workspace.rs#L151) 和 [`workspace.rs`](../../crates/fossilsense/src/server/workspace.rs#L586)。这意味着协议是增量的，内部文本存储仍然近似全量。

普通补全还会：

- 用 `text.lines().nth(line)` 从开头找到当前行，见 [`language_server.rs`](../../crates/fossilsense/src/server/language_server.rs#L390)；
- 在 local-word cache miss 时同步执行 `completion_words::extract_words(text)`，见 [`workspace.rs`](../../crates/fossilsense/src/server/workspace.rs#L344)；
- 对当前版本请求 `ParseFacts::COMPLETION`，overlay 随后可能请求 `HOVER_SEMANTICS`；
- 如果已缓存 facts 不是新请求的超集，则把 masks 做 union 后重新从头 parse，见 [`server.rs`](../../crates/fossilsense/src/server.rs#L330)。

parser 层的 `ParserHandle` 已经支持传入 `old_tree`，但 server live-document 路径最终仍把 `None` 作为 old tree，见 [`parser.rs`](../../crates/fossilsense/src/parser.rs#L556) 与 [`parser.rs`](../../crates/fossilsense/src/parser.rs#L660)。因此现有能力没有转化成编辑器级增量解析。

此外，semantic tokens 会把当前文档中的 wanted names 交给 `NameTable::colorable_kind_counts_for_family()`，后者仍遍历全部 active indices 再过滤 wanted set，见 [`semantic_tokens.rs`](../../crates/fossilsense/src/server/semantic_tokens.rs#L108) 与 [`name_search.rs`](../../crates/fossilsense/src/query/name_search.rs#L116)。这解释了为什么关闭 semantic tokens 可能显著改变补全尾延迟：它不是纯 UI 开关，而是减少了另一个全表 CPU consumer。

## cancellation 只覆盖了部分工作

live parse 有 `(uri, version)` 级 `AtomicBool`，新版本到来后会标记旧 parser 取消，且 parser input callback 会检查该标志。这个设计应当保留。

问题在于，ordinary completion 的 ranking closure、overlay graph/table 构建和许多其它 CPU closure 只是被放进 `spawn_blocking`。Tokio 官方文档明确说明：已经开始运行的 `spawn_blocking` task 不能通过 `abort()` 中止；对于大量 CPU-bound work，应该用 semaphore 限制并发，或者使用专用 CPU executor。见与当前锁文件一致的 [Tokio 1.52.3 `spawn_blocking` 文档](https://docs.rs/tokio/1.52.3/tokio/task/fn.spawn_blocking.html)。

当前主程序使用默认 `#[tokio::main]`，没有为交互 CPU work 定义独立 blocking budget；后台 indexer 内部又创建 Rayon pool。其结果可能是：

`Tokio blocking worker × Rayon parser workers × semantic tokens/completion` 同时活跃。

不一定每次都会真正 oversubscribe，但当前架构没有显式机制保证不会发生。相对占比必须通过 runtime trace 验证。

# 与成熟实现的对照

## clangd：动态覆盖层与可扩展 fuzzy index 是两个独立层

clangd 的 `SymbolIndex` 可以由多个实现通过 `MergedIndex` 叠加。`FileIndex` 存放打开文件和相关 header 的动态事实，`BackgroundIndex`/static index 提供工作区事实；消费者只看到合并后的 view。见 [clangd index 设计](https://clangd.llvm.org/design/indexing)。

这与当前系统的 immutable `EngineSnapshot + dirty overlay` 目标很接近，但 clangd 的关键差别是：打开文件覆盖层按文件维护，并不是在每个请求中复制完整 background index。

clangd 的 Dex 还提供可扩展 fuzzy search。它为名称生成 trigram posting lists，查询时先对 query trigrams 做 posting intersection，再加入 scope、proximity 和 type boosting iterator，并在完整 fuzzy score 之前设置候选上限。源码见固定提交 `c325d6f` 的 [Dex fuzzyFind](https://github.com/llvm/llvm-project/blob/c325d6fcb6db298f681ae1c450b89b4c255fb3ce/clang-tools-extra/clangd/index/dex/Dex.cpp) 和 [trigram 生成规则](https://github.com/llvm/llvm-project/blob/c325d6fcb6db298f681ae1c450b89b4c255fb3ce/clang-tools-extra/clangd/index/dex/Trigram.h)。

这里值得借鉴的不是具体权重，而是两阶段结构：

`倒排索引产生有限候选 → 完整 matcher 与动态 evidence 精排`

当前 `NameTable` 已经有 sorted prefix index，但生产 pooled recall 绕过了它并扫描全表。第一步应当是把现有 prefix index 变成生产 candidate generator，而不是直接照搬一整套 Dex。

## rust-analyzer：输入 revision 与派生事实的失效域分开

rust-analyzer 使用 salsa 做按需增量计算，并明确维护一个架构不变量：在函数体内打字不应该让其它函数的全局派生事实失效。它还在新 revision 到来时取消过时的高亮等计算。见 [rust-analyzer architecture](https://rust-analyzer.github.io/book/contributing/architecture.html)。

FossilSense 不需要引入 salsa，也不应在现有 parser/store/query 之间再建立一套平行语义模型。但可以借鉴它的失效思想：

- 文本版本变化不等于 declaration projection 变化；
- declaration projection 变化不等于 include/import edges 变化；
- project selection 变化不等于 base symbol universe 变化；
- 请求结果必须是精确版本，稳定中间结果则可以跨版本复用。

## gopls：file identity、snapshot 和 memoized computation 分层

gopls 把文件的 URI/content hash identity、特定 snapshot 的 version/content handle，以及 workspace `Snapshot` 分开维护；snapshot 同时负责 derived state 的 bookkeeping 与 invalidation。见 [gopls implementation](https://go.dev/gopls/design/implementation) 和 [gopls cache package](https://pkg.go.dev/golang.org/x/tools/gopls/internal/cache)。

这支持一个重要判断：缓存 key 不应该只选择“最安全但最粗”的全局 epoch。更好的 key 是把每层真正依赖的输入放进去，并让 snapshot 本身提供一致性边界。

## Roslyn 与 Tree-sitter：不可变不等于全量复制

Roslyn 的 syntax tree 和 solution 都是 immutable snapshot，但新版本会复用底层未变化节点；不可变性用于线程安全和一致性，结构共享用于控制时间与内存。见 [Roslyn syntax model](https://learn.microsoft.com/en-us/dotnet/csharp/roslyn-sdk/work-with-syntax) 和 [workspace model](https://learn.microsoft.com/en-us/dotnet/csharp/roslyn-sdk/work-with-workspace)。

Tree-sitter 本身也定位为增量解析库，可以用旧 tree 和 edit 高效更新；当前工程使用 Rust crate `tree-sitter 0.25.10`，对应接口见 [Parser API](https://docs.rs/tree-sitter/0.25.10/tree_sitter/struct.Parser.html) 与 [Tree API](https://docs.rs/tree-sitter/0.25.10/tree_sitter/struct.Tree.html)。

因此应该保留 `Arc` immutable snapshot，但把 request-local graph、path view 和 syntax tree 改成**base + small delta**，而不是把“不可变”实现成“每次完整 clone”。

## LSP：全字符参与匹配是产品约束

LSP completion 规范允许客户端在 identifier 输入期间通过 quick suggestions 请求补全，而不要求把所有 identifier 字符都列入 `triggerCharacters`；完整列表也可以由客户端继续过滤，`isIncomplete=true` 则表示继续输入时需要重新计算整张列表。见 [LSP completion specification](https://raw.githubusercontent.com/microsoft/language-server-protocol/gh-pages/_specifications/lsp/3.18/language/completion.md)。

但本系统的产品约束比协议最低要求更强：**当前补全上下文中的每个输入字符都必须进入当前 prefix，并参与候选匹配与结果更新。**因此不能通过移除字母、数字、下划线、Unicode identifier 字符或其它语境字符来规避服务端性能问题。当前全部 ASCII 字母和 `_` 的触发覆盖应保留，见 [`options.rs`](../../crates/fossilsense/src/server/options.rs#L162)；其它由客户端以 quick suggestions/`Invoked` 形式产生的字符输入，也必须保证进入最新请求。

这里允许取消已经过期的中间请求，但不允许丢失字符语义。例如连续输入 `abcdef` 时，前五个请求可以被 supersede，最终请求必须使用完整的 `abcdef` 做匹配，而不能只使用 head、采样字符或旧 prefix。性能优化必须发生在候选生成、缓存复用、结构共享和调度层，不能以降低参与匹配的字符覆盖为代价。

# 目标架构

## 把“请求新鲜度”和“候选宇宙”拆成不同身份

建议保留现有 `EngineEpoch`、`semantic_generation` 和 `document_version`，新增逻辑身份，不要复用一个新的万能 epoch。

| 身份 | 包含内容 | 变化条件 | 允许复用的结果 |
|---|---|---|---|
| `RecallUniverseId` | 所有 root 的 `EngineEpoch`、语言家族、项目选择/配置中真正影响召回的部分、recall index layout version | 内存语义快照、项目选择或召回配置变化 | base candidate postings、prefix/fuzzy pool |
| `ShadowRevision` | root-scoped `ShadowKey → shadow/tombstone`；`ShadowKey = (RootId, PersistedPathIdentity, SemanticFamily)` | 文档首次变 dirty、save/reconcile、close、持久化身份别名或语言家族变化 | durable candidate 的 suppression mask |
| `CompletionProjectionRevision` | 每个 dirty identity 的补全投影 fingerprint：名称、kind、role、range、语言/Go package 身份、include/import 出边、fallback 与 facts availability | 任何影响补全列表或 reach evidence 的投影变化 | **仅限补全列表**的 overlay segment 与 edge delta |
| `ExactOverlayRevision` | root、持久化 identity、语言家族、exact `document_version`/content hash、facts mask/availability | 任一输入变化 | 完整 `CandidateOverlaySnapshot`：source text、callable anchors、calls、records/members、aliases、resolve/hover 所需事实 |
| `DocumentRevision` | URI、`document_version`、cursor/context、当前 prefix | 每个编辑或光标语境变化 | 最终请求校验、local binding、local words、resolve data |

所谓“两级 generation”在实现上至少应体现两类边界：

1. **稳定候选层**：`RecallUniverseId`，不包含每键变化的 `overlay_epoch`。
2. **请求新鲜层**：`DocumentRevision + ShadowRevision + CompletionProjectionRevision + ExactOverlayRevision`。其中补全投影可以在证明结构未变时共享底层 segment；完整 overlay 和 resolve 必须保持 exact version。

`overlay_epoch` 可以继续作为捕获所有打开文档的一致性序号，但不再直接决定 base recall memo 是否可用。

这里必须区分“补全可见投影未变”和“完整 overlay 未变”。注释或函数体调用点变化也许不改变声明名称，却会改变 source text、call sites 或 resolve 范围；因此旧 `CandidateOverlaySnapshot` 不能仅凭 declaration fingerprint 跨版本复用。安全做法是把 completion-only projection 从完整 overlay 中拆出，并让 exact overlay 继续以当前文档版本为硬边界。

### 跨 `document_version` 复用如何保证 shadow 正确

安全顺序应当是：

1. 从 immutable base recall index 产生 durable IDs。
2. 对每个 root 先展开该 URI 在授权 workspace/external roots 下的全部持久化 identity alias，并立即建立 root-scoped `ShadowKey` tombstone；解析数量上限只能限制解析工作，不能限制 suppression。
3. 使用当前 `ShadowRevision` 的 mask 移除所有对应 dirty identities 的 durable IDs；相同相对路径位于不同 root、或属于不同 semantic family 时不得互相遮蔽。
4. 只合并由当前 captured document version 生成的 completion projection。若增量提取证明新旧 projection fingerprint 相同，可以结构共享旧 segment，但该证明不授权复用旧 source text、calls、records/members 或 resolve payload。
5. 如果某个 dirty identity 尚未完成解析、解析失败或被取消，保留 tombstone，只做 suppression，不加入旧 overlay facts。
6. 最后合并当前版本 local bindings、local words 和 lexical fallback，并按当前 reach/project/context 重排；完整 overlay 的使用和 `completionItem/resolve` 仍做 exact version/generation 校验。

这样，即使 base prefix pool 来自上一按键，它也不会让旧持久化声明重新泄漏。**tombstone 是复用安全的前提，而不是缓存 miss 时的补救措施。**

`CompletionMemo` 还应把 immutable base pool、root-scoped shadow mask 和 overlay pool 分开保存。跨版本复用的是 base pool，不是已经混入旧 overlay 的最终候选数组；否则即使 suppression 正确，旧的成员、别名或 fallback 项仍可能残留。

### prior pool 不能无条件缩窄

当前 matcher 的子序列匹配满足“新 prefix 的匹配集合是旧 prefix 的子集”，因此完整 prior pool 可以安全 narrowing。问题是未来 candidate generator 会有 hard budget；一个已经截断的 pool 不一定包含更长 prefix 的最佳候选。

因此 memo 需要保存：

- `pool_complete` 或 truncation certificate；
- 各 channel 的扫描/返回/截断信息；
- 产生 pool 的 normalized matcher contract version。

只有 matcher 单调、pool 完整且 `RecallUniverseId` 相同时才直接 narrowing。否则对新 prefix 重新执行一次**有界 index query**，而不是回退全表扫描。重新查倒排索引应该很便宜，所以 memo 是优化，不应成为正确性的唯一来源。

## `NameTable` 从评分表变成有界 candidate generator

当前 `NameTable` 的 compact SoA/segmented 方向应当保留，canonical payload hydration 仍然通过 `CandidateQueryService`。建议在同一个 semantic index 内增加 `CandidateRecallIndex`，它仍然只是性能索引，不是第二套语义模型。

这里增加一个不可退让的不变量：**倒排索引可以只用一部分高区分度 token 生成候选，但最终 matcher 必须对当前 normalized prefix 的全部字符执行校验和评分。** fuzzy match 可以允许候选名称中存在间隔，却不能跳过任意 query 字符；任何没有消费完整 query 的候选都不是有效匹配。达到候选预算时可以显式返回 `truncated/coverage`，不能通过忽略尾字符或只匹配 head/trigram 来伪造完整结果。

### 第一阶段：复用现有 sorted order，但新增真正有界的迭代接口

不能直接把当前 `prefix_candidates()` 接入生产 pooled recall。它虽然用二分查找定位 prefix range，但随后仍会遍历该 range、materialize `Vec` 并对全部结果排序；当几十万名称都以 `a` 开头时，单字符查询仍接近 `O(D)`。

第一阶段应新增 `PrefixRangeCursor`/`CandidateBudget` 一类接口：

- 对 immutable base 和每个 active delta 分别二分定位 prefix range，但以 cursor 惰性读取，不构造完整匹配数组；
- 用小顶堆做 segment 间有界 k-way merge，并显式记录 `segments_opened`、`postings_touched`、`entries_inspected`；
- current/reachable/same-project/global 等 channel 若已有 compact posting/bitmap，就先做 posting 交集；没有可用 posting 的 channel 只能消费统一 scan budget，并设置 `truncated/coverage`，不能暗中扫完整 range；
- 每个 channel 只产生受预算约束的候选，再做 union 和完整 `score_match + scope_tier + project/history`；
- scope、project 和 reachability 继续作为 ranking evidence，不能因为使用 bitmap/posting 就变成 hard filter；global channel 必须始终有配额；
- 多 root 使用一个请求级总预算，优先 current root，再消费其它 root，不能让 `R × per-root cap` 无界增长。

在每段都能直接产生所需 channel posting、共有 `S` 个 active segments 且总检查预算为 `B` 时，请求工作量可以约束到：

`O(R × S × log D + B × log S + B_union × log K)`

其中 `K` 是最终返回上限。这个式子描述的是**有界近似召回成本**，不是自动获得完整 top-k 的证明。动态 scope/project/history 分数与词典顺序并不单调；如果没有 block score upper bound 或完整 posting 穷尽，达到 `B` 后必须返回 `truncated=true`。只有 cursor/postings 确实耗尽时，才能声明该 channel complete。

### 第二阶段：增加 Dex 风格 fuzzy postings

当基准证明 fuzzy fallback 仍是主要成本后，再为 normalized name 建立：

- 1/2 字符 head tokens，用于短 prefix；
- 连续 trigram；
- camelCase/underscore segment head trigram；
- exact name hash；
- 可选的 static kind/role/project/file postings。

查询时先选择区分度最高的 posting 做交集或有界 union，得到候选后才执行现有 fuzzy matcher 和动态 evidence。对于 1 字符/2 字符 query，posting 仍可能很长，因此需要预计算的 per-channel static-quality heads 或严格 scan budget，不能假装倒排索引自动消除了短词大集合。

posting 内部可以使用排序 `u32` IDs 和压缩块；是否引入 bitmap 库必须单独核对许可证、VSIX 体积、build/runtime 影响和双代内存峰值。第一版可以不新增依赖。

### 内存预算必须把 recall index 算入不可回收 core

`CandidateRecallIndex` 不是预算外的免费结构。现有 declaration semantic index 的本地预算应明确改为：

`accounted_core_bytes = NameTable recall core + CandidateRecallIndex dictionaries/postings/block heads`

`payload_cache_budget = semanticIndex.memoryBudgetMB - accounted_core_bytes`，不足时按零处理。预算为 `0` 或小于 core 时仍要保留实现正确召回所必需的最小 core，但必须报告 core overflow/degradation；可选 trigram、block-max heads 或静态 channel postings 应分层启用，不能挤占 canonical payload 账本后仍宣称符合预算。

内存测试至少覆盖 `memoryBudgetMB=0`、小于 core、正常预算、delta 累积/compaction 和双代发布。`PathIndexView`、request overlay、任务队列、旧请求持有的 `Arc` 以及 runtime/reach graph 仍不属于这个局部 declaration-index 预算，因此还必须在真实 LSP 的 dirty-overlay 与双快照场景测进程 Private Bytes。现有 hydration 门禁也要继续覆盖 completion recall、resolve、payload cache 和 publication 全链路，而不是只验证新 postings 能装入内存。

### 排名与 top-k pruning

信息检索中的 WAND/Block-Max WAND 会给 posting block 保存最大可能贡献，从而安全跳过不可能进入 top-k 的 block。[Block-Max WAND 论文](https://research.engineering.nyu.edu/~suel/papers/bmw.pdf)说明了这种“上界 + 动态阈值”结构。

它可以作为后续优化，但不是第一步，因为当前 ranking 同时包含 tier、fuzzy quality、locality、project/history 等非平凡证据。只有在能够为每个 block 给出**保守总分上界**时，才能宣称 pruning 不改变 top-k；否则应明确采用“有界一阶段召回 + 精确二阶段重排”，并通过 `coverage/truncation` 暴露近似，而不能把近似说成精确搜索。

## `ReachGraph` 和 path lookup 改成持久化 view

建议把 request-local effective graph 表示为：

`EffectiveReachGraphView { base: Arc<ReachGraph>, edge_overrides: Arc<OverlayEdgeMap>, open_overrides, go_package_overrides }`

其中 `OverlayEdgeMap` 只存 dirty source 的完整替换出边或 tombstone。读取某个 source 时先查 overlay，未命中再读 base。请求不再复制 base maps。

reachability memo key 变成：

`(base EngineEpoch, OverlayGraphRevision, source FileId)`

dirty edge 改变时只让依赖该 source 的 memo 失效。第一版可以保守地清除这个 overlay revision 的 memo，不需要一开始就做复杂的反向依赖失效；关键是避免 clone `E`。

path lookup 则应作为 `EngineSnapshot` 的正式组成部分：

- normalized path → compact `FileId`；
- basename → small `FileId[]`；
- file language/external root metadata；
- dirty/unindexed path 的小 delta。

workspace 内已索引文件的 include resolution 使用 `PathIndexView { base, delta }`，不再每请求扫描 indexed file list，也不在热路径调用 `Path::is_file()`。但这不能被表述成“预计算全部 external roots”：当前受限 external root 可能进入 over-cap/path-only 模式，而且 watcher 只覆盖 workspace root，外部文件可以在没有事件通知的情况下创建或删除。

对这类授权 external path，应增加独立的**有界 existence cache/probe queue**：key 至少包含 external-root revision 与 normalized candidate path，值包含 positive/negative/unknown、TTL 和观察时间；cache 有最大条目数，单请求只有有限 enqueue 配额，后台 probe 有并发上限和单次超时。请求 miss 时不得同步等待网络盘或慢磁盘，也不得把 unknown 伪装成不存在；应把 reach 状态标为 `open/unresolved/incomplete`，在授权根内排队探测，并让后续请求或脏发布吸收结果。root 配置变化、TTL 到期和已知 create/delete 事件负责失效，不能假设外部 watcher 永远存在。

`NameTable` 的请求级表示同样改成 view：

`EffectiveNameTableView { base: Arc<NameTable>, shadow_mask, overlay_segment, direct_include_overrides }`

它不再调用 `with_updated_entries()` 构造一张新 table。`active_indices()`/posting iterator 在迭代时检查 shadow mask，并额外遍历小 overlay segment。

## parse tree 与 facts extraction 分层

建议把 live parse cache 从：

`(version, language, facts_mask, FileSemanticIndex)`

拆成：

- `DocumentTextSnapshot`：支持高效增量 edit 与 line/UTF-16 mapping；
- `SyntaxSnapshot(version, language, tree)`：只负责 tree-sitter tree 与 parser errors；
- `FactCache(version, fact_group → extracted facts)`：按 declarations、locals、includes/imports、members、calls、color refs 等组惰性提取；
- `OverlayProjection`：从 facts 生成 declaration/edge/fallback fingerprints。

一次编辑到来时：

1. 文本结构应用 range edit，并给出 byte/point edit。
2. 对旧 tree 执行 `Tree::edit`，用 old tree 增量 parse 新版本。
3. completion、overlay 和 semantic tokens 共享同一 `SyntaxSnapshot`。
4. 新 feature 需要更多 facts 时，只运行缺失 extractor；不重新 parse union mask。
5. facts extractor 仍需支持 cancellation，并对语法错误返回显式 availability/fallback。

local words 可以维护按行或按 chunk 的 token multiset，只重扫受编辑影响的行和可能跨行的边界。文本结构不一定马上引入第三方 rope；可以先实现 line-start index + chunked text，再用大文件 edit benchmark决定是否需要 piece table/rope 依赖。

需要注意：增量 tree 不是可信语义证明。它只是减少重复语法工作；现有 fallback、ambiguity 和 candidate contracts 仍然适用。

## foreground QoS 与 latest-request-wins

建议把 Tokio runtime、foreground CPU 和 background CPU 的职责分开：

- Tokio async workers 只做 JSON-RPC、短锁、状态机和 IO，不执行全文扫描或长循环。
- foreground completion/hover/navigation/semantic tokens 使用一个专用、固定大小的 CPU pool。
- background full/dirty index 和模型重建使用另一个低优先级 pool，并有动态并发预算。
- SQLite writer 保持单独的有界 pipeline，不与 foreground pool 相互嵌套。

每个 `(URI, feature-family)` 维护单调 `RequestEpoch`。一个 blocking job 同时持有：

- LSP cancellation token；
- exact `document_version`；
- latest request epoch；
- captured `EngineSnapshot` identity。

以下位置必须周期性检查 token：

- root 循环；
- overlay 文档循环；
- posting/active-entry 扫描；
- merge/dedup/rank；
- semantic-token wanted-name 处理；
- 大块序列化前。

检查不必每个 entry 都做，可以每固定 block 做一次。队列中尚未开始的旧任务直接丢弃；已经开始的任务协作返回 `Cancelled/Stale`。结果发布仍然执行 exact version/generation 校验。

资源预算可以采用“保留前台容量 + 后台 credit”方式：

- foreground 永远保留至少一个 CPU slot；
- 用户连续输入期间暂停领取新的 background parse batch；
- 短暂 quiet window 后逐步恢复 background credit；
- background 保留最小进度，避免永久饥饿；
- 禁止在 blocking pool worker 内再无界 fan-out Rayon tasks。

不要只依赖 Windows thread priority。OS priority 可以作为补充，但应用层 admission control、并发预算和 cooperative cancellation 更可预测，也更容易跨平台测试。

# LSP 与客户端策略

## 保持全字符匹配，合并的是旧工作而不是输入语义

客户端策略不再以减少 trigger coverage 为优化方向。应保留现有字母、`_` 和语境字符触发，并验证数字、Unicode identifier 输入以及 quick suggestions/`Invoked` 路径同样使用完整当前 prefix。每次 `didChange` 都要推进 exact `DocumentRevision`，最新 completion 必须看到该版本的全部输入字符。

可以被合并的是尚未开始或已经过期的**请求工作**：

- 新版本到达后，队列中的旧 completion 直接 supersede；正在运行的旧任务协作取消。
- 如果多个 `didChange` 在调度窗口内连续到达，可以只执行最新版本，但最新版本必须用完整 prefix 重新查询，不能漏掉中间输入形成的字符。
- `CompletionMemo` 只能复用完整旧 pool 并用完整新 prefix narrowing；不能按固定长度采样，也不能只用 head/trigram 直接产出最终结果。
- `.`, `->`, include/import 和手动 `Ctrl+Space` 仍保持即时高优先级；普通 identifier 同样保证全字符语义，只是在旧请求执行权上服从 latest-request-wins。

因此短 debounce 只能作为队列合并手段，并且必须证明不会使最新字符缺席匹配或造成不可接受的首结果延迟。它不能成为“少匹配几个字符”的降载机制；服务端 bounded recall 和 foreground QoS 仍然是根本方案。

## 让 `isIncomplete` 表达真实状态

`isIncomplete` 不应永远是 `true`。建议按现有 evidence 决定：

- candidate/time/channel budget 被耗尽：`true`；
- overlay parse 被取消、facts unavailable、reach open、某 root 未处理：`true`；
- short-prefix specialized index 不能证明完整：`true`；
- 所有相关 postings 已耗尽、overlay exact、没有 fallback/open 状态且未截断：`false`。

当为 `false` 时，客户端可以继续用**完整当前 prefix**过滤已收到列表，避免下一键重新请求；这种本地过滤同样必须让全部输入字符参与匹配。当为 `true` 时，retrigger 是正确行为，但新的 bounded server path 必须能承受它。

完整性不能只看 `items.len() < limit`。如果上游 candidate generator 因 budget 截断，即使最后去重后不足 limit，仍然是不完整。

# 优先实施的三个改动

下面顺序是**推荐实施顺序**，不是理论收益排序。它优先降低风险并建立可验证性。

| 优先级 | 改动 | 直接解决的问题 | 为什么先做 | 回滚边界 |
|---:|---|---|---|---|
| 1 | latest-request-wins、foreground/background CPU admission control、完整 E2E timing | 旧 blocking work 与后台任务挤占最新请求，无法判断真正瓶颈 | 不改变候选语义，能立即降低尾部浪费，并让后续优化可测 | feature flag 关闭专用调度器，回到现有 `spawn_blocking` |
| 2 | `RecallUniverseId`/request revision 分层 + persistent overlay graph/path/NameTable view | 每键 cache miss 与 `O(F+E)` materialization | identity 与数据结构必须一起改，否则只是把大 clone 缓存得更久 | 双路构建 old/new overlay，比较结果后切换 |
| 3 | 生产 `CandidateRecallIndex`：先 prefix channels，后 trigram fuzzy | 每个 root 冷态 `O(D)` 召回 | 这是最重要的渐近优化，但需要 differential oracle、内存门禁和 ranking coverage 证据 | 保留 full-scan oracle/legacy path，只在 budget/index 正常时启用 v2 |

parse tree/facts 分层应紧随这三项。若第一阶段 trace 证明大文件 `didChange/local_words/parse` 占主要 p99，可以把它提前与优先级 2 并行设计，但不要在没有数据时同时重写文本存储、parser facts、overlay 和 recall index。

# 验证方案

## 现有基准缺口

当前 `scripts/benchmark_large_workspace.ps1` 能覆盖 full index、engine hydration、部分 semantic query 和发布并发；[`large-workspace-runbook.md`](../benchmark/large-workspace-runbook.md)也定义了 full-index 120 秒和单代/双代内存门禁。

缺失的是连续编辑下的两层互补观测。第一层通过 stdio 驱动真实 LSP 服务端：

`initialize → didOpen → didChange → completion → cancellation → completionItem/resolve（current 与 stale）→ semanticTokens/full 或 range → response/write`

第二层必须在真实 VS Code Extension Host/renderer 中回放输入，记录 key event、quick suggestions、suggest widget update 和字符 paint。raw stdio harness 看不到 renderer/UI churn，不能单独回答“字符为什么晚显示”。现有 completion hot test 只有 p50/p95，semantic tokens 只有单次 perf log，也没有 queue wait、serialization/write、p99 或 background full/dirty indexing overlap。

## 新增 server replay 与 VS Code integration trace

建议先新增确定性的 server replay harness：启动 release `fossilsense lsp` 子进程，通过 stdio 发送 JSON-RPC 序列，并覆盖 cancel race、当前版本 resolve 与 stale resolve 拒绝。它负责拆出 transport、server queue、parse/overlay/recall/rank 和 response write，不声称测到 UI。

再增加 VS Code integration/trace case，使用真实扩展启动方式和 quick-suggestion 配置回放相同输入节奏；以共享 correlation id 或可对齐的 monotonic timestamp，把 Extension Host、LSP 进程与 renderer 事件关联起来。该层负责 `key-to-paint`、`key-to-widget`、client cancellation/retrigger 数量和 UI churn。

两层 raw result 都写入 ignored 的：

- `target/benchmark/e2e-lsp-<timestamp>.json`
- `target/benchmark/e2e-lsp-<timestamp>.md`

长期提交到 `docs/benchmark/` 的只应是可复现方法和经过审查的匿名聚合结果；本文只描述方案，不伪造测量数字。

每个 request 使用统一 correlation id，至少记录：

| 层 | 时间点/计数 |
|---|---|
| VS Code integration | key event、quick suggestion fire、cancel send、response receive、suggest widget update、下一次字符 paint |
| stdio/server replay | JSON encode start/end、stdio write/read、server receive、response write、current/stale resolve outcome |
| Document | didChange apply、bytes/lines copied、UTF-16 mapping、document version |
| Parse | gate wait、syntax incremental/full、facts extract、cancel observed、cache hit/miss |
| Overlay | cache identity、dirty docs、path-index build、filesystem metadata calls、edge overrides、graph clone entries/bytes、table override entries/bytes |
| Recall | universe id、memo hit kind、postings touched、active entries scanned、candidate budget、per-channel generated/returned/truncated |
| Rank | merge、dedup、full score、render、serialization item/byte count |
| Scheduling | async queue wait、CPU pool queue wait、active foreground/background jobs、stale jobs、stale CPU ms、cancellation-to-finish |
| Publication | captured/published engine epoch、semantic generation、shadow/projection/exact-overlay revision、stale/mismatch detected、stale rejected、mixed generation exposed |

需要同时记录 process CPU、Private Bytes、Working Set、thread count、context switches，以及 extension host/renderer CPU。否则无法判断字符回显迟滞属于 server CPU、transport、extension host 还是 UI。

## 基准矩阵

核心矩阵如下，不要求一次笛卡尔积全部跑完，可以用 pairwise case 覆盖：

| 维度 | Case |
|---|---|
| Workspace roots | 1、2 |
| Declaration scale | 小样本、每 root 合成数十万级 |
| Files/edges | 小图、大图；保持 declaration 不变单独放大 `F/E` |
| Dirty docs | 0、1、少量多个 |
| Current document | 小文件、数万行大文件 |
| Edit shape | 函数体普通字符、声明名变化、include/import 变化、Go package/build guard 变化 |
| Prefix | 1/2 字符、长 exact prefix、camelCase fuzzy、无匹配 |
| Background | idle、dirty index、full index、NameTable compaction/publication |
| Semantic tokens | on、off；当前已实现的 full、range |
| Autosave | on、off |
| Client policy | 全字符触发/quick suggestions 基线、latest-only 队列合并、条件 `isIncomplete`；所有 case 都使用完整当前 prefix |
| Cancellation | 不取消、每键取消旧请求、突发输入后只等待最后请求 |
| Language | C、C++、Go；同名跨 family 隔离 case |

性能结果按“最新版本请求”统计 p50/p95/p99/max，同时单独报告 stale work。semantic token delta 只有在未来真正实现并声明 capability 后才进入矩阵，不能把尚不存在的路径混入当前基线。平均值不能掩盖后台竞争时的 p99。[The Tail at Scale](https://research.google/pubs/the-tail-at-scale/)说明了交互系统为什么必须关注尾延迟而不是只看均值。

## 建议的非产品承诺型门禁

在拿到第一轮真实基线前，不应把任意毫秒数字写成客户 SLO。可以先建立以下结构性门禁：

- declaration 数增加约一个数量级时，bounded recall 的 `active_entries_scanned/postings_touched` 不随 `D` 线性增加；达到预算必须设置 truncation。
- files/edges 增加约一个数量级、dirty source 数不变时，request overlay 的复制条目数保持 `O(Δ)`；base graph/path clone entries 必须为 0。
- 开启 background indexing 后，foreground p99 的退化控制在团队确认的相对比例内，且 latest request 不在旧 completion 后排队。
- 客户端取消后，stale job 在有限检查 block 内停止；报告 `stale_cpu_ms / foreground_cpu_ms`。
- ordinary list recall 保持零 SQLite payload reads。
- 将一致性计数拆成 `stale_or_mismatch_detected`、`stale_rejected` 与 `mixed_generation_exposed`。故意制造的 race 应出现 detected/rejected，唯一必须为 0 的是 `mixed_generation_exposed`；stale result 只能拒绝或显式降级。
- 新 recall index 的单代与双代峰值继续通过现有 U-Boot hydration 384 MiB/512 MiB 门禁；该 case 必须实际经过 completion recall、resolve、payload hydration/cache 和 publication，而不是只构造 postings 后读取进程内存。
- 任何 full-index case 仍必须满足现有 120,000 ms 硬门禁。

在结构性门禁稳定后，再依据真实机器分布设定 completion server p95/p99 和 key-to-widget 的产品 SLO。

# TDD 与正确性验证

这类改造不能先写 fast path 再补测试。建议在每阶段编码前先建立会失败的测试：

## identity 与 overlay

- 函数体内普通字符编辑不改变 `RecallUniverseId`，但 exact `DocumentRevision` 必须变化。
- 文档首次 dirty 立即产生 path tombstone；overlay parse 未完成时 durable declaration 不得泄漏。
- 注释-only 编辑可以保持 `CompletionProjectionRevision`，但 `ExactOverlayRevision`、source text 与 resolve version 必须更新；旧完整 overlay 不得被返回。
- declaration 名称/kind/role/range、include/import edge、语言/Go package 身份、fallback 或 facts availability 变化时，`CompletionProjectionRevision` 必须变化。
- call-only、callable-anchor-only、record/member-only、alias-only 和 source-text-only 编辑分别证明：补全投影可按依赖复用，而完整 overlay 始终 exact-version。
- 相同相对路径位于两个 root、一个 URI 映射多个 external persisted identities、身份数超过当前解析上限，以及 C/C++/Go family 切换时，所有 root-scoped alias 都先 tombstone；解析上限不得让第九个及之后的旧声明泄漏。
- save/reconcile 只有在 content hash 与发布 revision 匹配时解除 shadow。
- 多 root 捕获的 base table/read handle/reach graph 仍来自各自同一 semantic generation。

## recall index

- 用 legacy full scan 作为测试 oracle，对随机名称、camelCase、underscore、Unicode 边界策略和多 channel evidence 做 differential top-k。
- 当 candidate budget 足够且 `truncated=false` 时，v2 与 oracle 结果完全一致。
- 当预算不足时，必须显式 `truncated/coverage`，不能宣称完整。
- 对每个 prefix 逐字符构造反例，证明删除或改变任意一个 query 字符都会影响匹配判定；posting token 只负责召回，最终 matcher 必须消费完整 normalized query。
- scope/project/reach evidence 的加入只改变排序，不删除 global fallback channel。
- overlay shadow/delta、delta compaction、跨 language family 和 canonical declaration ID 均有专门 case。
- 每个 request 的 postings/entries touched 不超过硬上限。
- `memoryBudgetMB=0`、小于 core、正常预算和 compaction 后分别核对 `NameTable + CandidateRecallIndex` core 账本、payload 余量与 degradation；双代时同时核对进程 Private Bytes。

## persistent reach/path view

- 对随机 base graph + dirty source replacements，旧 clone 实现和新 overlay view 的 reachable/open/reason 结果做 property comparison。
- unresolved、ambiguous、tombstone、first-layer external 和 Go package overlay 分别覆盖。
- 请求期间发布新 base snapshot 时，旧 view 仍只能看到 captured base + captured delta。
- request path resolution 不执行 filesystem metadata IO；用注入式 resolver/counter 断言。
- external over-cap/path-only root 覆盖 positive、negative、unknown、TTL、create/delete、无 watcher 和多 alias；cache miss 必须返回 open/incomplete 并有界排队，不能同步探测或判定不存在。
- 慢磁盘/网络路径探测覆盖 timeout、队列饱和、单请求 enqueue cap 与 cache size eviction，前台 completion 不等待 probe 完成。

## cancellation 与调度

- 启动超大候选扫描，在中途发布更新版本；旧 job 必须观察 token 并提前退出，新 job 优先完成。
- queued old job 在开始前被 supersede，不占 foreground CPU slot。
- background worker 持续有最小进度，但 active typing 时不能占满所有 CPU slots。
- cancellation 后不得写入 memo、parse cache 或 completion history 的最新版本位置。

## 增量 parse

- 对随机 UTF-16 range edits，把 incremental tree/facts 与同文本 full parse 做 differential comparison。
- 语法错误、缺失 include、半个 identifier、半个 string/comment 和跨行 edit 必须覆盖。
- completion/overlay/semantic tokens 同一 version 并发请求时只创建一个 syntax tree；额外 facts 只执行缺失 extractor。

# 分阶段交付与回滚

## Phase 0：建立观测与止血

增加完整 request timeline、latest-request token、foreground semaphore/CPU pool 和 stale work counters。先不改变 ranking 与 overlay 数据结构。

验收重点是：能够解释 key-to-response 的每个阶段；旧请求取消后不再持续占用大段 CPU；background overlap 的 p99 可重复。

## Phase 1：identity 与 persistent overlay

实现 `RecallUniverseId`、root-scoped `ShadowRevision`、`CompletionProjectionRevision` 和 exact-version `ExactOverlayRevision`，同时引入 `PathIndexView`、`EffectiveReachGraphView`、`EffectiveNameTableView`。保持旧 clone path 作为 oracle，可在测试或 debug shadow mode 中双算并比较。

验收重点是：连续 body typing 时 base recall memo 可复用；overlay cache 不再因无关字符变化重建；每请求 base graph/path clone entries 为 0；所有 shadow/tombstone tests 保持通过。

## Phase 2：bounded recall v2

先基于现有 sorted order 实现不 materialize 全 prefix range 的 bounded cursor，把它接入 production channels，并给所有缺少 posting 的 channel 和 fuzzy fallback 加硬 scan budget。确认质量、truncation 证据和内存后，再引入 trigram postings。

验收重点是：生产 ordinary completion benchmark 与真实 LSP replay 走同一入口；没有 prior pool 时也不会扫完整 `D`；coverage/truncation 证据完整；U-Boot hydration 与 full-index 门禁不退化。

## Phase 3：syntax/facts 增量化与全字符交互策略

建立 syntax snapshot/fact extractors，增量维护 line map/local words。随后验证全字符触发与 quick suggestions 的覆盖一致性，并 A/B 条件 `isIncomplete`、latest-only 队列合并和可选短调度窗口；所有实验都不得减少参与匹配的字符。

客户端调度策略放在服务端 bounded path 之后，是因为取消旧工作不能证明单次请求已经可扩展；反过来，一个可扩展服务端也不应该继续执行已经被新版本取代的重复请求。

建议每项使用独立 feature flag，例如：

- `completion.foregroundQoS`
- `completion.overlayViewV2`
- `completion.recallIndexV2`
- `completion.incrementalLiveParse`
- `completion.latestRequestCoalescing`

具体配置名在实现时应遵循现有命名和客户可见性规则；这里的名称只是回滚边界示意，不是已存在配置。

# 不建议的方案

- **只增大 `CompletionMemo` 容量**：generation 仍因每键变化，容量不能提高命中率。
- **只把 overlay clone 放进更大的缓存**：粗 identity 会继续 miss；放宽 key 又可能返回 stale overlay。
- **只并行扫描多个 root**：会降低单次 wall time的某些分位，但增加 CPU/内存带宽竞争和尾延迟，且复杂度仍是 `O(RD)`。
- **把 scope/project/reachability 改成 hard filter**：会违反 candidate model，也会在 open/ambiguous reach 状态下丢失正确候选。
- **普通补全直接读 SQLite**：会破坏 list hot-path contract；倒排候选必须常驻 compact memory，详情继续按 ID 惰性水合。
- **永远返回 `isIncomplete=false`**：客户端请求会减少，但截断列表会被错误当作完整，召回质量不可接受。
- **删除部分字符 trigger 或只匹配采样字符**：这会直接违反全字符参与匹配的产品底线，也只是隐藏服务端根因。允许丢弃过期工作，不允许丢失最新 prefix 中的任何字符。
- **只设置 Windows thread priority**：不能取消已经开始的旧 work，也不能防止 blocking/Rayon oversubscription。
- **默认解析 `ParseFacts::ALL`**：减少重复 parse 的同时可能把每键工作变得更重。应共享 syntax tree并按组惰性提取 facts。
- **一次性同时重写文本、parser、graph、NameTable 和 client**：无法归因性能变化，也难以在语义回归时安全回滚。

# 最终判断

production ordinary completion 的冷召回已经从全量 `O(D)` 扫描改为受 `16,384` 硬预算约束的 compact posting traversal，body-only 编辑也不再让 `overlay_epoch` 无条件击穿 completion universe；latest-request token、前台 admission 和 scan/selection 协作取消已经切断最主要的重复工作闭环。include/import cache miss 现在使用 captured base `Arc`、稀疏 reach override 和 dirty path delta，不再克隆 workspace-size graph/path；path-only external 兼容探测也已移出请求线程。source 初始化、metadata probe、candidate pop、suffix fallback 与跨源评分均有独立硬边界。当前 U-Boot 生产 handler 在 64 次真实 overlay miss 下 P95 为 `23.284 ms`，payload SQL reads 为 `0`，Phase 1/2 均已有正式门禁与独立复审证据。

1.5.1 的索引与自动补全发布约束已经满足：full-index 外层/引擎分别为 `34.162/33.238 s`，双代峰值 `494.07 MiB`，补全固定检查 `16,384` 项并在大表上显式截断。双代距离 512 MiB 仍只有约 `17.93 MiB` 余量，应在后续新增常驻结构时继续观察，但它不是本次发布阻断项。

剩余工作集中在当前文档文本、line map、local words 与 parse facts 的近 `O(L)` 重复工作，以及尚未覆盖的 stdio、扩展宿主和 VS Code key-to-paint 数据。按本次“够用即可发布”的范围，它们进入 Phase 3/后续版本，不再扩大 1.5.1 的发布改动面；后续实现仍需保持 immutable generation snapshot、dirty path shadow、old snapshot continued service、language family isolation、candidate ambiguity、完整输入匹配与 explicit truncation/fallback。

# 公开参考资料

- [Language Server Protocol completion specification](https://raw.githubusercontent.com/microsoft/language-server-protocol/gh-pages/_specifications/lsp/3.18/language/completion.md)
- [clangd index design](https://clangd.llvm.org/design/indexing)
- [clangd Dex fuzzyFind implementation, LLVM commit `c325d6f`](https://github.com/llvm/llvm-project/blob/c325d6fcb6db298f681ae1c450b89b4c255fb3ce/clang-tools-extra/clangd/index/dex/Dex.cpp)
- [clangd trigram generation, LLVM commit `c325d6f`](https://github.com/llvm/llvm-project/blob/c325d6fcb6db298f681ae1c450b89b4c255fb3ce/clang-tools-extra/clangd/index/dex/Trigram.h)
- [rust-analyzer architecture and cancellation](https://rust-analyzer.github.io/book/contributing/architecture.html)
- [gopls implementation architecture](https://go.dev/gopls/design/implementation)
- [gopls cache and Snapshot API](https://pkg.go.dev/golang.org/x/tools/gopls/internal/cache)
- [Roslyn immutable syntax model](https://learn.microsoft.com/en-us/dotnet/csharp/roslyn-sdk/work-with-syntax)
- [Roslyn immutable workspace model](https://learn.microsoft.com/en-us/dotnet/csharp/roslyn-sdk/work-with-workspace)
- [Tree-sitter 0.25.10 `Parser`](https://docs.rs/tree-sitter/0.25.10/tree_sitter/struct.Parser.html) 与 [`Tree`](https://docs.rs/tree-sitter/0.25.10/tree_sitter/struct.Tree.html)
- [Tokio 1.52.3 `spawn_blocking`](https://docs.rs/tokio/1.52.3/tokio/task/fn.spawn_blocking.html)
- [Faster Top-k Document Retrieval Using Block-Max Indexes](https://research.engineering.nyu.edu/~suel/papers/bmw.pdf)
- [The Tail at Scale](https://research.google/pubs/the-tail-at-scale/)
