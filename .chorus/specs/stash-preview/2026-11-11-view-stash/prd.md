---
title: PRD - TUI 与 VS Code 查看 stash 内容
proposalUuid: 51533c3f-8e1e-4da6-b21a-0c0cd8d1fe03
documentUuid: 05f0585f-88b8-4145-af3f-4fc3575e3b39
---

# PRD - TUI 与 VS Code 查看 stash 内容

## 问题与目标

用户反馈 gitpane 只能看到有 stash，无法查看具体内容。调研确认 TUI 选择 stash 行会触发 FocusRepoDetails 并展示当前工作区；VS Code 的 stash 叶子激活无操作。用户已通过两次交互确认：两端均实现；展示 tracked 合并 diff 和 stash 保存的未跟踪内容，不拆分 index/worktree。

Idea: c0eb2b81-3519-42c7-930a-04809ef51114

## 用户流程

TUI 保留 S 展开 stash 的交互。选择 stash 显示其文件列表，标题标识 stash@{n} 与摘要；沿用文件 diff 的键盘/鼠标入口。选择仓库或工作树恢复正常工作区视图，返回操作可退出 stash 浏览。stash 模式不得把所选文件传给工作区 stage/unstage/discard/edit 等操作。

VS Code 保留 stash 分组，在每个 stash 下按需加载文件子项；点击文件打开 VS Code 原生 diff。两侧均从保存的 Git 对象读取，不使用当前工作区或索引作为对照。虚拟文档为只读。刷新时保留合理的树状态；错误和空 stash 提供可见反馈。

## 内容与身份

- tracked：stash 第一父 tree 对比 stash tree，合并已暂存与未暂存更改。
- saved untracked：存在第三父时，展示该 tree 中保存的文件，作为新增内容；也覆盖 --all 保存的忽略文件。
- 文件状态支持新增、修改、删除、重命名。重命名保留原路径与新路径；二进制提供明确提示。路径按字面传递，覆盖空格、非 ASCII、方括号及前导短横线。
- stash 列表 index 仅用于展示；请求与虚拟文档使用不可变 OID。stash 重编号不能令请求指向另一个对象。对象不可用时显式报错并允许刷新重选。
- 空内容、读取失败和大 diff 沿用现有加载/错误/滚动约定；不执行 apply、pop、drop。子模块只展示保存的 gitlink 指针变化，不递归浏览子模块工作区。

## 验收与验证

1. 创建同时有 staged、unstaged、删除、新增、重命名和未跟踪文件的 stash -u，TUI 与 VS Code 均能浏览正确文件和 diff；普通 stash 无第三父也正常。
2. 修改当前工作区、索引或移动 HEAD 后，stash 预览仍对应保存时内容。仅未跟踪文件的 stash 也可查看；--all 保存的忽略文件可见。
3. 覆盖文件空格、中文及 pathspec 元字符；二进制提示可见。重编号不串位，缺失对象/空列表有反馈。
4. 查看前后比对 git status、index 内容及 stash reflog，保持不变；TUI stash 模式的工作区写操作不会执行。
5. 使用最小稳定边界补充 Git/bridge 契约测试、TUI 来源与动作切换测试、扩展适配及浏览交互测试；执行相关 Rust 和扩展检查。更新两端 README 的 stash 用法。

## 实现任务边界

1. 共享 Git stash 文件与 diff 读取、bridge API：使用仓库作用域 Git 命令或 libgit2，禁止依赖工作区推断内容；提供两端共用的 OID 与文件来源语义。
2. TUI 接入：stash 选择动作、来源状态、文件列表/diff、工作区动作保护、相关回归测试和说明。
3. VS Code 接入：stash 文件树、bridge 请求、只读虚拟文档、原生 diff、交互回归测试和说明。

Chorus 实现任务：共享读取 `6c668f79-f7ec-412c-9e3c-c14a867aaaa5`；TUI `e883db85-034d-4db1-8f7a-ac42a680fcb0`；VS Code `910a35f8-b32b-4894-920e-a60e4beaccbf`。两端任务依赖共享读取任务。

## 非目标

不增加 stash 写操作，不拆分 staged/unstaged 视图，不顺手改 stash 排序、缓存或无关导航。

## 交付与验证证据

代码与两端 README 已完成，以下检查全部通过。当前等待 Chorus 任务审查与人工验收，持久规格保持 active。

- `cargo fmt --all`、`cargo clippy --all-targets --all-features -- -D warnings`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`。
- `cargo test --bin gitpane stash` 通过 41 项。`cargo test --all-targets --all-features` 主测试集通过 951 项、忽略 1 项，其余目标通过 12 项。
- `python3 scripts/check_test_environment.py`：通过无 Git 环境检查。
- 扩展 `npm test`：437 tests，424 passed、13 skipped、0 failed；跳过项未计入已通过数量。
- `node vscode-extension/test/integration/stash-browser.js`：13 checks passed；既有 `browser.js`：dark/light 各 581 checks，restored 8 checks。
- `cargo build --bin gitpane` 后以真实 Node BridgeClient 连接临时仓库验证：混合 staged/unstaged、删除、重命名、未跟踪特殊路径、二进制、HEAD 后续变化与 stash 重编号均正确；查看前后 status、index、stash reflog 不变。
- 改动源码均小于 1000 行，Rust src 目录均不超过 10 个直接文件；`git diff --check` 通过。

TUI 任务复审前修复了两处问题。stash 列表加载按请求代次过滤迟到的结果。文件选择和 diff 刷新按来源与路径定位同一文件。新增三项回归均通过。

完整日志保存在本次会话的 `/tmp/gitpane-*.log`。本机无 just，已直接执行其等价 Cargo 检查。只读集成审查发现的三项 P2 已修复；保留一项 P3 体验限制：VS Code 新增 stash 引发重编号后，已有展开节点或预览可能失效，需要重新展开，不会打开另一份 stash 内容。
