# FossilSense 1.7.0 大型仓库性能结果

本记录包含 2026-09-07 的完整索引、U-Boot 冷暖读取模型与真实 LSP 自动补全测量。每个完整索引分别判定是否通过 120 秒门槛，不取平均值。内存采用 Windows Private Bytes，采样间隔 20 ms；MiB 为 1,048,576 字节。

## 固定输入与环境

| 项目 | 实际输入 |
|---|---|
| 测试源码提交 | `cb3c2faeb126c2ff025502762eaa0530f5c8f85c`，测试前后工作树干净 |
| Release 输入 SHA-256 | `85a86dafaf0f0f467360154789325bea6299a3612fb3b6b5e1a51efaa96413b4`，236 项 |
| 引擎版本 | `fossilsense 1.7.0`，release 构建 |
| 引擎 SHA-256 | `0f6b41277f039cf4a9fa2dd158197f0315d44b5f7e91923566eb9fb15ef992fd` |
| Rust / Cargo | `1.94.1`，`x86_64-pc-windows-msvc` |
| Node.js / pnpm | `22.23.2` / `10.34.5` |
| 测试机 | Windows NT 10.0.26220.0；i5-12500H；16 个逻辑处理器；25,459,482,624 字节物理内存 |
| U-Boot 版本 | `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c` |
| Wine 版本 | `6eb2e4c32cc9e271856146df11ed3a5c2cf29234`，工作树干净 |

U-Boot 保留已有的两处空行删除：原始提交中 `boot/vbe_abrec.c` 第 42、71 行。复现相同样本时，仅在新副本中删除这两处空行并保留 LF。文件由 2,340 字节变为 2,338 字节，SHA-256 应为 `3a45548e137ee35d0e989f712871bfa2486ff960087bb365e4b5411212170462`。本次没有恢复或覆盖已有样本修改；样本代码不纳入本仓库。

## 复现命令

从仓库根目录执行。两个样本分别位于 `samples/u-boot` 和 `samples/wine`，先核对上述版本与样本文件指纹。

```powershell
cargo build --release -p fossilsense
cargo test --release -p fossilsense --bin fossilsense --no-run
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -IncludeEngineHydration -IncludeCompletionReplay -CaseFilter u-boot-full-index,u-boot-engine-hydration,u-boot-completion-replay -TimeoutSeconds 120 -BenchmarkRoot target/verification/release-1.7.0/benchmark
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -CaseFilter wine-full-index -TimeoutSeconds 120 -BenchmarkRoot target/verification/release-1.7.0/benchmark
```

## 完整索引

| 用户关注点 | U-Boot | Wine | 产品结论 |
|---|---:|---:|---|
| 实际进程用时 | 34,255.102 ms | 59,852.466 ms | 两项均通过 120,000 ms 门槛 |
| 引擎 elapsed_ms | 34,051 ms | 59,654 ms | 两项均通过 120,000 ms 门槛 |
| 写入耗时 | 11,459 ms | 17,853 ms | 保留写入成本，方便复测比较 |
| 索引文件数 | 13,244 | 7,153 | 两个样本均完成全部文件索引 |
| 索引进程峰值 Private Bytes | 141.738 MiB | 948.738 MiB | Wine 构建峰值较高，需要继续观察和优化 |
| 数据库文件大小 | 404,131,840 bytes | 634,953,728 bytes | 保留磁盘开销 |

## U-Boot 读取、发布与补全

| 用户关注点 | 门槛 | 当前结果 | 产品结论 |
|---|---:|---:|---|
| 测试规模 | 至少 500,000 声明 / 10,000 文件 | 654,502 / 13,244 | 满足大型样本要求 |
| 冷单代峰值 | 384 MiB | 246.348 MiB | 通过 |
| 冷双代绝对峰值 | 512 MiB | 474.965 MiB | 通过 |
| 预热单代峰值 | 384 MiB | 337.410 MiB | 通过 |
| 预热旧索引旁建立第二代的绝对峰值 | 512 MiB | 482.754 MiB | 通过，余量 29.246 MiB |
| 旧请求的数据版本 | epoch / generation 均保持一致 | 两项断言均为 1 | 新索引构建未混入旧请求 |
| 真实补全请求数 | 64 | 64 | 全部执行 |
| 补全 P95 / 最大耗时 | P95 不超过 50,000 us | 34,933 / 37,962 us | P95 通过 |
| 每请求检查条目 | 1..=16,384 | 全部 16,384 | 保持有界处理 |
| 每请求候选预算 | 16,384 | 全部 16,384 | 门槛未放宽 |
| 补全列表读取完整声明的 SQLite 次数 | 0 | 0 | 列表阶段保持精简召回 |
| 每请求索引候选 | 必须非空 | 最少 350 | 每次真实使用大型索引 |
| 当前有效声明 | 至少 500,000 | 最少 654,502 | 未使用空索引替代测量 |
| 未处理完的候选 | 必须标记 truncated | 64 / 64 次 | 截断信息可见 |

Wine 的约 948.738 MiB 是完整索引构建进程峰值；384/512 MiB 门禁对应上表中的 U-Boot 读取模型和双代发布场景。不能把不同阶段的数据混为一项内存上限，也不能据本记录宣称 Wine 读取模型或所有后台构建都低于 512 MiB。本轮没有测量 VS Code 界面端的输入卡顿。

完整字段与实际命令保存在 [U-Boot 归档报告](release-1.7.0-uboot.json) 和 [Wine 归档报告](release-1.7.0-wine.json)。归档 JSON 保留生成报告的全部字段，仅统一为 UTF-8 与 LF；原始文件分别为 `target/verification/release-1.7.0/benchmark/large-workspace-20260907_054128.json` 和 `large-workspace-20260907_054350.json`。
