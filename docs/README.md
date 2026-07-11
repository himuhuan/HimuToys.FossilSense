# docs

FossilSense 的文档分三层。读错层会把过程稿当成规范。

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

## 归档层

`docs/archive/` 是历史过程文档，默认 **superseded / archived**：

| 目录 | 内容 |
|---|---|
| `archive/research/` | 补全/架构探索与外部评估原稿 |
| `archive/architecture/` | 已落地的重构评估长文 |
| `archive/delivery/` | 旧版 `DELIVERY-NOTE` |

OpenSpec 已完成 change 在 `openspec/changes/archive/`。`openspec/changes/` 下不应长期堆积已完成施工单。

## 使用约定

- 新能力先改 `CLAUDE.md` 的 can / cannot / fallback，再写代码。
- 探索可以写长，但落地后要么抽成短笔记，要么进 archive，不要留在活目录里“以后再整理”。
- 归档文档可以查决策痕迹，不能当 backlog。里面的未做项默认作废，除非重新 propose。
