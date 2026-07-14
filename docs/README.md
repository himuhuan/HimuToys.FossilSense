# docs

FossilSense 的文档分四层。读错层会把计划或过程稿当成当前规范。

## 权威层

当前事实只认这些：

- 仓库根目录 `CLAUDE.md`：产品合同、模块地图、补全/include/parser/store 规则
- `README.md`、`extensions/vscode/README.md`：用户可见能力与安装说明
- 代码与测试：行为以实现为准；文档与实现冲突时改文档，不改口说“文档更对”

## 活笔记

`docs/architecture/` 只放**仍然指导当前理解**的短笔记，不放施工过程。

目前：

- `architecture/engine-read-model.md`：请求侧读模型、`EngineSnapshot`、发布边界
- `architecture/project-context-read-model.md`：构建标记项目模型、补全请求选择与失效边界
- `benchmark/1.4.0-final-gates.md`：大工作区性能验收结论与复现入口
- `benchmark/large-workspace-runbook.md`：U-Boot / Wine 基准脚本使用说明

## 活跃计划

`docs/plan/` 保存已经立项、尚未完成的跨模块版本计划。它负责记录目标语义、全局模型、破坏性迁移、实施阶段和完成门禁，但**不是当前能力说明**。实现与计划不一致时，应先更新计划中的决策；能力真正落地后，再把当前事实写回权威层。

当前没有活跃计划。v1.4.2 计划已实现并归档为 `archive/plan/2026-07-13-v1.4.2-semantic-experience-strengthening.md`；v1.4.3 是保持 schema 16 与语义范围不变的 full-build `finalizing` 性能 hotfix。

计划完成后必须标记 `implemented` 并移入 `docs/archive/plan/`。`docs/plan/` 不保存已经结束的施工记录，也不和 `CLAUDE.md` 平行描述“当前已经支持什么”。

## 归档层

`docs/archive/` 是历史过程文档，默认 **superseded / archived**：

| 目录 | 内容 |
|---|---|
| `archive/research/` | 补全/架构探索与外部评估原稿 |
| `archive/architecture/` | 已落地的重构评估长文 |
| `archive/delivery/` | 旧版 `DELIVERY-NOTE` |

OpenSpec 已完成 change 在 `openspec/changes/archive/`。`openspec/changes/` 下不应长期堆积已完成施工单。

## 使用约定

- 新能力进入实现后，应在行为落地的同一变更中同步 `CLAUDE.md` 的 can / cannot / fallback；不能只改权威文档就把计划能力写成已经实现。
- 跨模块版本可以先在 `docs/plan/` 立项；计划只描述目标和施工合同，不能提前修改权威层使其看起来已经实现。
- 探索可以写长，但落地后要么抽成短笔记，要么进 archive，不要留在活目录里“以后再整理”。
- 归档文档可以查决策痕迹，不能当 backlog。里面的未做项默认作废，除非重新 propose。
