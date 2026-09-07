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
