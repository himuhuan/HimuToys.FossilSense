# FossilSense 1.4.2 交付说明

> 状态：implemented / ready for review
>
> 交付日期：2026-07-14

## VSIX 产物

本次自包含 VSIX artifact 为：

```text
dist/fossilsense-vscode-1.4.2_BUILD20260714_051300.vsix
```

产物包含 `extension/bin/fossilsense.exe`、打包后的 `extension/out/extension.js`、扩展 manifest、README 与 Relation Panel 资源；用户不需要另装 Rust、ctags、cscope 或 clangd。

产物可复核信息：

- VSIX SHA-256：`2a163d82f638bed7cd6523969f6a75fc05fe284b8b08b630c746a0bfc40db22b`
- release-input SHA-256：`840d2302c3919c3eb374ad852337c6ac0ebf72ec98b34005846e1f0a1ffa1dcb`（149 个输入文件）
- packaged source commit：`237d0e6f28e45bda0b88868bd18bb7797ace6b1f`
- aggregate payload SHA-256：`c0e6afa6a32817155b0012ba3eecf11cf3603da5b6d569d269f2f33c8b9b26ca`
- 打包时工作树状态：clean（`worktreeDirty=false`）

## 用户可见行为变化

- Hover、Go to Definition、Signature Help、函数补全文档与 Call Hierarchy 现在共享 schema 16 callable candidate pipeline 和同一请求期 open-document overlay。
- 完整且可靠的 call arity 会优先保留兼容签名；Signature Help 的 active signature 选择首个 proven-compatible 候选，未知 arity 保留为可解释 fallback。
- `.h/.c` counterpart 只在 canonical signature 完全一致、external linkage、source 到 header reach 闭合且两个方向均唯一时成立。1:N、N:1、open/incomplete facts 与 dirty tombstone 均不声称配对。
- 调用点 Definition 与 Call Hierarchy 保持 source-definition-first；严格配对锚点跳到对侧；未配对的声明/定义锚点仍可跳转自身。
- `struct` / `class` / `union` Hover 使用精确 record range 展示有界完整声明；唯一 `typedef` 链显示 `(aka. ...)` 与终点 record。
- 所有未同步 open documents 都能参与 callable、record、alias 与 Call Hierarchy 请求；dirty include 会形成 request-local reach graph，保存前即可反映关系变化。

## 已知限制与非目标

- 结果仍是 best-effort candidates，不是编译器语义绑定；不会读取编译参数或执行预处理器。
- 成员调用、函数指针、callable object、宏展开、模板与参数类型重载不在自由函数正式绑定范围内；unsupported 形态不会伪装成已解析目标。
- alias ambiguity、cycle、unsupported declarator、source stale/oversized/unreadable 会降级到原始签名，不猜测唯一 `aka`。
- C++ `using T = ...` 的 alias trace/`aka` 尚不支持；它不会被当成已解析 typedef 链。
- open 或不完整的 include reach、签名不一致、internal linkage、1:N/N:1 会关闭严格 counterpart，但普通同名候选仍可返回。
- Call Relations wire protocol 仍为 v2；本次只更新内部 resolver 和 schema，不提供 v1 adapter 或 schema 15 生产双读。

## 验证结果

验证已执行：

- `cargo fmt --all -- --check`：通过。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`：通过。
- `cargo test --workspace --all-features`：794 passed，4 ignored，0 failed；LSP smoke 2 passed。
- VS Code 扩展 `compile` 与 `test`：通过。
- architecture fitness golden tests：8 cases passed；正式报告 0 fail，仅保留 large-file warnings。
- release hardening 与 benchmark entry-point 自测：通过。
- VSIX 打包：通过，内置 `fossilsense.exe` 17,129,472 bytes（约 16.34 MiB），产物 5,584,250 bytes（约 5.33 MiB）。
