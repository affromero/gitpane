---
slug: stash-preview
title: 查看 stash 内容
status: active
created: 2026-11-11
---

## Intent

在恢复 stash 之前，用户能在 gitpane 的 TUI 和 VS Code 扩展中查看该 stash 保存的文件与具体更改。

## Requirements

选择 stash 后按其不可变 OID 读取内容。默认展示保存时 HEAD 到 stash tracked tree 的合并更改，以及可选第三父提交保存的未跟踪文件（含使用 --all 保存的忽略文件）。不分开展示暂存区和工作区。

- [x] TUI 可从 stash 行进入文件列表并查看文件 diff，显示 stash 身份并可返回工作区视图。
- [x] VS Code 可展开 stash 文件列表，并在原生 diff 编辑器查看保存时的两侧内容。
- [x] 覆盖新增、修改、删除、重命名和保存的未跟踪文件；二进制明确提示，不渲染为文本。
- [x] 当前工作区、索引、HEAD 变化不会污染已选 stash 的文件与 diff。
- [x] stash 删除或重编号时不会静默打开另一个 stash；缺失对象与空内容有可见反馈。
- [x] 查看前后工作区、索引和 stash 列表不变；stash 视图不执行工作区暂存、丢弃或打开当前文件等写操作。
- [x] 相关 Rust、bridge 与扩展测试通过，使用说明同步更新。

## Non-goals

- apply、pop、drop、创建 stash 或其他 stash 写操作。
- 分别展示暂存区与工作区更改。
- 重构现有 stash 遍历、排序、持久化及无关导航。
