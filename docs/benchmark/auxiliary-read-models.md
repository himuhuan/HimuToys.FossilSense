# 增量辅助读取模型验证

## 验证范围

保存文件后，备用补全名称、include 补全、Go import 补全、头文件路径索引和已索引文件列表复用不可变基础数据，只替换有界的累计变化。当前五组件合计最多保留 256 个变化分区、32 MiB 变化数据；组件内部另有更小上限。容器容量计入估算。达到上限或无法确定依赖时完整重建，不发布不完整的替代数据。

此范围不包含原有 NameTable、可达关系图或 Go package 图写入器的全部工作，因此局部行数保持稳定不代表整次保存的工作量与仓库大小无关。

本版本使用常驻基础表。磁盘分片、单独的 manifest 文件、32 MiB 热点缓存及文件回收未实现。此前 U-Boot include 补全表约 23 MiB；这一完整驻留样本不足以支持再引入 32 MiB 缓存的收益结论，也不能据此否认冷数据分页的潜在收益。Go 必须使用非空样本单独验证。

## 快速正确性与工作量检查

在仓库根目录运行：

```powershell
cargo test -p fossilsense --bin fossilsense -- segmented_
```

测试覆盖同一变化在 10/1,000 个头文件下的辅助读取行数、全部五个未变化组件共享、头文件重命名、重复事件、连续删除/重新加入、多个来源共享 include 目标、多个 Go package 共享导入路径、Windows 大小写目录、已发布备用名称与未保存文档的合并、累计预算、配置/模块/数据库实例变化及取消发布。排序和结果与完整重建对照；旧快照保持可用。生产 include 测试确认达到 16,384 次检查时返回不完整列表。

辅助发布统计中的 `scoped_rows_read` 只计局部 SQL 返回行；`delta_partitions`、`delta_bytes` 表示发布后的累计变化，`copied_directory_entries` 表示发生更新的组件原有变化目录项数，`new_delta_bytes` 是这些新变化容器的保守容量估算，不能解释成整个进程的分配量。`shared_components` 计完全未变化的 Arc。组件完整重建记录在 `full_components`；整组回退记录 `full_reason`。这些统计不计完整重建 SQL 行数。

## include / Go import 真实请求回放

```powershell
cargo build --release -p fossilsense
cargo test --release -p fossilsense --bin fossilsense --no-run
cargo test --release -p fossilsense --bin fossilsense -- benchmark_auxiliary_completion_replay --ignored --nocapture
```

先构建以免编译混入测量。测试创建固定结构的临时样本：1,000 个头文件、128 个可导入 Go package 和调用文档。初次请求、重复请求、头文件重命名后三个阶段，每阶段分别执行 64 次真实 LSP include 和 Go import 补全，共 384 次。每次检查结果非空、名称前缀正确、已删除头文件不再出现；各阶段 P95 不超过 50 ms。计时覆盖服务端请求入口到响应，不含索引建立、文件变更和测试初始化；不代表 VS Code 界面延迟。输出 `auxiliary_replay` JSON 和 `auxiliary_update` JSON，分别记录各阶段延迟/候选数量及更新工作量。

## 大型仓库整体验收

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -IncludeEngineHydration -IncludeCompletionReplay -IncludeBindingReplay -IncludeCacheReplay -IncludeLspLifecycle -CaseFilter u-boot-full-index,wine-full-index,u-boot-engine-hydration,u-boot-completion-replay,u-boot-binding-replay,u-boot-declaration-cache-replay,u-boot-lsp-lifecycle -ObserveFullIndexTime -AllowTransientMemoryPeak -TimeoutSeconds 600 -LifecycleTimeoutSeconds 600
```

以脚本 `-ListCases` 返回的名称为准。保留原始报告中的源码/样本指纹、机器、命令、索引时长、写入时长、Private Bytes/RSS 和数据库大小。v1.7.1 完整索引耗时为观察指标；普通补全 64 请求/P95≤50 ms、检查预算 16,384、列表完整声明 SQL 为 0，以及 U-Boot 单代 384 MiB、双代加载 512 MiB 的保守内存检查继续执行。在线生命周期按稳定内存验收：后台任务全部结束后保留服务和正常缓存，连续至少 10 秒、100 次采样的最大值须不超过 512 MiB；单次连续超线须不超过 10 秒。10 ms 周期采样器保留 Duration 精度累计，最后才转成毫秒，报告峰值及累计/最长超线时长。默认脚本仍保留严格峰值模式，显式使用 `-AllowTransientMemoryPeak` 启用本版本的稳定值策略。额外回放不能替代大型门禁。

Wine 完整命令行索引与在线双代重建分别验收；前者成功不代表后者已通过内存准入。进程内存实测是放行依据，分类容量估算用于解释组成。

## 2026-09-08 最终生产构建测量

机器：Windows 10.0.26220.0，i5-12500H，16 个逻辑处理器，物理内存 25,459,482,624 B。样本和完整命令保存在 [核心原始数据](results/v1.7.1-core.json)、[在线生命周期原始数据](results/v1.7.1-lifecycle.json) 和 [辅助补全原始数据](results/v1.7.1-auxiliary.json)。U-Boot 提交 `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c`；Wine 提交 `6eb2e4c32cc9e271856146df11ed3a5c2cf29234`；样本修改指纹在各报告中保存。

生产引擎 SHA-256：`0f03956ee543d83c97c8648ed6e5c7b6fbbd85ef8962f4b7b4162bd37aec4599`。核心/辅助回放测试程序 SHA-256：`d389a7c087990284e1858ba162e0e4f6393258d74452076cbf1037db1153334f`；补充精确持续时间测量的生命周期测试程序 SHA-256：`843b62d32d9af21d703044ec9cffabed77fb34a5c1bb540f21b8b0c3630b5980`。核心报告与生命周期报告之间仅调整验收测试和文档，生产引擎字节一致。源码基点及各自工作树指纹完整保存在 JSON，测量后的结果文档不属于被测程序。

| 检查 | 实测 | 结论 |
| --- | ---: | --- |
| U-Boot 普通补全，64 次 | P95 29.853 ms | ≤50 ms；检查 16,384 项、列表完整声明 SQL 为 0 |
| 重建期间普通补全，64 次 | P95 46.778 ms | ≤50 ms，所有请求含索引候选 |
| U-Boot 单代预热峰值 | 344.031 MiB | ≤384 MiB |
| U-Boot 双代加载峰值 | 496.855 MiB | ≤512 MiB |
| 在线运行稳定最大值 | 337.855 MiB | 100 次采样，10838 ms，≤512 MiB |
| 在线运行瞬时峰值 | 522.707 MiB | 超线累计 1081 ms，最长连续 1071 ms；≤10 s |
| 悬停/定义绑定回放 | 384 次正确目标检查通过 | 覆盖冷、热及编辑并发；不承诺所有导航请求低于50 ms |
| 声明缓存回放 | 512 次正确目标检查通过 | 热缓存每类64次命中、0次详情读取；边界/淘汰保持预算 |

| 完整索引 | 声明 / 文件 | elapsed / write ms | 外部计时 ms | 峰值 Private Bytes | 数据库 B |
| --- | ---: | ---: | ---: | ---: | ---: |
| u-boot-full-index | 655130 / 13244 | 60248 / 14966 | 60515.229 | 155959296 | 476860416 |
| wine-full-index | 520486 / 7153 | 106375 / 18958 | 106573.642 | 807481344 | 691109888 |

完整索引均强制重建数据库，未清空操作系统文件缓存。耗时作观察数据，不能将重复运行后的文件缓存收益归因于辅助索引重构。Wine 的命令行完整索引通过；在线双代重建仍不作为已验收能力。

| 辅助补全场景 | include P95 ms | Go import P95 ms | 请求数 |
| --- | ---: | ---: | ---: |
| first-pass | 2.827 | 0.052 | 各64次 |
| warm | 3.140 | 0.043 | 各64次 |
| after-rename | 3.233 | 0.035 | 各64次 |

重命名头文件的辅助更新读取 3 行、保留 11 个变化分区、估算新变化容量 8,358 B，未复制旧变化目录项，2 个组件完全共享，未发生完整回退。另有 10/1,000 个头文件的同影响集合测试，辅助读取行数一致且五组件完全共享。

首次预热发布测试因超过512 MiB被阻断。include表在去重后收紧容量，固定1,000头文件样本的表容量从1,674,078 B降至860,838 B，之后双代加载检查通过。在线真实并发峰值仍略超512 MiB，按稳定内存与持续时间分别验收；保留这个峰值而非宣称彻底消除波动。计时器逐次取整的低估问题用1,000次10.9ms回归复现，改为Duration累计后获得本节最终数据。
