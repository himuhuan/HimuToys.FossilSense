# 资源与交互验收方法

以下为 1.7.2 起的当前方法。后面的带日期结果保留当时策略与原始结论，不能用作当前构建的验收证据。

## 按修改范围执行

局部 Rust 修改：`powershell -NoProfile -File scripts/verify.ps1 -Profile Local -TestFilter candidate_service::`。扩展或脚本修改使用 `-Scope Extension` 或 `-Scope Scripts`。Local Rust 必须给出过滤器，避免无意运行全部测试。

默认脚本入口测试只用合成指标验证报告与失败处理；需要复现旧版六组真实回放时，显式运行 `scripts/test_benchmark_entrypoints.ps1 -IncludeLegacyReplay`。

准备合入：`powershell -NoProfile -File scripts/verify.ps1 -Profile Merge -SkipInstall`。完整日志和逐项退出状态、耗时保存在 `target/verification/`。CI 显式运行 Merge；打包默认不重复源码测试，需要检查加打包时使用 `build.ps1 -Verify`。复用结果前核对相关源码、测试、配置和工具链，打包本身不证明测试通过。

性能敏感行为调整后，选择受影响的场景。例如索引资源与发布调整：

    powershell -NoProfile -File scripts/verify.ps1 -Profile Performance -CaseFilter u-boot-full-index,u-boot-lsp-lifecycle

Performance 先构建 release，再运行显式选择的场景。已有数据库可以用于单独生命周期回放；没有数据库时先选同一个样本的 full-index。不要因每轮会话或 reviewer 接手重复测量。纯职责迁移复用行为回归；验收脚本策略用合成报告测试。

## 资源判断

关系语义基础的代表场景使用 `scripts/verify.ps1 -Profile Performance -CaseFilter relation-semantic-foundation`。脚本生成 501 个源码文件、至少 500,000 个声明和引用位点，完成真实索引与引擎加载，验证成员/回调调用及实体引用，再执行 64 次真实补全。证据目录保存源码和脚本 SHA-256 清单、数据库及 `metrics.json`；报告包含新事实行数/载荷字节、索引耗时、数据库体积、全过程进程内存峰值与任务结束后 10 秒、100 次采样的稳定最大值。该合成场景记录资源观测，不套用 U-Boot 的内存参考值，也不替代后续完整仓库生命周期验收。

完整索引默认记录耗时，不以统一 120 秒拒绝；`TimeoutSeconds` 和 `LifecycleTimeoutSeconds` 是防止执行挂起的超时，触发仍为失败。历史复现才使用 `-StrictFullIndexTime` 或 `-StrictMemoryPeak`。旧 ObserveFullIndexTime、AllowTransientMemoryPeak 参数兼容保留，当前默认已开启。

| 场景 | 当前判断 | 产品含义 |
| --- | --- | --- |
| U-Boot 在线稳定内存 | 任务后至少 10 秒、100 次采样，最大值 ≤512 MiB | 长期资源负担参考 |
| U-Boot 在线短暂增长 | 峰值 ≤768 MiB，连续超线 ≤10 秒，累计 ≤30 秒 | 同时限制幅度、单次持续时间和反复增长 |
| U-Boot 冷/暖加载专项 | 单代384 MiB、双代512 MiB保留为专项参考检查，按影响选用 | 与在线生命周期分开解释 |
| Wine 完整索引/在线回放 | 保存真实规模、时间、内存与正确性结果；不套用 U-Boot 内存数字 | 不从 CLI 成功推断在线成功 |
| 真实补全 | 64 次、P95 ≤50 ms、每次1..=16,384项、预算16,384、零详情SQL、有索引候选与截断标记 | 保护日常交互和查询有界 |
| 数据正确性 | 数据库完整性、旧新代一致、取消不发布、删除不复活 | 不因资源放宽而放宽 |

生产资源档位与测量参考预算是不同用途：默认进程准入目标1 GiB，可选择512 MiB或2 GiB；临时预留仍256 MiB。具体选择依据机器余量和实际工作量，不按仓库目录总大小机械换算。内存不足最多等待30秒，之后明确失败并保留旧索引；工作区记录补偿扫描状态，下次保存或刷新补齐未成功的更新；释放资源或调高档位、重启服务后执行完整重建。

对比版本时保持机器、样本版本和测量命令一致，记录有效文件数、声明数、源码字节数和最大文件（可获得时）、关系数量、elapsed_ms、write_ms、Windows Private Bytes或其他平台RSS、数据库及日志文件体积。源码字节数应排除.git、构建产物与不参与索引的资源；数据库体积不代表累计磁盘写入或SSD损耗。重点解释首次可用、打开已有索引、保存更新和重建期间交互，严重退化需分析，不能用空结果换速度。

## 2026-09-06 preserve-c-declaration-context 验证结果

样本为 U-Boot `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c`。测试机为 Windows NT 10.0.26220.0、12th Gen Intel(R) Core(TM) i5-12500H、16 个逻辑处理器、25,459,482,624 bytes 物理内存；Rust 为 `rustc 1.94.1 (e408947bf 2026-03-25)`，目标为 `x86_64-pc-windows-msvc`。

执行命令：

    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -IncludeEngineHydration -IncludeCompletionReplay -CaseFilter u-boot-full-index,u-boot-engine-hydration,u-boot-completion-replay -TimeoutSeconds 120

| 用户关注点 | 发布门槛 | 本次结果 | 产品结论 |
|---|---:|---:|---|
| 完整索引实际用时 | <=120,000 ms | 76,053.677 ms | 通过，程序内部 `elapsed_ms` 也为 74,983 ms |
| 索引写入 | — | 25,174 ms | 已记录，便于后续定位索引耗时变化 |
| 数据规模 | 至少 500,000 声明、10,000 文件 | 654,605 声明、13,244 文件 | 样本规模达标 |
| 完整索引进程峰值 | — | Private Bytes 131.89 MiB | 已记录；Working Set 为 139.36 MiB |
| 单代读取模型 | <=384 MiB | 冷态 246.88 MiB；预热 336.23 MiB | 通过；预热场景余量 47.77 MiB |
| 双代发布绝对峰值 | <=512 MiB | 冷态 475.23 MiB；预热 483.18 MiB | 通过；最小余量 28.82 MiB |
| 补全等待时间 | P95 <=50,000 us | P50 30,396 us；P95 41,624 us；最大 46,082 us | P95 通过，余量 8,376 us |
| 单次补全检查规模 | `1..=16,384`，预算 16,384 | 64 次均检查 16,384 项，预算均为 16,384 | 通过，工作量保持有界 |
| 补全详情 SQL | 0 | 0 | 通过，高频列表阶段未读取完整声明 |
| 索引候选与截断 | 每次返回索引候选；未处理完必须标记截断 | 每次至少 350 个索引候选；64 次均标记截断 | 通过 |
| 数据库文件 | 必须记录 | 384,421,888 bytes（366.61 MiB） | 已记录 |

预热旧代缓存为 72.41 MiB，目标 72.06 MiB；发布前有效缓存预算 96.07 MiB，发布时收缩 66,560 项、72.41 MiB。旧请求的 epoch 和 generation 均保持一致。原始报告由脚本生成在 `target/benchmark/large-workspace-20260906_222753.json` 与同名 Markdown；`target/` 不纳入版本控制，本节保留可重复核查的结果摘要。

## LSP 生命周期的计时范围

生命周期回放包含缓存预热、一次完整重建，以及三轮增量更新和压实。`-LifecycleTimeoutSeconds 240` 只保护整套场景不会无限等待；普通完整索引仍由 `-TimeoutSeconds 120` 限制。生命周期内的完整重建同时检查内部 `lsp_lifecycle_elapsed_ms` 和从提交重建请求到发布完成的 `lsp_lifecycle_rebuild_wall_ms`，两者都必须不超过 120,000 ms。缺少任一指标即失败。缓存预热和后续增量场景不计入该完整重建计时。

每完成一个场景，脚本都会保存标记为 `partial` 的 JSON 检查点。后续场景失败时，已完成的独立结果仍可复查；该检查点不表示整套门禁通过。正常完成后输出的正式 JSON 和 Markdown 报告保持不变。

## 2026-09-08 字节准入与解析并发复测

机器：Windows 10.0.26220，Intel Core i5-12500H，16 个逻辑处理器，物理内存 25,459,482,624 bytes。运行时基于源码提交 `320a5c507f21e2cef3a9c47067fcec1c3e0533d9` 加改动指纹 `5b9e14a6dc0f389ce00f327ea0fcb877d42e40a2a1bbd5b26e5e095e1a6ce554`；后续补充的失败保护/等价性测试与说明不改变被测运行路径。

U-Boot 样本提交 `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c`，样本改动指纹 `05e10fb4a42f9dbb9f2a05688dccb29e111a2b768b62353df098f79acf8c4b72`；Wine 样本提交 `6eb2e4c32cc9e271856146df11ed3a5c2cf29234`，无工作区改动。完整索引分别使用本文的 `u-boot-full-index` / `wine-full-index`，`-Repeats 1 -IncludeFullIndex -TimeoutSeconds 120`；U-Boot 同次添加 hydration 和 completion。生命周期命令为 `scripts/benchmark_lsp_lifecycle.ps1 -Database <样本DB> -Workspace samples/<样本> -Sample <样本>`。

| 场景 | 实际 / 内部用时 ms | write_ms | 数据规模 | 峰值 Private Bytes | DB bytes | 结果 |
|---|---:|---:|---|---:|---:|---|
| U-Boot 独立完整索引 | 54,483.919 / 53,330 | 12,157 | 654,502 声明 / 13,244 文件 | 155,451,392 | 404,066,304 | 通过 120s |
| Wine 独立完整索引，第一次 | 108,870.429 / 108,627 | 18,853 | 519,704 声明 / 7,153 文件 | 828,051,456 | 634,474,496 | 通过 120s |
| Wine 独立完整索引，第二次 | 109,362.865 / 109,167 | 18,731 | 同上 | 811,077,632 | 634,474,496 | 通过 120s；保留重复运行，不择优 |
| U-Boot 单代 / 冷双代 / 预热双代 | 分项报告 | — | 654,502 声明 / 13,244 文件 | 258,363,392 / 498,548,736 / 506,359,808 | 404,066,304 | 通过 384/512 MiB |
| U-Boot 完整 LSP 生命周期 | 重建墙钟 66,344 / 内部 58,743 | 13,180 | 64 次补全，3 次保存更新 | 530,808,832 | 404,062,208 | 通过；旧/新代一致、真实压实取消并完成 |
| Wine 热 LSP 重建 | 场景在约 82.54s 失败 | 未完成 | 单线程最小解析预约仍不足 | 无成功生命周期峰值结论 | 原数据库继续使用 | 资源受限，未验收 |

U-Boot 独立补全回放 P95 为 28,002 us；生命周期中的补全 P95 为 44,677 us。两者都执行 64 次真实补全，处理预算与检查上限保持 16,384，列表详情 SQL 为 0，均返回索引候选并明确截断。不能将服务端结果解释为 VS Code 界面延迟。

Wine 热重建失败没有被改成通过：它说明部分大型仓库在旧索引继续服务时可能无法完成完整重建，仓库级结果会继续使用旧版本。默认 512 MiB 准入和两种独立完整索引的 120s 标准保持不变。小型受控压力回归通过生产调度入口验证失败后原 epoch、数据库身份、typed 行、Hover、F12 和索引补全保持可用，许可归零且 staging 清理；它是失败保护证据，不替代 Wine 生命周期成功证据。

原始报告为 `target/verification/v1.7.1/proposal2/final/large-workspace-20260908_044206.json`、`large-workspace-20260908_044604.json`，以及 `target/verification/v1.7.1/final/large-workspace-20260908_044405.json`。生命周期日志为 `proposal2/final/lifecycle-u-boot-final-20260908_043644.log` 和 `lifecycle-wine-final-20260908_043815.log`。


## 2026-09-12 关系语义基础验证结果

执行入口：`scripts/verify.ps1 -Profile Performance -CaseFilter relation-semantic-foundation`。Windows 10.0.26220、Intel Core i5-12500H、16 个逻辑处理器、25,459,482,624 bytes 物理内存；Rust 1.97.0，`x86_64-pc-windows-msvc`。源码为 `be9c8289e83c46b57a8ddfd12d635e531440a6f8` 加本次未提交实现；287 个源码/脚本文件的 SHA-256 清单摘要为 `AF86F486B61DC4F2D471A5909FBAEE3ADBC24AF474253D589E5506CC24134BF4`，运行后逐项核对无变化。

| 检查 | 本次结果 |
| --- | --- |
| 合成输入 | 501 文件，25,012,599 bytes 源码，500,508 声明 |
| 新关系事实 | 500,012 行；JSON 载荷 223,005,423 bytes |
| 完整索引 / 写入 | 48,252 / 13,235 ms |
| 数据库体积 | 568,471,552 bytes（约 542.14 MiB） |
| 进程 Private Bytes 峰值 | 156,352,512 bytes（约 149.11 MiB），100 ms 采样 |
| 稳定窗口 | 任务后至少 10 秒、100 次采样；最大 123,174,912 bytes（约 117.47 MiB） |
| 真实补全 | 64 次，P95 9,458 µs；每次检查 16,384 项，预算均为 16,384 |
| 召回正确性 | 每次至少返回 300 个索引候选，64 次均有截断标记，详情 SQL 为 0 |
| 真实关系 | 已知 owner 方法及函数指针/对象回调产生 2 个合并调用关系，排除另一 owner 的同名方法；实体引用返回 1 个匹配位点 |

Performance 退出 0，总耗时 302.90 秒，包含约 3 分 53 秒的 release 编译；测试本体 63.50 秒。新行为回归 38 项、parser 184 项、调用服务 28 项、存储 132 项、索引基本契约 30 项及补偿扫描恢复 1 项通过，各过滤器存在重叠，不累加为独立总数；Clippy 和 Local Scripts 也通过。独立 reviewer 最终准入通过。

证据为 `target/verification/Performance-20260912-174721-31084/` 与 `target/benchmark/relation-20260912-174722-30876/`。后者保留 `environment.json`、`inputs.sha256`、`metrics.json`（64 次召回和全部稳定采样）、合成源码及数据库。上述资源值是 release 测试进程的观测，不替代 U-Boot/Wine 生命周期或 VS Code 界面延迟验收；真实大仓库生命周期、集成 Merge、统一 QuerySession 和关系窗口由后续集成提案承接。
