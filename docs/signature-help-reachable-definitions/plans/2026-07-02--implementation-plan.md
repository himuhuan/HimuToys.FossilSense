# Signature Help Reachable Definitions 实现计划

> **给代理执行者：** 必须使用 `himupowers:subagent-driven-development`（推荐）或 `himupowers:executing-plans` 逐任务执行本计划。
> 需求文档：`docs/signature-help-reachable-definitions/requirements.md`

**目标：** 为 FossilSense 增加 best-effort C/C++ 函数参数提示，并让参数提示与查找定义共享现有 include reachability / resolver 排序证据。
**架构：** 新增协议无关的 `query::signatures` 纯逻辑模块，负责调用点识别、签名参数拆分、函数候选投影和排序保真；新增 `server::signature_help` 负责 LSP `SignatureHelp` 组装。`goto_definition` 继续使用 `rank_definitions_into_candidates_with_scope`，本计划补强其函数可达性回归测试，避免 signature-help 与 definition ranking 发生概念漂移。
**技术栈：** Rust 2021, `tower-lsp` / `lsp-types` 0.94.1, SQLite via `rusqlite`, tree-sitter 现有解析入口, `cargo test`, VS Code extension TypeScript / pnpm 文档与编译检查。

## 全局约束

- FossilSense 默认没有可靠编译环境；不得依赖 clangd、完整 IntelliSense、`compile_commands.json`、外部 ctags 或编译参数。
- 返回的是 best-effort candidate，不是编译级语义绑定；必须暴露 confidence / fallback / ambiguity。
- 新能力必须复用 `DefinitionCandidate`, `ScopeTier`, `ResolutionConfidence`, `ResolutionReason`, `ReachScope`, `OpenReason` 和共享 `resolver`，不得新增平行的 smart/semantic 排序概念。
- tier 必须严格主导 match quality 和 locality；不得恢复旧 magic score。
- Include scope open 时不得硬过滤候选；不确定候选应走 `Unknown` / `Ambiguous` / `Fallback` 表达。
- 一期不实现 overload resolution、argument type matching、模板/命名空间/继承/访问控制、function-like macro 参数提示、预处理分支求值、函数指针目标推断或 member function receiver 类型推断。
- 请求期工作必须只依赖当前 open document 上下文和 exact-name SQLite 查询；不得扫描整个 workspace 或做 broad fuzzy search。
- 缺索引、缺 workspace root、解析失败、签名拆分失败和 unsupported call shape 必须降级为空或 reduced result，不得让 LSP 请求失败。
- Phase one 不新增用户设置；FossilSense server active 时发布 signature-help provider，`fossilsense.mode = off` 或 Stop Server 停用。
- 修改 README、extension README、package 描述时必须同步 can / cannot / fallback，不得写成精确语义服务。

## 文件结构

- 新建 `crates/fossilsense/src/query/signatures.rs`
  - 职责：协议无关 signature-help 查询辅助。
  - 输入：open document text + LSP line/UTF-16 column、stored function signature string、`Vec<SymbolRecord>`、current relative path、optional `ReachScope`。
  - 输出：`CallContext`, `SignatureParts`, `ParameterSpan`, `RankedSignatureCandidate`。
  - 错误策略：返回 `None` 或空 vec；不 panic，不引入 `tower-lsp` 类型。
  - 依赖：`query::text::byte_offset_at`, `store::SymbolRecord`, `model::DefinitionCandidate`, `reachability::ReachScope`, `query::definitions::rank_definitions_into_candidates_with_scope`。
- 修改 `crates/fossilsense/src/query.rs:13-21`
  - 职责：声明并导出 `signatures` 模块的公共 helper。
- 修改 `crates/fossilsense/src/query/definitions.rs:226-518`
  - 职责：补强 go-to-definition 函数可达性和 open-scope 排序回归测试。
- 修改 `crates/fossilsense/src/server.rs:10-42`
  - 职责：导入 LSP signature-help 类型并声明 `mod signature_help;`。
- 修改 `crates/fossilsense/src/server/options.rs:1-76`
  - 职责：提供 `signature_help_options()` helper，集中定义 trigger/retrigger characters，并单测。
- 修改 `crates/fossilsense/src/server/language_server.rs:21-90`
  - 职责：初始化 capabilities 时发布 `signature_help_provider`。
- 修改 `crates/fossilsense/src/server/language_server.rs:758-760` 附近
  - 职责：实现 `LanguageServer::signature_help` trait method，转发给 `Backend::provide_signature_help`。
- 新建 `crates/fossilsense/src/server/signature_help.rs`
  - 职责：LSP orchestration；读取 document snapshot、计算 call context、root/current_rel/reach scope、exact-name DB query、组装 `SignatureHelp`。
  - 输入：`SignatureHelpParams`。
  - 输出：`LspResult<Option<SignatureHelp>>`。
  - 错误策略：`unwrap_query("signature help", result)` 记录结构化错误；普通不可用状态返回 `Ok(None)`。
- 修改 `README.md:11-128`
  - 职责：同步 baseline capabilities 与 Completion 章节，描述 signature help can/cannot/fallback。
- 修改 `extensions/vscode/README.md:10-115`
  - 职责：同步 extension capability 与 settings 说明。
- 修改 `extensions/vscode/package.json:2-8`
  - 职责：描述中加入 signature help，不新增 setting。

### Task 1：新增调用点识别纯逻辑

**覆盖需求：** FR2, FR6, FR9, NFR4, NFR5

**文件：**
- 新建：`crates/fossilsense/src/query/signatures.rs`
- 修改：`crates/fossilsense/src/query.rs:13-21`
- 测试：`crates/fossilsense/src/query/signatures.rs`

**接口：**
- 消费：`query::byte_offset_at(text, line, character) -> usize`
- 产出：`CallContext { name: String, active_argument: u32 }`
- 产出：`call_context_at(text: &str, line: u32, character: u32) -> Option<CallContext>`

- [ ] **步骤 1：写失败测试**

在 `crates/fossilsense/src/query/signatures.rs` 中先加入测试模块和期望 API：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_context_after_open_paren_is_first_argument() {
        let text = "int main(void) {\n  foo(\n}\n";
        let ctx = call_context_at(text, 1, 6).expect("call context");
        assert_eq!(ctx.name, "foo");
        assert_eq!(ctx.active_argument, 0);
    }

    #[test]
    fn call_context_counts_only_top_level_commas() {
        let text = "void f(void) {\n  foo(a, bar(b, c), arr[1, 2], \n}\n";
        let ctx = call_context_at(text, 1, 34).expect("call context");
        assert_eq!(ctx.name, "foo");
        assert_eq!(ctx.active_argument, 3);
    }

    #[test]
    fn call_context_uses_nearest_nested_call() {
        let text = "void f(void) {\n  outer(alpha, inner(beta, \n}\n";
        let ctx = call_context_at(text, 1, 28).expect("call context");
        assert_eq!(ctx.name, "inner");
        assert_eq!(ctx.active_argument, 1);
    }

    #[test]
    fn call_context_rejects_control_keywords() {
        let text = "void f(void) {\n  if (ready, \n}\n";
        assert!(call_context_at(text, 1, 13).is_none());
    }
}
```

修改 `crates/fossilsense/src/query.rs` 先声明模块与导出：

```rust
mod signatures;

pub use signatures::{call_context_at, CallContext};
```

- [ ] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense query::signatures -- --nocapture`

预期：编译失败，失败原因是 `call_context_at` / `CallContext` 尚未实现，或 `mod signatures` 文件刚创建但 API 缺失。

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/query/signatures.rs` 中实现：

```rust
use super::byte_offset_at;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallContext {
    pub name: String,
    pub active_argument: u32,
}

pub fn call_context_at(text: &str, line: u32, character: u32) -> Option<CallContext> {
    let offset = byte_offset_at(text, line, character).min(text.len());
    let bytes = text.as_bytes();
    let mut i = offset;
    let mut paren_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut brace_depth = 0i32;
    let mut active_argument = 0u32;

    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => paren_depth += 1,
            b'(' if paren_depth > 0 => paren_depth -= 1,
            b']' => bracket_depth += 1,
            b'[' if bracket_depth > 0 => bracket_depth -= 1,
            b'}' => brace_depth += 1,
            b'{' if brace_depth > 0 => brace_depth -= 1,
            b',' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                active_argument += 1;
            }
            b'(' if bracket_depth == 0 && brace_depth == 0 => {
                let name = identifier_before(bytes, i)?;
                if is_control_keyword(&name) {
                    return None;
                }
                return Some(CallContext { name, active_argument });
            }
            _ => {}
        }
    }
    None
}

fn identifier_before(bytes: &[u8], open_paren: usize) -> Option<String> {
    let mut end = open_paren;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident_continue(bytes[start - 1]) {
        start -= 1;
    }
    if start == end || !is_ident_start(bytes[start]) {
        return None;
    }
    std::str::from_utf8(&bytes[start..end]).ok().map(str::to_string)
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_control_keyword(name: &str) -> bool {
    matches!(name, "if" | "for" | "while" | "switch" | "return" | "sizeof" | "defined")
}
```

- [ ] **步骤 4：运行测试并确认通过**

运行：`cargo test -p fossilsense query::signatures -- --nocapture`

预期：Task 1 的 4 个测试通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/query.rs crates/fossilsense/src/query/signatures.rs
git commit -m "feat: detect signature help call context"
```

### Task 2：解析存储签名为参数 label ranges

**覆盖需求：** FR5, FR6, FR9, NFR5

**文件：**
- 修改：`crates/fossilsense/src/query/signatures.rs`
- 测试：`crates/fossilsense/src/query/signatures.rs`

**接口：**
- 消费：Task 1 的 `query::signatures` 模块。
- 产出：`ParameterSpan { start: u32, end: u32 }`
- 产出：`SignatureParts { label: String, parameters: Vec<ParameterSpan> }`
- 产出：`signature_parts(signature: &str) -> SignatureParts`

- [ ] **步骤 1：写失败测试**

追加测试：

```rust
#[test]
fn signature_parts_extracts_simple_parameters() {
    let parts = signature_parts("int foo(int a, const char *name)");
    assert_eq!(parts.label, "int foo(int a, const char *name)");
    let labels: Vec<&str> = parts
        .parameters
        .iter()
        .map(|span| &parts.label[span.start as usize..span.end as usize])
        .collect();
    assert_eq!(labels, vec!["int a", "const char *name"]);
}

#[test]
fn signature_parts_keeps_void_parameter_list_empty() {
    let parts = signature_parts("void reset(void)");
    assert!(parts.parameters.is_empty());
}

#[test]
fn signature_parts_ignores_nested_commas() {
    let parts = signature_parts("void visit(int (*cb)(int, int), int flags)");
    let labels: Vec<&str> = parts
        .parameters
        .iter()
        .map(|span| &parts.label[span.start as usize..span.end as usize])
        .collect();
    assert_eq!(labels, vec!["int (*cb)(int, int)", "int flags"]);
}

#[test]
fn malformed_signature_returns_whole_label_without_parameters() {
    let parts = signature_parts("int broken(int a, ");
    assert_eq!(parts.label, "int broken(int a, ");
    assert!(parts.parameters.is_empty());
}
```

- [ ] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense query::signatures -- --nocapture`

预期：编译失败或新增测试失败，原因是 `signature_parts` / spans 尚未实现。

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/query/signatures.rs` 增加：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureParts {
    pub label: String,
    pub parameters: Vec<ParameterSpan>,
}

pub fn signature_parts(signature: &str) -> SignatureParts {
    let label = signature.trim().trim_end_matches('{').trim().to_string();
    let Some((open, close)) = parameter_list_bounds(&label) else {
        return SignatureParts { label, parameters: Vec::new() };
    };
    let inner = &label[open + 1..close];
    if inner.trim().is_empty() || inner.trim() == "void" {
        return SignatureParts { label, parameters: Vec::new() };
    }
    let Some(parameters) = split_parameter_spans(&label, open + 1, close) else {
        return SignatureParts { label, parameters: Vec::new() };
    };
    SignatureParts { label, parameters }
}

fn parameter_list_bounds(label: &str) -> Option<(usize, usize)> {
    let bytes = label.as_bytes();
    let open = bytes.iter().position(|byte| *byte == b'(')?;
    let mut depth = 0i32;
    for (idx, byte) in bytes.iter().enumerate().skip(open) {
        match *byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, idx));
                }
            }
            _ => {}
        }
    }
    None
}

fn split_parameter_spans(label: &str, start: usize, end: usize) -> Option<Vec<ParameterSpan>> {
    let bytes = label.as_bytes();
    let mut spans = Vec::new();
    let mut part_start = start;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    for idx in start..end {
        match bytes[idx] {
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b'{' => brace += 1,
            b'}' => brace -= 1,
            b',' if paren == 0 && bracket == 0 && brace == 0 => {
                push_trimmed_span(label, part_start, idx, &mut spans);
                part_start = idx + 1;
            }
            _ => {}
        }
        if paren < 0 || bracket < 0 || brace < 0 {
            return None;
        }
    }
    if paren != 0 || bracket != 0 || brace != 0 {
        return None;
    }
    push_trimmed_span(label, part_start, end, &mut spans);
    Some(spans)
}

fn push_trimmed_span(label: &str, start: usize, end: usize, spans: &mut Vec<ParameterSpan>) {
    let mut s = start;
    let mut e = end;
    let bytes = label.as_bytes();
    while s < e && bytes[s].is_ascii_whitespace() {
        s += 1;
    }
    while e > s && bytes[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    if s < e {
        spans.push(ParameterSpan { start: s as u32, end: e as u32 });
    }
}
```

- [ ] **步骤 4：运行测试并确认通过**

运行：`cargo test -p fossilsense query::signatures -- --nocapture`

预期：Task 1 和 Task 2 的 query signature tests 全部通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/query/signatures.rs
git commit -m "feat: parse indexed function signatures"
```

### Task 3：函数候选过滤、排序和签名保留

**覆盖需求：** FR3, FR4, FR7, FR8, NFR2, NFR3

**文件：**
- 修改：`crates/fossilsense/src/query/signatures.rs`
- 修改：`crates/fossilsense/src/query.rs:17-21`
- 测试：`crates/fossilsense/src/query/signatures.rs`

**接口：**
- 消费：`rank_definitions_into_candidates_with_scope(records, current_rel_path, scope)`
- 消费：`store::SymbolRecord`
- 产出：`RankedSignatureCandidate { candidate: DefinitionCandidate, signature: String }`
- 产出：`rank_function_signature_candidates(records, current_rel_path, scope, limit) -> Vec<RankedSignatureCandidate>`
- 产出：`SIGNATURE_HELP_LIMIT: usize = 10`

- [ ] **步骤 1：写失败测试**

追加测试：

```rust
fn symbol_record(name: &str, kind: &str, role: &str, path: &str, signature: &str) -> crate::store::SymbolRecord {
    crate::store::SymbolRecord {
        id: 0,
        name: name.to_string(),
        kind: kind.to_string(),
        role: role.to_string(),
        path: path.to_string(),
        start_line: 1,
        start_col: 0,
        end_line: 1,
        end_col: 0,
        signature: signature.to_string(),
        guard: None,
        source: "workspace".to_string(),
        directly_included: false,
    }
}

#[test]
fn signature_candidates_keep_only_functions_and_preserve_signature() {
    let records = vec![
        symbol_record("foo", "macro", "definition", "inc/foo.h", "#define foo(x) (x)"),
        symbol_record("foo", "function", "declaration", "inc/foo.h", "int foo(int x);"),
    ];
    let ranked = rank_function_signature_candidates(records, "src/main.c", None, 10);
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].signature, "int foo(int x);");
    assert_eq!(ranked[0].candidate.kind, "function");
}

#[test]
fn signature_candidates_use_reachability_tier_order() {
    let records = vec![
        symbol_record("foo", "function", "definition", "other/foo.c", "int foo(float x)"),
        symbol_record("foo", "function", "declaration", "inc/foo.h", "int foo(int x);"),
    ];
    let reach = crate::reachability::ReachScope {
        files: ["src/main.c".to_string(), "inc/foo.h".to_string()].into_iter().collect(),
        open: false,
        reason: None,
    };
    let ranked = rank_function_signature_candidates(records, "src/main.c", Some(&reach), 10);
    assert_eq!(ranked[0].candidate.path, "inc/foo.h");
    assert_eq!(ranked[0].candidate.tier, crate::model::ScopeTier::Reachable);
    assert_eq!(ranked[0].signature, "int foo(int x);");
}

#[test]
fn signature_candidates_cap_results_after_ranking() {
    let records = vec![
        symbol_record("foo", "function", "definition", "a.c", "int foo(int a)"),
        symbol_record("foo", "function", "definition", "b.c", "int foo(int b)"),
    ];
    let ranked = rank_function_signature_candidates(records, "main.c", None, 1);
    assert_eq!(ranked.len(), 1);
}
```

- [ ] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense query::signatures -- --nocapture`

预期：编译失败或 tests 失败，原因是 ranking API 尚未实现。

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/query/signatures.rs` 增加：

```rust
use std::collections::HashMap;

use crate::model::DefinitionCandidate;
use crate::reachability::ReachScope;
use crate::store::SymbolRecord;

pub const SIGNATURE_HELP_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedSignatureCandidate {
    pub candidate: DefinitionCandidate,
    pub signature: String,
}

pub fn rank_function_signature_candidates(
    records: Vec<SymbolRecord>,
    current_rel_path: &str,
    scope: Option<&ReachScope>,
    limit: usize,
) -> Vec<RankedSignatureCandidate> {
    let functions: Vec<SymbolRecord> = records
        .into_iter()
        .filter(|record| record.kind == "function")
        .collect();
    let signatures: HashMap<(String, u32, u32, String), String> = functions
        .iter()
        .map(|record| {
            (
                (
                    record.path.clone(),
                    record.start_line,
                    record.start_col,
                    record.role.clone(),
                ),
                record.signature.clone(),
            )
        })
        .collect();
    crate::query::rank_definitions_into_candidates_with_scope(functions, current_rel_path, scope)
        .into_iter()
        .filter_map(|candidate| {
            let key = (
                candidate.path.clone(),
                candidate.range.start_line,
                candidate.range.start_col,
                candidate.role.clone(),
            );
            signatures.get(&key).cloned().map(|signature| RankedSignatureCandidate {
                candidate,
                signature,
            })
        })
        .take(limit)
        .collect()
}
```

在 `crates/fossilsense/src/query.rs` 导出：

```rust
pub use signatures::{
    call_context_at, rank_function_signature_candidates, signature_parts, CallContext,
    ParameterSpan, RankedSignatureCandidate, SignatureParts, SIGNATURE_HELP_LIMIT,
};
```

- [ ] **步骤 4：运行测试并确认通过**

运行：`cargo test -p fossilsense query::signatures -- --nocapture`

预期：Task 1-3 的 query signature tests 全部通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/query.rs crates/fossilsense/src/query/signatures.rs
git commit -m "feat: rank function signature candidates"
```

### Task 4：补强 go-to-definition open-aware 可达性回归

**覆盖需求：** FR4, FR7, FR8, NFR2, NFR6

**文件：**
- 修改：`crates/fossilsense/src/query/definitions.rs:226-518`
- 测试：`crates/fossilsense/src/query/definitions.rs`

**接口：**
- 消费：`rank_definitions_into_candidates_with_scope`
- 消费：`ReachScope { files, open, reason }`
- 产出：函数定义排序回归测试，确保 signature help 与 goto-definition 共用的 resolver 语义稳定。

- [ ] **步骤 1：写失败测试**

在 `crates/fossilsense/src/query/definitions.rs` 的 `mod tests` 中追加：

```rust
#[test]
fn goto_with_scope_orders_current_reachable_external_unknown() {
    let mut external = record_ext(
        "foo",
        "function",
        "definition",
        "C:/sdk/foo.h",
        3,
        true,
    );
    external.directly_included = true;
    let candidates = vec![
        record("foo", "function", "definition", "other/foo.c", 20),
        external,
        record("foo", "function", "definition", "inc/foo.h", 7),
        record("foo", "function", "definition", "src/main.c", 30),
    ];
    let scope = crate::reachability::ReachScope {
        files: ["src/main.c".to_string(), "inc/foo.h".to_string()].into_iter().collect(),
        open: true,
        reason: Some(crate::reachability::OpenReason::AmbiguousInclude),
    };
    let ranked = rank_definitions_into_candidates_with_scope(candidates, "src/main.c", Some(&scope));
    let paths: Vec<&str> = ranked.iter().map(|candidate| candidate.path.as_str()).collect();
    assert_eq!(paths, vec!["src/main.c", "inc/foo.h", "C:/sdk/foo.h", "other/foo.c"]);
    assert_eq!(ranked[3].tier, crate::model::ScopeTier::Unknown);
    assert_eq!(ranked[3].confidence, ResolutionConfidence::Ambiguous);
}

#[test]
fn goto_open_unresolved_scope_uses_fallback_for_unknown_candidates() {
    let candidates = vec![record("foo", "function", "definition", "other/foo.c", 20)];
    let scope = crate::reachability::ReachScope {
        files: HashSet::new(),
        open: true,
        reason: Some(crate::reachability::OpenReason::UnresolvedInclude),
    };
    let ranked = rank_definitions_into_candidates_with_scope(candidates, "src/main.c", Some(&scope));
    assert_eq!(ranked[0].tier, crate::model::ScopeTier::Unknown);
    assert_eq!(ranked[0].confidence, ResolutionConfidence::Fallback);
    assert_eq!(ranked[0].reason, ResolutionReason::GlobalFallback);
}
```

- [ ] **步骤 2：运行测试并确认失败或 expose 现有行为**

运行：`cargo test -p fossilsense query::definitions::tests::goto_with_scope_orders_current_reachable_external_unknown query::definitions::tests::goto_open_unresolved_scope_uses_fallback_for_unknown_candidates -- --nocapture`

预期：如果现有 implementation 已满足，则 tests 通过，记录为 regression coverage；如果失败，失败点必须是 tier/order/confidence projection 与需求不一致。

- [ ] **步骤 3：写最小实现或只保留回归**

若步骤 2 失败，修正 `rank_definitions_into_candidates_with_scope` 附近逻辑，保持以下结构：

```rust
let tier = crate::resolver::scope_tier(
    &record.path,
    external,
    record.directly_included,
    Some(&ctx),
);
let (confidence, reason) =
    crate::resolver::confidence_reason_for(tier, true, scope.and_then(|s| s.reason));
```

不得把 `scope.reason` 丢弃；不得把 open scope 中 not-in-set workspace candidate 降成 `Global`。

- [ ] **步骤 4：运行测试并确认通过**

运行：`cargo test -p fossilsense query::definitions -- --nocapture`

预期：definitions tests 全部通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/query/definitions.rs
git commit -m "test: cover reachable definition ordering"
```

### Task 5：发布 signature-help capability

**覆盖需求：** FR1, NFR6, NFR7

**文件：**
- 修改：`crates/fossilsense/src/server/options.rs:1-76`
- 修改：`crates/fossilsense/src/server.rs:10-42`
- 修改：`crates/fossilsense/src/server/language_server.rs:21-90`
- 新建：`crates/fossilsense/src/server/signature_help.rs`
- 测试：`crates/fossilsense/src/server/options.rs`

**接口：**
- 消费：`lsp_types::SignatureHelpOptions`
- 产出：`signature_help_options() -> SignatureHelpOptions`
- 产出：server capability `signature_help_provider: Some(signature_help_options())`

- [ ] **步骤 1：写失败测试**

在 `crates/fossilsense/src/server/options.rs` tests 中追加：

```rust
#[test]
fn signature_help_options_trigger_on_paren_and_comma() {
    let options = signature_help_options();
    assert_eq!(
        options.trigger_characters,
        Some(vec!["(".to_string(), ",".to_string()])
    );
    assert_eq!(options.retrigger_characters, Some(vec![",".to_string()]));
}
```

- [ ] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense server::options::tests::signature_help_options_trigger_on_paren_and_comma -- --nocapture`

预期：编译失败，原因是 `signature_help_options` 尚未实现。

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/server/options.rs` import 中加入 `SignatureHelpOptions`：

```rust
use tower_lsp::lsp_types::{CompletionList, CompletionResponse, InitializeParams, SignatureHelpOptions};
```

增加 helper：

```rust
pub(super) fn signature_help_options() -> SignatureHelpOptions {
    SignatureHelpOptions {
        trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
        retrigger_characters: Some(vec![",".to_string()]),
        ..Default::default()
    }
}
```

在 `crates/fossilsense/src/server.rs` import 中加入 signature-help 类型：

```rust
ParameterInformation, ParameterLabel, SignatureHelp, SignatureHelpOptions, SignatureHelpParams,
SignatureInformation,
```

在 module declarations 增加：

```rust
mod signature_help;
```

在 options import 增加：

```rust
signature_help_options,
```

创建 `crates/fossilsense/src/server/signature_help.rs`，先放入可编译 skeleton：

```rust
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{SignatureHelp, SignatureHelpParams};

use super::Backend;

impl Backend {
    pub(super) async fn provide_signature_help(
        &self,
        _params: SignatureHelpParams,
    ) -> LspResult<Option<SignatureHelp>> {
        Ok(None)
    }
}
```

在 `crates/fossilsense/src/server/language_server.rs` capabilities 中加入：

```rust
signature_help_provider: Some(signature_help_options()),
```

在 trait impl 中加入 method：

```rust
async fn signature_help(&self, params: SignatureHelpParams) -> LspResult<Option<SignatureHelp>> {
    self.provide_signature_help(params).await
}
```

- [ ] **步骤 4：运行测试并确认通过**

运行：`cargo test -p fossilsense server::options::tests::signature_help_options_trigger_on_paren_and_comma -- --nocapture`

预期：新增 options test 通过，project 编译通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/server.rs crates/fossilsense/src/server/options.rs crates/fossilsense/src/server/language_server.rs crates/fossilsense/src/server/signature_help.rs
git commit -m "feat: advertise signature help provider"
```

### Task 6：组装 LSP SignatureHelp 响应

**覆盖需求：** FR1, FR3, FR5, FR6, FR7, FR9, NFR2, NFR3, NFR4, NFR5, NFR6, NFR7

**文件：**
- 修改：`crates/fossilsense/src/server/signature_help.rs`
- 测试：`crates/fossilsense/src/server/signature_help.rs`

**接口：**
- 消费：`query::call_context_at`
- 消费：`query::rank_function_signature_candidates`
- 消费：`query::signature_parts`
- 消费：`Backend::document_snapshot`, `Backend::root_for_uri`, `Backend::reach_scope_for`, `Backend::unwrap_query`
- 产出：`Backend::provide_signature_help(params) -> LspResult<Option<SignatureHelp>>`
- 产出：helper `signature_information_for(ranked, active_argument) -> SignatureInformation`

- [ ] **步骤 1：写失败测试**

在 `crates/fossilsense/src/server/signature_help.rs` 追加 pure helper tests：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(signature: &str, tier: crate::model::ScopeTier) -> crate::query::RankedSignatureCandidate {
        let (confidence, reason) = crate::resolver::confidence_reason_for(tier, true, None);
        crate::query::RankedSignatureCandidate {
            signature: signature.to_string(),
            candidate: crate::model::DefinitionCandidate {
                name: "foo".to_string(),
                kind: "function".to_string(),
                role: "definition".to_string(),
                path: "inc/foo.h".to_string(),
                range: crate::model::CandidateRange {
                    start_line: 0,
                    start_col: 0,
                    end_line: 0,
                    end_col: 0,
                },
                source: "workspace".to_string(),
                tier,
                base_match: 1000,
                confidence,
                reason,
            },
        }
    }

    #[test]
    fn signature_information_uses_parameter_offsets() {
        let info = signature_information_for(&candidate("int foo(int a, const char *name)", crate::model::ScopeTier::Reachable), 1);
        assert_eq!(info.label, "int foo(int a, const char *name)");
        let params = info.parameters.expect("parameters");
        assert_eq!(params.len(), 2);
        assert_eq!(info.active_parameter, Some(1));
    }

    #[test]
    fn signature_information_documents_rank_reason() {
        let info = signature_information_for(&candidate("int foo(int a)", crate::model::ScopeTier::External), 0);
        let doc = match info.documentation.expect("documentation") {
            Documentation::String(value) => value,
            Documentation::MarkupContent(markup) => markup.value,
        };
        assert!(doc.contains("tier: external"));
        assert!(doc.contains("confidence: heuristic"));
        assert!(doc.contains("reason: external_first_layer"));
    }

    #[test]
    fn signature_information_omits_out_of_range_active_parameter() {
        let info = signature_information_for(&candidate("int foo(int a)", crate::model::ScopeTier::Global), 3);
        assert_eq!(info.active_parameter, None);
    }
}
```

- [ ] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense server::signature_help -- --nocapture`

预期：编译失败或 tests 失败，原因是 helper/LSP assembly 尚未实现。

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/server/signature_help.rs` 扩展实现：

```rust
use anyhow::Result;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    Documentation, ParameterInformation, ParameterLabel, SignatureHelp, SignatureHelpParams,
    SignatureInformation,
};

use super::{uri_to_path, Backend};
use crate::pathing;
use crate::query;
use crate::store::IndexStore;

impl Backend {
    pub(super) async fn provide_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> LspResult<Option<SignatureHelp>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let Some((_version, text)) = self.document_snapshot(&uri).await else {
            return Ok(None);
        };
        let Some(call) = query::call_context_at(&text, position.position.line, position.position.character) else {
            return Ok(None);
        };
        let Some(root) = self.root_for_uri(&uri).await else {
            return Ok(None);
        };
        let current_rel = uri_to_path(&uri)
            .and_then(|path| pathing::relative_slash_path(&root, &path).ok())
            .unwrap_or_default();
        let reach_scope = self.reach_scope_for(&uri).await.map(|(_, reach)| reach);
        let limit = query::SIGNATURE_HELP_LIMIT;

        let result = tokio::task::spawn_blocking(move || -> Result<Vec<SignatureInformation>> {
            let db_path = pathing::default_index_path(&root)?;
            if !db_path.exists() {
                return Ok(Vec::new());
            }
            let store = IndexStore::open_readonly(&db_path)?;
            let ranked = query::rank_function_signature_candidates(
                store.symbols_by_name(&call.name)?,
                &current_rel,
                reach_scope.as_deref(),
                limit,
            );
            Ok(ranked
                .iter()
                .map(|candidate| signature_information_for(candidate, call.active_argument))
                .collect())
        })
        .await;

        match self.unwrap_query("signature help", result).await {
            Some(signatures) if !signatures.is_empty() => Ok(Some(SignatureHelp {
                signatures,
                active_signature: Some(0),
                active_parameter: Some(call.active_argument),
            })),
            _ => Ok(None),
        }
    }
}

pub(super) fn signature_information_for(
    ranked: &query::RankedSignatureCandidate,
    active_argument: u32,
) -> SignatureInformation {
    let parts = query::signature_parts(&ranked.signature);
    let parameters: Vec<ParameterInformation> = parts
        .parameters
        .iter()
        .map(|span| ParameterInformation {
            label: ParameterLabel::LabelOffsets([span.start, span.end]),
            documentation: None,
        })
        .collect();
    let active_parameter = if parameters.is_empty() || active_argument as usize >= parameters.len() {
        None
    } else {
        Some(active_argument)
    };
    SignatureInformation {
        label: parts.label,
        documentation: Some(Documentation::String(format!(
            "tier: {}\nconfidence: {}\nreason: {}",
            ranked.candidate.tier.as_str(),
            ranked.candidate.confidence.as_str(),
            ranked.candidate.reason.as_str()
        ))),
        parameters: (!parameters.is_empty()).then_some(parameters),
        active_parameter,
    }
}
```

If `SignatureHelp.active_parameter` highlights a nonexistent parameter for out-of-range calls in VS Code, revise the response assembly so `active_parameter` is `None` when all returned signatures omit `active_parameter`:

```rust
let active_parameter = signatures
    .iter()
    .any(|signature| signature.active_parameter.is_some())
    .then_some(call.active_argument);
```

- [ ] **步骤 4：运行测试并确认通过**

运行：

```bash
cargo test -p fossilsense server::signature_help -- --nocapture
cargo test -p fossilsense query::signatures -- --nocapture
```

预期：server signature helper tests 和 query signature tests 全部通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/server/signature_help.rs
git commit -m "feat: return ranked signature help"
```

### Task 7：文档和扩展描述同步

**覆盖需求：** FR10, NFR1, NFR2, NFR6, NFR8

**文件：**
- 修改：`README.md:11-128`
- 修改：`extensions/vscode/README.md:10-115`
- 修改：`extensions/vscode/package.json:2-8`
- 测试：文档文本检查、Rust tests、VS Code compile

**接口：**
- 消费：Task 1-6 已实现行为。
- 产出：用户可见 can / cannot / fallback 文档。

- [ ] **步骤 1：写失败检查**

运行以下搜索，当前应命中旧声明：

```bash
rg -n "no signature help|no signature help, parameter hints|There is still \\*\\*no\\*\\* signature help" README.md extensions/vscode/README.md
```

预期：命中旧文档，说明文档尚未同步。

- [ ] **步骤 2：修改文档**

在 `README.md` baseline capability 中加入 signature help：

```markdown
**best-effort signature help / parameter hints** for indexed functions,
```

在 `README.md` Completion 章节替换旧限制句为：

```markdown
- **Signature help / parameter hints are best-effort.** When the cursor is inside
  a simple function call, FossilSense finds exact-name indexed function
  declaration/definition candidates, ranks them with the same include
  reachability tiers used by Go to Definition, and shows stored signatures with
  the active argument index when the parameter list can be split conservatively.
  It does not perform overload resolution, argument type matching, template or
  namespace lookup, function-like macro expansion, or function-pointer target
  inference. When a signature is too complex to split safely, the whole stored
  signature is shown without fabricated parameter labels.
```

In `extensions/vscode/README.md` Current Capability, update lightweight completion paragraph to remove the old "no signature help" claim and add a separate bullet:

```markdown
- Best-effort Signature Help: inside simple function calls, shows indexed
  function signatures ranked by the same include reachability tiers as Go to
  Definition. Candidates are hints, not overload resolution; unsupported call
  shapes or unsplittable signatures degrade to empty or whole-signature results.
```

In `extensions/vscode/package.json`, update description:

```json
"description": "FossilSense C/C++ best-effort navigation: symbols, outlines, ranked definitions, signature help, role-grouped references, and lightweight completion."
```

- [ ] **步骤 3：运行文档检查并确认通过**

运行：

```bash
rg -n "no signature help|There is still \\*\\*no\\*\\* signature help" README.md extensions/vscode/README.md
```

预期：无命中。

- [ ] **步骤 4：运行代码验证**

运行：

```bash
cargo test -p fossilsense
cd extensions/vscode
pnpm run compile
```

预期：Rust tests 全部通过；VS Code extension TypeScript compile 通过。

- [ ] **步骤 5：提交**

```bash
git add README.md extensions/vscode/README.md extensions/vscode/package.json
git commit -m "docs: describe best-effort signature help"
```

### Task 8：集成验证与发布准备检查

**覆盖需求：** UR1, UR2, UR3, UR4, UR5, FR1-FR10, NFR1-NFR8

**文件：**
- 修改：无固定代码文件；只在发现失败时回到对应任务文件修正。
- 测试：workspace-level verification。

**接口：**
- 消费：Task 1-7 的全部产出。
- 产出：可交付验证证据；如准备演示或发布，产出 self-contained VSIX。

- [ ] **步骤 1：运行完整 Rust 验证**

运行：

```bash
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/signature-help-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/signature-help-mini.sqlite
```

预期：tests 全部通过；force index 和 incremental index 都成功退出。

- [ ] **步骤 2：运行扩展验证**

运行：

```bash
cd extensions/vscode
pnpm run compile
```

预期：TypeScript compile 成功。

- [ ] **步骤 3：需要可安装演示包时运行 VSIX packaging**

如果本次交付是对外演示或发布，运行：

```bash
cd extensions/vscode
pnpm run package
```

预期：`dist/fossilsense-vscode-<version>_BUILD<YYYYMMDD_HHMMSS>.vsix` 存在，且 package script 已复制 `target/release/fossilsense.exe` 到 extension `bin/`。

- [ ] **步骤 4：人工 smoke 场景**

在 VS Code Extension Development Host 打开 `samples/mini-c`，完成：

```text
1. Run FossilSense: Start Server.
2. Wait for index status ready.
3. In a C file, type a known indexed function call such as `hello_value(`.
4. Confirm signature help appears.
5. Type a comma in a multi-argument test sample if one exists or add a temporary local call to an indexed multi-argument function.
6. Confirm active parameter advances.
7. Run Go to Definition on the same function and confirm candidate order matches reachable/current expectations.
```

预期：signature help 可用；unsupported shape 返回空或 reduced result；go-to-definition 顺序不反转。

- [ ] **步骤 5：提交最终验证修正**

如果步骤 1-4 发现修正，按影响文件提交：

```bash
git add crates/fossilsense/src README.md extensions/vscode/README.md extensions/vscode/package.json
git commit -m "fix: stabilize signature help integration"
```

如果没有额外修正，不创建空提交。

## 需求覆盖检查

- UR1 覆盖：Task 1, Task 2, Task 5, Task 6, Task 8。
- UR2 覆盖：Task 3, Task 4, Task 6, Task 8。
- UR3 覆盖：Task 6, Task 7, Task 8。
- UR4 覆盖：Task 1-8。
- UR5 覆盖：Task 3, Task 4, Task 6, Task 7。
- SC1 覆盖：Task 1, Task 2, Task 6, Task 8。
- SC2 覆盖：Task 1, Task 2, Task 6, Task 8。
- SC3 覆盖：Task 3, Task 4, Task 6, Task 8。
- SC4 覆盖：Task 3, Task 4, Task 6, Task 8。
- SC5 覆盖：Task 2, Task 6, Task 7。
- SC6 覆盖：Task 6, Task 8。

## 最终验证命令

```bash
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/signature-help-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/signature-help-mini.sqlite
cd extensions/vscode
pnpm run compile
```

发布或演示交付时追加：

```bash
cd extensions/vscode
pnpm run package
```
