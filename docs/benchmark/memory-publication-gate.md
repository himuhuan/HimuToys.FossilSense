# 内存发布门禁

本方法用于复现 FossilSense 在大型 C/C++ 工作区中的完整索引、冷态读取模型和已预热旧代发布检查。结构分项用于定位变化来源；Windows 使用 Private Bytes，Linux/macOS 使用 RSS 作为是否通过的依据。

执行前记录样本名称与固定提交、测试机器的操作系统/CPU/内存、Rust 版本和可用的 U-Boot 数据库路径。样本必须至少有 500,000 个当前有效声明和 10,000 个文件。

先在仓库根目录构建，避免首次编译时间进入 120 秒索引门禁：

    cargo build --release -p fossilsense
    cargo test --release -p fossilsense --bin fossilsense --no-run

完整索引门禁：

    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -CaseFilter u-boot-full-index -TimeoutSeconds 120

冷态与热旧代发布门禁：

    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -IncludeEngineHydration -IncludeCompletionReplay -CaseFilter u-boot-full-index,u-boot-engine-hydration,u-boot-completion-replay -TimeoutSeconds 120

同一生产 LSP 生命周期门禁：

    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeLspLifecycle -CaseFilter u-boot-lsp-lifecycle,wine-lsp-lifecycle -TimeoutSeconds 180

生命周期 case 使用一个 LSP 进程完成旧索引发布、详情缓存预热、生产 full rebuild、并发 Hover/F12/补全、dirty 保存、名称压实和压实时再次保存。并发补全固定执行 64 次，逐项检查 P95、召回检查量、处理预算、索引候选、截断标记和详情 SQL 次数。case 还记录阶段覆盖、活动构建峰值、预留/保留字节、取消次数、旧请求 epoch、新请求 generation、数据库身份、Hover/F12 分位延迟、完整索引 `elapsed_ms`/`write_ms`、数据库大小和进程峰值。

| 检查项 | U-Boot 门槛 | Wine 当前判定 |
|---|---:|---|
| 样本规模 | 至少 500,000 个声明、10,000 个文件 | 记录实际规模 |
| 同时活动的重型构建 | 峰值必须为 1 | 峰值必须为 1 |
| 临时预留 | 峰值大于 0 且不超过 256 MiB | 相同 |
| 进程峰值 | 不超过 512 MiB | 首次记录真实值，不套用 U-Boot 结论 |
| 完整索引 | `elapsed_ms` 不超过 120,000 ms | 相同 |
| 请求一致性 | 旧/新 epoch、generation 与数据库身份均不得混用 | 相同 |
| 压实过期 | 至少观察到一次协作取消，最终许可与预留归零 | 相同 |

结果 JSON 与 Markdown 必须保存源码提交与改动指纹、样本提交与改动指纹、机器信息、完整命令、`elapsed_ms`、`write_ms`、峰值内存、数据库文件大小、冷单代/双代、热单代、缓存收缩、第二代增量和名称索引分项。完整索引的实际运行时间或 `elapsed_ms` 任一超过 120,000 ms 即失败。冷或热场景中单代超过 384 MiB、发布窗口绝对峰值超过 512 MiB、内存采样缺失、缓存预热未达到可用预算的 75%，或旧请求代次不一致，也都判定失败；不得以多次平均值、小样本或机器波动放行。

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
