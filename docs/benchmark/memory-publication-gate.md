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

结果 JSON 与 Markdown 必须保存样本提交、机器信息、完整命令、`elapsed_ms`、`write_ms`、峰值内存、数据库文件大小、冷单代/双代、热单代、缓存收缩、第二代增量和名称索引分项。完整索引的实际运行时间或 `elapsed_ms` 任一超过 120,000 ms 即失败。冷或热场景中单代超过 384 MiB、发布窗口绝对峰值超过 512 MiB、内存采样缺失、缓存预热未达到可用预算的 75%，或旧请求代次不一致，也都判定失败；不得以多次平均值、小样本或机器波动放行。
