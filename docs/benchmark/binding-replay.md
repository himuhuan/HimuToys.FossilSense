# 悬停和跳转回放

先准备 U-Boot 完整索引，并在 release 模式下构建引擎及测试程序：

    cargo build --release -p fossilsense
    cargo test --release -p fossilsense --bin fossilsense --no-run
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -CaseFilter u-boot-full-index -TimeoutSeconds 120
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeBindingReplay -CaseFilter u-boot-binding-replay -TimeoutSeconds 120

回放要求索引至少包含 500,000 个声明和 10,000 个文件。从真实 C 文件选取 64 个函数定义，依次执行冷缓存、热缓存以及编辑并发场景；每个场景均执行 64 次悬停和 64 次定义跳转。跳转结果必须包含所选声明的精确位置，悬停必须包含对应名称，以及按原始 callable 指纹确认的同实体位置集合中的路径；允许展示有关系证据的头声明。此断言证明返回位置有依据，不证明全局唯一或排除所有额外候选。编辑在原文件末尾追加注释，与请求并发执行，因此新旧文档版本的目标位置相同；版本捕获竞争和更改目标位置另由确定性回归测试覆盖。

输出包括每种场景的请求数、正确目标数、P50/P95/P99、解析缓存命中，以及捕获、解析、绑定、overlay、查询、源码加载和渲染阶段耗时。`sqlite_read_sessions` 统计本次请求在线程中执行的 typed SQLite 读取会话；一个会话可能执行多条 SQL，不能把它解释成 SQL 语句数。计时从生产请求入口开始，包含空结果和被取消的请求；回放只接纳实际完成并返回正确结果的请求。

当前仅对目标正确性、请求数和样本规模设置断言，不把普通补全的 50 ms 标准套用到悬停或跳转。完整索引和补全的独立性能门禁仍须通过。报告由同一个大型工作区脚本保存，包含源码及样本版本、机器信息、执行参数和数据库大小，供成员解析、实体导航和缓存变更复用。

## 2026-09-08 测量

Windows 10.0.26220.0，i5-12500H，16 个逻辑处理器，物理内存 25,459,482,624 B。
U-Boot 提交 `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c`，样本修改指纹 `05e10fb4a42f9dbb9f2a05688dccb29e111a2b768b62353df098f79acf8c4b72`。
源码基点 `e1031f431ec99b84cf28caff305024d412a9a550`，测量时工作树指纹 `f84de894d73f0073b2a9567cc6aae5799f2800e97716802d3bf2135e522bb242`；本节在测量后补入，不改变被测 Rust 程序。

| 场景 | 请求 | P50 ms | P95 ms | P99 ms | 已验证范围 |
| --- | ---: | ---: | ---: | ---: | --- |
| 冷缓存悬停 | 64 | 21.034 | 74.206 | 295.370 | 名称及有依据的位置 |
| 冷缓存 F12 | 64 | 16.971 | 36.787 | 73.373 | 包含正确源码位置 |
| 热缓存悬停 | 64 | 13.221 | 23.560 | 29.118 | 名称及有依据的位置 |
| 热缓存 F12 | 64 | 9.460 | 20.603 | 22.456 | 包含正确源码位置 |
| 编辑并发悬停 | 64 | 29.418 | 126.575 | 382.238 | 捕获文本对应的候选 |
| 编辑并发 F12 | 64 | 36.621 | 96.622 | 133.563 | 捕获文本对应的位置 |

全部 384 次请求完成；输出无后台 panic。回放脚本同时检查进程退出码及 `panicked at`、fatal runtime error、stack overflow，避免后台异常被测试框架的成功汇总掩盖。编辑并发尾延迟明显高于热缓存，不据此宣称悬停、跳转均达到 50 ms。

| 大型门禁 | 实测 | 门槛 |
| --- | ---: | ---: |
| U-Boot 完整索引外部计时 / elapsed_ms | 56,370.293 / 55,144 ms | 两者均 ≤120,000 ms |
| U-Boot 写入 / 数据库 / 峰值 Private Bytes | 12,730 ms / 404,066,304 B / 157,044,736 B | 完整索引记录项 |
| 声明 / 文件 | 654,502 / 13,244 | ≥500,000 / ≥10,000 |
| 单代 / 冷缓存双代 / 热缓存发布峰值 Private Bytes | 258,625,536 / 499,806,208 / 507,645,952 B | 单代≤384 MiB；双代峰值≤512 MiB |
| 普通补全 P95 | 29.772 ms | ≤50 ms |
| 普通补全详情 SQLite / 每请求候选预算 | 0 / 16,384 | 0 / 16,384 |
| Wine 完整索引外部计时 / elapsed_ms | 110,624.831 / 110,426 ms | 两者均 ≤120,000 ms |
| Wine 写入 / 数据库 / 峰值 Private Bytes | 18,580 ms / 634,474,496 B / 806,211,584 B | CLI 索引记录项 |

Wine 样本提交 `6eb2e4c32cc9e271856146df11ed3a5c2cf29234`，样本无修改。其脚本从干净的版本主目录启动，原始报告的源码变更指纹因此为空；实际运行程序仍为共享 target 中本次冻结构建。测试后核对程序 SHA-256 为 `547452CF5239B86BA60B36A6D9300B0A08119C55BA4C9CCB23774F4915B675F4`，最后写入时间为 `2026-09-07T22:10:03.138791Z`，早于两次测量；原始元数据保留并附核对说明。后续复测必须从被测工作树启动脚本。

本地原始记录位于 `target/verification/v1.7.1/proposal3/final/`：`binding_replay_final.log`、`large-workspace-20260908_061547.json` 与 `large-workspace-20260908_061816.json`。完整复测使用本文开头命令及大型门禁脚本的 hydration、completion、Wine case，保持测量时无并行编译。
