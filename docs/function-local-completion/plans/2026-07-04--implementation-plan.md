# Function Local Completion 实现计划

Status: planned

> **给代理执行者：** 必须使用 `himupowers:subagent-driven-development`（推荐）或 `himupowers:executing-plans` 逐任务执行本计划。
> 需求文档：`docs/function-local-completion/requirements.md`

**目标：** 在普通标识符补全中有限纳入光标所在函数的参数和光标前局部变量，同时保持 FossilSense 的 best-effort、热路径和降级约束。
**架构：** Rust 侧扩展 request-time parser facts，新增当前函数局部绑定模型；`query::local_completion` 负责纯过滤和排序；普通 completion LSP 路径复用 open-document live parse cache，把结构化 `LocalBinding` 候选注入现有候选池。include completion 和 member completion 继续优先短路。
**技术栈：** Rust 2021, tower-lsp, tree-sitter C/C++ 现有解析入口, existing `query::NameTable`, existing `resolver::pack_score`, `cargo test`, VS Code extension docs/pnpm compile.

## 全局约束

- 不依赖 clangd、ctags、compile commands、外部构建系统或编译参数。
- 本功能只增强普通标识符补全；不改 hover、signature help、references、go-to-definition、semantic coloring、include completion 或 member completion 行为。
- 返回的是 best-effort completion candidate，不是编译级语义绑定；文档和 UI detail 不得暗示精确 C/C++ scope resolution。
- 不做 SQLite schema migration；参数和局部变量只来自当前 open document 的 request-time parse。
- Completion 请求不得扫描 workspace 或执行 broad disk IO；局部绑定工作必须限制在当前文档和当前函数范围。
- `CompletionList.isIncomplete` 对成功、空结果和截断结果都保持 `true`。
- 短前缀继续只接受 exact、prefix、词边界子串；不得把普通子串或子序列长尾通过局部候选重新放大。
- Include-path completion 和 `.` / `->` member completion 必须继续在普通 identifier completion 之前返回。
- 解析失败、缺 root、缺 live parse、无法定位函数体、无法提取 declarator identifier 时降级到现有 indexed + raw local word completion。
- 新增类型和 helper 必须靠近现有 parser/query/server 边界，不引入平行 smart/semantic 排序概念。
- 每个任务完成后运行对应测试；最终验证至少运行 `cargo test -p fossilsense` 和 extension compile。

## 文件结构

- 修改 `crates/fossilsense/src/parser.rs`
  - 职责：公开 `LocalBinding` / `LocalBindingKind` request-time 模型，并在 `FileSemanticIndex` 中保存结构化局部补全绑定。
  - 输入：`parse(path, source)` 的当前文件文本。
  - 输出：`FileSemanticIndex.local_bindings: Vec<LocalBinding>`。
  - 错误策略：parse fallback 时 `local_bindings` 为空；不返回 `Err`。
- 修改 `crates/fossilsense/src/parser/ast.rs`
  - 职责：从 `function_definition` 中保守提取参数和函数体内声明，记录函数 byte range、声明 byte offset、binding kind 和 optional type text。
  - 输入：tree-sitter `function_definition` / `parameter_declaration` / `declaration` nodes。
  - 输出：`AstIndex.local_bindings` 和既有 `local_declarations` 保持兼容。
- 修改 `crates/fossilsense/src/parser/tests.rs`
  - 职责：验证参数、局部变量、光标后声明、函数外不启用、parse fallback 为空。
- 新建 `crates/fossilsense/src/query/local_completion.rs`
  - 职责：协议无关的当前函数局部补全过滤和排序。
  - 输入：`&[parser::LocalBinding]`、document text、LSP line/UTF-16 column、prefix、limit。
  - 输出：`Vec<LocalCompletionCandidate>`，包含 name、kind、detail、score、decl_start_byte。
  - 错误策略：无 enclosing function 或 prefix 不匹配时返回空 vec。
- 修改 `crates/fossilsense/src/query.rs`
  - 职责：声明并导出 `local_completion` helper。
- 修改 `crates/fossilsense/src/server.rs`
  - 职责：新增 `CompletionCandidateSource::LocalBinding`，调整 same-name dedup 优先级，并提供把 query local candidate 转为 LSP `CompletionItem` 的 helper。
- 修改 `crates/fossilsense/src/server/language_server.rs`
  - 职责：普通 completion 路径在 include/member 之后复用 `get_or_parse_document`，把 local binding candidates 注入候选池。
- 修改 `crates/fossilsense/src/server/tests.rs`
  - 职责：验证 `LocalBinding` 与 Indexed/LocalWord 同名 dedup、kind/detail、不会破坏 existing local word 规则。
- 修改 `README.md`
  - 职责：同步当前函数局部补全 can/cannot/fallback。
- 修改 `extensions/vscode/README.md`
  - 职责：同步扩展当前能力描述，不新增 setting。

### Task 1：Parser local binding model and extraction

**覆盖需求：** FR2, FR3, FR4, FR5, FR10, FR11, NFR2, NFR4, NFR7

**文件：**
- 修改：`crates/fossilsense/src/parser.rs`
- 修改：`crates/fossilsense/src/parser/ast.rs`
- 测试：`crates/fossilsense/src/parser/tests.rs`

**接口：**
- 消费：现有 `parse(path, source) -> FileSemanticIndex`
- 消费：现有 `declarator_identifier(node, source) -> Option<(Node, &str)>`
- 产出：`LocalBindingKind::{Parameter, LocalVariable}`
- 产出：`LocalBinding { name, kind, type_text, decl_start_byte, function_start_byte, function_end_byte }`
- 产出：`FileSemanticIndex.local_bindings: Vec<LocalBinding>`

- [ ] **步骤 1：写失败测试**

在 `crates/fossilsense/src/parser/tests.rs` 追加：

```rust
#[test]
fn local_bindings_collect_parameters_and_locals_in_function() {
    let src = "int f(int count, struct Foo *foo) {\n    int cursor_limit = count;\n    char *name;\n    return cursor_limit;\n}\n";
    let index = parse(Path::new("a.c"), src);
    let names: Vec<(&str, super::LocalBindingKind)> = index
        .local_bindings
        .iter()
        .map(|binding| (binding.name.as_str(), binding.kind))
        .collect();
    assert!(names.contains(&("count", super::LocalBindingKind::Parameter)));
    assert!(names.contains(&("foo", super::LocalBindingKind::Parameter)));
    assert!(names.contains(&("cursor_limit", super::LocalBindingKind::LocalVariable)));
    assert!(names.contains(&("name", super::LocalBindingKind::LocalVariable)));
    assert!(index
        .local_bindings
        .iter()
        .all(|binding| binding.function_start_byte < binding.function_end_byte));
}

#[test]
fn local_bindings_ignore_file_scope_declarations() {
    let src = "int global_value;\nvoid f(void) {\n    int local_value;\n}\n";
    let index = parse(Path::new("a.c"), src);
    assert!(index.local_bindings.iter().any(|b| b.name == "local_value"));
    assert!(index.local_bindings.iter().all(|b| b.name != "global_value"));
}

#[test]
fn local_bindings_are_empty_on_lexical_fallback() {
    let src = "#define Z 1\n";
    let index = parse(Path::new("a.unknown"), src);
    assert!(index.local_bindings.is_empty());
}
```

- [ ] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense parser::tests::local_bindings -- --nocapture`

预期：编译失败，失败原因是 `LocalBindingKind` 或 `FileSemanticIndex.local_bindings` 尚未实现。

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/parser.rs` 增加：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBindingKind {
    Parameter,
    LocalVariable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBinding {
    pub name: String,
    pub kind: LocalBindingKind,
    pub type_text: Option<String>,
    pub decl_start_byte: usize,
    pub function_start_byte: usize,
    pub function_end_byte: usize,
}
```

把 `FileSemanticIndex` 增加字段：

```rust
pub local_bindings: Vec<LocalBinding>,
```

在 normal parse product 和 `lexical_fallback` 中分别填充 `ast.local_bindings` / `Vec::new()`。

在 `crates/fossilsense/src/parser/ast.rs`：

```rust
use super::{
    AliasTarget, FieldDef, LocalBinding, LocalBindingKind, LocalDeclaration, Occurrence,
    ParseFacts, RecordConfidence, RecordDef, RecordKind, Symbol, SymbolKind, SymbolRole,
    SyntacticRole, TypeAlias,
};

pub(super) struct AstIndex {
    pub(super) parse_error_count: usize,
    pub(super) occurrences: Vec<Occurrence>,
    pub(super) fields: Vec<FieldDef>,
    pub(super) enum_constants: Vec<Symbol>,
    pub(super) aliases: Vec<TypeAlias>,
    pub(super) records: Vec<RecordDef>,
    pub(super) local_declarations: Vec<LocalDeclaration>,
    pub(super) local_bindings: Vec<LocalBinding>,
}
```

初始化 `local_bindings: Vec::new()`。在 AST DFS 中遇到 `function_definition` 时调用：

```rust
if facts.contains(ParseFacts::LOCAL_DECLS) && node.kind() == "function_definition" {
    collect_function_local_bindings(node, source, &mut out.local_bindings);
}
```

实现 helper：

```rust
fn collect_function_local_bindings(
    function: tree_sitter::Node<'_>,
    source: &str,
    out: &mut Vec<LocalBinding>,
) {
    let function_start_byte = function.start_byte();
    let function_end_byte = function.end_byte();

    if let Some(declarator) = function.child_by_field_name("declarator") {
        collect_parameter_bindings(
            declarator,
            source,
            function_start_byte,
            function_end_byte,
            out,
        );
    }

    if let Some(body) = function.child_by_field_name("body") {
        collect_local_variable_bindings(
            body,
            source,
            function_start_byte,
            function_end_byte,
            out,
        );
    }
}
```

Implement `collect_parameter_bindings`, `collect_local_variable_bindings`, and `type_text` with conservative tree traversal:

```rust
fn type_text(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("type")
        .and_then(|type_node| type_node.utf8_text(source.as_bytes()).ok())
        .map(compact_whitespace)
        .filter(|text| !text.is_empty())
}
```

For parameters, traverse descendant `parameter_declaration` nodes beneath the function declarator and push `LocalBindingKind::Parameter` when `declarator_identifier` returns a name. For locals, traverse descendant `declaration` nodes beneath the body and push `LocalBindingKind::LocalVariable` for each field-name `declarator`.

Keep the existing `local_declarations` collection unchanged so member completion receiver inference still sees record-typed parameters, locals, and file-scope variables.

- [ ] **步骤 4：运行测试并确认通过**

运行：

```bash
cargo test -p fossilsense parser::tests::local_bindings -- --nocapture
cargo test -p fossilsense parser::tests::infers_receiver_record_for_local_param_and_file_scope -- --nocapture
```

预期：新增 local binding tests 通过；既有 member receiver inference 测试仍通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/parser.rs crates/fossilsense/src/parser/ast.rs crates/fossilsense/src/parser/tests.rs
git commit -m "feat: collect current function local bindings"
```

### Task 2：Query helper for function-local completion filtering

**覆盖需求：** FR2, FR3, FR4, FR5, FR8, FR11, NFR3, NFR4, NFR8

**文件：**
- 新建：`crates/fossilsense/src/query/local_completion.rs`
- 修改：`crates/fossilsense/src/query.rs`
- 测试：`crates/fossilsense/src/query/local_completion.rs`

**接口：**
- 消费：Task 1 的 `parser::LocalBinding`
- 消费：`query::byte_offset_at(text, line, character) -> usize`
- 消费：`query::completion_word_score(prefix, name, 0) -> Option<i32>`
- 消费：`resolver::pack_score(ScopeTier::Current, base_match, 0) -> i32`
- 产出：`LocalCompletionCandidate { name, kind, detail, score, decl_start_byte }`
- 产出：`local_completion_candidates(bindings, text, line, character, prefix, limit) -> Vec<LocalCompletionCandidate>`

- [ ] **步骤 1：写失败测试**

新建 `crates/fossilsense/src/query/local_completion.rs`，先加入 tests 和期望 API：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{LocalBinding, LocalBindingKind};

    fn binding(
        name: &str,
        kind: LocalBindingKind,
        decl_start_byte: usize,
        function_start_byte: usize,
        function_end_byte: usize,
    ) -> LocalBinding {
        LocalBinding {
            name: name.to_string(),
            kind,
            type_text: Some("int".to_string()),
            decl_start_byte,
            function_start_byte,
            function_end_byte,
        }
    }

    #[test]
    fn local_completion_keeps_parameters_and_prior_locals() {
        let text = "int f(int count) {\n    int cursor_limit;\n    cur\n}\n";
        let cursor = text.find("cur\n").expect("cursor");
        let bindings = vec![
            binding("count", LocalBindingKind::Parameter, text.find("count").unwrap(), 0, text.len()),
            binding("cursor_limit", LocalBindingKind::LocalVariable, text.find("cursor_limit").unwrap(), 0, text.len()),
        ];
        let hits = local_completion_candidates(&bindings, text, 2, 7, "cur", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "cursor_limit");
        assert!(hits[0].score > 0);
        assert!(bindings[1].decl_start_byte < cursor);
    }

    #[test]
    fn local_completion_excludes_later_declarations() {
        let text = "int f(void) {\n    fut\n    int future_value;\n}\n";
        let bindings = vec![binding(
            "future_value",
            LocalBindingKind::LocalVariable,
            text.find("future_value").unwrap(),
            0,
            text.len(),
        )];
        let hits = local_completion_candidates(&bindings, text, 1, 7, "fut", 10);
        assert!(hits.is_empty());
    }

    #[test]
    fn local_completion_requires_cursor_inside_function_range() {
        let text = "int f(void) { int local_value; }\nloc\n";
        let bindings = vec![binding("local_value", LocalBindingKind::LocalVariable, 18, 0, 31)];
        let hits = local_completion_candidates(&bindings, text, 1, 3, "loc", 10);
        assert!(hits.is_empty());
    }

    #[test]
    fn local_completion_preserves_short_prefix_noise_gate() {
        let text = "void f(void) {\n    int Foobar;\n    int FooBar;\n    ba\n}\n";
        let bindings = vec![
            binding("Foobar", LocalBindingKind::LocalVariable, text.find("Foobar").unwrap(), 0, text.len()),
            binding("FooBar", LocalBindingKind::LocalVariable, text.find("FooBar").unwrap(), 0, text.len()),
        ];
        let hits = local_completion_candidates(&bindings, text, 3, 6, "ba", 10);
        assert_eq!(hits.iter().map(|hit| hit.name.as_str()).collect::<Vec<_>>(), vec!["FooBar"]);
    }
}
```

在 `crates/fossilsense/src/query.rs` 预先声明：

```rust
mod local_completion;
pub use local_completion::{local_completion_candidates, LocalCompletionCandidate};
```

- [ ] **步骤 2：运行测试并确认失败**

运行：`cargo test -p fossilsense query::local_completion -- --nocapture`

预期：编译失败，失败原因是 `LocalCompletionCandidate` / `local_completion_candidates` 尚未实现。

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/query/local_completion.rs` 实现：

```rust
use crate::model::ScopeTier;
use crate::parser::{LocalBinding, LocalBindingKind};
use crate::resolver;

use super::{byte_offset_at, completion_word_score};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCompletionCandidate {
    pub name: String,
    pub kind: LocalBindingKind,
    pub detail: String,
    pub score: i32,
    pub decl_start_byte: usize,
}

pub fn local_completion_candidates(
    bindings: &[LocalBinding],
    text: &str,
    line: u32,
    character: u32,
    prefix: &str,
    limit: usize,
) -> Vec<LocalCompletionCandidate> {
    let byte_offset = byte_offset_at(text, line, character).min(text.len());
    let mut hits: Vec<LocalCompletionCandidate> = bindings
        .iter()
        .filter(|binding| {
            binding.function_start_byte < byte_offset
                && byte_offset <= binding.function_end_byte
                && binding.decl_start_byte < byte_offset
        })
        .filter_map(|binding| {
            let base_match = completion_word_score(prefix, &binding.name, 0)?;
            let score = resolver::pack_score(ScopeTier::Current, base_match, 0);
            Some(LocalCompletionCandidate {
                name: binding.name.clone(),
                kind: binding.kind,
                detail: local_binding_detail(binding),
                score,
                decl_start_byte: binding.decl_start_byte,
            })
        })
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.decl_start_byte.cmp(&a.decl_start_byte))
            .then_with(|| a.name.cmp(&b.name))
    });
    dedup_by_name_keep_first(&mut hits);
    hits.truncate(limit);
    hits
}

fn local_binding_detail(binding: &LocalBinding) -> String {
    let role = match binding.kind {
        LocalBindingKind::Parameter => "parameter",
        LocalBindingKind::LocalVariable => "local",
    };
    match binding.type_text.as_deref() {
        Some(type_text) if !type_text.is_empty() => format!("{role}: {type_text}"),
        _ => role.to_string(),
    }
}

fn dedup_by_name_keep_first(hits: &mut Vec<LocalCompletionCandidate>) {
    let mut seen = std::collections::HashSet::new();
    hits.retain(|hit| seen.insert(hit.name.clone()));
}
```

- [ ] **步骤 4：运行测试并确认通过**

运行：

```bash
cargo test -p fossilsense query::local_completion -- --nocapture
cargo test -p fossilsense query::text::tests::local_word_short_prefix_rejects_plain_substring -- --nocapture
```

预期：local completion helper tests 通过；现有 raw local word 短前缀测试仍通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/query.rs crates/fossilsense/src/query/local_completion.rs
git commit -m "feat: rank current function local completion candidates"
```

### Task 3：Completion candidate source and dedup priority

**覆盖需求：** FR6, FR7, FR8, FR11, NFR5, NFR7, NFR8

**文件：**
- 修改：`crates/fossilsense/src/server.rs`
- 修改：`crates/fossilsense/src/server/tests.rs`
- 测试：`crates/fossilsense/src/server/tests.rs`

**接口：**
- 消费：Task 2 的 `query::LocalCompletionCandidate`
- 产出：`CompletionCandidateSource::LocalBinding`
- 产出：`completion_items_for_local_bindings(hits) -> Vec<CompletionCandidate>`
- 产出：same-name priority `LocalBinding > Indexed > LocalWord` for dedup

- [ ] **步骤 1：写失败测试**

在 `crates/fossilsense/src/server/tests.rs` 追加：

```rust
#[test]
fn completion_dedup_keeps_local_binding_over_same_name_indexed_and_local_word() {
    use crate::model::{ResolutionConfidence, ScopeTier};

    let indexed = super::CompletionCandidate {
        name: "count".to_string(),
        tier: ScopeTier::Reachable,
        confidence: ResolutionConfidence::Reachable,
        score: 30_000,
        item: CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        },
        source: super::CompletionCandidateSource::Indexed,
    };
    let local_binding = super::CompletionCandidate {
        name: "count".to_string(),
        tier: ScopeTier::Current,
        confidence: ResolutionConfidence::Heuristic,
        score: 40_000,
        item: CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::VARIABLE),
            detail: Some("parameter: int".to_string()),
            ..Default::default()
        },
        source: super::CompletionCandidateSource::LocalBinding,
    };
    let local_word = super::CompletionCandidate {
        name: "count".to_string(),
        tier: ScopeTier::Global,
        confidence: ResolutionConfidence::Fallback,
        score: 1_000,
        item: CompletionItem {
            label: "count".to_string(),
            kind: Some(CompletionItemKind::TEXT),
            ..Default::default()
        },
        source: super::CompletionCandidateSource::LocalWord,
    };

    let deduped = dedup_completion_candidates(vec![indexed, local_word, local_binding]);
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].source, super::CompletionCandidateSource::LocalBinding);
    assert_eq!(deduped[0].item.kind, Some(CompletionItemKind::VARIABLE));
}

#[test]
fn local_binding_candidates_render_variable_kind_and_detail() {
    let hits = vec![crate::query::LocalCompletionCandidate {
        name: "cursor_limit".to_string(),
        kind: crate::parser::LocalBindingKind::LocalVariable,
        detail: "local: int".to_string(),
        score: 42_000,
        decl_start_byte: 10,
    }];

    let candidates = completion_items_for_local_bindings(hits);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].name, "cursor_limit");
    assert_eq!(candidates[0].source, super::CompletionCandidateSource::LocalBinding);
    assert_eq!(candidates[0].item.kind, Some(CompletionItemKind::VARIABLE));
    assert_eq!(candidates[0].item.detail.as_deref(), Some("local: int"));
}
```

- [ ] **步骤 2：运行测试并确认失败**

运行：

```bash
cargo test -p fossilsense server::tests::completion_dedup_keeps_local_binding_over_same_name_indexed_and_local_word server::tests::local_binding_candidates_render_variable_kind_and_detail -- --nocapture
```

预期：编译失败，失败原因是 `LocalBinding` source 和 rendering helper 尚未实现。

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/server.rs`：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionCandidateSource {
    Indexed,
    LocalBinding,
    LocalWord,
}
```

调整 `completion_candidate_beats`：

```rust
fn completion_source_rank(source: CompletionCandidateSource) -> u8 {
    match source {
        CompletionCandidateSource::LocalBinding => 3,
        CompletionCandidateSource::Indexed => 2,
        CompletionCandidateSource::LocalWord => 1,
    }
}

fn completion_candidate_beats(
    source: CompletionCandidateSource,
    key: (model::ScopeTier, model::ResolutionConfidence),
    score: i32,
    prev_source: CompletionCandidateSource,
    prev_key: (model::ScopeTier, model::ResolutionConfidence),
    prev_score: i32,
) -> bool {
    let rank = completion_source_rank(source);
    let prev_rank = completion_source_rank(prev_source);
    rank > prev_rank || (rank == prev_rank && (key > prev_key || (key == prev_key && score > prev_score)))
}
```

Add helper near `exact_indexed_completion_candidates_for_local_word`:

```rust
fn completion_items_for_local_bindings(
    hits: Vec<query::LocalCompletionCandidate>,
) -> Vec<CompletionCandidate> {
    hits.into_iter()
        .map(|hit| CompletionCandidate {
            name: hit.name.clone(),
            tier: model::ScopeTier::Current,
            confidence: model::ResolutionConfidence::Heuristic,
            score: hit.score,
            item: CompletionItem {
                label: hit.name,
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some(hit.detail),
                sort_text: Some(format!("{:08}", 100_000_000 - hit.score)),
                ..Default::default()
            },
            source: CompletionCandidateSource::LocalBinding,
        })
        .collect()
}
```

- [ ] **步骤 4：运行测试并确认通过**

运行：

```bash
cargo test -p fossilsense server::tests::completion_dedup -- --nocapture
cargo test -p fossilsense server::tests::local_binding_candidates_render_variable_kind_and_detail -- --nocapture
```

预期：新增 dedup/render tests 通过；既有 `completion_dedup_keeps_indexed_kind_over_same_name_local_word` 仍通过或按新 priority 更新断言为 LocalBinding 不参与时 Indexed 仍胜过 LocalWord。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/server.rs crates/fossilsense/src/server/tests.rs
git commit -m "feat: prefer structured local completion candidates"
```

### Task 4：Integrate local bindings into ordinary completion

**覆盖需求：** FR1, FR2, FR3, FR4, FR7, FR8, FR9, FR10, FR11, NFR3, NFR4, NFR6, NFR8

**文件：**
- 修改：`crates/fossilsense/src/server/language_server.rs`
- 修改：`crates/fossilsense/src/server/tests.rs`
- 测试：Rust targeted completion tests

**接口：**
- 消费：Task 1 的 `Backend::get_or_parse_document(...) -> Option<Arc<FileSemanticIndex>>`
- 消费：Task 2 的 `query::local_completion_candidates(...)`
- 消费：Task 3 的 `completion_items_for_local_bindings(...)`
- 产出：ordinary completion path injects local binding candidates after include/member context gates and before raw local words

- [ ] **步骤 1：写失败测试**

在 `crates/fossilsense/src/server/tests.rs` 添加 pure pipeline helper test。如果 Task 3 helper 仍是 private，先把 test 放在同一 `server` module tests 中直接调用：

```rust
#[test]
fn local_binding_pipeline_uses_open_document_bindings_before_local_words() {
    let src = "int f(int count) {\n    int cursor_limit;\n    cur\n}\n";
    let parsed = crate::parser::parse(std::path::Path::new("a.c"), src);
    let hits = crate::query::local_completion_candidates(
        &parsed.local_bindings,
        src,
        2,
        7,
        "cur",
        crate::query::COMPLETION_LIMIT,
    );
    let candidates = super::completion_items_for_local_bindings(hits);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].name, "cursor_limit");
    assert_eq!(candidates[0].source, super::CompletionCandidateSource::LocalBinding);
}
```

This test verifies the parse-to-query-to-server candidate chain before changing the LSP handler.

- [ ] **步骤 2：运行测试并确认失败**

运行：

```bash
cargo test -p fossilsense server::tests::local_binding_pipeline_uses_open_document_bindings_before_local_words -- --nocapture
```

预期：失败原因是 local binding pipeline is not wired or helper visibility/imports are missing.

- [ ] **步骤 3：写最小实现**

在 `crates/fossilsense/src/server/language_server.rs` ordinary completion handler 中，保留现有顺序：

```rust
if let Some((form, partial)) = includes::include_completion_context(...) {
    return self.complete_include(...).await;
}

if query::is_member_completion_context(...) {
    return self.complete_members(...).await;
}
```

在计算 `prefix` 后，添加 request-time parse：

```rust
let parsed_document = match uri_to_path(&uri) {
    Some(path) => self.get_or_parse_document(&uri, &path, version, &text).await,
    None => None,
};
let local_binding_hits = parsed_document
    .as_ref()
    .map(|index| {
        query::local_completion_candidates(
            &index.local_bindings,
            &text,
            position.line,
            position.character,
            &prefix,
            query::COMPLETION_LIMIT,
        )
    })
    .unwrap_or_default();
```

Move `local_binding_hits` into the `spawn_blocking` closure and inject before raw local words:

```rust
candidates.extend(completion_items_for_local_bindings(local_binding_hits));
```

Keep raw `local_words_for` unchanged, including the `if word == &prefix { continue; }` guard and exact-indexed upgrade path.

The completion memo generation may remain tied to `NameTable` generations because local bindings are recomputed from the current document version before each request. Do not cache local binding candidates inside `CompletionMemo`.

- [ ] **步骤 4：运行测试并确认通过**

运行：

```bash
cargo test -p fossilsense server::tests::local_binding_pipeline_uses_open_document_bindings_before_local_words -- --nocapture
cargo test -p fossilsense server::tests::completion_memo -- --nocapture
cargo test -p fossilsense query::local_completion -- --nocapture
```

预期：local binding pipeline test 通过；existing completion memo tests 仍通过；query local completion tests 通过。

- [ ] **步骤 5：提交**

```bash
git add crates/fossilsense/src/server/language_server.rs crates/fossilsense/src/server/tests.rs
git commit -m "feat: include current function locals in completion"
```

### Task 5：Documentation sync for current-function local completion

**覆盖需求：** FR12, NFR1, NFR5, NFR6, NFR8

**文件：**
- 修改：`README.md`
- 修改：`extensions/vscode/README.md`

**接口：**
- 消费：Task 1-4 已实现行为。
- 产出：用户可见 can / cannot / fallback 文档。

- [ ] **步骤 1：写失败检查**

运行：

```bash
rg -n "current-function|function-local|参数|局部变量|local variable|parameter" README.md extensions/vscode/README.md
```

预期：当前文档没有描述 ordinary completion 会结构化纳入当前函数参数和局部变量，或者只描述 raw current-file word fallback。

- [ ] **步骤 2：修改文档**

在 `README.md` 的核心能力或补全说明中加入：

```markdown
* **⌨️ 降噪与持续补全：** 结合全局索引、当前函数参数/局部变量和当前文件词表，短前缀智能降噪，长前缀模糊匹配。
```

在 `README.md` Completion 能力说明附近加入限制：

```markdown
普通标识符补全会在光标位于函数体内时，有限纳入当前函数参数和声明早于光标的局部变量。这些候选来自当前 open document 的容错解析，可覆盖未保存编辑；它们是 best-effort 局部绑定提示，不是完整 C/C++ block-scope 或模板/宏语义解析。解析失败、无法确认函数边界或 declarator 不清晰时，会回退到已有索引候选和当前文件词表。
```

在 `extensions/vscode/README.md` Lightweight Completion bullet 中加入：

```markdown
When the cursor is inside a detected function body, ordinary identifier
completion also adds best-effort current-function parameters and local
variables declared before the cursor from the open document snapshot. These
structured local candidates are distinct from raw current-file word fallback;
unsupported parse shapes degrade to the existing indexed and word completion.
```

- [ ] **步骤 3：运行文档检查并确认通过**

运行：

```bash
rg -n "current-function|局部变量|local variables declared before the cursor" README.md extensions/vscode/README.md
```

预期：命中新文档描述；没有出现 compile-grade、exact scope、semantic binding 等过度承诺。

- [ ] **步骤 4：运行轻量编译检查**

运行：`cd extensions/vscode && pnpm run compile`

预期：文档改动不影响 extension compile。

- [ ] **步骤 5：提交**

```bash
git add README.md extensions/vscode/README.md
git commit -m "docs: describe function-local completion"
```

### Task 6：Integration verification and release readiness

**覆盖需求：** UR1, UR2, UR3, UR4, UR5, UR6, FR1-FR12, NFR1-NFR8

**文件：**
- 修改：只在验证发现问题时回到对应任务文件修正。
- 测试：workspace-level verification。

**接口：**
- 消费：Task 1-5 的全部产出。
- 产出：可交付验证证据；对外演示或发布时产出 self-contained VSIX。

- [ ] **步骤 1：运行完整 Rust 验证**

运行：

```bash
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/function-local-completion-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/function-local-completion-mini.sqlite
```

预期：Rust tests 全部通过；force index 和 incremental index 都成功退出。

- [ ] **步骤 2：运行扩展验证**

运行：

```bash
cd extensions/vscode
pnpm run compile
```

预期：TypeScript compile 成功。

- [ ] **步骤 3：人工 smoke 场景**

在 VS Code Extension Development Host 打开 `samples/mini-c` 或一个临时 C 文件，完成：

```text
1. Run FossilSense: Start Server.
2. Wait for index status ready.
3. Create or edit a function:
   void local_demo(int count) {
       int cursor_limit;
       cur
   }
4. Request completion after `cur`.
5. Confirm `cursor_limit` appears as a local variable candidate.
6. Request completion after `cou`.
7. Confirm `count` appears as a parameter candidate.
8. Move cursor before a later declaration and confirm that later local is not shown as structured local binding.
9. Type `obj.` or `#include "` and confirm member/include completion still takes precedence.
```

预期：当前函数参数和光标前局部变量出现；后声明局部不作为 structured local candidate 出现；include/member completion 未被普通 local completion 拦截。

- [ ] **步骤 4：发布或演示需要 VSIX 时运行 packaging**

如果本次交付是对外演示或发布，运行：

```bash
cd extensions/vscode
pnpm run package
```

预期：`dist/fossilsense-vscode-<version>_BUILD<YYYYMMDD_HHMMSS>.vsix` 存在，且 package script 已复制 `target/release/fossilsense.exe` 到 extension `bin/`。

- [ ] **步骤 5：提交验证修正**

如果步骤 1-4 发现修正，按影响文件提交：

```bash
git add crates/fossilsense/src README.md extensions/vscode/README.md
git commit -m "fix: stabilize function-local completion"
```

如果没有额外修正，不创建空提交。

## 需求覆盖检查

- UR1 覆盖：Task 1, Task 2, Task 4, Task 6。
- UR2 覆盖：Task 1, Task 2, Task 4, Task 6。
- UR3 覆盖：Task 2, Task 4, Task 5。
- UR4 覆盖：Task 2, Task 3, Task 4, Task 5。
- UR5 覆盖：Task 1, Task 4, Task 6。
- UR6 覆盖：Task 3, Task 4, Task 6。
- SC1 覆盖：Task 1, Task 2, Task 4, Task 6。
- SC2 覆盖：Task 1, Task 2, Task 4, Task 6。
- SC3 覆盖：Task 2, Task 4, Task 6。
- SC4 覆盖：Task 1, Task 2, Task 4, Task 6。
- SC5 覆盖：Task 3, Task 4。
- SC6 覆盖：Task 4, Task 6。
- SC7 覆盖：Task 1, Task 4, Task 6。
- SC8 覆盖：Task 1, Task 2, Task 4, Task 6。

## 最终验证命令

```bash
cargo test -p fossilsense
cargo run -p fossilsense -- index samples/mini-c --db target/function-local-completion-mini.sqlite --force
cargo run -p fossilsense -- index samples/mini-c --db target/function-local-completion-mini.sqlite
cd extensions/vscode
pnpm run compile
```

发布或演示交付时追加：

```bash
cd extensions/vscode
pnpm run package
```
