# FossilSense 1.5.0 全面测试报告

> 状态：原始测试已完成；发布判定 **NO-GO**；整改持续执行中
> 开始时间：2026-07-30 22:59:29 +08:00
> 完成时间：2026-07-31 00:02:07 +08:00（总耗时约 1 小时 03 分）
> 最新整改更新：2026-07-31 04:51:43 +08:00
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

状态：阶段完成；R-02、仓库完整验证、干净提交打包与制品 hardening 均已通过。

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

完整验证期间没有修改产品源码。恢复后的文档与首轮记录提交为 `ce2858fe08e0d4fa3ea21d1232048456b27e40d2`；随后从该提交创建隔离的干净 worktree 打包，避免主工作树中用户保留的未跟踪 `AGENTS.md` 被写入 `release-build.json.worktreeDirty`。

阶段制品：

| 项目 | 值 |
|---|---|
| VSIX | `dist/fossilsense-vscode-1.5.0_BUILD20260731_013155.vsix` |
| VSIX SHA-256 | `6bc403605b21500d6926d88c3e12d1ad07f9f961390952ccedd7523fbcefdcd5` |
| source commit | `ce2858fe08e0d4fa3ea21d1232048456b27e40d2` |
| `worktreeDirty` | `false` |
| release-input SHA-256 / file count | `d6598a50949ed32ac2518daec6b304f30f9fb8752dce5d36e912d2a1e7d734c1` / 205 |
| artifact-payload SHA-256 | `44569a08002460e5f0211eee0a0a0e73af48c98c66e332aa357c4cef0089776b` |
| `verify_release_hardening.ps1 -Version 1.5.0` | PASS |

该 VSIX 证明 1.5.0 自包含打包链路和 R-02 修复有效，但不是最终发布制品：后续任何 Rust、扩展或 release-input 改动都会使它按仓库规则作废，必须在 R-03 与最终门禁结束后重新生成。

下一阶段进入 R-03：以现有 A/B 数据为基线，先把数据库增长、full-build writer 峰值、publication 校验和 parser 提取拆成可独立度量的成本，再选择不削弱原子发布/完整性契约的优化点。

### 阶段 4A：full-build SQLite cache 参数实验（拒绝）

状态：实验完成并回退；没有产品源码或测试改动进入提交，仅保留可复现证据。

cache 实验开始前，对当时的 `target/benchmark/index-u-boot-rebuild.sqlite` 做只读 `dbstat`：文件 419,995,648 bytes，4 KiB page、102,538 pages、freelist 0。`declaration_facts` 为 193,822,720 bytes；`idx_declaration_facts_name` 23,212,032、`idx_declaration_facts_file_id` 8,224,768、`idx_declaration_facts_logical_key` 15,290,368、`idx_declaration_facts_locator` 24,080,384，五项合计 264,630,272 bytes。该工作数据库随后按 benchmark 入口的既定行为被后续运行覆盖，因此这组数字是实时记录的历史快照，不作为阶段 4B 的精确 A/B 基线；后续严格对比使用与 32 MiB JSON 同次保留的 SQLite。CLI explicit full-index 不构建 server `SemanticDeclarationIndex`；可直接影响进程峰值的局部参数包括 MEMORY rollback journal、`temp_store=MEMORY`、32 MiB page cache、解析线程/批次缓冲和 full-build call-string map。

首先按 TDD 给 `full_build_defers_call_indexes_until_facts_are_complete` 增加 cache-size 断言，然后依次试验 8、16、24 MiB，始终保留 MEMORY journal、exclusive locking、`synchronous=OFF`、deferred indexes、`quick_check(1)`、`foreign_key_check` 和 checkpoint。8 MiB 的正确性测试通过，但 U-Boot full-index 被 60 秒硬门禁终止，因此立即否决。16/24 MiB 的事实计数均保持 13,244 files、654,890 declarations、91,919 callable anchors、582,522 call sites。

| page cache | engine / wrapper | write | peak Private | 判定 |
|---:|---:|---:|---:|---|
| 8 MiB | 超过 60 秒，脚本终止 | 未形成报告 | 未形成报告 | 硬失败 |
| 16 MiB | 51,352 / 51,631.852 ms | 34,688 ms | 162,394,112 bytes | 通过绝对门禁，但写入代价过高 |
| 24 MiB 第一次 | 46,337 / 46,659.794 ms | 29,717 ms | 172,326,912 bytes | 通过 |
| 24 MiB 第二次 | 46,818 / 47,062.708 ms | 30,270 ms | 162,361,344 bytes | 通过，但峰值与第一次相差约 9.5 MiB |
| 32 MiB 同机基线 | 42,862 / 43,162.560 ms | 26,157 ms | 172,347,392 bytes | 更快，且相邻峰值与 24 MiB 第一次等价 |

原始结果：

- `target/benchmark/large-workspace-20260731_015014.json`（16 MiB）
- `target/benchmark/large-workspace-20260731_015311.json`、`large-workspace-20260731_015459.json`（24 MiB）
- `target/benchmark/stage4a-cache-ab/cache32-run1/large-workspace-20260731_015645.json` 与同目录 `index-u-boot-rebuild.sqlite`（阶段 3 干净 VSIX 内 32 MiB 基线二进制及同次数据库）

结论：缩小 cache 可在部分运行中降低采样峰值，但 24 MiB 的两次 peak Private 相差约 10 MiB，同机 32 MiB 相邻运行又与其第一次几乎相同，无法把变化可靠归因于 cache；反之 write/engine 变慢 3.5–4.0 秒可重复，8 MiB 还直接违反 60 秒门禁。因此该参数调整风险收益不成立，`store.rs` 与维护测试已完整回退到阶段前状态。

下一阶段转向可确定计量且有望同时减少 DB、写放大和索引构建时间的方案：先通过 SQL 消费者审计与 schema 回归测试确认 `idx_declaration_facts_locator` 没有 SQL 消费者，再把它作为独立 schema 26 变更移除；name/logical-key 复合索引另行评估，不与 locator 改动混在同一阶段。

### 阶段 4B：移除未消费的 declaration locator 索引

状态：阶段完成；实现、TDD、完整 Rust 门禁、U-Boot full-index 和独立 reviewer 均已通过。

源码查询链复核只发现 `locator_fingerprint` 被写入 declaration fact、从 typed read view 水合并在候选合并时进行内存比较；没有 SQL `WHERE`、`JOIN` 或 `ORDER BY` 消费该字段。这个结论只支持删除查找索引，不支持删除字段：locator 仍是声明稳定身份和 overlay 合并契约的一部分。

先新增 `opening_schema_25_rebuilds_without_locator_lookup_index` 回归测试。修改前该测试稳定失败，实际 schema version 为 `25`、期望为 `26`；随后把 schema 提升到 26，并从 `CREATE_LOOKUP_INDEXES_SQL` 删除 `idx_declaration_facts_locator`。修改后的定向测试通过，同时验证旧 schema 25 声明事实会被迁移机制安全失效、`locator_fingerprint` 列仍保留、旧 locator 索引不会被重新创建。当前改动没有更改 locator payload、typed read model、查询排序或候选语义。

独立 test-executor 顺序执行的门禁全部通过：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS，1.914 s |
| `cargo test -p fossilsense` | PASS，48.673 s |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS，9.551 s |
| `cargo build --release -p fossilsense` | PASS，55.839 s |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS，101.527 s |
| U-Boot release full-index | PASS，wrapper 36,882.710 ms；engine 35,804 ms，均低于 60,000 ms |

本轮 U-Boot 仍为 13,244 files、654,890 declarations、91,919 callable anchors、582,522 call sites，未发生事实数量退化；分段耗时为 discover 1,043 ms、parse 6,203 ms、write 19,573 ms、include edge 1,941 ms、secondary index 2,112 ms、publication 4,170 ms。峰值 Working Set 163,180,544 bytes，Private Bytes 157,540,352 bytes。原始结果为 `target/benchmark/large-workspace-20260731_020735.json` 与同名 Markdown。

页级结果与修改目标一致：schema 为 26、page size 4,096、page count 96,593、freelist 0，`idx_declaration_facts_locator` 对象数为 0。严格使用阶段 4A 保留的同次 32 MiB schema 25 数据库对比：page count 102,440、数据库 419,594,240 bytes、旧 locator 索引 24,076,288 bytes；阶段 4B 数据库为 395,644,928 bytes，净减 23,949,312 bytes（5.71%）。主要降幅可直接归因于移除 locator 索引，其余 declaration 对象保持同一数量级：fact table 193,830,912 bytes、name index 23,195,648、file-id index 8,224,768、logical-key index 15,142,912。

与阶段 4A 的 32 MiB 同机 schema 25 样本相比，本轮观察到 wrapper 从 43,162.560 降至 36,882.710 ms、engine 从 42,862 降至 35,804 ms、write 从 26,157 降至 19,573 ms、Private Bytes 从 172,347,392 降至 157,540,352。单次进程采样和机器状态仍可能影响时间/内存，因此这里只把数据库页减少视为强归因结果，不把全部耗时和峰值改善宣称为 locator 索引的确定收益。

独立 reviewer 未发现产品代码、schema 兼容性或 locator 语义问题；其唯一 P3 finding 是初稿混用了阶段 4A 历史工作库与保留的 32 MiB A/B 工件，上述页数、字节和百分比已统一按保留工件修正。残余风险是底层 `IndexStore::migrate` 本身没有显式事务：默认工作区索引通过 side-by-side staging、完整性检查和 manifest 切换保护旧读者，但显式 `--db` 诊断路径若在 DDL 中途进程崩溃，仍可能留下需要再次重建的部分数据库。这是既有迁移架构风险，不由本阶段引入，后续应以故障注入和 crash-atomic 边界为独立阶段评估。

### 阶段 4C：declaration name/logical-key 索引拓扑实验（拒绝）

状态：阶段完成；保持 schema 26 现有两个索引，不修改产品源码。

源码审计确认，生产 logical-key 查询只有 `by_logical_key_family_limited`：SQL 同时约束 `name`、`logical_key_digest` 和 language，并按 declaration `id` 排序、最多读取 `exact_name_limit + 1`，当前默认 limit 为 256。普通精确名称查询同样按 `id` 排序并限量。SQLite 二级索引隐含 rowid，因此单列 name 索引可以在 `name = ?` 时直接按 `id` 有界返回；单列 logical-key 索引也可以在 digest 等值时直接按 `id` 返回。全仓没有只按 logical-key digest 查询的 SQL，但这不等于可以无代价删除该索引：删除后，limit 只能限制输出行，不能限制为找到 digest 而访问的全部同名行。

当前 U-Boot schema 26 数据库包含 654,890 declarations、481,059 个 distinct names、584,103 个 distinct logical digests，distinct `(name, digest)` 组合也是 584,103。最高碰撞名称为 `MBEDTLS_PRIVATE`（753 行），其后为 `LOG_CATEGORY` 416、`__packed` 415、`CFG_SYS_SDRAM_BASE` 406；当前 name 索引为 23,195,648 bytes，logical-key 索引为 15,142,912 bytes。

在 `target/benchmark/stage4c-index-lab/` 的隔离副本中比较三种拓扑，canonical benchmark DB 与 U-Boot 样本均未改动：

| 方案 | 空间结果 | 查询计划/实测 | 判定 |
|---|---:|---|---|
| 保持 name + logical 单列索引 | 38,338,560 bytes | name-only 与 exact logical 均直接按对应索引和 rowid 返回 | 保留 |
| 保留 name、删除 logical | 节省 15,142,912 bytes | `MBEDTLS_PRIVATE` 的 name+logical 热查询从 0.0356 ms 增至 0.2761 ms，约 7.8 倍；最坏访问量随同名总数增长 | 拒绝 |
| 用 `(name, logical_key_digest)` 复合索引替换两者 | 复合索引 28,766,208 bytes，净省 9,572,352 bytes | logical 查询更精确，但 name-only `ORDER BY id` 出现 `USE TEMP B-TREE FOR ORDER BY` | 拒绝 |

复合索引方案的 name-only 实测也确认排序代价：`MBEDTLS_PRIVATE` 在 LIMIT 101 时热查询由 0.0740 增至 0.2064 ms，LIMIT 501 时由 0.2435 增至 0.4918 ms；不存在名称的热查询由 0.0340 增至 0.0393 ms。虽然当前样本的绝对延迟仍小，但大型工作区查询必须有界，不能用一次 U-Boot 的低毫秒结果放行随同名行数增长的扫描或排序。保留 name 与复合索引又会比当前两个单列索引多占约 13.6 MB，因此也没有空间收益。

这些微测只选择底表 declaration `id`，没有包含 active-revision/file joins、language predicate 和完整 row hydration；它们用于确认 planner、访问量与相对退化方向，不用于宣称 completion resolve 的端到端延迟。

结论：阶段 4B 已移除真正没有 SQL 消费者的 locator 索引；name 单列索引保护常规 exact-name 请求热路径，logical-key 单列索引保护低频但仍须有界的 stale-overlay resolve fallback。继续删除或直接合并会用最坏情况查询复杂度换取 9–15 MB 空间，不符合“大仓库优先”和候选查询必须有界的架构约束。当前 digest 已是固定 12-byte BLOB；若后续仍要压缩这 15 MB，更保守的未测方向是保留完整字段和最终相等校验，只对确定长度的 digest 前缀建立表达式索引，并让 SQL 同时约束前缀与完整 digest。该方案不改变 persisted fact payload，但前缀碰撞桶、planner 稳定性、`ORDER BY id`、实际页节省和端到端 fallback 都必须作为独立阶段验证，不能从本阶段结果直接放行。

### 阶段 4D：logical digest 前缀表达式索引实验（拒绝）

状态：阶段完成；没有修改产品源码、schema 或测试。

当前 `logical_key_digest` 是 `serde_json(LogicalEntityKey)` 的 BLAKE3 前 12 bytes，schema 强制为固定 12-byte BLOB；SQL 用完整 digest 等值查找后，Rust 仍以完整 `LogicalEntityKey` 做最终碰撞复核。阶段 4D 在 `target/benchmark/stage4d-prefix-lab/` 的隔离副本中保留完整字段和最终校验，只把 12-byte lookup index 替换为 `substr(logical_key_digest, 1, N)` 表达式索引，并让 SQL 同时约束前缀、完整 digest、name 与 language。SQLite 3.41.2 对四种确定性 `substr` 表达式索引都能稳定选择，`ORDER BY id` 未出现临时 B-tree，因此实验没有被兼容性或 planner 偶然失配提前否决。

基线为 654,890 declarations、完整 digest 索引 15,142,912 bytes、数据库文件与 `dbstat` live pages 均为 395,644,928 bytes：

| 前缀长度 | 前缀索引 | 相对完整索引节省 | `dbstat` live bytes | live-page 降幅 | distinct prefix / full digest |
|---:|---:|---:|---:|---:|---:|
| 4 bytes | 8,527,872 | 6,615,040 | 389,005,312 | 1.68% | 584,071 / 584,103 |
| 6 bytes | 9,842,688 | 5,300,224 | 390,320,128 | 1.35% | 584,103 / 584,103 |
| 8 bytes | 11,157,504 | 3,985,408 | 391,634,944 | 1.01% | 584,103 / 584,103 |
| 10 bytes | 12,492,800 | 2,650,112 | 392,970,240 | 0.68% | 584,103 / 584,103 |

隔离副本通过 drop/create 索引形成，没有执行 `VACUUM`：四个文件的物理长度仍为 395,644,928 bytes，差额分别成为 1,621、1,300、979、653 个 freelist pages。上表只量化 live allocated pages；fresh full rebuild 能否形成同样的最终文件长度没有实测，因此不据此宣称物理文件已经缩小。

4-byte 前缀在当前样本出现 32 个双-digest 前缀碰撞桶，共涉及 64 个不同 full digests，使 distinct prefix 比 distinct full digest 少 32；6/8/10-byte 在当前 U-Boot 没有不同 digest 的前缀碰撞，但相同 digest 的重复声明仍使最大行桶达到 321。该样本事实不能证明其他大型或对抗性工作区不会出现不同 digest 的同前缀桶。完整 digest 不存在但共享现有高密度前缀时，底表 `id` 微测的热中位数从完整索引的 0.0227 ms 增至前缀索引的 0.1572–0.2347 ms；8-byte 方案为 0.1615 ms。普通存在键几乎不变，而最高重复 digest 的 8-byte 方案由 0.2107 增至 0.3171 ms。微测只用于访问方向判断，不代表带 joins、language filter 和 typed row hydration 的端到端 resolve 延迟。

核心风险不是当前样本的低毫秒绝对值，而是 SQL `LIMIT 257` 只限制满足完整 digest/name/language 的输出，不能限制前缀索引在完整 digest 过滤前访问的候选；缺失或稀疏匹配会扫描整个前缀桶。8-byte/10-byte 前缀的随机碰撞概率很低，但随机概率不能替代请求扫描上界，也不能证明对任意大型工作区的最坏情况。为此引入扫描预算又必须向上暴露 truncation/coverage，并扩大 resolver 协议与语义范围。

结论：最激进的 4-byte 方案只减少 live allocated pages 1.68% 且已出现不同 digest 碰撞；风险较低的 8-byte/10-byte 仅减少 1.01%/0.68%。收益不足以交换更宽的碰撞桶、缺失键退化和新的不确定性协议，因此保持完整 12-byte digest 等值索引。阶段 4C reviewer 提出的保守候选已经实测关闭，不进入 TDD 或大型门禁。

### 阶段 4E：压缩 declaration 固定宽度标量（已完成）

状态：实现、reviewer findings 的 TDD 修复、全部最终门禁和修复后独立复核均已通过。

列级审计确认当前 producer 生成的 locator/guard fingerprint 都是 BLAKE3 截断后的 24 个小写十六进制字符；上层 semantic model、CandidateHandle 和 LSP completion data 仍以 String 暴露，但 SQLite 不需要用 24-byte TEXT 保存等价的 12-byte 值。`backing_kind` 同样只表示 `callable_anchor`、`record`、`type_alias`、`source_range`、`none` 五种稳定形态，当前 U-Boot/Kubernetes 实际只出现前四种，却在每行重复保存 6–15 个字符。三列都不参与 SQL filter、join 或排序，因此可以只改变持久化编码，不改变查询拓扑。

U-Boot 654,890 行中 locator payload 为 15,717,360 bytes，guard fingerprint 非空 38,532 行、payload 924,768 bytes，backing-kind TEXT payload 8,005,683 bytes；Kubernetes 339,903 行的 locator 全部满足 24-char hex、guard 全部为空，backing kind 同样只有四个值。`target/benchmark/stage4e-scalar-lab/` 的隔离 shadow-table 实验保留所有非目标列并抽查首/中/尾行的严格 hex 往返、NULL 与 kind mapping：

| 样本 | 原 declaration table | compact shadow | 页级减少 |
|---|---:|---:|---:|
| U-Boot | 193,830,912 bytes / 47,322 pages | 176,832,512 / 43,172 | 16,998,400 bytes（8.77%） |
| Kubernetes | 141,946,880 bytes / 34,655 pages | 132,763,648 / 32,413 | 9,183,232 bytes（6.47%） |

实现先新增 `declaration_storage_compacts_fingerprints_and_backing_kind_without_changing_views`。修改前定向测试因 `backing_kind` 仍是 SQLite TEXT 稳定失败，另一个 schema 26→27 测试实际得到 version 26、期望 27。随后 schema 27 把 locator 与 nullable guard fingerprint 改为带 12-byte CHECK 的 BLOB，把 backing kind 改为 0–4 CHECK INTEGER；write path 复用严格 hex decoder，read view 恢复原小写 24-char String 和原 backing-kind 字符串。定向测试现已验证 guarded/null guard、locator/guard 精确往返、四种实际 backing 的 raw code 与 typed enum 一致，以及 schema 25/26 都会失效重建。当前没有修改 locator/guard 算法、CandidateHandle JSON、overlay 比较、logical-key lookup 或最终候选语义。

完整门禁中，第一轮 Clippy 唯一发现 `backing_kind` 已是 `&str` 却再次借用的 `needless_borrow`；机械修正后由主代理和独立 test-executor 分别复跑通过。最终结果：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo test -p fossilsense` | PASS，单元与集成合计 1005 passed / 6 ignored |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS |
| `cargo build --release -p fossilsense` | PASS |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS |
| U-Boot full-index | PASS，wrapper 37,598.159 ms；engine 36,528 ms，均低于 60,000 ms |
| U-Boot engine hydration | PASS，654,890 declarations / 13,244 files；单代 183,693,312 bytes，双代 348,549,120 bytes |

U-Boot 事实计数保持 13,244 files、654,890 declarations、91,919 callable anchors、582,522 call sites。parser fact 8 最终 full-index 分段为 discover 1,150 ms、parse 6,242、write 20,021、check 10、include edge 1,992、secondary index 2,120、publication 4,286；峰值 Working Set 171,794,432 bytes、Private Bytes 163,610,624 bytes。hydration recall 仍为 93,747,480 bytes，首/次代构建 3,872/3,884 ms；单代约 175.2 MiB、双代约 332.4 MiB，分别低于 384/512 MiB 硬线。最终原始报告为 `target/benchmark/large-workspace-20260731_034326.json` 与同名 Markdown。

最终 parser fact 8 / schema 27 U-Boot 数据库为 378,793,984 bytes、92,479 pages、freelist 0。与阶段 4B 同机 schema 26 的 395,644,928 bytes 相比，物理文件净减 16,850,944 bytes（4.26%）；`declaration_facts` 从 193,830,912 降到 176,820,224 bytes，净减 17,010,688（8.78%），与 shadow-table 预测一致。全部 654,890 行 locator 都是 12-byte BLOB，38,532 个非空 guard 都是 12-byte BLOB，其余 guard 保持 NULL，backing code 全是 0–4 的 SQLite INTEGER；全部 file revision 都是 parser fact 8，`quick_check(1)=ok`、foreign-key violations=0。schema 26 单次对照的 wrapper/engine 更快约 0.72 秒，但一次运行的 parse、机器与缓存波动无法归因；本阶段只把页级体积视为强收益，未宣称时间或进程峰值改善。

为交叉验证 Go，最终 release binary 还对同一 Kubernetes 工作树新建 `target/validation-1.5.0/kubernetes-stage4e-parser8.sqlite`：17,861 files、339,903 declarations、226,316 anchors、1,182,317 call sites，engine 49,905 ms，分段为 discover 1,700、parse 11,802、write 21,610、check 12、include edge 3,456、secondary index 4,647、publication 5,926 ms；schema 27 DB 为 479,526,912 bytes、117,072 pages、freelist 0，`declaration_facts` 为 132,784,128 bytes。全部 file revision 都是 parser fact 8，339,903 行 locator、nullable guard 和 backing encoding 都满足 BLOB/INTEGER 约束，`quick_check=ok`、foreign-key violations=0。它相对原始 schema 25 run-1 的 501,284,864 bytes 减少 21,757,952（4.34%），该差额合并了阶段 4B 去 locator 索引与阶段 4E 标量压缩，不能全部归因于本阶段。随后 no-change 增量 17,861 files 全部 skipped，事实计数不变，engine 4,087 ms、parse/write/publication 均为 0，证明新库可直接复用而非意外重建。

独立 reviewer 随 backing 契约审计发现一项既存 P2 与两项阶段边界 P3。P2 是 Go `type UserID string` 等 defined type 把整个 declaration range 放进 `SourceRange` backing，而 store 一直要求该 backing 等于 name range：debug index 会 panic，release 水合会把完整 byte end 与名字的 line/character end 拼成不一致 range；初轮 Kubernetes 库已有 4,557 行受影响。新增 store roundtrip 测试先稳定触发 debug assertion，现 Go parser 统一写入 name range，并把 `PARSER_FACT_VERSION` 从 7 提升到 8，确保任何结构仍为 schema 27 的旧 parser rows 也会失效。另两个失败测试分别证明 `u8::from_str_radix` 会接受 `+f` 前导符号、SQLite `BETWEEN 0 AND 4` 会放行 REAL 0.5；hex decoder 现先显式验证每个 byte 都是 ASCII hex，backing CHECK 现同时要求 `typeof='integer'`。uppercase 仍被接受并在 read view 规范化为原小写协议。

上述三项定向测试均已转绿。parser fact 8 最终 U-Boot/Kubernetes 重建确认旧事实会按预期失效；Kubernetes 中 `SourceRange` backing 与 name byte range 不一致的 Go declaration 已从初轮的 4,557 行降到 0。独立 test-executor 随后复跑完整 Rust suite、Clippy、release build/no-run 和 U-Boot full-index/hydration，全部通过；修复后独立 reviewer 复核确认三项 finding 完整关闭且产品代码没有新回归，仅纠正了报告中的单次耗时差。初轮 parser fact 7 数据仅保留为发现问题的证据，不再作为放行结果。

### 阶段 4F：关系编码去除重复 logical signature（已完成）

状态：实现、完整 Rust 门禁、U-Boot full-index/hydration、Kubernetes full/no-change 与修复后独立复核均已通过。

最初候选是合并 `canonical_signature` 与 `logical_canonical_signature`。源码审计证明直接合并不成立：普通 C/C++ declaration 的两值通常相同，但前者是 Hover、补全详情、LSP 展示与 overlay 使用的 presentation fact，后者属于 `LogicalEntityKey` 并参与 digest 后的精确身份匹配。Go 普通 declaration 的展示签名非空而 logical signature 为 NULL；名为 `init` 的 Go callable 则把逻辑值改为稳定 entity key，以区分多个物理初始化入口。真实 Kubernetes 中这包括 1,291 个顶层 `init` function 与 44 个名为 `init` 的 method。任意删除一列、按语言猜测或把 NULL 当作“与展示值相同”都会破坏至少一种现有语义。

两库的 schema 27 / parser fact 8 列级审计为：

| 样本 | declarations | display signature payload | logical signature 分布与 payload |
|---|---:|---:|---:|
| U-Boot | 654,890 | 31,682,279 UTF-8 bytes | 654,890 个值全部与 display 相同；31,682,279 UTF-8 bytes |
| Kubernetes | 339,903 | 45,073,634 UTF-8 bytes | 338,554 NULL、14 个相同值、1,335 个自定义值；33,001 UTF-8 bytes |

因此实现没有合并语义字段，而只压缩两者之间的持久化关系。schema 28 把 logical 列改为 nullable tagged BLOB：NULL 精确表示 logical `None`；单字节 `x'00'` 表示与非空 display 值相同；`x'01'` 后跟完整 UTF-8 bytes 表示显式 override，连显式空字符串也不会与 `x'00'` 混淆。write path 仍先序列化完整 `LogicalEntityKey` 并计算原 digest，再选择存储 tag；typed read view 严格验证 tag/UTF-8 并恢复原 `Option<String>`。SQLite CHECK 拒绝未知 tag、带 payload 的 same tag、非 BLOB 以及引用空 display 的 same tag；没有根据 language、name 或 declaration kind 推导值。

`target/benchmark/stage4f-signature-lab/` 先后验证了删除列、额外 state 列与单列 tag。早期 `CREATE TABLE AS SELECT` shadow 因丢失 `id INTEGER PRIMARY KEY` 的 rowid alias，给每行重复保存 ID，使 Kubernetes 结果虚增约 1 MiB；该数字已废弃。最终实验使用与生产完全相同的显式 declaration DDL，只替换目标列，`PRAGMA table_info` 确认主键仍为 INTEGER PK，且两库关系重建 mismatch 都为 0：

| 样本 | schema 27 declaration table | tagged-BLOB shadow | 页级变化 |
|---|---:|---:|---:|
| U-Boot | 176,820,224 bytes / 43,169 pages | 143,060,992 / 34,927 | -33,759,232 bytes（-19.09%） |
| Kubernetes | 132,784,128 bytes / 32,418 pages | 132,788,224 / 32,419 | +4,096 bytes（+0.003%） |

TDD 先加入 `declaration_storage_tags_logical_signature_relations_without_changing_views` 与 schema 27→28 重建用例。修改前前者实际读到 SQLite TEXT 和完整重复签名、期望 BLOB `00`，后者实际 version 27、期望 28，均稳定失败。转绿测试覆盖 C same、Go NULL、Go `init` override、typed logical-key 查询和 schema 失效；reviewer 复核后又把持久化边界扩到显式空 override 的 SQLite→typed round-trip、可通过 tag CHECK 但必须由 typed view 拒绝的非法 UTF-8，以及 schema 对空 BLOB、same tag 带 payload、未知 tag、TEXT、INTEGER 和 same→NULL display 的拒绝。schema 版本升级会让旧 TEXT rows 在进入 BLOB decoder 前完整重建，不需要修改 parser fact 8。

独立 reviewer 最终确认 tagged relation 的 SQLite CHECK、任意 display/logical `Option<String>` 组合、完整 logical-key 序列化与 digest 输入、typed decoder、SELECT 列索引与 schema 27 失效均无剩余 finding；两项 P3 仅涉及报告把字符数误称为 bytes，以及恶意 payload/空 override 覆盖不足，修正后窄范围复核已关闭。修正测试后完整 Rust suite、格式与 Clippy 再次通过。

独立 test-executor 的最终门禁：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo test -p fossilsense` | PASS，unit 1005 + CLI 1 + LSP 2 = 1008 passed / 6 ignored |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS |
| `cargo build --release -p fossilsense` | PASS |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS |
| U-Boot full-index | PASS，wrapper 37,150.273 ms；engine 36,084 ms，均低于 60,000 ms |
| U-Boot engine hydration | PASS，单代 183,697,408 bytes；双代 348,962,816 bytes |

U-Boot 分段为 discover 1,123 ms、parse 6,478、write 19,616、check 9、include edge 1,960、secondary index 2,094、publication 4,155；峰值 Working Set 169,414,656 bytes、Private Bytes 158,949,376。hydration recall 为 93,747,480 bytes，首/次代 3,844/3,853 ms。原始报告是 `target/benchmark/large-workspace-20260731_041545.json` 与同名 Markdown。最终 schema 28 DB 为 345,210,880 bytes、84,280 pages、freelist 0，相对同事实 schema 27 的 378,793,984 bytes 减少 33,583,104（8.87%）；`declaration_facts` 为 143,114,240 bytes，相对 176,820,224 减少 33,705,984（19.06%）。654,890 行全部为合法单字节 `x'00'`，事实计数不变，schema/parser 分别为 28/8，完整性检查通过。

Kubernetes 最终放行库是 `target/validation-1.5.0/kubernetes-stage4f-schema28-fresh.sqlite`。full-index engine 47,732 ms，分段为 discover 1,511、parse 9,963、write 21,589、check 13、include edge 3,557、secondary index 4,563、publication 5,677；随后 no-change 17,861 个文件全部 skipped，engine 4,072 ms，parse/write/publication 均为 0。事实计数保持 339,903 declarations、226,316 anchors、1,182,317 calls；最终 DB 为 479,776,768 bytes、117,133 pages、freelist 0，`declaration_facts` 为 132,800,512 bytes。相对 schema 27 表只增加 16,384 bytes（0.012%），整库增加 249,856（0.052%，含 fresh rebuild 页布局波动）；338,554 NULL、14 个 `x'00'`、1,335 个 `x'01'` 全部合法，schema/parser=28/8，`quick_check=ok`、foreign-key violations=0。

补证过程中还暴露了一个与本阶段编码无关、但必须后续处理的既存风险：在已有的 479,768,576-byte Kubernetes 显式 DB 上再次执行 `index --force`，新一代在约 50 秒内已经发布，但进程随后停在 finalizing 超过 5 分钟并持续占用 CPU，远超 60 秒口径。中止后的只读现场 `target/validation-1.5.0/kubernetes-stage4f-schema28.sqlite` 显示 active manifest 仍正确指向 17,861 个文件、pending=0，但旧代尚未清理：file revisions 35,722、declaration rows 679,806，恰为双代，文件膨胀到 769,638,400 bytes。该现场不作为有效放行库；阶段 4G 将单独追踪发布后 obsolete-revision cleanup 的复杂度、崩溃恢复和空间回收，避免与已验证的 tagged encoding 混成一个提交。

### 阶段 4G：显式 `--force` 全量索引旁路发布（已完成）

状态：实现、TDD、完整 Rust 门禁、U-Boot fresh/full-force 双路径、engine hydration、SQLite 完整性复核与修复后独立 reviewer 均已通过。

现场只读审计确认，阶段 4F 暴露的超时发生在语义代次已经提交之后，而不是 parser、writer、二级索引或完整性校验。原实现对已有显式 `--db` 执行 `--force` 时直接在同一个 SQLite 文件内写入新 revision；`commit_index_build` 先原子切换 active generation 并提交，再在事务外用 `DELETE FROM file_revisions ...` 回收旧 revision。17,861 个旧 revision 通过外键级联关联约 496 万条旧事实，而 full-build 又临时移除了 call revision 索引；父表逐行级联因而反复扫描大表。进程被中止时新代仍正确可读，但旧事实和空间债务留在同一文件，且下一次启动没有专门的自动重试入口。

本阶段没有给每个大型事实表永久增加 revision 索引，也没有削弱提交后的旧快照可读性。显式 `--db PATH --force` 现在与默认工作区的 side-by-side 原则一致，但仍直接原子替换用户指定的 `PATH`，不引入 manifest：

1. 所有显式数据库写路径——full index、普通 incremental 和 dirty-file incremental——先对规范化目标获取稳定 sibling lock DB 的 SQLite `BEGIN EXCLUSIVE`。锁文件使用目标路径 hash 的固定短名称并永久保留 8 KiB 主文件；事务在正常返回或进程死亡时由 SQLite 自动释放，不使用会遗留错误 owner 状态的 PID 文件。
2. `--force` 在锁内严格捕获旧目标状态、semantic generation 以及主库/WAL/SHM/journal 的文件身份；读取、权限或 generation 格式错误不再静默降为 generation 0。staging 使用与目标 basename 无关的固定长度唯一名称，新库从空 schema 开始并继承代次编号。
3. parser、fact 写入、include/Go graph、call indexes、`quick_check(1)`、`foreign_key_check` 和 checkpoint 全部在未发布 staging 上完成。新库没有 inactive revision，因此不会进入旧代级联回收。
4. 发布前仍在同一 writer lock 内重新比较目标文件身份并重读 generation；变化即拒绝覆盖。随后旧目标通过 SQLite `journal_mode=DELETE` 排空 WAL，目标或 staging 的 `-wal`、`-shm`、`-journal` 检查使用可传播元数据错误的 fail-closed 路径。
5. Windows 使用 `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`，其他平台使用同目录 rename。发布前任一步失败时旧目标保持原位，RAII guard 只删除本次唯一 staging 及其 sidecars；损坏但没有 sidecar 的旧目标仍允许由 `--force` 恢复。

TDD 首先加入 `explicit_force_rebuild_publishes_a_fresh_database_without_old_cleanup_debt`。测试在第一代显式库安装一个拒绝删除 `file_revisions` 的触发器；修改前第二次 `--force` 稳定返回 `maintenance_warning=post-publication cleanup failed` 并留下旧 schema/旧 revision，修改后第二代在全新库中发布，generation 从 1 连续到 2、旧触发器不存在、旧 symbol 消失且只有一个 active revision。首轮 reviewer 随后发现旧 WAL 排空与 rename 间存在 cooperating-writer TOCTOU、generation 读取错误会被吞成 0、长 basename 会放大 staging 组件，以及 WAL/失败回滚测试不足；这两项 P1 与两项 P2 均先补测试再修复。最终覆盖相同目标第二 writer 被拒绝、非合作外部推进触发文件身份/generation 复核失败、非法 generation 不降级、真实 persistent WAL 被 checkpoint/drain、活跃 WAL writer 阻止发布时旧 generation/旧 symbol 仍可读且 staging 被回收、固定 staging 组件长度、人工 sidecar fail-closed，以及默认 manifest 和非 force incremental 不回归。修复后 reviewer 逐项确认 2 P1/2 P2 全部关闭，没有新 finding。

独立 test-executor 的门禁结果：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS，约 3.60 s |
| `cargo test -p fossilsense` | PASS，unit 1013 passed / 6 ignored，CLI 1 passed，LSP 2 passed；0 failed |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS，约 19.78 s |
| `cargo build --release -p fossilsense` | PASS |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS |

大型门禁使用 U-Boot commit `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c`；样本有两处既存空白差异，因此准确标记为 dirty。机器为 Acer Nitro AN515-58、Intel Core i5-12500H（12 cores / 16 logical processors）、约 24 GiB RAM、Windows 11 Pro Insider build 26220。标准脚本先删除目标，验证 fresh 显式 full-index；随后不删除同一数据库，直接再次执行完全相同的 release `index ... --db ... --force`，专门覆盖原超时路径：

| 路径 | wrapper / engine | write | publication | peak Working Set / Private | 结果 |
|---|---:|---:|---:|---:|---|
| fresh explicit full-index | 37,266.403 / 36,225 ms | 19,692 ms | 4,218 ms | 163,053,568 / 153,350,144 bytes | PASS |
| existing explicit DB 再次 `--force` | 36,482.081 / 36,296 ms | 19,897 ms | 4,300 ms | 171,094,016 / 160,079,872 bytes | PASS |

两轮都保持 13,244 files、654,890 declarations、91,919 callable anchors、582,522 call sites，远低于 60,000 ms 硬线。标准 fresh 原始报告为 `target/benchmark/large-workspace-20260731_055014.json` 与同名 Markdown。existing-DB 轮使用同一脚本的 20 ms process sampling 逻辑直接执行，最终数据库 generation=2，但只含 13,244 个 active/file revisions、pending=0、一个 committed build、654,890 declaration rows；文件 345,067,520 bytes、84,245 pages、freelist=0，`quick_check=ok`、foreign-key violations=0，目录中只有预期的 8,192-byte writer-lock 主文件，没有 staging/WAL/SHM/journal 残留。与旧实现的 35,722 revisions、679,806 declarations、769,638,400-byte 双代中止现场相比，新路径不再产生发布后清理工作。

本阶段改变了 full-build publication 架构，因此额外执行 U-Boot engine hydration。结果为 654,890 declarations / 13,244 files，compact recall 93,747,480 bytes，单代 Private 183,939,072 bytes、旧快照存活时双代峰值 348,995,584 bytes，首/次构建 3,906/3,790 ms；分别低于 384/512 MiB 门禁。原始报告为 `target/benchmark/large-workspace-20260731_055202.json` 与同名 Markdown。

边界保持明确：旁路构建在发布前同时保留旧库与新 staging，要求足够的临时磁盘空间；进程被强杀时旧目标仍安全，但唯一 staging 可能成为可人工识别的孤儿文件，不能像正常错误返回那样依赖 Drop 回收。同一目标的 FossilSense writer 遵守 sibling lock，因此不会进入 WAL drain/rename 竞态；直接绕过协议写 SQLite 的外部程序明确不受支持，最终身份复核后到 rename 仍存在极小窗口，但已有 drain 后与 rename 前两次 sidecar 检查，Windows 打开句柄还会使 `MoveFileExW` 保守失败。残余测试缺口是尚未单独故障注入 Windows 打开句柄导致的 `MoveFileExW` 失败。阶段 4F 的中止现场只读保留用于证据，没有就地修理；下一阶段 4H 单独为既存 cleanup debt 和普通增量发布设计有界批量回收与恢复测试，而未来显式 `--force` 已不再制造这种债务。

### 阶段 4H：inactive revision 持久化恢复与增量有界清理（已完成）

状态：实现、TDD、真实 Kubernetes 历史债务恢复、普通增量性能复核、完整 Rust 门禁、U-Boot full-index/hydration 硬门禁和修复后独立 reviewer 均已通过。

阶段 4G 已让未来显式 `--force` 不再在目标库中制造双代事实，但普通增量发布仍沿用旧清理协议，阶段 4F 保留的中止现场也证明既存数据库需要可恢复入口。原实现先提交 active generation，再在事务外从父表逐行删除旧 `file_revisions`，依赖 SQLite 外键级联清理约 10 张事实表；部分 revision 外键没有索引时，复杂度会退化为“旧 revision 数 × 大事实表扫描”。清理失败只返回当次 maintenance warning，没有 durable 状态；重启后若没有新的同类变更，也不会保证重试。直接从父表开始删除还把行为正确性隐含委托给逐条 cascade，无法在关闭外键以做集合清理时保持 caller anchor、record target 和跨文件 orphan 的等价语义。

本阶段把发布与回收改为显式的两阶段生命周期：

1. schema metadata 增加 durable `cleanup_required` 标记。新库初始为 `0`；旧 schema 首次打开或发现 abandoned staging build 时原子写入 `1`。active manifest、build committed、pending rows 删除和标记 `1` 在同一提交事务完成，因此进程可在任意后续位置终止而不会丢失恢复责任。
2. `begin_index_build` 在接受新 staging 前检查标记。需要恢复时执行一次 workspace 范围集合清理；成功与完整性验证在同一 cleanup 事务内把标记清为 `0`，任一步失败则整个 cleanup 回滚并保留 `1`，下一次 begin 自动重试。
3. 正常增量提交捕获本次 changed/deleted file ID 作为 cleanup scope。旧 revision 和 orphan file 只从这个小集合通过索引召回，不再扫描全库 revision/file；空 scope 不触发清理验证。full/recovery/call-string GC 保留全库模式，因为这些路径本来就是低频维护边界。
4. cleanup 在事务外确认当前连接外键已开启，再临时关闭外键，以一个 `BEGIN IMMEDIATE` 事务建立 scoped file、obsolete revision、orphan file、record ID 和 callable-anchor ID 临时集合。十类直接事实按“obsolete revision 或 orphan file”集合删除；call sites 额外按已删除 caller anchor 清理，member/type-alias 对已删除 record target 精确执行 `SET NULL`，include edge 同时覆盖 src/dst file，最后才删除 anchor、record、revision 和 file parent，行为与 schema 的 CASCADE/SET NULL 等价。
5. recovery/full/call-string 路径仍运行完整 `PRAGMA foreign_key_check`。普通增量使用有界 parent-set validation，逐类验证本次会影响的 revision/file、active/pending、include src/dst、caller anchor、member record 和 alias target，不把每次文件保存变成全库外键扫描。事务后若无法恢复 `foreign_keys=ON`，store 会进入 `maintenance_blocked` fail-closed 状态并保留 durable 标记；同一连接不再允许继续写入。

TDD 首先加入四个必失败场景：abandoned build 的 revision 必须被回收、事实必须先于 revision parent 删除、cleanup 中途失败必须整体回滚且留下标记并在下次 begin 重试、当前 schema 缺失标记必须只审计一次。随后补齐十类事实、跨文件 orphan declaration/record/anchor、caller-anchor cascade、member/alias `SET NULL` 和 include 双向关系。首轮 reviewer 据此发现 orphan-only 且没有 obsolete revision 时会跳过事实清理、普通增量仍可能全库扫描 revision/file 和完整 FK check，以及恢复外键失败后同一连接仍可写三项问题；每项均先补回归测试再修复。最终 reviewer 逐一核对当前 schema 的全部 parent/child FK、普通 scoped SQL 和索引契约，确认原有 2 项 P2、1 项 P3 全部关闭，没有新的 P0–P3 finding。

为保证集合删除和 manual validation 的查询上界，deferred lookup indexes 在既有 call indexes 之外补齐 12 个清理相关索引：七张事实表的 revision、type-alias target record、pending file/revision、call-site file 和 include-edge destination。普通打开会补建缺失索引；full build 仍先删除并在事实写完后统一创建，避免把二级索引维护放进 bulk insert 热路径。U-Boot 同事实数据库最终为 363,204,608 bytes，相对阶段 4G 的 345,067,520-byte 数据库增加 18,137,088 bytes（约 5.26%）；两个 pending 索引在提交后为空，主要增量来自十个有内容的永久维护索引。这是用约 18.1 MB 可计量空间换取普通增量和历史恢复的确定查询路径，不隐藏为零成本优化。

真实恢复验证直接复制阶段 4F 保留的 769,638,400-byte Kubernetes schema 28 中止现场；原件保持只读不变。现场含 35,722 个 revisions、17,861 个 active revisions、679,806 declarations、2,364,634 call sites，约 496 万条事实属于旧代。从进程启动、迁移/索引补齐到自动恢复完成并进入 `checking 0/17861` 的上界为 14,725.550 ms；恢复后 revisions 精确等于 17,861 active rows，`cleanup_required=0`、`quick_check=ok`、foreign-key violations=0。补齐维护索引后的首次 no-change 为 wrapper 6.245 s / engine 5.967 s；随后单文件增量为 wrapper 4.323 s / engine 4.043 s，其中 discover 1,499 ms、check 114、parse 3、write 4、include-edge 1,302，说明正常发布没有退化成历史债务的 workspace 级回收。

该恢复事务只做逻辑回收，不在在线路径运行 `VACUUM`。Kubernetes 修复副本因建索引和 free pages 最终保持 895,877,120 bytes；删除的旧事实成为可复用 freelist，物理文件不会立即缩小。需要压缩物理文件时应使用阶段 4G 已验证的显式 side-by-side `--force`，而不是在普通增量发布后阻塞读者做 in-place compaction。

独立 test-executor 的最终门禁：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS，约 3.58 s |
| `cargo test -p fossilsense` | PASS，unit 1020 passed / 6 ignored，CLI 1 passed，LSP 2 passed；0 failed |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS，约 14.89 s |
| `cargo build --release -p fossilsense` | PASS |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS |
| U-Boot full-index | PASS，wrapper 39,313.536 ms；engine 38,193 ms，均低于 60,000 ms |
| U-Boot engine hydration | PASS，单代 184,025,088 bytes；双代 348,672,000 bytes |

最终 U-Boot full-index 保持 13,244 files、654,890 declarations、91,919 callable anchors 和 582,522 call sites。分段为 discover 977 ms、parse 6,136、write 19,550、check 8、include edge 3,181、secondary index 3,296、publication 4,403；峰值 Working Set 170,319,872 bytes、Private Bytes 158,736,384。数据库为 363,204,608 bytes、88,673 pages、freelist 0，`quick_check=ok`、foreign-key violations=0。hydration compact recall 为 93,747,480 bytes，首/次构建 3,759/3,821 ms；单代远低于 384 MiB、旧快照存活时双代远低于 512 MiB。原始报告为 `target/benchmark/large-workspace-20260731_071517.json` 与同名 Markdown。

边界保持明确：首次打开缺少 marker/index 的大型旧库会承担一次迁移与 recovery 成本；recovery 是 workspace 规模的单个原子写事务，写者在其间被阻塞，但 WAL 中已有读快照可以继续服务。普通增量则只按本次 file scope 回收。full/recovery/call-string GC 的完整 FK check 仍与库规模相关，故不应进入每次请求路径。在线清理只保证逻辑一致和空间可复用，不承诺立即缩小 SQLite 文件。

### 阶段 4I：schema migration 错误与进程崩溃原子性（已完成）

状态：实现、TDD、同步故障回滚、真实子进程 `abort` / WAL 恢复、历史 schema 回归、完整 Rust 门禁、U-Boot full-index/no-change 性能验证和三轮修复后独立 reviewer 均已通过。

阶段 4B 留下的 migration 风险经当前源码重新确认仍真实存在。`IndexStore::open` 在已有显式 `--db` 的非 force 路径会原地打开目标；旧 `migrate` 依次创建/读取 meta、删除 legacy views/tables、执行 23 项 data-table drop、创建当前 schema/views/indexes，最后更新 schema/workspace/generation/cleanup metadata，但这些步骤分散在多个 autocommit 语句。默认工作区 rebuild 和显式 `--force` 已有 side-by-side staging 保护，普通已有显式 DB 则可能在中途 SQL 错误或进程终止后留下“旧数据已删、新 schema 只建一半、schema_version 仍旧”的目标文件。原实现还用 `parse().ok()` 读取 schema version，非数字值会被误当成没有版本的新数据库，并可能被覆盖成当前版本。

本阶段把 migration 定义为一个明确的 SQLite 原子发布边界：

1. `migrate` 改为持有可变连接并使用 `TransactionBehavior::Immediate`。从 `CREATE TABLE IF NOT EXISTS meta`、版本读取、parser-fact 探测、legacy/current data drop、schema/view/trigger 创建、普通与 deferred index 处理，到最终四类 metadata 写入和 commit，全部使用同一个 transaction，没有通过裸 `self.conn` 绕过。
2. stored schema version 现在区分“键不存在”和“值损坏”。缺失键仍表示新库；存在但不能解析为整数时在任何 destructive DDL 前返回带上下文错误，transaction drop 回滚 meta 建表等先行操作，不再把损坏数据库重新分类为新库。
3. `create_deferred_indexes=false` 的 full-build 打开也在同一 migration transaction 内删除 deferred lookup indexes。旧实现是在 migration commit 后用裸 `execute_batch` 删除，错误可能留下部分索引已删而 metadata 已成功的状态；现在 SQL 错误会回滚整个 schema/index/meta 变化。
4. `BEGIN IMMEDIATE` 会串行化 migration writer，但 WAL 旧读快照仍可继续读取。current-schema 打开原本就会执行 metadata upsert；本阶段把这些写操作的锁范围显式化，没有在请求读路径引入 migration，`open_readonly` 仍不修改 schema。

TDD 使用独立 `migration_atomicity.rs`，没有继续膨胀原 625 行 resilience 文件。第一条 RED 在 schema 27 库保留旧 `files` 行，并创建与当前 lookup index 同名的合法旧表，使 migration 在新 schema 已创建后确定失败；修改前失败现场丢失旧表/行并残留大部分新 schema，修改后完整 `sqlite_master`、旧行和 schema/workspace/generation metadata 逐项相同，移除阻断对象后可成功重试。第二条 RED 证明 `not-a-version` 会被旧实现静默升级为 28；修改后 open fail-closed，旧 payload 和 metadata 不变，当前 schema 对象为 0。

首轮 reviewer 随后指出 `create_deferred_indexes=false` 的事务外删除仍是 P2，并要求真实 crash 证据。测试使用 `cfg(test)`、线程局部、命中即清空的一次性 failpoint：deferred indexes 全部删除后注入同步错误。故障点在事务外时 schema 快照稳定缺失维护索引；移入 transaction 后错误返回且快照完全恢复，正常重试会按 full-build 契约继续 defer。故障注入 enum、thread-local、setter 和调用点均不进入 production build，也不会在并行测试线程间共享状态。

进程崩溃测试以 WAL 模式 schema 27 临时库启动当前 Rust test executable 的精确 helper。子进程在 `DROP_DATA_TABLES_SQL` 完成、当前 schema 尚未创建时命中 failpoint，随后直接 `std::process::abort()`，不进行 Rust unwinding。父进程重开库后确认完整 schema snapshot、旧 `files` 行、schema version 27、semantic generation 11、旧 workspace root 和 `quick_check=ok`，再正常重试至 schema 28。第二轮 reviewer 发现仅检查“非 91 异常退出”可能把 failpoint 前普通 panic 当成目标 abort；修复后 abort 点先在测试独占 tempdir 写入精确 `destructive-drop-complete\n` marker、执行 `sync_all()`、关闭文件并紧邻调用 abort。父进程必须同时验证 marker 存在且逐字节完整、子进程异常退出且不是 helper 兜底 91、旧 WAL 状态恢复和后续迁移成功。最终 reviewer 确认该 P2 关闭，原有 migration P2/P3 全部关闭，没有新的 P0–P3 finding。

独立 test-executor 的最终门禁：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS，约 2.00 s |
| `cargo test -p fossilsense` | PASS，unit 1025 passed / 6 ignored，CLI 1 passed，LSP 2 passed；0 failed |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS，约 8.83 s |
| `cargo build --release -p fossilsense` | PASS |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS |
| U-Boot full-index | PASS，wrapper 39,438.100 ms；engine 38,351 ms，均低于 60,000 ms |
| U-Boot existing explicit DB no-change | PASS，wrapper 4,664.912 ms；engine 4,420 ms |

最终 U-Boot full-index 保持 13,244 files、654,890 declarations、91,919 callable anchors 和 582,522 call sites。分段为 discover 998 ms、parse 7,050、write 18,694、check 8、include edge 3,136、secondary index 3,246、publication 4,537；峰值 Working Set 167,923,712 bytes、Private Bytes 160,256,000。fresh DB 为 362,872,832 bytes、88,592 pages、freelist 0，schema 28、`cleanup_required=0`、`quick_check=ok`、foreign-key violations=0。原始报告为 `target/benchmark/large-workspace-20260731_091507.json` 与同名 Markdown。

同一已有显式数据库随后不带 `--force` 打开，13,244 个文件全部 skipped；engine 4,420 ms，其中 discover 808、check 50、include edge 2,994，parse/write/secondary-index/publication 都为 0。关闭后数据库为 363,126,784 bytes、88,654 pages、freelist 0，file revisions 与 active revisions 均为 13,244，`cleanup_required=0`、`quick_check=ok`、foreign-key violations=0。该路径直接覆盖本阶段要保护的原地 current-schema migration，并证明事务化没有把正常重开退化为 full rebuild。

按用户要求，本阶段中途清除了已完成阶段的本地生成物：`target/benchmark` 与 `target/validation-1.5.0` 当时合计 14,866,136,504 bytes（约 13.85 GiB），包括阶段 4C–4H SQLite 实验副本和旧原始 benchmark/validation 文件；历史数值已保留在本报告，但此前章节引用的本地原始路径不再存在。Stage 4I 的 benchmark 随后重新创建当前 `target/benchmark`，只保留本阶段 JSON/Markdown、当前 U-Boot DB 和预期的 8 KiB writer lock。

边界保持明确：该事务保证 SQLite schema migration 在 SQL 错误、Rust error return 和被验证的进程 `abort` 下不会暴露半迁移状态；它不扩张为对介质永久损坏、文件系统违反持久化承诺或已提交数据库被外部程序篡改的恢复保证。full-build migration 成功后的 disposable runtime PRAGMA/专用 call-string setup 属于未发布 build target 初始化，不在原地 schema migration 的发布边界内。schema 仍为 28，因为持久化形状没有变化。

### 阶段 4J：Windows 发布失败回收与默认索引跨进程串行化（已完成）

状态：实现、真实 Windows 句柄故障注入、跨进程竞争与强制终止测试、完整 Rust 门禁、U-Boot full-index/hydration/no-change 门禁和两轮独立 reviewer 均已通过。

阶段 4G 已让显式 `--force` 使用旁路数据库和原子替换，但当时仍缺少 Windows 目标被不共享删除的句柄占用时的真实 `MoveFileExW` 失败证据。当前源码复核还发现默认 generation 发布的另一个泄漏窗口：staging 数据库先被 rename 为最终 `index-g*.sqlite`，随后才创建并替换 `active-index` manifest；若 manifest 临时文件碰撞、写入失败或 Windows 拒绝替换 manifest，旧 active generation 保持安全，但新封存数据库和 manifest 临时文件没有所有权 guard，会永久留在缓存目录。

本阶段先用 Win32 `CreateFileW` 打开目标并只声明 `FILE_SHARE_READ | FILE_SHARE_WRITE`，明确省略 `FILE_SHARE_DELETE`。显式发布测试证明 `MoveFileExW` 失败时旧目标逐字节不变、唯一 staging 被回收，关闭句柄后同一路径可成功重试；该分支是既有正确行为的 Windows 回归证据。默认 manifest 测试在旧实现上则稳定失败：旧 manifest/旧 generation 可读，但未发布的新 generation 仍存在。实现增加 `UnpublishedDefaultIndex` 所有权 guard：数据库 rename 成功后立即接管完整 SQLite family；manifest 临时文件只有在 `create_new` 成功后才纳入所有权，避免误删碰撞的外部文件；仅在 `atomic_replace` 成功后解除回滚。同步错误和 Drop 会尽力删除新数据库及 `-wal`、`-shm`、`-journal` sidecar，同时保留旧 manifest、旧数据库和并非本次创建的冲突文件。

首轮 reviewer 在失败回收实现上没有发现所有权或 Windows API 问题，但指出一个更高优先级的跨进程发布竞态：默认 publisher A 封存 generation 后暂停，publisher B 切换 manifest 并清理目录时看不到 A 的进程内 lease，可能删除 A 的 generation；A 随后仍能把 manifest 切到已不存在的文件。较早开始、较晚完成的构建还可能覆盖较新内容并使 semantic generation 回退。LSP 内的异步 gate 只能串行化单进程任务，不能约束另一个 LSP 或 CLI 进程。

修复把原先只服务显式目标的 SQLite sibling lock 泛化为 `IndexWriterLock`。默认索引始终以 workspace cache generation family 内稳定的 `index.sqlite` fallback 路径作为逻辑目标，显式索引仍以规范化后的 `--db` 目标作为逻辑目标；完整路径经 BLAKE3 导出同目录稳定的 8 KiB lock 数据库。全量和 dirty 默认写入都在读取 `active-index`、旧 generation 或 schema 之前执行 `BEGIN EXCLUSIVE`，并把连接保留到数据库提交、manifest 切换和清理全部结束；显式 replacement snapshot、WAL drain 和发布前身份/generation 复核仍在同一锁持有期内。竞争 writer 在 250 ms busy timeout 后明确失败，不读取陈旧 generation 继续构建。锁数据库不删除，避免 close/delete/open 把 cooperating writers 分裂到不同 inode；连接正常释放、错误展开或进程终止时，SQLite/操作系统会释放事务锁。

跨进程 TDD 使用当前 Rust test executable 启动精确子测试。父进程先持有默认逻辑目标锁，子进程执行真实默认 `force` index；修改前子进程成功构建并发布 generation 1，父测试因预期 `locked` 错误而稳定 RED，修改后在任何 active-generation 读取和写入前被拒绝并转为 PASS。reviewer 复审确认原 P1 完整关闭且无新 finding，同时指出尚缺“持锁进程被强制终止”的直接证据。补充测试让子进程取得锁后把精确 `writer-lock-acquired\n` 写入唯一临时文件、`sync_all()`、关闭并原子 rename 为 ready marker；父进程验证完整 marker 后直接终止并有界轮询回收子进程，随后同一默认目标可立即重新取得锁。测试 guard 覆盖正常作用域退出和可展开 panic；helper 另有 30 秒自限时，因此父测试被 timeout、`abort` 或外部强杀时也不会永久遗留持锁进程。

`CreateFileW` 的 Rust 签名需要 `windows-sys/Win32_Security` 类型，因此 Cargo 只为既有 `windows-sys 0.61` 增加该 compile-time feature；没有新增 crate、许可证、运行时 DLL 或 VSIX 外部依赖。Windows 专用测试隔离在 `pathing/windows_tests.rs`，主 `pathing.rs` 保持 796 行，没有越过架构 fitness 的 800 行阈值。

独立 test-executor 的最终门禁：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS，1.779 s |
| `cargo test -p fossilsense` | PASS，unit 1032 passed / 6 ignored，CLI 1 passed，LSP 2 passed；0 failed |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS，1.318 s（增量复跑） |
| `cargo build --release -p fossilsense` | PASS，54.36 s |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS |
| U-Boot full-index | PASS，wrapper 38,090.534 ms；engine 37,039 ms |
| U-Boot engine hydration | PASS，单代峰值 183,783,424 bytes；双代绝对峰值 348,844,032 bytes |
| U-Boot existing explicit DB no-change | PASS，wrapper 4,403.945 ms；engine 4,162 ms |

大型门禁使用 U-Boot commit `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c`；样本保留 `boot/scene.c`、`boot/vbe_abrec.c` 两处既存空白差异，准确标记为 dirty。机器为 Acer Nitro AN515-58、Intel Core i5-12500H（12 cores / 16 logical processors）、25,459,482,624 bytes RAM、Windows 11 Pro Insider build 26220。

full-index 保持 13,244 files、654,890 declarations、91,919 callable anchors 和 582,522 call sites。分段为 discover 781 ms、parse 6,114、write 18,547、check 8、include edge 3,131、secondary index 3,404、publication 4,371；峰值 Working Set 165,855,232 bytes、Private Bytes 158,576,640。fresh DB 为 363,053,056 bytes、88,636 pages、freelist 0。hydration compact recall 为 93,747,480 bytes，首/次代构建 4,304/4,009 ms；单代峰值约 175.27 MiB、旧快照存活时双代绝对峰值约 332.68 MiB，分别低于 384/512 MiB 硬门禁。原始报告为 `target/benchmark/large-workspace-20260731_100103.json` 与同名 Markdown。

同一已有显式数据库随后不带 `--force` 打开，13,244 个文件全部 skipped；engine 4,162 ms，其中 discover 787、check 52、include edge 2,767，parse/write/secondary-index/publication 都为 0。关闭后数据库为 363,302,912 bytes、88,697 pages、freelist 0，schema 28、semantic generation 2、13,244 file revisions 与 13,244 active revisions、pending revisions 0、staging builds 0、654,890 declarations、`cleanup_required=0`、`quick_check=ok`、foreign-key violations=0。这同时证明大型真实目标在 full-index 结束后已释放 sibling lock，普通重开没有退化为 full rebuild。

边界保持明确：跨进程锁只约束遵守 FossilSense 协议的 writer，不能阻止直接绕过 sibling lock 修改 SQLite/manifest 的外部程序；Windows 文件系统拒绝删除失败产物时，Drop 回收仍只能 best-effort，但旧发布状态保持安全。显式 `--db --force` 在进程被强杀时仍可能留下唯一命名的 `.fossilsense-index-build-*` staging；自动删除任意用户指定目录中的此类文件缺少可证明的 durable ownership，当前不以文件名猜测所有权。该残余风险是磁盘累积而非半发布或旧库损坏，后续若处理应先引入严格 claim/owner 协议，不能直接扩大启动清理范围。

### 阶段 4K：恢复 indexer 测试的 SQLite 架构边界（已完成）

状态：架构 RED、最小测试重构、完整 Rust 门禁、architecture fitness/golden 门禁和独立 reviewer 均已通过；production 源码与二进制行为未改变。

Stage 4J 提交后的附加架构扫描发现当前 HEAD 有一项历史失败：`indexer/tests/basic.rs` 从阶段 4G 的显式 side-by-side 发布测试开始直接导入 `rusqlite::Connection`。三处 SQL 分别安装拒绝旧 revision cleanup 的 trigger、检查替换库没有继承 trigger 且只含一代 revision，以及以 `BEGIN IMMEDIATE` 持有未提交 WAL writer。场景本身是有效的 indexer 端到端故障注入，但连接、SQL schema 知识和事务操纵越过了 store/persistence 边界。`scripts/verify_architecture_fitness.ps1` 因此稳定报告 `sqlite-boundary` ERROR、`fail=1 / warn=14`；该失败从提交 `0737008` 起存在，不是 Stage 4J 产品实现引入。

现有 architecture golden 的 `forbidden_dependency` fixture 已证明非 store 模块出现 `rusqlite` 必须失败，因此该真实仓库门禁就是本阶段的 RED，不增加 allowlist或削弱规则。修复保留两个 indexer 端到端测试及其全部产品断言，只新增严格 `#[cfg(test)] pub(crate) store::test_support`：

- `install_old_revision_cleanup_guard(path)` 封装固定 trigger 安装，函数返回时关闭连接；
- `inspect_explicit_replacement(path)` 只返回精确 trigger count 与 revision count，不暴露 query 或连接；
- `hold_external_wal_writer(path)` 返回不透明 `ExternalWalWriter`，内部仍按原测试启用 WAL、执行 `BEGIN IMMEDIATE` 并创建未提交表；`release(self)` 只允许固定 `ROLLBACK`，提前 panic 时连接 Drop 同样回滚。

接口不接受任意 SQL、不返回 `rusqlite` 类型、不暴露 execute/query 方法；`test_support.rs` 只由 `#[cfg(test)]` 模块声明引用，非测试构建不会解析或编译其中的连接、SQL 和 helper。架构规则、golden fixture 与 allowlist 均未修改，因此 PASS 来自依赖真正回到 persistence 边界，而不是文本规避。独立 reviewer 逐项核对 trigger SQL、参数化精确名称检查、WAL writer 生命周期和原端到端断言，未发现 finding。

独立 test-executor 的最终结果：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS，2.088 s |
| `cargo test -p fossilsense` | PASS，unit 1032 passed / 6 ignored，CLI 1 passed，LSP 2 passed；0 failed |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS，1.238 s |
| `scripts/verify_architecture_fitness.ps1` | PASS，fail 0 / warn 14 / allowlisted 0 |
| `node scripts/test_architecture_fitness.js` | PASS，golden 8/8 |

14 项 warning 全部是既有超过 800 行的 production source 提示；本阶段新增的 `store/test_support.rs` 只有场景化测试 helper，没有新增大文件或 warning。因为当前提交对 production build 的有效 token stream 为零变化，没有重复运行 U-Boot full-index/hydration；Stage 4J 的 release 二进制与 `large-workspace-20260731_100103` 数据仍精确对应当前 production 源码。后续最终全仓门禁必须继续保持 architecture fail=0，不能把这次修复降格为 allowlist。

### 阶段 4L：默认 generation 的跨进程读租约与安全回收（已完成）

状态：真实跨进程 RED、Windows 强制终止、显式路径隔离、新旧快照重叠、并发析构、孤儿锁与 SQLite sidecar 测试，完整 Rust/架构门禁、U-Boot full-index/hydration 和四轮修复后独立 reviewer 均已通过；最终 targeted review 为 `No findings`。

源码审计确认 Stage 4J 只串行化 writer，旧 generation 的读租约仍是进程内 `OnceLock<Mutex<HashMap<PathBuf, Weak<()>>>>`。另一个 FossilSense 进程持有旧 `CallReadHandle` 时，新进程无法看见该 lease，会在 manifest 切换后的目录清理中删除旧数据库路径；`CallReadHandle` 每次请求按 path 重新打开 SQLite，因此 Unix 后续请求会直接得到 missing file，Windows 仅可能被文件共享语义偶然掩盖。原 `capture` 还先开闭数据库、再取得进程内 lease，存在同进程 TOCTOU。

最终实现没有引入 PID/heartbeat sidecar、第三方 crate、schema 版本或 parser fact 变化，而是把 SQLite DELETE-journal 锁作为操作系统自动回收的跨进程事实：

1. 默认 index family 保留稳定的 `.fossilsense-generation-leases.sqlite`，只用于短时协调 reader acquisition、manifest publication 与 cleanup；publication 在封存 generation 到原子替换 manifest 的整个可见性窗口持有 family exclusive transaction。
2. 每个 canonical `index-g*.sqlite` 使用按文件名 BLAKE3 导出的独立 hidden lease SQLite。reader 在 family coordinator 内创建/打开该文件，执行 `BEGIN` 与真实 guard `SELECT` 取得 SHARED lock，并在锁内复核目标仍是 regular file；成功后只长期保留本 generation 的 shared transaction。cleanup 在 family coordinator 下逐 generation 尝试零等待 `BEGIN EXCLUSIVE`，因此当前 G2 reader 不会阻止回收无人使用的 G1，而跨进程 G1 reader会明确使 G1 跳过。
3. `IndexDbLease::acquire` 现在明确表示显式路径且没有目录副作用；只有由默认索引调用链选择的 `acquire_default_generation` 才创建跨进程租约。CLI 的显式 `--db C:\...\index-g1-custom.sqlite` 即使 basename 碰巧符合 generation 形状，也不会创建 hidden family DB 或触发任意用户目录清理。server、默认 CLI query 与并发 publication benchmark 已切到受保护入口；显式 benchmark/temp DB 保持原路径语义。
4. 最后一个 `Arc<IndexDbLeaseToken>` 的唯一析构先关闭 per-generation shared transaction，再触发目录维护，删除了基于 `Arc::strong_count` 选举清理者的竞态。不同旧 generation 同时析构时，reader-release 专用 cleanup 对 family coordinator 最多等待 1 秒；普通 publication/staging 维护仍使用 nonblocking acquisition，避免把常规 writer 清理变成长等待。
5. 目标在 manifest resolve 与 lease acquisition 之间消失时，构造函数现在返回错误而不是悬空 handle，并在仍持 family coordinator 时关闭新 shared connection、取得 exclusive 后回收 unused hashed lease。目录维护还会识别并安全回收进程异常退出留下的无读者 hashed lease family；活跃 shared transaction会使 sweep保守跳过。generation 归组同时覆盖 `-wal`、`-shm` 与 `-journal`。

TDD 的首个子进程用当前 Rust test executable 持有 G1 lease、原子发布精确 ready marker并阻塞在父进程 stdin。旧实现的另一个进程运行 cleanup 后实际删除 G1，断言稳定 RED；新实现连续运行两次 cleanup 都保留 G1。父进程随后直接 kill child、有界轮询回收，再次 cleanup 会删除 G1，直接证明进程终止由 SQLite/OS 释放 shared transaction。reviewer 发现的三个初版设计问题也分别先形成 RED：仅按 basename 推断会给显式 `index-g1-custom.sqlite` 创建 family DB；family-wide lifetime shared lock 会让 G2 永久阻塞 G1 回收；目标已删时 `at_generation` 会返回成功悬空 handle。修正后显式路径无副作用，G1/G2 同时持有时释放 G1 会立即回收 G1，missing target 明确失败。

后续 reviewer 的并发与残留审查又形成并关闭四类失败证据：两个最终 clone 并发 drop 在旧 `strong_count` 实现上第 1 次即漏清理；两个不同 generation 同时 drop 在第 6 组漏掉其中一代；missing target 会留下 hashed lease SQLite；旧 rollback journal 不会被归组。最终测试对同 token 和不同 generation 各执行 64 轮 barrier 并发释放，另覆盖真实 hashed orphan 的后续 sweep、active cross-process lease 的连续两次 cleanup、以及 WAL/SHM/journal 三类 sidecar。最后一轮 reviewer 专门复核 close→exclusive→unlink 顺序、family/per-generation 锁序、Unix split-inode、Windows 删除、析构递归和 1 秒等待，结论为无 P0–P2 finding。

独立 test-executor 与最终本机门禁：

| 命令/门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS，最终复跑 1.812 s |
| `cargo test -p fossilsense` | PASS，unit 1041 passed / 6 ignored，CLI 1 passed，LSP 2 passed；总计 1044 passed / 0 failed |
| `cargo clippy -p fossilsense --all-targets -- -D warnings` | PASS |
| `scripts/verify_architecture_fitness.ps1` | PASS，fail 0 / warn 14 / allowlisted 0 |
| `node scripts/test_architecture_fitness.js` | PASS，golden 8/8 |
| `cargo build --release -p fossilsense` | PASS，45.52 s |
| `cargo test --release -p fossilsense --bin fossilsense --no-run` | PASS，94 s |
| U-Boot full-index | PASS，wrapper 39,692.479 ms；engine 38,702 ms |
| U-Boot engine hydration | PASS，单代峰值 183,795,712 bytes；双代绝对峰值 348,880,896 bytes |

大型门禁继续使用 U-Boot commit `6741b0dfb41dc82a284ab1cff4c58af6ef2f3f9c`；样本保留 `boot/scene.c`、`boot/vbe_abrec.c` 两处既存空白差异并准确视为 dirty。机器与 Stage 4J 相同：Acer Nitro AN515-58、Intel Core i5-12500H（12 cores / 16 logical processors）、25,459,482,624 bytes RAM、Windows 11 Pro Insider build 26220。

full-index 保持 13,244 files、654,890 declarations、91,919 callable anchors 和 582,522 call sites。分段为 discover 2,422 ms、parse 6,621、write 18,201、check 10、include edge 3,004、secondary index 3,381、publication 4,351；峰值 Working Set 171,491,328 bytes、Private Bytes 161,083,392。fresh DB 为 363,139,072 bytes、88,657 pages，schema 28、semantic generation 1、13,244 file/active revisions、`cleanup_required=0`、`quick_check=ok`、foreign-key violations=0。hydration compact recall 为 93,747,480 bytes，首/次代构建 4,377/4,339 ms；单代约 175.28 MiB、双代约 332.72 MiB，分别低于 384/512 MiB 硬门禁。原始报告为 `target/benchmark/large-workspace-20260731_113419.json` 与同名 Markdown。

边界保持明确：租约只保护遵守 FossilSense 默认 generation 协议的 reader/cleanup，不能阻止外部程序直接删除数据库；reader-release cleanup 的 1 秒等待仍是有界 best-effort，极端长期占用可把空间回收延后到下一次 publication、staging maintenance 或 reader release，但不会发布半更新快照。稳定 family coordinator 约 8 KiB；per-generation lease DB 只在对应 reader 生命周期需要，正常释放、失败 acquisition、旧代删除与后续 orphan sweep都会回收。Stage 4J 已记录的显式 `--db --force` 进程强杀后唯一 staging 残留仍不按文件名自动删除，因为任意用户目录缺少可证明的 durable ownership；该边界是磁盘残留，不影响旧数据库和 manifest 正确性。按用户指示，Stage 4L 通过后不再扩展低概率边缘审计，转入最终发布验证。

### 1.5.0 最终发布判定（可发布）

最终 release source commit 为 `6e8aa03769f285515fc065b538848b1eaddbe1dc`。仓库一键入口 `build.ps1` 完整通过：locked pnpm install 未修改 lockfile；Rust 再次通过 unit 1041 / CLI 1 / LSP 2、0 failed；扩展 TypeScript compile/test 通过；release Rust binary、esbuild 单文件 client 和 `release-build.json` 被装入同一个 VSIX；`verify_release_hardening.ps1 -Version 1.5.0` 验证版本 1.5.0、schema 28、parser fact 8、resolver 5、relation protocol 2、内置 Windows binary 可执行性和全部 payload hash 后返回 PASS。

随后执行完整仓库门禁 `scripts/verify.ps1 -SkipInstall`，格式、Clippy、Rust 1044 项、architecture golden 8/8、真实架构扫描 fail 0 / warn 14 / allowlisted 0、release-hardening fixtures、benchmark entrypoint fixtures 与 VS Code 扩展测试全部通过，最终输出 `FossilSense verification passed.`。14 项 warning 仍全部是报告已知的既有 production 大文件提示，没有新增架构失败。

最终可发布工件：

| 字段 | 值 |
|---|---|
| VSIX | `dist/fossilsense-vscode-1.5.0_BUILD20260731_113858.vsix` |
| 文件大小 | 6,199,143 bytes |
| VSIX SHA-256 | `a57a9eab726fd3a9ea31ba9ba721adabd3aef0de433e2f48786bb5fd676e4986` |
| release-input SHA-256 | `3bbaab3b38268d3eb0477553ce6f6a7d79240327c2f83a1eda839a5af4611b85`（213 files） |
| artifact payload SHA-256 | `9d634e6ba36abc0ede686b0ecd56273c48cb7fe321400af2d27011835bc5c998` |
| native binary SHA-256 | `cd99125543cb64a62193d6cc1dd577e8cfe1f9f793455b47decfcfe6723528ec` |
| source commit | `6e8aa03769f285515fc065b538848b1eaddbe1dc` |

`release-build.json` 的 `worktreeDirty=true` 仅由用户持有且未跟踪的根 `AGENTS.md` 产生；该文件未修改、未提交，也不属于 213 个 release inputs。所有 tracked 文件在打包时干净，发布硬化脚本按实际 release-input 内容指纹验证通过。当前结论是 1.5.0 已满足代码正确性、架构、完整性、大型工作区 60 秒性能、双代内存、自包含打包和发布硬化要求；无需继续处理低优先级边缘项即可发布。
