# 引擎读模型与请求边界

Status: current (2026-07-10)

对于 FossilSense 的 LSP 请求路径而言，真正要保护的不是“有没有抽象”，而是**一次请求看到的索引读模型是否自洽**。索引在后台持续增量，请求却不能一边读旧 `NameTable`、一边读已经被原地改掉的 `ReachGraph`。

## 问题

早期缓存是分片发布的：名字表、可达图、include 表、引用文件列表可以各自更新。对单个字段来说这很自然，但对一次 goto / completion / coloring 来说会出问题——请求中途如果发生 dirty include 刷新，旧快照里的图可能被原地改写，于是同一请求混到两代索引状态。

因此读路径需要一个明确边界：请求开始时固定一套读模型，请求期间不再逐项追新。

## `EngineSnapshot`

每个工作区发布一份不可变的 **`EngineSnapshot`**。它统一携带：

- name table
- reach graph
- include table
- reference file list
- degraded state

后台构建时，这些部件在旁路组装；只有全部就绪后，才通过一次 map 交换发布。构建期间旧快照继续服务请求。发布失败时，不能露出半更新状态。

每次成功发布分配显式单调 **`EngineEpoch`**。`0` 只表示还没有发布过索引读模型，不是“第 0 代可用快照”。

## dirty reach graph

include 边增量更新时，必须生成**新的** `ReachGraph`，再进入下一份 snapshot。不能对旧快照已经持有的图做原地修改。否则“不可变快照”只是名义上的。

snapshot publisher 串行协调：同一时刻只允许一条发布路径把完整快照推出去。

## `RequestContext`

请求开始时捕获一个 `Arc<EngineSnapshot>`，以及当次请求需要的 settings 等输入。之后 handler 从这份 context 读，而不是重新去摸全局 staging 缓存。

这意味着：

- 请求内看到的索引世代是稳定的
- 文档 live parse / local-word 等请求期产物可以另有 revision 规则，但不应破坏“索引读模型一次性捕获”这个边界
- perf / verbose 日志可以用 epoch 标识这次请求站在哪一代引擎上

## 和 parser / store 的关系

`parse()` 仍然是唯一解析入口。调用方通过 `persistent_facts()` / `request_facts()` / `fact_availability(...)` 声明自己要哪类事实，而不是把整个 `FileSemanticIndex` 当杂物袋掏。

跨模块的 durable 读取走 `store::views` 的窄视图和 typed row。`rusqlite` 与 SQL-to-domain 转换留在 store 边界内。旧的宽 `IndexStore` query wrapper 可以给测试当 oracle，不应再当生产读路径。

## 刻意不做的

这些不是当前读模型合同的一部分：

- 多层 semantic fingerprint / revision vector：等 invalidation fan-out 有测量再说
- 统一 priority executor / 传播式 cancellation：先保住行为，再按延迟基线加
- dense `FileId`、bitmap reachability、SCC 压缩：没有大仓库图 profiling 前不换
- crate 拆分或 Salsa：模块边界还在收敛时，先把手工 invalidation 做对

> **和 `CLAUDE.md` 的关系**
>
> 本文是读模型边界的理解笔记。字段级规则、补全/include/着色合同以 `CLAUDE.md` 为准。若两者冲突，改这篇笔记，不要平行发明第二套术语。
