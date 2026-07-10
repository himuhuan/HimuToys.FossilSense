> **Status: archived** (2026-07-10)
>
> 历史交付说明。当前版本交付说明见 dist/DELIVERY-NOTE-<version>.md。
# FossilSense 交付说明 — v0.7.0（有限 include 分析）

**VSIX**: `dist/fossilsense-vscode-0.7.0.vsix`（3.93 MB，自包含原生二进制，安装后无需另编译 Rust）
**安装**: VS Code → Extensions → `...` → Install from VSIX，或
`code --install-extension "dist/fossilsense-vscode-0.7.0.vsix"`

OpenSpec 变更：`limited-include-analysis`（B 档：索引外部头但降权；第一层直接包含的头参与语义着色）

## 本次能力范围

新增**有限的 `#include` 分析**，由新配置 `fossilsense.includePaths`（VS Code 数组设置，或 `fossilsense.json` 的 `includePaths` 字段）驱动。为空时行为与上一版完全一致（零变化）。

### 能做什么

- **指定任意多个外部参考头目录**（绝对路径），如 `C:\TDM-GCC-64\x86_64-w64-mingw32\include`。两处来源（VS Code 设置 + `fossilsense.json`）会合并去重。
- **`#include` 行内补全**：光标在 `"…"`/`<…>` 内时给出头文件名与子目录候选（当前文件目录 + 工作区头 + `includePaths`）。引号 vs 尖括号只影响排序，不抑制候选。在 `auto` 下检测到 clangd/cpptools/ccls 时让位。
- **`#include` 跳转到头文件**：在 include 行上 Go to Definition 打开解析到的头；多命中返回排序候选列表；解析不到则不返回（绝不编造）。始终可用，不受补全模式影响。
- **外部头符号入库**：被索引、可工作区符号搜索/补全，但**排在工作区符号之后**（导航与补全都让工作区优先）。
- **第一层着色**：被工作区文件直接 `#include` 的外部头（派生的 `directly_included` 标记），其宏/类型/枚举量参与语义着色；只通过传递包含进来的外部头**不**参与着色。
- **健壮性**：目录缺失/非目录/重复条目跳过并在输出通道提示，绝不报错；错平台头（如开发 Linux 却配 MinGW 头）只是惰性参考文本，FossilSense 从不编译，没有触发报错的路径；单个外部头解析失败只跳过该文件；外部目录超过上限（默认每根 ~20k 文件 / ~512 MB）退回"仅路径解析、不入符号"。
- **增量**：外部头按 mtime/size 指纹增量，未变更不重解析。改 `includePaths` 会重启服务以生效。

### 还不能做什么（明确边界）

- 不跑预处理器：不评估 `#if`/条件编译、不做宏展开来决定"哪个 include 生效"、不追 `#include_next`。
- 不做传递包含图：着色只看直接包含的第一层。
- 不做 include 图的符号作用域收窄：着色与导航仍是全局的（与既有行为一致）。
- 不自动探测系统/工具链头路径：必须显式指定（保持"诚实降级"）。
- 不做表达式/类型推断（沿用既有成员补全的限制）。
- 扩展名过滤只索引配置的头/源扩展名；无扩展名的 C++ 标准库头（如 `<vector>`）能在补全里出现、但不入符号索引。

## 验证情况

- `cargo test -p fossilsense`：**142 passed / 0 failed**（含 config / store / indexer / includes / server / query 新增用例）。
- `cargo build` 与 `pnpm run compile`：均通过，无错误、无 dead-code 警告。
- **真实工具链实测**（`samples/mini-c` + `includePaths = C:\TDM-GCC-64\x86_64-w64-mingw32\include`，release 构建）：
  - 冷索引 1521 文件 / **372,916 符号** / 约 **10.1s**（discover 0.35s、parse 3.6s、write 6.1s）；
  - 增量再索引全部跳过、约 **85ms**；
  - `query symbol size_t` / `printf` 命中外部头、带绝对路径与条件编译 guard；
  - 全程**零报错**，验证了对 Windows MinGW 头（含 GCC 扩展、错平台无关）的容错。
- 说明：正确性验收样本 `example/HimuOS` 本地不存在（git-ignored，未入库），故realistic-scale 与"错平台头不报错"的验收以上述 1521 文件 / 372k 符号的真实 MinGW 头树替代完成。

## 安装后快速试用

1. 安装 VSIX，打开一个 C/C++ 工作区。
2. 设置 `fossilsense.includePaths` 加入 `C:\TDM-GCC-64\x86_64-w64-mingw32\include`。
3. 在某 `.c` 里输入 `#include <std` 触发补全；在 `#include <stdio.h>` 行上 Go to Definition 跳转到该头；若某工作区文件直接 `#include <stddef.h>`，则 `size_t` 等第一层类型会着色。
