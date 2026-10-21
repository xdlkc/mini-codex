# Codex 对齐验收

final result: blocked

更新日期：2026-10-21。用户指定 **Codex**。完整零差异目标尚未完成。

## 运行环境恢复与当前验证

工作树复制追加：已按同版本 worker 的私有 index / binary diff / 3-way apply 序列替换分支复制。支持特殊 index 与子模块状态，保留源 index，目标 index 保持 HEAD；按参照处理 Git attributes、可执行位、空目录和未跟踪聊天目录。13 种源代码序列及 host 文件复制规则已重放，真实 app-server 的 14 种场景为 **66 checks / 55 RPCs**。无提交的嵌套仓库跳过，有提交的嵌套仓库按参照非递归 host copy 的错误边界拒绝。通用工作树复制仍保留其暂存区契约。

退出恢复追加：**11 个移交持久化检查点**在独立进程中直接退出，重启后两侧文件、原始 index、分支与 checkout 租约均恢复。真实归档失败 **7 checks / 9 RPCs**；新聊天与 Git 结果被保留，警告在重启后仍可查询。原生开发候选完成主窗口移到工作树、弹出窗口续聊、主窗口同步历史、带回本地及草稿保留；[往返与草稿](docs/screenshots/66-native-handoff-roundtrip.png)已留证。正常 Quit 为退出码 0，最终草稿仍准确。未把此流程等同于全部进程中断或全部参照视觉通过。

终端 shell OSC 标题按参照去掉目录前缀和控制字符，环境操作保留固定标题；[中文标题与 ANSI 输出](docs/screenshots/65-codex-terminal-shell-title.png)已实测。已删除的环境选择回退到默认配置；显式 None 和无效配置不自动启用。前端全量 **334 passed、0 failed**。最近完成的后端全量为 **287 passed、0 failed、3 ignored**。其后追加的外部 attributes、未跟踪聊天目录与嵌套仓库规则已通过 14 场景 debug smoke；11 个退出检查点的独立测试也已通过。最终包已重新生成，但最后一轮完整回归与包内启动复验受下述系统加载故障阻塞。

终端与窗口追加核对：按安装版 `terminal-panel-fcb64dc1d3de.js` 的快捷键映射及 host 的 terminal-focused command-key 处理补齐新建终端、清除、行首/行尾及左右删除。修复未定义复制函数，并让应用级捕获监听器把终端控制键留给 shell。原生窗口标题随当前聊天、重命名和返回首页更新。前端全量 **332 passed、0 failed**，桌面构建通过。

原生开发候选已实测 Command-T 新建标签、Command-K 不打开搜索、Command-Left/Right 插入位置，以及 Command-Backspace/Delete 的删除结果；后三者由 shell 写出的验收文件核对。重命名后系统窗口标题立即更新。弹出窗口的草稿成功返回主窗口，正常 Quit 返回退出码 0，草稿仍在 profile 中，两个终端 shell 均已回收；见[标题与草稿](docs/screenshots/64-native-window-title-draft.png)。这是窗口流程验收，不作为 Git handoff 完整多窗口验收的替代。该候选包已生成；最后追加的 Git 复制修复正在进入最终包。

交互终端追加：Codex 终端现使用真实 PTY 与锁定版本的 xterm 6.0.0，支持 ANSI、中文输入、尺寸变化、Ctrl+C、多标签、独立 shell 状态、页面重载恢复和后台输出。按键按顺序发送；历史重放不回送设备查询；输出确认控制背压。关闭会终止 shell 作业并回收 shell，之后才释放 checkout 租约。手动终端与模型工具执行分别处理。

这一版本最终后端全量 **279 passed、0 failed、3 ignored**（337.16 秒），终端专项 7 项通过。真实终端 smoke 为 14 checks，包内复验 14 checks / 102 RPCs；Environment 19 checks / 188 RPCs、Workspace 49 checks / 47 RPCs、Handoff 58 checks / 94 RPCs 通过。初次回归发现 PTY 背压与 macOS 退出回收互等，已增加未确认输出时关闭的测试并修复顺序。Ctrl+C 压力夹具现先确认提示符恢复；锁释放检查对并发 fork/exec 的临时继承作 1 秒有界确认，持有期间的拒绝断言保持不变。

浏览器实测[ANSI 与中文输入](docs/screenshots/60-codex-interactive-terminal.png)、独立终端状态、[页面重载恢复](docs/screenshots/59-codex-terminal-reload.png)及标签关闭。500 × 900 时页面无横向溢出，`stty size` 实际报告 51 × 53，见[窄窗口](docs/screenshots/61-codex-terminal-narrow.png)。[原生 Release 副本](docs/screenshots/62-native-codex-terminal.png)执行后真实写出 `native-result=ready`；关闭运行中的终端后，验收写入进程停止。测试副本只调整 bundle 标识与本地签名。

随后按同版本源代码修正环境编辑器：完全空白操作行被忽略，只填一半时显示缺项；保留脚本空白并使用参照的多行 TOML；图标选择保留原有 null，显式选择后使用原始 SVG。读取失败提供返回和重试。前端完整 **328 passed、0 failed**。实屏保存同时包含两种三引号和反斜杠的脚本，磁盘 TOML 解析后字节内容一致，空白操作未写入、运行图标已保存、保存没有执行 setup；见[图标与编辑](docs/screenshots/63-codex-environment-icons.png)。该追加 UI 的 App、DMG、PKG 已更新。最终包 Terminal 14 checks / 101 RPCs、Desktop 10 checks / 177 RPCs 通过；App 签名、DMG 完整性、PKG/App 二进制一致及 SHA256 清单通过。完整参照视觉 gate 仍未通过。

2026-10-21 本地环境追加：配置编辑、项目与继承配置、详情、平台脚本、revision 冲突保护、新工作树设置、输出、停止、失败重试、清理后归档和清理前恢复已实现。`.worktreeinclude` 与 ignored `AGENTS.override.md` 使用安装包的 Git 交集规则复制，保留目标已有文件并拒绝目标 symlink 目录。主动选择环境才运行 setup，聊天分支不自动选环境。

这一版本完整 Rust headless 回归 **272 passed、0 failed、3 ignored**（344.51 秒），前端 **315 passed、0 failed**。初次完整回归发现旧 checkout 环境读取和活动源聊天创建的错误，已修复；目录布局改变后两个旧夹具及一个 smoke 的父目录创建方式也已更新。新 App、DMG、PKG 已重建，包内 Environment 19 checks / 201 RPCs、Workspace 49 checks / 47 RPCs、Handoff 58 checks / 94 RPCs、Desktop 10 checks / 177 RPCs 通过。签名、DMG 完整性、PKG/App 二进制一致和 SHA256 清单通过。

浏览器已实际保存配置、创建工作树并执行 Bash；核对源目录未运行 setup，目标收到准确的源/目标路径，顶部操作继承导出变量。原生 Release 副本读到同一配置，修改并保存名称，TOML 与界面成功反馈一致；[原生环境详情](docs/screenshots/58-native-codex-environment.png)已留证。测试副本只调整 bundle 标识与签名。500 × 900 无页面水平溢出。该结果不是同状态参照的视觉通过；交互式 PTY 后续结果见上文。

2026-10-21 继续执行时，原句柄 `47587`、`81045`、`3752` 均已返回成功终态。两个元数据保护测试通过；旧 Node 检查与后端构建也已结束。新启动的 Node、rustc、cargo 工作正常。重新运行的基线为 Rust 246 passed、3 ignored，Node 全量 300 passed。旧的加载阻塞已解除。

本轮新增 checkout 共享执行锁与移交排他锁，涵盖新建目标、已有目标、磁盘恢复及语音执行，并保留到最终状态写入结束。两个新增锁/恢复用例通过；最新二进制的 Handoff smoke 为 58 checks / 94 RPCs，权限为 17 checks / 19 RPCs，Guardian 为 33 checks / 40 RPCs，Turn 为 9 checks / 53 RPCs，均使用隔离目录和本机 mock。

持久命令授权已复用固定版本的公开 Codex Starlark 引擎、shell 解析和危险命令分类器，保留 Apache 许可证与 NOTICE。规则保存到本应用 profile 的 `rules/default.rules`。本地与 SSH 仅接受原请求提供的参数前缀；单次批准不保存规则。7 项策略单元测试、2 项真实远程批准测试、26 项权限/审批界面测试通过。新增 exec-policy smoke 为 27 checks / 15 RPCs / 18 次本机模型请求，其中 14 组匹配结果与安装版 CLI 完全一致。另用隔离的官方 app-server 验证了实际批准、写入规则和新聊天复用。公共源码版本与安装包版本不视为相同；该交叉验证只证明覆盖的行为。

自动审查现使用官方默认风险策略、结构化输出、目录驱动的 reviewer/override/推理强度选择。3 项审查与来源单元测试、6 项模型目录测试通过。新增拒绝恢复 smoke 为 13 checks / 20 RPCs / 11 次本机请求，覆盖精确 developer 授权、无自动执行、运行中注入、取消/重启保留和分支来源降级。Guardian smoke 更新为 34 checks / 40 RPCs / 35 次请求。安装版 CLI 的独立探针也确认非 denied 忽略、精确 developer 上下文及不执行/不新建 turn。前端全量 303 passed。

审批耗时按真实执行开始计时，排除等待审批的时间；权限 smoke 更新为 18 checks / 19 RPCs / 24 次请求。安装版 CLI 在等待 500 ms 后批准，报告执行耗时 0 ms，且批准前已有 commandExecution item/started；实现保留该事件时机。

浏览器实测拒绝动作的标记文件未创建；展开显示原因和高风险授权提示，刷新后重新打开聊天仍有审查结果，控制台无 warning/error。见 [最新审查界面](docs/screenshots/51-codex-guardian-policy.jpg)。

移交回归发现并修正授权上下文保存与停止移交共用启动锁的问题，改用独立上下文锁。最新 debug 的 Handoff smoke 为 58 checks / 94 RPCs；Release 包内程序复验为 58 checks / 95 RPCs，拒绝恢复复验为 13 checks / 20 RPCs。两次全量回归分别暴露旧移交夹具未释放已完成租约，以及文档并发锁 2 秒等待过短；前者已按协调器生命周期修正，后者改为 10 秒有界等待，桌面状态模块 12 passed、1 ignored。并行恢复锁测试还出现一次未取得预期恢复错误的失败；测试现保留错误诊断，仅对 busy 结果作 2 秒有界等待，随后仍严格核对未完成恢复屏障，其它错误立即失败。最终 Rust headless 全量为 **260 passed、0 failed、3 ignored**（326.14 秒，4 个测试线程），前端全量 **303 passed、0 failed**。3 个 ignored 均为由跨进程用例单独启动的辅助 worker。

Release App、DMG 和 PKG 已构建，App 本地签名及 DMG checksum 验证通过。原生窗口使用当时 Release 候选包的副本和独立 profile；仅调整测试副本的 bundle 标识与本地签名，以便与旧窗口隔离。未修改原有预览、原生窗口和草稿。实测发送、审查拒绝、搜索重新打开聊天、键盘展开详情，标记文件与持久规则均未产生。见 [原生 Release 截图](docs/screenshots/52-native-codex-release.png)。部分高层点击未送达预期控件，详情使用 Tab/Space 完成，不将这些点击计为通过。后续状态锁修复只调整后台等待，没有改动界面。最终包已重建，包内 Desktop smoke 10 checks / 177 RPCs、Documents smoke 10 groups / 48 RPCs 通过。App 签名、DMG checksum 和 SHA256 清单通过，PKG 内可执行文件与最终 App 完全一致。原有 5173 预览与原生进程仍在运行，未重启或载入新代码，以保留原有草稿。最新修复在本次桌面包中；本次 UI 服务、临时依赖和失败夹具已回收。下面保留历史阻塞证据；它不是当前运行状态。

### 当前追加复验的运行环境阻塞

2026-10-21 16:43（Asia/Shanghai）记录：最后一轮全量中 286 项已通过、1 项失败，4 个辅助 worker 被隔离；失败项为 `terminal::tests::session_lists_keep_creation_order_and_thread_isolation_and_shutdown_closes_all`。该失败尚未取得完整诊断，不能归因于系统加载故障。新增的退出检查点测试等待一个新 worker，该 worker 与后启动的复制模块测试均停在 macOS `_dyld_start`，尚未进入 libtest。该轮没有完成，未计作全量通过；终端失败也尚未复验解决。Node 版本探针与最终包版本探针也未能在 3 秒内到达入口；Rust 版本探针成功。字节相同的临时包内程序副本仍在 5 秒探针内停滞。不能据此判定产品逻辑失败，也不能宣称最新包的运行复验通过。

最终 App、DMG、PKG 已生成；App 签名、DMG checksum、PKG/App 二进制一致及 `dist/SHA256SUMS.txt` 全部通过。最后包内 fork-state smoke 在 initialize 前超时，没有返回 stderr。此前候选包已通过 12 场景 / 60 checks / 48 RPCs；最终补齐嵌套仓库规则的 debug 为 14 场景 / 66 checks / 55 RPCs。最新最终包仍需在运行环境恢复后复验。

已终止并回收本次确认停在加载阶段的测试和探针，关闭临时预览及原生副本，删除中断夹具与用完的临时文件。没有修改系统安全设置。原 5173 预览进程 `42018`、原生进程 `90047` 和参照进程 `22920` 保留，未重启或读取原草稿。完整 1:1 目标仍未完成。

### 历史执行阻塞

2026-10-21 02:46（Asia/Shanghai）再次检查：执行句柄 `47587`、`81045`、`3752` 均仍报告运行中且没有新增输出，没有据此重启相同任务。独立的系统 Ruby 健康探针启动真实工具链的 `rustc --version`，5 秒内没有版本输出；探针随后终止并回收自己创建的 rustc 子进程，句柄 `8095` 返回退出码 1。此前任务自有测试的采样停在 `_dyld_start`；目前尚未确定系统故障根因。

该故障当时连续三轮阻止后端回归、完整 Node 测试、桌面构建和原生验收。独立界面验证的 23 项用例与截图保留，未被用作集成验证通过的依据。恢复后的结果见上文。完整目标未缩小，也未完成。

## 对照对象

- 参照：本机 ChatGPT App `26.1013.40934`，build `20770` 的 Codex 首页。原始截图为 3584 × 2098，按 2 倍密度归一为 1792 × 1049。
- 实现：[浏览器首页](docs/screenshots/33-codex-parity-home.png)，视口为 1792 × 1049；项目为 `mini-codex`，浅色主题，模型为 GPT-6.1 Sol，思考为极高。
- [同尺寸主区域对照](docs/screenshots/38-codex-parity-comparison.png)已合并展示参照与实现。为保留必要证据，图中只含主区域，不保存参照账户的聊天标题。
- [原生窗口](docs/screenshots/37-native-codex-parity.png)另以 1280 × 820 检查。它与参照窗口尺寸不同，不能据此声明原生逐像素一致。

## 已修复与复验

- 首页与输入区已按同尺寸截图调整。取消四张 Codex 引导卡，恢复 Codex 原始图标，集中展示项目、执行位置、模型与思考选项。
- 修正模型菜单压缩行高造成的说明文字裁切。[原生模型菜单](docs/screenshots/35-codex-parity-models.png)可完整显示模型、思考、速度与刷新操作。
- 修正异步保存后，后退操作丢失前进历史的问题。浏览器实测先后退到空白会话，再前进恢复原会话与标题。
- 修正更新输入区时重复移动 DOM 节点造成的焦点丢失。浏览器与原生窗口实测选择思考后，焦点保持在模型按钮。
- 浏览器实测发送消息、Rust 流式回复、历史恢复、跨会话草稿、已读/未读、分区创建/移动/移除/刷新保留，以及目录项目切换和移除。模型请求仅使用 loopback mock。
- [窄窗口](docs/screenshots/36-codex-parity-narrow.png)实测 735 × 953。页面 `scrollWidth` 为 735，输入区底部为 953，无水平溢出。临时视口已恢复。
- Work 模式实屏检查通过。Codex 控件离开模式后回到原位置。
- 最终浏览器页面未记录应用错误。桌面调试包已更新到 `dist/Mini ChatGPT.app`。
- 聊天分支主流程已追加验收。[浏览器子菜单](docs/screenshots/39-codex-fork-worktree.png)提供当前目录/同一工作树、新工作树和无旧历史的新聊天。新工作树使用当前 HEAD 的 detached checkout，复制本地文件及未跟踪文件，原目录、原始 index 字节和原聊天历史不变。参照的工作树分支不保留暂存标记；通用“新工作树并携带改动”仍保留其原有暂存区语义。
- 浏览器实测原草稿返回后恢复，新分支草稿为空；“在此工作树中新建聊天”没有创建副本，实际 Git 目录仍为该工作树。子菜单在 500px 和 1792px 视口之间切换后均在边界内。Escape 返回父菜单，右方向键重新打开子菜单。
- [原生分支菜单](docs/screenshots/40-native-codex-forks.png)没有被父菜单裁切。原生窗口已成功创建同一工作树分支并显示复制的历史与 detached HEAD。菜单文字采用安装包 `zh-CN-7ba80cbb133d.js` 的 `threadHeader.fork*` 等条目。

## 仍阻止完整通过的差异

| 优先级 | 差异 | 完成条件 |
|---|---|---|
| P1 | “帮我批准”已接独立模型审查、只读检查、单次决策和历史。路径保护已复验，持久命令规则已接官方引擎并交叉验证。官方默认风险策略、目录驱动的模型选择和拒绝恢复接口已实现；完整原生交互、账户切换及企业自定义策略尚未验收。 | 验证账户切换与完整原生交互，并核对自定义策略契约。不能把已覆盖的本地流程当成完整官方 Auto-review。 |
| P1 | Handoff 已接入 Goal 保留、系统中断暂停、归档删除 heartbeat，以及来源和 checkout 的跨进程排他。Git 成功但复制聊天失败时按参照保留 Git 并警告。11 个持久化阶段退出、原生主/弹出窗口往返和真实归档失败已验证；Git 命令执行中强制退出、页面重载及全部参照视觉尚待验收。 | 完成各阶段中断恢复和真实多窗口操作；对照同状态参照窗口，验证警告、重试、来源归档及目标执行。 |
| P2 | 特殊 Git 状态的分支能力尚未完整对齐。已支持 intent-to-add、assume-unchanged、skip-worktree、稀疏/冲突/split index 和子模块状态；14 种真实分支场景通过，嵌套仓库支持/拒绝边界已核对。完整桌面错误反馈仍待逐项比较。 | 验证完整参照桌面入口、错误文案及失败清理。 |
| P1 | 最新全量中终端列表/关闭回归失败，原因未明；本地环境配置、设置/清理、归档恢复和真实 PTY 主流程此前已通过回归；主要终端快捷键和 shell 标题已核对；全部菜单、shell 启动动作队列及完整参照视觉尚未逐项验收。 | 先复现并修复 `session_lists_keep_creation_order_and_thread_isolation_and_shutdown_closes_all`；再对照全部终端状态与同状态参照窗口。 |
| P2 | 参照账户的 Your dot 与部分应用栏入口没有对应能力。 | 明确并实现目标版本中要求保留的入口与实际行为。 |
| P2 | 全部会话内容、菜单、设置、悬停、焦点、动画及中英标签尚未逐屏对齐。 | 同版本、同尺寸、同主题、同交互状态下逐项比较，修复差异。 |

参照账户与隔离测试账户的历史、连接数量和身份不同。这些内容差异未被当作布局误差。当前不能声称交互和功能完全相同。

## 验证记录

审批卡片交互的追加验证：

- 安装包 `pending-request-item-panel-a6ee45e20086.js` 明确将“拒绝”发送为 `decline`；命令理由作为标题，文件的会话授权位于“允许一次”旁的范围菜单。Codex 渲染器已据此修正。`cancel` 仍是单独的中断协议动作。Chat/Work 保留原有渲染器。
- 系统 JavaScriptCore 使用临时适配器与独立 Realm 运行权限界面 13 项和新增审批卡片 10 项，共 23 passed。覆盖回包决策、文件范围选项、重复提交拦截、理由/路径按文本处理、中文标签、焦点与菜单键盘操作，以及桌面待审批记录清理。尚未计为 Node 完整测试通过。
- 隔离浏览器页实际装载生产审批渲染器与 `renderPendingApprovals`，点击确认了 `acceptForSession`、`decline` 与 `accept` 三种回包。文件菜单 ArrowDown 可进入和移动，Escape 关闭并返回范围按钮。[范围菜单](docs/screenshots/47-codex-approval-options.jpg)与[窄窗口](docs/screenshots/48-codex-approval-narrow.jpg)已保存。500 × 900 下 `scrollWidth=500`，卡片与菜单在边界内，按钮纵向排列；控制台无 warning/error。该页只模拟 RPC 回包，没有执行 Rust 命令。
- 测试页和本机夹具服务已关闭，视口恢复。系统进程清单确认了任务自有的停滞测试和 rustfmt，终止后对应句柄返回 137；未终止正常预览或原生窗口。进程总数低于当前进程限制，尚不能据此确定加载故障的根因。其它挂起句柄仍未返回终态，未据观察超时重复启动它们。
- 最新后端路径保护回归、Node 完整测试、桌面构建与原生审批流程仍待完成。桌面包保留上一轮已验证版本，不能声明零差异。

沙箱执行与批准时机的追加验证：

- 官方 CLI 的隔离探针使用本机 mock、独立配置与临时仓库。已确认普通命令不弹审批、沙箱失败不自动升级、显式升级获批前不执行、Never 拒绝升级；普通补丁与需要额外访问的补丁分开处理。文件会话授权不覆盖其它文件或 shell，命令取消会中断 turn 且无下一次模型请求；`.git`、`.codex` 和 `.agents` 的普通写入被拒绝。
- 新 `smoke-permissions.py` 在最终路径保护修改前通过 17 checks / 19 RPCs / 24 次本机请求。覆盖真实写入边界、单次授权不外溢、文件授权范围、取消中断、Stop 终止后台子进程和超时失败。更新后的审批 smoke 33 checks / 40 RPCs / 35 次本机请求；Turn 9 checks / 53 RPCs / 15 次请求，Automations 12 checks / 335 RPCs / 21 次请求通过。
- Rust 初次全量回归暴露了元数据保护表达式错误，以及两个仍使用旧审批语义的测试。表达式和测试已修正，另加不存在父目录与 `..` 的越界回归。随后测试进程停在系统加载阶段，采样仅有 `_dyld_start`，未进入 libtest。对任务自有测试制品补签名后仍未恢复；未调整系统安全设置。最新完整回归、前端复验和实屏验收尚未完成，不能把旧结果计为新修复通过。
- 系统 `sandbox-exec` 已单独确认最新表达式拒绝元数据写入。`git diff --check` 通过。桌面包未替换，原有预览与草稿保留。

自动审批执行器与权限菜单的最新复验：

- 已核对本机 CLI `0.163.0-alpha.10.2` 的 schema 与安装包权限菜单。`guardian_subagent` 和 `auto_review` 归一到独立 reviewer，`user` 保留人工审批。审查事件为 `item/autoApprovalReview/started|completed`，状态包含 inProgress、approved、denied、timedOut 和 aborted。
- `smoke-guardian.py`：32 checks / 38 RPCs / 34 次 loopback Responses 请求。覆盖独立请求与工具集合、单次允许/拒绝、只读读取与写入拦截、无效 JSON 的三次尝试、中断后无迟到执行、人工模式、原沙箱不被放宽、设置修改、分支/重启历史及审查 token 触及 Goal 预算后不执行动作。模型响应伪造用户角色会被拒绝，不进入用户历史；被拒绝的命令不显示为已执行。普通输出中的 denied 等字样不影响审批状态，内部状态不进入模型上下文；拒绝的提案不计为 Goal 的实际工作。判定内容来自本机 mock；它验证实际 Rust 执行链路，不验证真实模型的风险判定准确率。
- [权限菜单](docs/screenshots/45-codex-permissions.png)与[审查结果](docs/screenshots/46-codex-approval-review.png)已在浏览器实测。临时目录中确认了审查前无文件、批准后文件存在。刷新并重新打开聊天后仍显示审查结果，未记录应用 console error。没有同状态参照截图，不能声明像素一致。
- 前端新脚本 13 项通过，覆盖模式与沙箱独立、自定义策略标识、完整访问的产品确认、主机捕获、新聊天默认值、配置加载失败/连接变更时禁止沿用旧权限，以及跨主机审查结果隔离。实屏发现的主机默认值覆盖聊天权限问题另有回归用例，相关 8 个脚本共 93 项通过。实测旧聊天恢复人工只读模式，新聊天恢复主机的自动审查/工作区写入默认值及输入焦点。
- Rust 全量 236 passed、3 ignored；后续 appserver 9 项、agent 6 项、session 9 项通过。API 模块新增输出角色验证后 19 项通过，覆盖 added、done 和 final 三种伪造入口。Turn smoke 9 checks / 53 RPCs、Automations 12 checks / 347 RPCs、Handoff 56 checks / 92 RPCs 通过，均使用隔离目录和本机 mock。

Goal、heartbeat 与部分失败语义的最新复验：

- 官方 CLI 在隔离 profile 中验证：fork 的目标无 Goal；archive/unarchive 保留原 Goal；闲置活动 Goal 不因归档变成 paused。安装包中的系统中断路径会先暂停 Goal；host 归档副作用删除 heartbeat 定义。
- Rust 全量 233 passed、3 ignored；随后新增 2 项调度边界测试，模块复验 19 passed、1 ignored。验证归档后无法触发/重新激活 heartbeat，以及定义删除后仍能完成进行中 run 的账本。
- Handoff smoke：56 checks、91 RPCs、4 次 loopback 模型请求。验证源修改与执行的真实跨进程锁、Goal 保留与暂停、归档重启不执行、显式取消归档后恢复、heartbeat 清理、复制失败保留 Git 与原聊天，以及持久警告查询。
- Automations 12 checks / 349 RPCs、Voice 9 groups / 74 RPCs、Turn 9 checks / 53 RPCs、Desktop 10 checks / 177 RPCs 通过。前端相关 9 个脚本 94 passed，另含语音/调度的 4 个脚本 65 passed。
- 浏览器[复制失败状态](docs/screenshots/44-codex-handoff-warning.png)已实际触发。源 Goal 与草稿保留；对话框宽 420px，无水平溢出；技术原因放入“详情”。此状态没有同条件的参照截图，不能据此声称像素一致。
- 桌面调试包已更新。全部 smoke 使用隔离临时目录和本机 mock，未调用外部模型。

- `cargo test --no-default-features -- --test-threads=4`：215 passed，3 ignored。后者为已有独立 worker 入口，未将它们计为通过。
- Codex chrome、sidebar、projects，以及既有 workspace、models、desktop state、execution context、remote、native windows、popover 的 10 个 JS 脚本：111 passed。最后的 chrome/sidebar/projects/popover 复验：45 passed。
- Desktop JSONL smoke：10 checks，177 RPCs。覆盖真实目录绑定、缺失目录无写入、实际任务目录和重启恢复。
- Workspace JSONL smoke：49 checks，47 RPCs，2 次 loopback model mock。覆盖工作树携带本地改动与失败回滚。
- `node --check`、`git diff --check`、桌面构建与 `scripts/package-macos.sh --debug --app-only` 通过。构建仍有既有 dead-code 警告。

追加分支验证：

- Rust 全量：217 passed、3 ignored；其后新增的工作树清理边界用例另行运行，2 passed。覆盖后续编辑、ignored 文件和新归属的保留。
- 11 个相关 JS 脚本：118 passed，其中新的分支脚本 7 passed。覆盖捕获主机、重复点击、失败后重试、迟到结果与新聊天的历史隔离。
- `smoke-forks.py`：35 checks、34 real JSONL RPCs、3 次 loopback model request。覆盖历史/UI 快照、标题序号与归档后来源、nullable 参数、实际目录、重启、独立继续、运行中的原聊天以及失败后资源回滚。
- 既有 Desktop smoke 10 checks / 177 RPCs、Turn smoke 8 checks / 45 RPCs / 13 次 loopback mock、Workspace smoke 49 checks / 47 RPCs / 2 次 loopback mock 均再次通过。
- 补充用例曾在系统加载器阶段停滞。该进程被明确终止后，完成本地测试制品签名并重新运行；实际测试进入后全部通过。没有调整系统安全设置。

追加 handoff 后端验证：

- Rust 全量 231 passed、3 ignored。新增的直接接入已有工作树用例随后另行通过。覆盖真实分支迁移、源与目标两侧的 stash、冲突后原始 index/文件/权限恢复、后续编辑保留、跨仓库恢复拒绝和未完成迁移的重启恢复。原暂存 blob 在移除 reflog 后执行真实 `git gc --prune=now` 仍可读取。
- 既有 Desktop smoke 10 checks / 177 RPCs、Turn smoke 8 checks / 45 RPCs / 13 次 loopback mock、Workspace smoke 49 checks / 47 RPCs / 2 次 loopback mock、Fork smoke 35 checks / 34 RPCs / 3 次 loopback mock 再次通过。
- `smoke-handoff.py`：37 checks、31 real JSONL RPCs、3 次 loopback model request。覆盖新工作树迁移、带回本地、保留两侧文件的直接接入、历史/UI 快照、标题与置顶、项目/搜索/推理强度、原聊天归档、目标独立回复、重启，以及 Git 准备完成后的真实 metadata 保存失败回滚。无效请求不会停止运行中的源回复；显式中断的有效迁移会保留已停止回复的快照。
- 该阶段只完成后端，后续界面验收与更新如下。

追加 handoff 界面与状态验证：

- 通过执行位置菜单完成浏览器“新建工作树”和“直接接入既有工作树”，以及原生 WebKit“带回本地”的实际操作。文字草稿、附件、文件引用与置顶均保留；来源草稿被移走。原生附件来自通过真实 Rust 文件 API 导入的验收资料库，作为已有草稿装入。浏览器扩展的文件 URL 权限不足，自动文件选择器上传未通过；没有扩大权限或将该步骤计为通过。
- 迁移时关闭进度框并切换到另一聊天，新编辑保持可见。完成后打开目标聊天，历史和原草稿仍在。直接接入前后，两侧真实 Git 分支/index/文件指纹完全相同。
- 500 × 900 视口的对话框边界为 x=40…460、y=267…633，页面 scrollWidth=500。临时视口已恢复。Tab 会循环回关闭按钮，Escape 关闭并恢复执行位置按钮焦点。高层键盘接口未送达事件后，使用该浏览器受支持的按键接口完成实测。
- 对话框中文、移交/成功图标与 compact 宽度按安装包核对。参照弹窗尚未取得同状态截图，不能据此声明逐像素一致。见[原生对话框](docs/screenshots/41-native-codex-handoff.png)、[原生状态](docs/screenshots/42-native-codex-handoff-state.png)、[既有工作树](docs/screenshots/43-codex-handoff-existing.png)。
- 继续对话暴露并修复了顶部标题被新输入覆盖、发送后的引用草稿未清理，以及纯文件引用在用户气泡中泄漏展开正文的问题。新消息保留独立文件卡片，点击可预览；模型仍收到实际文件内容。刷新后，已发送的引用不再回填到输入区。旧记录未作破坏性改写。
- Rust 全量 232 passed、3 ignored；最后的引用呈现修复另跑 appserver 7 项通过。9 个相关前端脚本共 93 passed，其中 handoff 14、文件 UI 9、桌面状态 8。Handoff smoke 42 checks / 36 RPCs / 3 次 loopback；Turn smoke 9 checks / 53 RPCs / 15 次 loopback。包含请求 ID 的运行中状态、重启结果恢复、引用的历史和分支保留。默认桌面构建和 App 打包通过。
- 迁移前等待已开始的附件导入保存，完成后才调用后端；进行中会停用该聊天的 Composer，并阻止快捷提交。新编辑不会与已发送/已迁移的附件上下文混合。
- 预览服务已更新，原正常配置目录的已保存草稿指纹未改变。既有 Chrome 用户标签的观察通道超时，未强制刷新，也未将该标签的实屏更新计为通过。Codex 内的预览打开请求返回 queued；隔离浏览器与原生窗口的实测证据独立保留。
