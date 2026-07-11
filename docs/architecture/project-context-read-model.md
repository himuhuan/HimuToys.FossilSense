# 项目上下文读模型

Status: current (2026-07-11)

项目上下文把文件系统里的构建标记转换为普通标识符补全的弱证据。它不解析构建文件、不证明 C/C++ 可见性，也不改变 `ScopeTier`；definition、references、coloring、workspace symbol、Hover、Signature Help、member 和 include completion 都不消费它。

## 数据流

```text
workspace marker discovery
          │
          ▼
 ProjectContextIndex ── nearest ancestor ── request URI / Auto
          │
          ├── tag workspace NameEntry with optional ProjectKey
          └── build ProjectKey -> entry-index recall map
                         │
                         ▼
       one immutable EngineSnapshot publication
                         │
     Auto / Manual / Unspecified + selection epoch
                         │
                         ▼
 ordinary completion same-project quota + bounded evidence
```

`ProjectKey` 由 workspace root hash 和规范化相对项目路径组成，避免 multi-root 下相同相对路径碰撞。Windows 路径归属按路径段、大小写不敏感地比较；保存的手动 key 会规范化成当前发现模型中的真实拼写。

## 发布与失效

完整或 dirty 索引发布时，`ProjectContextIndex` 与带项目标签的 `NameTable` 属于同一 `EngineSnapshot`，请求只能看到完整旧代或完整新代。marker 创建、删除或重命名只重新发现项目，并在上一份已发布的内存 `NameTable` 上重标 ownership；该路径不重读可能正在写入的 SQLite，也不重解析未变化的 C/C++ 文件。

completion memo generation 同时包含所有 workspace engine epoch、selection epoch 和 effective project。marker 发布或 Auto/Manual/Unspecified 选择改变后，旧候选池不得复用。项目发现失败时 snapshot 把 `projectContext` 标为 degraded，NameTable 不带项目 key，补全继续走原有基线。

## 热路径与边界

补全请求只读取已捕获 snapshot 和 selection：Auto 从请求 URI 查最近祖先项目，Manual 使用已验证 key，Unspecified 返回空。same-project 只增加有界召回代表和 ranking evidence，不过滤其他项目。无 effective project 时不扩充召回预算、不增加 tie-break 或 annotation，完整补全输出保持基线。

文件系统遍历只允许出现在读模型构建/marker refresh 路径。architecture fitness 会阻止 ordinary completion service 引入 `std::fs`、`ignore` 或 marker discovery 调用。
