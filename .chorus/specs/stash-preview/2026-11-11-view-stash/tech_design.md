---
title: 技术设计 - stash 只读预览
proposalUuid: 51533c3f-8e1e-4da6-b21a-0c0cd8d1fe03
documentUuid: f1200cb7-09ea-461d-9c5e-b01a24beda98
---

# 技术设计 - stash 只读预览

## 改动前接入点

原实现中 src/components/repo_list/mod.rs 的 stash 选择发送 FocusRepoDetails，src/app/actions.rs 因而加载工作区。src/components/file_list/mod.rs 维护文件与 diff；src/git/commit_files.rs 已有 tree diff 可复用。扩展 extension.js 将 bridge 的 stash 转为 webview 节点，view.js 的 stash 叶子激活没有文件入口。实现限定在这些路径及对应 Git/bridge 模块，不重构无关遍历与 graph。

## 共享读取契约

新增 stash 专用 Git 读取模块，输入仓库路径和完整 stash OID，提供文件列表与单文件内容/diff。文件条目包含来源 tracked/saved-untracked、状态、旧/新路径；展示标签不承担身份。bridge 暴露两类只读请求供扩展使用，TUI 直接调用共享 Rust 函数。先实现共享层，再接入两端。

tracked 比较 stash 第一父的 tree 与 stash tree；saved-untracked 比较空树与第三父 tree。重命名保留两侧路径，删除右侧为空，新增左侧为空。二进制不解码为文本；gitlink 展示对象指针变化。沿用现有项目错误和 diff 格式，避免重复引入文本渲染实现。

Git 命令通过 crate::git::process::git_command 生成，外部输入以 argv 传递；路径字面匹配，完整 OID 先解析校验，不使用拼接 shell。对象丢失显式报错；stash 重编号仍按原 OID 读取同一快照。

实际快照读取通过 libgit2 完成：先由原仓库解析 ODB（含 linked worktree/commondir/alternates），再用 Repository::from_odb 创建独立内存对象视图并附加空 Config/Index。全部 tree/blob/diff/patch 由此视图生成，避免 patch 生成时隐式读取当前索引或 attributes。二进制仅在两侧 blob OID 不同时提示内容不同；纯权限或重命名仍保留 patch 文件头。新增的 linked-worktree 与损坏 index 回归已通过。

## TUI 来源状态

为文件列表维护明确的 WorkingTree/Stash 来源。stash 选择动作携带 repo 与 OID，加载对应文件；来源随异步请求传递，完成时丢弃与当前选择不符的结果。刷新工作区状态不能覆盖 stash 内容。标题显示 stash 标签与摘要，返回/选择 repo 或 worktree 恢复普通模式。

stash 模式中所有工作区写动作在动作分发边界拒绝执行；不要仅隐藏快捷键提示。单文件 diff 使用来源对应的 Git 读取，不走当前 git diff HEAD 路径。

## VS Code 浏览与文档

stash 节点通过不透明 id 映射扩展持有的 repo/OID，按需请求文件；不要依赖 stash index 解析身份。文件节点激活后，扩展创建包含不可变对象身份、文件来源和旧/新侧的虚拟文档 URI，由只读内容 provider 通过 bridge 读取并调用 vscode.diff。对 URI 参数做结构与仓库归属校验，树快照不发送无必要的内容。沿用 textContent 展示路径和摘要。

空列表、二进制与读取失败提供可见状态；异步结果绑定 stash 身份，切换或刷新后不回填到其他节点。文档与文件请求不执行 stash apply/pop/drop 或任何工作区写命令。

## 验证边界

共享 Git 测试使用独立临时仓库，覆盖第三父存在/缺失、tracked 与 untracked、特殊路径、重命名、二进制、仅未跟踪 stash 及查看前后状态不变。TUI 测试来源切换与写动作保护；扩展测试树请求、原生 diff 参数、虚拟文档两侧内容和错误反馈。各任务执行受影响检查，集成后统一运行必要的项目检查并记录结果。
