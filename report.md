# FossilSense 1.5.0 全面测试报告

> 状态：原始测试已完成；发布判定 **NO-GO**；整改持续执行中
> 开始时间：2026-07-30 22:59:29 +08:00
> 完成时间：2026-07-31 00:02:07 +08:00（总耗时约 1 小时 03 分）
> 最新整改更新：2026-07-31 01:29:35 +08:00
> 测试对象：`release/v1.5.0`，source commit `fff8f89c045fee1be428472a4d823ee16b223059`

## 1. 范围与判定口径

本次测试以当前源码、测试、清单和脚本为唯一实现事实，覆盖 Rust 引擎、CLI、LSP、VS Code 扩展、架构约束、发布加固，以及真实大型工作区的扫描、全量索引、查询、内存、超时和降级行为。

C 语言大型样本使用真实 U-Boot；Go 大型样本使用 Kubernetes。U-Boot 官方 full-index 硬门禁为 release 进程与引擎输出 `elapsed_ms <= 60,000`；engine hydration 要求至少 500,000 个 declarations 和 10,000 个 files，完整单代读模型不超过 384 MiB，旧快照存活时双代绝对峰值不超过 512 MiB。Go 没有仓库内专用 hydration gate，因此使用同一 release CLI、20 ms 进程采样和可复现查询矩阵评价，不将 U-Boot 专用阈值伪装为 Go 官方门禁。

## 2. 测试环境与样本

| 项目 | 实际值 | 备注 |
|---|---:|---|
| 操作系统 | Windows 11 专业版 Insider Preview 10.0.26220，64 位 | Windows 内存口径使用 Private Bytes |
| CPU | Intel Core i5-12500H，12 核 16 线程 | MaxClockSpeed 3100 MHz |
| 物理内存 | 25,459,482,624 bytes（约 23.71 GiB） | 开始测试时可用约 11.75 GiB |
| Rust | rustc 1.94.1 / cargo 1.94.1 | stable 工具链 |
| Node.js | v24.15.0 | 与仓库声明的 Node.js 22 有偏差 |
| pnpm | 9.15.4 | 与仓库声明的 pnpm 10 有偏差 |
| F: 可用空间 | 10,008,010,752 bytes（约 9.32 GiB） | 克隆和性能数据库生成前 |

版本事实一致：`crates/fossilsense/Cargo.toml` 与 `extensions/vscode/package.json` 均为 `1.5.0`。

当前主仓库工作树在测试前已有非产品源码差异：`CLAUDE.md` 处于删除状态，`AGENTS.md` 为未跟踪文件；本次不会修改或恢复这些用户状态。

### 2.1 U-Boot

| 项目 | 值 |
|---|---|
| remote | `https://github.com/u-boot/u-boot.git` |
| commit | `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c` |
| commit 时间 | 2026-07-10T15:55:23-06:00 |
| tracked files | 38,176 |
| 默认支持扩展文件 | 13,250 |
| 工作树 | dirty：`boot/scene.c` 1 行增/1 行删；`boot/vbe_abrec.c` 2 行删 |

上述 U-Boot 改动是测试开始前已存在的用户状态，本次保留不动。它使样本不能称为严格上游 clean checkout，所有结果均据实标注。

### 2.2 Kubernetes（Go）

| 项目 | 值 |
|---|---|
| remote | `https://github.com/kubernetes/kubernetes.git` |
| commit | `5aef4d9a009d870af0d6abd11e3c648338595a7b` |
| commit 时间 | 2026-07-30T14:15:41Z |
| tracked files | 31,299 |
| Go files | 17,874 |
| C/C++ files | 6 |
| `go.mod` files | 39 |
| checkout 大小 | 332,305,561 bytes（约 316.91 MiB） |
| 工作树 | clean；`git fsck --no-progress` 通过 |

第一次 shallow clone 因 GitHub TLS 流提前结束失败（`curl 56`、`unexpected EOF`）；失败的 clone 未留下目录。第二次使用 HTTP/1.1 与 `--filter=blob:none` 后成功，样本对象和工作树均已验证。该事件属于外部样本获取波动，不计入 FossilSense 产品结果。

### 2.3 历史性能参考

仓库内 `docs/benchmark/1.4.0-final-gates.md` 保存了可复现的 1.4.0 结果：U-Boot 13,244 files / 631,893 symbols，writer 22.464 s，engine full elapsed 31.429 s，数据库 231.43 MiB，重建峰值 Private Bytes 74.34 MiB。该数据用于识别数量级偏差；由于本次 U-Boot commit、文件数、机器和源码版本均不同，它不是严格同机 A/B，不能据此单独判定回归。

## 3. 测试矩阵与结果

### 3.1 静态、单元、集成、扩展与发布门禁

执行命令：

`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify.ps1 -SkipInstall`

结果：**失败（严重发布阻断）**。格式检查和 Clippy 已通过；Rust 测试运行 1002 项，993 passed、3 failed、6 ignored，耗时 40.21 s。脚本因 `cargo test -p fossilsense` 返回 101 立即停止。为区分串联脚本中止与后续门禁本身失败，随后把未运行项目逐项独立补跑。

失败项与原始断言：

| 测试 | 失败断言 |
|---|---|
| `query::tests::compact_name_entry_stays_within_three_ids_and_flags_layout` | `compact entries must not regain per-symbol pointers` |
| `store::tests::read_view_migration::core_symbol_features_route_through_candidate_sets_and_stable_handles` | `src/server/completion_candidate_documentation.rs must route semantic results through new_with_declarations(` |
| `store::tests::read_view_migration::feature_and_cli_call_sites_use_read_views_for_exact_store_queries` | `src/main.rs must route semantic results through CandidateQueryService::new(` |

这些失败发生在 Rust 源码/架构不变量检查中，与 Node.js 24 或 pnpm 9 的环境偏差无关。测试继续执行前先做只读根因定位，不修改产品源码。

补跑结果：

| 门禁 | 结果 |
|---|---|
| `cargo test -p fossilsense --test lsp_smoke` | PASS，2/2 |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS |
| `node scripts/test_architecture_fitness.js` | PASS，8 cases |
| `node scripts/architecture_fitness.js` | PASS（exit 0），0 fail / 14 warn |
| `scripts/test_benchmark_entrypoints.ps1` | PASS |
| `pnpm run test` | PASS，TypeScript compile 与扩展测试均通过 |
| `scripts/test_release_hardening.ps1` | **FAIL**：required document `CLAUDE.md` missing |

架构脚本的 14 个 warning 均为超过 800 行的大文件，最高为 `parser/go.rs` 1639 行，其次 `server/workspace_config.rs` 1036 行；它们不阻断当前脚本，但反映 1.5.0 Go 与工作区实现的维护复杂度增长。

#### VSIX 打包与制品核验

`pnpm run package` 成功完成 release Rust 构建、扩展 bundle、原生二进制 staging、输入指纹生成和 VSIX 打包。生成物为：

| 项目 | 结果 |
|---|---|
| VSIX | `dist/fossilsense-vscode-1.5.0_BUILD20260731_000111.vsix` |
| VSIX 大小 | 6,143,237 bytes（5.86 MiB） |
| VSIX SHA-256 | `76b2c4da3a067fd1aec2353930cf750ab2f231e0f764bebde5f8f57088df76bd` |
| 内嵌引擎 | `fossilsense 1.5.0`，17.97 MiB |
| 引擎 SHA-256 | `fd7cd39e1a2649a18322c4a046b65bd0b7518599c1af3903a88ebe80a813ae57` |
| release-input SHA-256 | `2b61cbc82c9c5f8f74cf14c6ceafcad285abc4561a3f4fc0f4d12d43de1f2bb0`（205 files） |
| artifact-payload SHA-256 | `5922e508e2f17d9a7e4377ea226e28b58fd69a0239d1ee3a0e2dca6a195fc53d` |
| source commit / dirty | `fff8f89c045fee1be428472a4d823ee16b223059` / `true` |

打包后直接执行 `verify_release_hardening.ps1 -Version 1.5.0`，脚本完整检查 VSIX 中的 package version、原生二进制、bundle、manifest、release-input 与 aggregate payload 指纹；唯一报告的失败仍是根 `CLAUDE.md` 缺失。也就是说，制品内部完整性与 1.5.0 版本绑定通过，但当前工作树的文档/cleanliness 发布条件不通过。`release-build.json` 在测试开始时残留的 1.4.5 元数据已经由本次打包正确刷新为 1.5.0，不再是独立问题。

### 3.2 CLI 与语言功能

release 构建通过，`cargo build --release -p fossilsense` 用时 53.54 s；二进制 `--version` 为 `fossilsense 1.5.0`，help 明确声明 C/C++ and Go。

小样本结果：

| 样本 | scan | forced index | 声明 / callable anchors / call sites | elapsed |
|---|---:|---:|---:|---:|
| `samples/mini-c` | 3 files | 3 indexed / 0 skipped | 15 / 7 / 3 | 60 ms |
| `samples/mini-go` | 3 files | 3 indexed / 0 skipped | 6 / 3 / 3 | 35 ms |

两种语言的 `query symbol`、`query def`、`query refs`、`query calls`（incoming/outgoing）共 10 条命令均 exit 0。C definition 在 `hello_value` 使用点只返回当前文件 definition；Go symbol 保留同名 method/free function 和 build guard `windows || tinygo`；references 返回全词文本候选；calls 输出 coverage、confidence、evidence、truncation/scan 证据。

发现两个功能现象：

1. C 与 Go 的 incoming CLI 输出都把每条 relation 显示成 callee 自身，而不是 caller。例如以 `hello_value` definition 为根查询 incoming，唯一结果仍打印 `hello_value`；以 `sensor::Read` 为根时结果也打印 `sensor::Read`。独立诊断确认 relation 数据方向正确，只有 `main.rs:464-475` 无论 direction 都固定格式化 `relation.callee`。LSP incoming 在 `server/call_hierarchy.rs:330-357` 正确使用 `relation.caller`，不受影响。该项是确认的中等严重度 CLI 展示缺陷，见 F-01。
2. `query def` 对 `main.go` 的 `sensor.Read()` 返回 `sensor::Sample::Read` method 与 `sensor::Read` free function 两个 fallback 候选。独立复核确认 CLI 在 `main.rs:329-339` 传入 `current_reach=None`、`reach_graph=None`，因此没有 Go package/import reach 证据；保留同名候选符合“候选不是绑定”的 best-effort 边界。LSP navigation 会传入 server snapshot 的 reach scope/graph，不能把该 CLI 降级外推为编辑器路径失败。可改进点是让 CLI 水合 reach graph 或明确打印降级原因，而不是 hard filter 到唯一结果。

### 3.3 U-Boot 性能与内存

执行命令：

`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -IncludeEngineHydration -CaseFilter u-boot-full-index,u-boot-engine-hydration -TimeoutSeconds 60`

原始报告：

- `target/benchmark/large-workspace-20260730_232413.json`
- `target/benchmark/large-workspace-20260730_232413.md`

Full-index **通过 60 秒硬门禁**：

| 指标 | 1.5.0 本次结果 |
|---|---:|
| wrapper elapsed | 45,031.767 ms |
| engine `elapsed_ms` | 44,740 ms |
| files / indexed / skipped | 13,244 / 13,244 / 0 |
| declarations | 654,890 |
| callable anchors / call sites | 91,919 / 582,522 |
| discover / parse / write | 1,320 / 7,277 / 26,826 ms |
| include edges / secondary indexes / publication | 2,031 / 2,173 / 4,358 ms |
| peak Working Set | 183,435,264 bytes（174.94 MiB） |
| peak Private Bytes | 174,190,592 bytes（166.12 MiB） |
| SQLite database | 419,655,680 bytes（400.21 MiB） |

Engine hydration **通过规模和内存硬门禁**：

| 指标 | 结果 | 门禁 |
|---|---:|---:|
| declarations / files | 654,890 / 13,244 | >= 500,000 / >= 10,000 |
| resident recall bytes | 102,136,088（97.40 MiB） | 计入 semantic budget |
| single private | 192,331,776（183.42 MiB） | <= 384 MiB |
| single peak private | 192,348,160（183.44 MiB） | <= 384 MiB |
| two-generation / absolute peak | 365,854,720（348.91 MiB） | <= 512 MiB |
| first / second build | 3,981 / 3,927 ms | 观察值 |

虽然官方硬门禁全部通过，但与仓库保存的 1.4.0 U-Boot 历史参考相比存在高风险偏差。为排除机器和样本噪声，本次又在 `samples/fossilsense-v1.4.0-baseline` 从 tag `v1.4.0` / commit `d3ea9d412b9e00833a4d6dd55853edc05d309b66` 构建 release 基线，并使用同一台机器、同一 U-Boot 工作树、同一 20 ms 采样脚本、同一 60 秒门限和独立 DB 立即执行 A/B。

严格 A/B 结果：

| 指标 | v1.4.0 同机基线 | v1.5.0 | 变化 |
|---|---:|---:|---:|
| wrapper elapsed | 34,391.517 ms | 45,031.767 ms | +30.94% |
| engine elapsed | 34,113 ms | 44,740 ms | **+31.15%** |
| discover | 1,211 ms | 1,320 ms | +9.00% |
| parse | 4,444 ms | 7,277 ms | **+63.75%** |
| write | 23,292 ms | 26,826 ms | **+15.17%** |
| include edge | 1,983 ms | 2,031 ms | +2.42% |
| secondary index | 2,642 ms | 2,173 ms | -17.75% |
| publication | 0 ms | 4,358 ms | 新增 4.358 s |
| declarations/symbols | 631,893 | 654,890 | +3.64% |
| callable anchors | 91,155 | 91,919 | +0.84% |
| call sites | 582,841 | 582,522 | -0.05% |
| peak Working Set | 83,136,512 B | 183,435,264 B | **+120.65%** |
| peak Private Bytes | 74,457,088 B | 174,190,592 B | **+133.94%** |
| SQLite DB | 242,958,336 B（231.70 MiB） | 419,655,680 B（400.21 MiB） | **+72.73%** |

原始基线报告为 `target/benchmark/v1.4.0-ab/large-workspace-20260730_234356.json`。这个 A/B 使用同一当前 dirty U-Boot checkout，因此三处纯空白变化对两版完全一致。仓库不存在 v1.4.5 tag，所以只能使用可复现的 v1.4.0 发布基线；这一限制不影响“1.5.0 相对 1.4.0 明确退化”的结论。

为排除单次和冷/热顺序偏差，又按 A-B-A-B 各补一次。第二轮原始报告：

- v1.5.0：`target/benchmark/large-workspace-20260730_234902.json`
- v1.4.0：`target/benchmark/v1.4.0-ab/large-workspace-20260730_234956.json`

两次结果范围与双样本中位值：

| 指标 | v1.4.0 范围 / 中位 | v1.5.0 范围 / 中位 | 中位变化 |
|---|---:|---:|---:|
| engine elapsed | 33.108–34.113 s / 33.611 s | 44.049–44.740 s / 44.395 s | **+32.09%** |
| parse | 4.383–4.444 s / 4.414 s | 6.565–7.277 s / 6.921 s | **+56.81%** |
| write | 23.292–23.447 s / 23.370 s | 26.737–26.826 s / 26.782 s | **+14.60%** |
| publication/validation | 0 / 0 | 4.358–4.397 s / 4.378 s | 新增 |
| peak Working Set | 71.85–79.29 MiB / 75.57 MiB | 174.94–183.11 MiB / 179.03 MiB | **+136.91%** |
| peak Private | 63.83–71.01 MiB / 67.42 MiB | 166.12–178.44 MiB / 172.28 MiB | **+155.53%** |
| DB | 231.61–231.70 MiB / 231.65 MiB | 400.21–400.44 MiB / 400.33 MiB | **+72.81%** |

### 3.4 Kubernetes（Go）性能与鲁棒性

#### Scan

使用 release 二进制和 20 ms 外层采样。Kubernetes tracked tree 有 17,874 个 `.go` 与 6 个 C-family 文件；遵循 `.gitignore`、默认排除和 scope 后，FossilSense 实际扫描 17,861 files。

| 指标 | 结果 |
|---|---:|
| exit | 0 |
| elapsed | 4,199.565 ms |
| peak Working Set | 13,242,368 bytes（12.63 MiB） |
| peak Private | 6,316,032 bytes（6.02 MiB） |
| stderr | 0 bytes |

#### Forced full-index

两个全新独立 SQLite DB、同一 release binary、同一 20 ms 采样，180 s 外层保护；两次都低于 60 s 观察线且计数完全一致。

| 指标 | run 1 | run 2 | 中位/说明 |
|---|---:|---:|---:|
| outer elapsed | 49,876.394 ms | 51,946.468 ms | 50,911.431 ms |
| engine elapsed | 49,494 ms | 51,591 ms | 50,542.5 ms |
| files / indexed / skipped | 17,861 / 17,861 / 0 | 相同 | 稳定 |
| declarations | 339,903 | 339,903 | 稳定 |
| callable anchors | 226,316 | 226,316 | 稳定 |
| call sites | 1,182,317 | 1,182,317 | 稳定 |
| discover | 1,565 ms | 1,554 ms | 1,559.5 ms |
| parse | 10,547 ms | 12,409 ms | 11,478 ms |
| write | 22,792 ms | 22,965 ms | 22,878.5 ms |
| include edge | 3,589 ms | 3,400 ms | 3,494.5 ms |
| secondary index | 4,569 ms | 4,659 ms | 4,614 ms |
| publication/validation | 5,654 ms | 5,788 ms | 5,721 ms |
| peak Working Set | 285,007,872 B | 257,454,080 B | 258.67 MiB 中位 |
| peak Private | 287,993,856 B | 259,600,384 B | 261.11 MiB 中位 |
| DB | 501,284,864 B | 501,583,872 B | 478.21 MiB 中位 |
| stderr | 0 | 0 | PASS |

Go 没有仓库内官方 full-index/hydration hard gate；“低于 60 s”在这里是与大仓发布口径一致的观察，不冒充专用门禁。两次最终计数一致，DB 页布局仅相差约 0.06%，说明并行解析/写入没有造成事实不稳定。

#### Incremental no-change index

在 run 1 DB 上不带 `--force` 重跑：17,861/17,861 files 全部 skipped，indexed 0，declarations/call anchors/call sites 保持 339,903 / 226,316 / 1,182,317。engine elapsed 4,072 ms，外层 4,418.072 ms，parse/write/publication 均为 0；peak Private 66,502,656 bytes（63.42 MiB），DB 大小不变且无残留 WAL。增量无变化路径通过。

#### 持久化语义事实与容错

read-only SQLite 核查：

| 事实 | 数量 |
|---|---:|
| Go revisions / C revisions | 17,858 / 3 |
| revision status `ok` | 17,861 |
| lexical fallback revisions | 0 |
| package facts / distinct package names | 17,858 / 1,771 |
| import facts / distinct import paths | 109,123 / 3,662 |
| Go package edges | 38,417 |
| importable packages | 3,884 |
| build-guard files / distinct guards | 2,715 / 399 |
| files with tree-sitter error nodes | 127（0.71%） |
| total / max per-file error nodes | 2,914 / 194 |
| open packages: unresolved import | 3,475 |
| open packages: build constraint unknown | 137 |
| open packages: unsupported language boundary | 7 |

127 个含 error nodes 的文件仍全部为 status `ok`、fallback false，说明容错 AST 路径继续产出事实而未崩溃。CGO `import "C"` 在 vendor 的 10 个文件中被识别，并按 package 聚合为 7 个 `unsupported_language_boundary`；build constraints 被保留为可见 guard。unresolved imports 主要是工作区外依赖/模块边界，系统用 open-package reason 明确降级而非猜测唯一绑定。

#### 真实查询

选择 `cmd/kube-apiserver/apiserver.go:33` 的 `app.NewAPIServerCommand()` 与 `cmd/kube-apiserver/app/server.go:71` definition：

| 查询 | 结果 | 外层/核心指标 |
|---|---|---:|
| symbol `NewAPIServerCommand` | 唯一 definition | 1,507.812 ms 进程；加载 339,903-row name table |
| def at qualified use | 唯一 `app/server.go:71` | 182.539 ms 进程 |
| refs | 5 个 whole-word hits | 2,201.923 ms 进程 |
| calls from `main` | 200 returned / 325 total，3 call sites | relation query 67 ms / 62,956 us |
| calls from `NewAPIServerCommand` | 200 returned / 595 total，16 call sites | 74 ms / 58,204 us |
| incoming to `NewAPIServerCommand` | 3/3 | 33 ms / 28,687 us |

所有查询 exit 0，coverage 均为 17,861 eligible/analyzed、0 fallback，`scan_limited=false`。高重复名 `Run`/`Name` 在 Go 大仓会产生大量 ambiguous candidates，但页面硬限 200，`relations_total_in_scan` 明确暴露被分页尾部，符合 best-effort + bounded contract。incoming 的三条展示仍触发 F-01：数据方向正确，但 CLI 打印三次 callee 自身。

## 4. 严重回归与偏差

### R-01：Rust 全量测试门禁失败（严重）

状态：已确认，可稳定复现；根因已只读锁定，不修复。

当前 1.5.0 在自身 `cargo test -p fossilsense` 门禁中有 3 个失败，覆盖 compact name entry 的内存布局上限，以及 completion documentation、CLI 精确查询必须统一经过 typed read view / `CandidateQueryService` 的架构约束。这会直接阻断仓库完整验证和发布放行。

根因并不是三个独立运行时绕过：

1. **真实布局回归**：`crates/fossilsense/src/query.rs:135-145` 的 `CompactNameEntry` 原有 `i64 + 3 个 u32 ID + kind/role/flags`，commit `853e1497417692afe15b4461233332223a8a16f4` 为 Go/C family 隔离新增独立 `semantic_family` 字段，却没有把它压入已有 flags。`crates/fossilsense/src/query/tests.rs:152-157` 的 `size_of::<CompactNameEntry>() <= 24` 门禁继续有效，因此稳定失败。它会增加每个常驻 NameTable/recall entry 的内存，是大工作区内存预算的实际偏差；最小方向是保留 family 信息但压入现有位域/flags，恢复不超过 24 bytes。
2. **陈旧文本断言**：`completion_candidate_documentation.rs:77-88` 和 `:196-207` 已改用 `CandidateQueryService::new_with_declarations_for_family(...)`，随后仍调用 `semantic_candidates` / `resolve_candidate_handle`。`read_view_migration.rs:117-124` 仍只匹配旧字符串 `new_with_declarations(`，因此是假阳性，不是实际绕过 typed declarations。
3. **陈旧文本断言**：`main.rs:325-338` 已改用 `CandidateQueryService::new_for_family(...)` 并调用 `semantic_candidates`。`read_view_migration.rs:89-92` 仍只匹配旧字符串 `CandidateQueryService::new(`，同样是假阳性。

独立 explorer 还单独重跑了 compact layout 测试，失败可重复。两项陈旧测试应更新为识别 family-aware 构造器；按用户要求，本次只记录诊断，不修改实现或测试。

### R-02：发布加固测试缺少必需文档（严重发布偏差）

状态：已确认；根因已独立复核，不修复。

`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test_release_hardening.ps1` 失败。脚本先成功派生并打印 `version=1.5.0 schema=25 parser=7 resolver=5 relation_protocol=2`，随后 `verify_release_hardening.ps1` 报告 `Required document is missing: ...\CLAUDE.md`。

该删除状态在测试开始前已存在，本次没有删除或恢复它。因此它不是测试过程引入的产品源码回归，但它使当前 1.5.0 工作树无法通过发布硬化门禁，也会影响 release-input 指纹与最终 VSIX 放行。`AGENTS.md` 为未跟踪文件，不能自动替代脚本固定要求的 `CLAUDE.md`。

具体调用链是 `test_release_hardening.ps1:8-22` 调用 `verify_release_hardening.ps1`；后者在 `:374-382` 无条件把根 `CLAUDE.md` 交给 `Require-DocumentPatterns`，缺失即在 `:224-236` 记录失败。脚本不存在 `AGENTS.md` fallback。

需要精确区分影响：`CLAUDE.md`/`AGENTS.md` 不在 `Get-ReleaseInputFingerprint` 的 fixed inputs 或源码 roots 中，因此这次文档变化本身不改变 release-input SHA-256，也不直接进入 VSIX payload；但 hardening 文档门禁会在 VSIX 放行前失败，且 `Get-SourceState` 会把候选标成 `worktreeDirty=true`。因此结论是“当前发布候选不可放行”，而不是 fingerprint 算法或 1.5.0 版本派生错误。

### R-03：U-Boot full-index 性能、DB 与峰值内存回归（严重）

状态：**严格同机同样本 A/B 已确认**；官方 60 秒与 384/512 MiB 门禁仍通过；主要体积与阶段来源已锁定，不修复。

同机同 U-Boot A/B 确认 engine elapsed +31.15%、DB +72.73%、peak Private +133.94%，明显大于 declaration count +3.64%。10.627 s 的 engine 增量主要由新增 publication 4.358 s、writer +3.534 s、parser +2.833 s 构成，secondary index 反而减少 0.469 s。发布评审不能只看 `<60 s` 而忽略该数量级退化。

已证实的数据库分布（SQLite `dbstat`，read-only）：

| 分类 | bytes | 占 DB |
|---|---:|---:|
| `declaration_facts` + 4 个 declaration indexes | 264,290,304 | 62.98% |
| call tables + call indexes | 100,966,400 | 24.06% |
| `member_facts` + member indexes | 27,365,376 | 6.52% |
| 其他 | 27,033,600 | 6.44% |

最大单体是 `declaration_facts` 193,773,568 bytes；随后是 locator index 23,973,888、name index 23,166,976、call-site facts 29,282,304、logical-key index 15,151,104。当前 declaration canonical read model 本身及索引已经大于 1.4.0 的整个 231.43 MiB 数据库，是存储增长的主要、可验证来源。

schema 历史也支持这个结论：v1.4.0 为 schema 15，随后 `ecaf337`→16、`fdc29e2`→17（持久化 declaration facts）、`6b248ed`→18（compact declaration index）、`edc1b73`→19（semantic fact pipeline）、`d5e7c9d`→22（Go parser/persisted facts）、`853e149`→24（Go package isolation）、`46b3e80`→25（Go LSP alignment）。schema 15 的 `symbol_facts` 仅保存 name/kind/role/ranges/signature/guard/container；schema 25 的 `declaration_facts` 还保存 qualified name、两套 range、canonical signature、declarator shape、owner/linkage/language/fidelity/provenance、logical key、locator、backing 等，并新增 name/file/logical-key/locator 四个索引。因此 DB 增长主要是有意语义模型扩展的成本，但当前没有新的存储预算门禁来判断该成本是否可接受。

阶段根因的精确边界：

- 本次 benchmark 传入显式 `--db`，所以 `indexer.rs:326-334` 的 `publication_ms=4,358` 实际计时 `checkpoint_full_rebuild()`，不是 server semantic index hydration。`store.rs:271-275` 先调用 `validate_full_build()`（SQLite `quick_check(1)` 与 `foreign_key_check`），再 checkpoint；该显式 DB 末尾校验由 commit `e779f290dfc9d2e85dd00caca228c0e997ffd78e` 新增。v1.4.0 对此分支没有末尾校验，所以 publication 为 0。校验是当前 full-build 原子发布契约的一部分，耗时属于必要正确性成本，但仍必须计入 end-to-end 回归。
- 同一 `e779f29` 把 full rebuild 从 v1.4.0 的 WAL/NORMAL 改为 `journal_mode=MEMORY`、`synchronous=OFF`、exclusive locking、`temp_store=MEMORY`、32 MiB SQLite page cache。加上 DB 从 231.70 MiB 增到 400.21 MiB，这是 full-index 进程 peak Private 从 71.01 MiB 增到 166.12 MiB 的直接源码相关因素。CLI `index` 不构建 server `SemanticDeclarationIndex`，所以不能把这次索引峰值误归因于 hydration NameTable；各内存分项没有单独采样，精确贡献仍是推断。
- v1.4.0→HEAD 的 parser diff 为约 7,048 insertions / 1,575 deletions，关键 C-family 提交包括 `7f321d4` 的 declaration facts、`fdc29e2` 的持久化消费、`edc1b73` 的 semantic pipeline；当前 U-Boot 没有 Go 输入，因此 `d5e7c9d` 的 Go parser 代码本身不进入该 parse 样本。parse +63.75% 与 C-family 额外 declaration/object/member/alias/signature/initializer 事实提取一致，但尚无 per-extractor profiler，具体占比是推断。

编译器 `-Zprint-type-sizes` 输出确认 `query::CompactNameEntry` 为 32 bytes（alignment 8），而测试门禁要求 <=24。按 654,890 declarations 计算，单代仅此布局增长增加 5,239,120 bytes（4.996 MiB，占 measured recall 5.13%），双代约 9.993 MiB。它是真实且应修复的局部内存回归，但不足以解释本次 peak Private 或 DB 的大幅增长。

U-Boot dirty diff 仅有三处空白行变化：一处空白行改为含制表符、两处删除空白行，没有宏、声明或控制流变化，不能解释 declarations +22,997 或 DB +168.78 MiB。

根因 explorer 两次因模型容量不足退出，独立 reviewer 的页级扫描也在长时间无响应后被中断；委托行为已执行但未获得可用结论。上述诊断由主代理使用只读 `sqlite3 -readonly`、schema diff、git history 和编译器布局输出完成，没有修改数据库或源码。

此外已记录两项环境偏差（Node/pnpm 版本）与 U-Boot 样本 dirty 状态；它们已作为有效性限制纳入本报告，不会被隐去。

## 5. 观察到的问题

- `extensions/vscode/bin/release-build.json` 在测试开始时仍记录 `packageVersion: 1.4.5`，与当前清单 1.5.0 不一致。本次 `pnpm run package` 已将其刷新为 1.5.0，内嵌副本及所有 payload hash 均通过 hardening 校验；该观察项已关闭。发布加固仍因 R-02 的 `CLAUDE.md` 缺失失败。

### F-01：`query calls --incoming` 打印 callee 而非 caller（中等）

复现于 mini-c 与 mini-go。`CallRelationService::query_at` 会把 requested direction 传入查询；`CallCatalog::relation_page` 在 `call_catalog.rs:444-462` 正确选择 incoming map；`materialize_relation` 在 `:568-599` 同时保留真实 caller/callee。已有 call service 测试断言 incoming caller 正确。

错误只在 CLI presentation：commit `a9f677e7` 引入的 `main.rs:464-475` 始终读取 `relation.callee`。LSP call hierarchy incoming 在 `server/call_hierarchy.rs:330-357` 使用 `relation.caller`，所以引擎数据与编辑器路径未受影响。当前缺少 CLI calls 输出测试。按用户要求仅记录：最小修复方向是 incoming 选择 caller、outgoing 选择 callee，并增加 CLI 输出回归测试。

## 6. 结论

**FossilSense 1.5.0 当前发布候选判定为 NO-GO，不建议发布。**

阻断理由有三项：

1. `cargo test -p fossilsense` 有 3 项失败，导致仓库标准 `verify.ps1` 无法完成。其中一项是 `CompactNameEntry` 从不超过 24 bytes 增至实测 32 bytes 的真实常驻召回结构布局回归；另两项是 family-aware 构造器演进后未同步的陈旧文本断言。无论运行时影响如何分类，发布测试门禁本身当前不绿。
2. `verify_release_hardening.ps1 -Version 1.5.0` 因必需的根 `CLAUDE.md` 缺失而失败，生成的 VSIX 被标记 `worktreeDirty=true`。VSIX 内部版本、二进制与指纹一致，但不能据此绕过仓库发布硬化规则。
3. 同机、同 U-Boot、A-B-A-B 的 release 对照确认，1.5.0 相对 v1.4.0 的 engine full-index 中位耗时 **+32.09%**、SQLite DB **+72.81%**、peak Private **+155.53%**。1.5.0 仍以 44.395 s 中位通过 60 s 绝对门禁，hydration 也通过 384/512 MiB 门禁，但这不抵消相对上一可复现发布基线的严重退化。数据库增长已锁定到扩展后的 canonical declaration facts 与四个索引；耗时增长主要来自新增 publication/validation、writer 与 C-family parser 事实提取。

功能侧总体结果较好：mini C/Go、LSP smoke、扩展测试、架构可执行门禁、U-Boot hydration、Kubernetes 两次全量索引与无变化增量索引均完成且没有崩溃。Kubernetes 17,861-file 实际工作集两次全量索引中位 50.543 s，事实计数完全一致，所有 revision 均为 `ok`，复杂 build constraints、CGO 边界和 unresolved imports 均以显式证据降级；真实 definition/references/call 查询保持有界并暴露 coverage/truncation。确认的功能缺陷只有 F-01：CLI incoming calls 展示 callee 而非 caller，底层 relation 与 LSP 路径不受影响。

第 1–6 节记录的原始全面测试轮次按当时要求没有修复任何产品源码、测试或发布配置；长期整改实现从第 7 节开始。测试开始前已有的 `CLAUDE.md` 删除和未跟踪 `AGENTS.md` 在阶段 1、2 中保持原状；阶段 3 恢复前者，后者继续保留且不纳入提交。

## 7. 1.5.0 长期整改进度

### 阶段 1：恢复 Rust 门禁并压缩常驻召回布局

状态：阶段完成；实现、正确性/大型工作区门禁和独立代码审查均已通过，本节随阶段提交记录。

本阶段按 TDD 修复 R-01。首先复现 `compact_name_entry_stays_within_three_ids_and_flags_layout` 的 32-byte 失败，并新增 `compact_name_flags_round_trip_family_and_scope_evidence`，在实现 `CompactNameFlags` 前确认测试因缺少目标类型而失败。随后把 `semantic_family`、`external`、`directly_included` 压入一个透明 `u8` 位域；`CompactNameEntry` 恢复为 24 bytes，同时保留 C/Go family 和外部可达性证据。

独立审查未发现阻断问题，并确认 full build、增量 delta、项目上下文重建、compaction 和 dirty include overlay 都经过同一编码/解码闭环。审查指出 `directly_included=true && external=false` 的非法组合尚无明确策略；新增测试先复现后，构造器现统一执行 `directly_included => external` 归一化，避免 workspace declaration 携带无效的 direct-external 证据。审查后的全量 Rust 测试、格式和 Clippy 已再次通过。

两条报告已知的陈旧架构断言已改为识别 `new_for_family` 与 `new_with_declarations_for_family`。修正前置断言后，同一测试还暴露一个此前被级联 panic 隐藏的陈旧要求：`candidate_service/semantic.rs` 已调用 `exact_name_hits_scoped_for_family`，但测试仍只匹配 `exact_name_hits_scoped`。该门禁也已同步到 family-aware 路径；typed read view、stable handle、`semantic_candidates` 与 `payloads_by_ids` 要求均未放宽。

正确性验证：

| 命令/门禁 | 结果 |
|---|---|
| `cargo test -p fossilsense query::tests::compact_name_` | PASS，布局、位域往返、C/Go 预算前过滤共 3 项 |
| `cargo test -p fossilsense store::tests::read_view_migration` | PASS，4/4 |
| `cargo test -p fossilsense` | PASS，exit 0 |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS |
| `cargo build --release -p fossilsense` | PASS |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS |

大型工作区验证命令：

`powershell -NoProfile -ExecutionPolicy Bypass -File scripts/benchmark_large_workspace.ps1 -Repeats 1 -IncludeFullIndex -IncludeEngineHydration -CaseFilter u-boot-full-index,u-boot-engine-hydration -TimeoutSeconds 60`

原始结果：

- `target/benchmark/large-workspace-20260731_010714.json`
- `target/benchmark/large-workspace-20260731_010714.md`

| 指标 | 原报告 1.5.0 | 阶段 1 | 变化 |
|---|---:|---:|---:|
| resident recall bytes | 102,136,088（97.40 MiB） | 93,747,480（89.40 MiB） | -8,388,608 bytes（-8.21%） |
| single private | 192,331,776（183.42 MiB） | 183,623,680（175.12 MiB） | -8,708,096 bytes（-4.53%） |
| two-generation / absolute peak | 365,854,720（348.91 MiB） | 349,024,256（332.86 MiB） | -16,830,464 bytes（-4.60%） |
| first / second hydration build | 3,981 / 3,927 ms | 3,829 / 3,798 ms | 观察值下降 |
| full-index engine `elapsed_ms` | 44,740 ms | 43,965 ms | -1.73%（单次观察） |
| full-index wrapper elapsed | 45,031.767 ms | 44,999.572 ms | -0.07%（单次观察） |
| full-index peak Private | 174,190,592 bytes | 180,518,912 bytes | +3.63%（单次进程采样波动） |
| SQLite database | 419,655,680 bytes | 419,995,648 bytes | +0.08% |

阶段 1 的确定性收益是 compact vector 容量对应的常驻 recall 减少 8 MiB，双代并存绝对峰值减少约 16.05 MiB；U-Boot 的 654,890 declarations、13,244 files 规模门禁以及 60 秒、384 MiB、512 MiB 硬门禁全部通过。full-index 进程峰值和数据库没有随该内存布局修复下降，说明 R-03 的主要存储/进程峰值来源仍在 canonical declaration facts、索引、发布校验及其他运行时结构中，继续保持开放，不用阶段 1 的局部收益替代后续归因。

### 阶段 2：修复 CLI incoming call relation 展示

状态：阶段完成；F-01 已由真实二进制集成测试复现、修复并完成阶段验证，本节随阶段提交记录。

新增 `crates/fossilsense/tests/cli_calls.rs`，在临时 C 工作区创建 `target` 与调用它的 `caller`，通过实际 `fossilsense index` 建库后分别执行 incoming/outgoing CLI 查询。修复前测试稳定失败：incoming 的 `root` 和关系行都打印 `target`，尽管 relation 数量、call site 和底层 caller 数据正确。测试只匹配制表符分隔的关系行，避免被 `root`、coverage 或统计输出误判。

`main.rs` 的 presentation 现按每条 `CallRelation.direction` 选择 counterpart：incoming 使用必有的 `relation.caller`，outgoing 继续使用可选的 `relation.callee`，未解析 outgoing 仍显示 `<unresolved>`。该修改不触及 call catalog、candidate ranking、confidence/evidence、分页、LSP call hierarchy 或扩展模型。

验证结果：

| 命令/门禁 | 结果 |
|---|---|
| `cargo test -p fossilsense --test cli_calls -- --nocapture`（修复前） | FAIL，incoming 关系行错误打印 `target` |
| 同一命令（修复后） | PASS，1/1；incoming=`caller`、outgoing=`target` |
| `cargo test -p fossilsense` | PASS：unit 997 passed / 6 ignored；CLI integration 1/1；LSP smoke 2/2 |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS |

本阶段是 stdout presentation 修复，没有改变索引、存储、查询复杂度或常驻读模型，因此不重复运行大型工作区性能门禁。

### 阶段 3：恢复发布文档与完整仓库门禁

状态：R-02 已修复，仓库完整验证通过；干净提交上的 VSIX 打包与制品 hardening 待执行。

`scripts/test_release_hardening.ps1` 在阶段开始时再次稳定失败，首个错误为根 `CLAUDE.md` 缺失。进一步核对 `verify_release_hardening.ps1` 的 `Require-DocumentPatterns` 后确认，仅恢复提交树旧版仍会因为旧文档写 `1.4.5` 而缺少当前 release version `1.5.0`。因此恢复完整的跟踪文档，并只把项目版本事实从 `1.4.5` 同步到 `1.5.0`；没有放宽 hardening，也没有用未跟踪的 `AGENTS.md` 替代长期文档。

验证结果：

| 命令/门禁 | 结果 |
|---|---|
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test_release_hardening.ps1`（恢复前） | FAIL：required document `CLAUDE.md` missing |
| 同一命令（恢复并同步版本后） | PASS |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify.ps1 -SkipInstall` | PASS |
| Rust tests | unit 997 passed / 6 ignored；CLI integration 1/1；LSP smoke 2/2 |
| architecture fitness | golden 8/8；0 fail / 14 large-file warn |
| benchmark entry-point tests | PASS |
| extension `pnpm test` | PASS，TypeScript compile 与扩展测试通过 |

完整验证期间没有修改产品源码。下一步先提交恢复后的文档与本节记录，再从该干净提交创建隔离 worktree 打包，避免主工作树中用户保留的未跟踪 `AGENTS.md` 被写入 `release-build.json.worktreeDirty`。打包和最终制品 hardening 通过后再进入 R-03 性能/存储优化。
