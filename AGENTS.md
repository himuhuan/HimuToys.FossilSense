# AGENTS.md

## 项目与修改边界

FossilSense 支持 Windows 和 Linux 上的大型 C/C++ 仓库，Go 支持为实验能力。无需完整编译环境，VSIX 必须自带本地引擎。提供候选分析，不承诺完整编译器语义；证据不足时保留歧义、降级或截断信息，不能猜测唯一答案。

- 修改前用 `rg` 阅读相关实现与测试，当前源码和配置优先于历史记录。引擎入口在 `crates/fossilsense/src/`，扩展在 `extensions/vscode/src/`；版本和命令查 Cargo、package 清单及脚本，只读本任务所需内容。
- 复用既有服务，保持 parser → indexer → store → query → server 的依赖边界，LSP 转换只在最外层。Go 按 package/import 判断可见性，不与 C/C++ 混查。
- 查询必须有界，SQL 只取必要字段；高频路径不扫描或复制全库。排序不能擅自变成过滤。普通补全用精简 `NameTable`，列表阶段不读完整声明 SQL；详情以声明 ID、原始名称回到 `CandidateQueryService`，最终展示读取 `DeclarationReadRow`。禁止常驻完整声明副本或第二套语义模型。
- 编辑器文档、数据库和运行时快照保持版本一致；重建未完成不得对外读取，完整性和外键检查通过才发布。失败或取消保留旧索引，遗漏更新保留补偿扫描标记。在线增量保留 WAL 与 `synchronous=NORMAL`。
- 声明缓存预算先扣除精简召回数据，设为 0 也保留基本召回；它不代表进程预算。不能用空结果、丢候选或长期不更新换取资源达标。
- 新依赖需说明许可证、平台和独立运行影响；用户可见行为变化同步两份 README。

## 测试、构建与发布

当前开发版本：1.7.2；版本号以 Cargo 和扩展清单为准。

新增模块先明确期望行为和测试。行为修复先用已有测试或新增回归证明失败，再修改并确认通过；覆盖足够不重复新增。纯职责迁移复用行为测试，保留遮蔽、声明角色、未保存覆盖、删除、取消和版本一致性等有效回归。文档与提示词只检查内容和差异。

验证由一个执行者负责，主代理判断证据是否充分，不因交接重复执行。输入变化使相关证据失效；新失败或具体未决风险只补查受影响部分。

| 场景 | 入口与范围 |
| --- | --- |
| 局部修改 | `scripts/verify.ps1 -Profile Local`；Rust 必须指定 `-TestFilter`，扩展用 `-Scope Extension`，脚本用 `-Scope Scripts` |
| 准备合入 | `scripts/verify.ps1 -Profile Merge`，集中执行一次 |
| 性能敏感修改或发布 | `-Profile Performance -CaseFilter <场景>`，最终实现选代表场景；纯迁移不跑大仓库重建 |
| 已授权打包 | `build.ps1`；尚需 Merge 检查时用 `build.ps1 -Verify` |

使用 Windows PowerShell 或 PowerShell 7（Linux 用 `pwsh`）、Node.js 22、pnpm 10；Rust 以 `rust-toolchain.toml` 为准。Linux 构建还需 C/C++ 编译器与系统开发头文件。Linux 入口为 `pwsh -NoProfile -File ./build.ps1`；按宿主系统和架构打包，Linux 使用 glibc，产物名包含平台和架构。验证步骤由 `scripts/verification_plan.ps1` 维护。

性能测量遵循 [资源与交互验收方法](docs/benchmark/memory-publication-gate.md) 的当前方法，不能套用历史 PASS。保留真实补全 64 次、每次 1..=16,384 项、零详情 SQL、有索引候选和未完成截断；大型验收至少 500,000 声明。资源参考按机器与场景解释，缺失测量不能通过。

命令报告退出状态、耗时和失败摘要。打包不代表正确性或性能通过；关键输入改变后旧 VSIX 失效。发布记录产物名及 SHA-256、release-input SHA-256、源码提交、验收结果和能力边界。

## 协作与授权

| 任务 | 分工 |
| --- | --- |
| 新模块、重构或 spec 实施 | 大范围探索交 explorer；主代理实施，test-executor 验证，reviewer 独立复核 |
| 明确问题修复 | 主代理修复并运行对应回归，不自动全套验证 |
| 模糊想法 | 按需探索，主代理明确方案；未获实施授权不改代码 |
| 文档、仓库或 Codex 配置调整 | 主代理直接处理，不启动产品测试 |

仅在独立探索、耗时检查或重要复核值得委托时派代理。使用 `fork_turns="none"`，提供范围和上下文，要求返回文件行号、符号与证据。explorer 最多 3 个；主代理不重复其探索，子代理不继续派生。reviewer 提案由主代理据证据裁决，说明接受、延期或否决及理由，不自动扩大范围。

除非用户主动调用，否则不加载 spec 类技能。OpenSpec apply 仅用于用户指定实施的已有 change、`/opsx:apply` 或明确继续该 change；一般分析、规划和“继续开发”不算调用。

按本次授权完成工作，不自动提交、合并、推送或发布；这些动作须有明确授权。

## 阶段清理

实现冻结、必要验证完成、review 无待修项后，提交前才清理；此前保留证据。只永久删除本阶段临时产物：`target/verification/`、`target/benchmark/`、`target/extension-smoke/`、`target/benchmark-entrypoint-test-*` 及本阶段临时数据库和日志。

保留编译/依赖缓存、外部样本（包括 U-Boot、Wine）、待发布或核验的 VSIX、长期性能结果及跟踪文件。缓存仅在不兼容、损坏或用户要求时删除；样本仅在用户要求，或确认损坏/版本错误且立即重备时删除。空间不足先报告，不擅自清样本；禁止整删 `target/` 或无范围 `cargo clean`。清理后只查目标、空间和 Git 状态，不重跑验证。

## 文档与表达

README 与本文件以普通读者约 2 分钟读完为目标；本文件尽量控制在约 2,000 字符，只保留规则和入口，不堆术语、原理解释或重复条款。先说结论和用户影响，多组数据用表格；区分已完成、未覆盖和后续规划，不把过程当成产品结论。

长期文档仅保留本文件、两份 README 和 `docs/benchmark/` 的复现方法与结果；`CLAUDE.md` 只链接本文件。不新增研究、计划、交付总结或提示词副本。
