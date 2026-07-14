# FossilSense 1.4.3 交付说明

> 状态：implemented / ready for review
>
> 交付日期：2026-07-14

## VSIX 产物

本次自包含 VSIX artifact 为：

```text
dist/fossilsense-vscode-1.4.3_BUILD20260714_162613.vsix
```

产物包含 `extension/bin/fossilsense.exe`、打包后的
`extension/out/extension.js`、扩展 manifest、README 与 Relation Panel 资源；用户不
需要另装 Rust、ctags、cscope 或 clangd。

产物可复核信息：

- VSIX SHA-256：`dbf27de75ae40cbd85667821af18116da66f7ad8c39225bcd9ccdd72f1bd3002`
- release-input SHA-256：`c881d86f57b93c55c1352b934b1ced8e19398b2beb367ba4669aed377da80b81`（149 个输入文件）
- packaged source commit：`eebf3e3c94469d17fc2da6595c9d49ff325077ec`
- aggregate payload SHA-256：`d29afb003b2246d327c38f1921e4bab3764b9131b94a3c4942805c3efd154db6`
- 打包时工作树状态：clean（`worktreeDirty=false`）

## 用户可见行为变化

- 修复 schema 16 全量构建在 `finalizing` 阶段错误回收 canonical/presentation
  signature 字符串的问题。
- 避免 SQLite 对仍被引用的 `call_strings` 发起删除并反复扫描大型 call-fact
  表；U-Boot 等大型工作区可以继续进入 call-index build、publication 和 ready。
- schema 版本仍为 16，callable、record、typedef、arity、严格 `.h/.c`
  counterpart 和 all-open-document overlay 的语义范围保持不变。

## 已知限制与非目标

- 本版本是 full-build 性能 hotfix，不扩展 C/C++ 编译级绑定能力；结果仍是带
  confidence、ambiguity、coverage 和 fallback 的 best-effort candidates。
- 成员调用、函数指针、callable object、宏展开、模板与参数类型重载仍不支持。
- 本次不改变 SQLite schema，因此不会通过新 schema 号主动淘汰已经成功发布的
  schema 16 索引；此前卡在 publication 前的工作区会按现有 side-by-side 规则重建。

## 验证结果

验证已执行：

- schema-16 call-string cleanup 定向回归：通过；full-build 在尚未建立 call-fact
  indexes 时仍能保留 canonical/presentation signature strings，且
  `PRAGMA foreign_key_check` 无条目。
- `cargo fmt --all -- --check`：通过。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`：通过。
- `cargo test --workspace --all-features`：795 passed，4 ignored，0 failed；LSP smoke
  2 passed。
- VS Code 扩展 `compile` 与 `test`：通过。
- architecture fitness golden tests：8 cases passed；正式报告 0 fail，仅保留
  large-file warnings。
- release hardening 与 benchmark entry-point 自测：通过。
- U-Boot cold full-index：在 120 秒硬超时下 35,055.853 ms 完成（内部
  34,219 ms；write 25,108 ms；secondary indexes 1,993 ms），13,244 个文件、
  631,893 个 symbols、91,155 个 callable anchors、582,841 个 call sites；无
  cleanup warning，SQLite integrity 为 `ok`，foreign-key violations 为 0。
- U-Boot 数据库保留了旧 GC 条件会误删的 131,816 个 schema-16-only signature
  strings；`call_strings` 总数为 233,091。
- VSIX 打包：通过，内置 `fossilsense.exe` 17,133,056 bytes（约 16.34 MiB），
  产物 5,586,778 bytes（约 5.33 MiB）。
