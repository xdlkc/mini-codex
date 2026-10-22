# Codex 桌面工作台对齐记录

更新日期：2026-10-21。当前目标已明确为 **Codex**。历史目标：对齐 macOS ChatGPT App 的 Chat、Work、Codex 视觉、交互和产品功能；后端使用 Rust。本轮 Mini、全局入口、Work 本地内容类型与原生核心流程已完成，详见 [本地验收记录](NATIVE_LOCAL.md)。本文另保留完整官方产品的差异，不把本地交付等同于在线生态或像素级一致。

本次范围已由用户确认限定为本地可实现的功能。云同步、分享、多人协作和云执行移出本次范围；下表保留这些产品差异。本地新增内容与原生实际操作结果见下方“本轮本地交付”；真实外部账户/设备与完整官方生态分别保留边界。

## 参考与证据边界

- **安装包静态证据（S）**：`/Applications/ChatGPT.app/Contents/Info.plist` 标记版本 `26.1013.40934`，build `20770`。`Contents/Resources/app.asar` 内的 `webview/assets/` 包含界面样式、功能模块和交互标签。静态存在只能证明包内有相关实现；不能证明当前账户、平台或 feature gate 下实际可见、可用。
- **浏览器截图证据（W）**：[ChatGPT 网页参考](screenshots/01-chatgpt-web-reference.png)。截图实际显示 Chat/Work 切换入口、Codex 导航、项目/最近分组和居中输入区。它是网页版截图，不是桌面 App 截图；不能证明 Work、Codex 页面或其他弹窗的实际行为。
- **官方产品文档（O）**：[Pages](https://learn.chatgpt.com/docs/space/pages)、[Work with agents](https://learn.chatgpt.com/docs/space/agents)、[Collaboration](https://learn.chatgpt.com/docs/space/collaboration)、[Scheduled tasks](https://learn.chatgpt.com/docs/automations?surface=app)、[Long-running work](https://learn.chatgpt.com/docs/long-running-work)。Pages 说明块编辑与选区修订，协作另有共享权限。调度文档区分独立运行与回到原 chat，并说明本地运行需要机器及 App 在线；Goal 文档说明会话范围、进度操作和权限边界。文档不能证明当前账户已获得相应服务。
- **原生窗口观察（N）**：本机工具已观察到一个 Codex 会话窗口，其中显示应用栏、工作区下拉菜单、连接分组、会话输入区和摘要面板。尚未完成同尺寸、同状态的原生逐屏比对。
- **当前项目证据（L）**：[桌面前端](../src/gui_desktop.js)、[调度前端](../src/gui_scheduled.js)、[视觉样式](../src/gui_desktop.css)、[基础界面](../src/gui_app.html)、[Rust RPC](../src/appserver.rs)、[本地产品状态](../src/desktop.rs)、[会话存储](../src/session.rs)、[调度状态](../src/scheduler.rs)、[真实运行器](../src/scheduled_runtime.rs)、[Goals](../src/goals.rs)。源码存在与运行验收分别记录。

静态证据定位：`app-shared-dc7535e633d7.css`、`app-initial-e7c27dd40beb.css` 提供字体、颜色、间距和圆角 token；`app-initial-90eb42395b7d.js` 提供模式与 sidebar 标签；`thread-app-shell-chrome-448dfc36547b.js` 提供 tabs/panels 标签；`space-sidebar-2a096dadea6b.js` 提供 Work 内容层级；`composer-utility-bar-31c2288673e6.js` 提供执行位置标签；`command-messages-dce42665bef7.js` 提供命令标签。更新参考 App 后，应重新核实文件名和内容。

## 差异矩阵

### 2026-10-20 Codex 追加对齐（进行中）

本轮用户将范围明确为 **Codex**。参照仍为本机 `26.1013.40934`，不将已有 Chat/Work 功能算作 Codex 对齐完成。

- 外壳增加全局导航栏、前进/后退、居中首页、原始 Codex 图标、项目与执行位置栏、合并模型/思考/速度菜单。来源图标记录在 `assets/codex-mark.svg` 与 `assets/codex-icons.json`；运行时使用主题颜色。
- 侧栏增加连接/项目/单列表分组、聊天与置顶的独立排序、手动排序、折叠、展开、已读/未读和通知。已连接主机独立读取，失败保留对应主机缓存。自定义分区支持聊天及项目归类；删除分区保留其中内容，归属与折叠状态持久化。
- Codex 项目持久化目标主机的真实目录。创建和打开均验证目录，不创建缺失的源目录。移动聊天到项目按聊天所属主机读取项目。
- 新工作树的 `includeLocalChanges` 默认为关闭。打开后保留暂存区与工作区区别、未跟踪文件、删除、二进制、权限与 symlink，源目录和 index 字节保持不变。该选项要求 base 为当前 HEAD；仅额外复制 `.worktreeinclude` 选中的 ignored 普通文件及 ignored `AGENTS.override.md`。无法无损表达的 Git 状态会明确拒绝。失败回滚只清理本次创建的资源。
- 有运行中回复或 active heartbeat 的聊天在归档前呈现具体操作。取消不修改状态。归档失败会恢复本次暂停的跟进；归档成功后提供撤销。

**尚未达到完整 1:1**：原始桌面参照已捕获并与同尺寸浏览器首页比较；完整菜单、设置、会话内容、原生窗口和全部功能还需逐屏验收。`guardian-approvals` 已接独立模型执行器、官方默认风险策略、模型目录选择与专用授权上下文。普通命令保持原沙箱，失败不会自动提权；持久命令规则使用官方 Starlark 引擎。企业自定义策略和完整原生交互仍待验收。聊天在 checkout 与 worktree 之间的 handoff 也仍缺完整退出/重载与多窗口验收。下表的 pending 不因局部改动自动改为通过。

本轮追加验证：Rust headless 全量 215 passed、3 ignored；10 个相关 JS 脚本 111 passed；Desktop smoke 10 checks / 177 RPCs；Workspace smoke 49 checks / 47 RPCs。浏览器已操作真实 Rust 后端的流式回复、草稿、导航、已读状态、分区和目录项目；模型请求使用 loopback mock。原生窗口已打开并验证模型菜单和选择后的焦点。735 × 953 窄窗口无水平溢出。完整视觉 gate 仍为 blocked，详见 [验收记录](../design-qa.md)与[同尺寸主区域对照](screenshots/38-codex-parity-comparison.png)。

### 2026-10-21 运行恢复与审批补齐

运行环境已恢复。新增 checkout 共享执行锁和移交排他锁，覆盖新的目标工作树、已有工作树、恢复和语音。执行租约保留到最终状态写入结束。

“允许类似命令”将原请求提供的参数前缀保存到本应用 profile 的 `rules/default.rules`。使用固定公开版本的 Codex Starlark 解析、匹配与持久化，以及官方 Bash/PowerShell 解析和危险命令分类。显式拒绝优先，单次批准不保存规则，SSH 规则只写入目标主机。

自动审查使用同版本的官方默认策略、输出 contract 和 JSON schema。按离线模型目录选择隐藏 reviewer、模型 override 和实际支持的推理强度；API key 与 ChatGPT 的默认 reviewer 不同，缺少匹配项时保留父模型回退。模型输入去掉私有展示字段，审查记录单独标明人、自动继续、工具与继承授权的来源。

`thread/approveGuardianDeniedAction` 对非 denied 事件不写入授权。对 denied 事件仅注入该具体动作的 developer 上下文，不执行动作或新建 turn。运行中和取消后的上下文会在执行边界保存；分支保留历史内容，但不将继承授权当作该分支的直接授权。安装版 CLI 已独立验证接口的非 denied、精确上下文与无自动执行行为。

新增许可证及出处见 `src/vendor/CODEX-UPSTREAM.md`。公开源码不等同于安装版，交叉验证只覆盖探针中的行为。最新完整结果和桌面包状态见 [验收记录](../design-qa.md)。

最终验证：Rust headless 260 passed、0 failed、3 isolated workers ignored；前端 303 passed。Release App、DMG、PKG 已生成，签名、DMG 完整性、安装包二进制与 App 一致性及 SHA256 清单通过。最终包 Desktop 10 checks / 177 RPCs，Documents 10 groups / 48 RPCs；本轮 Release 候选的 Handoff 58 checks / 95 RPCs、拒绝恢复 13 checks / 20 RPCs 和原生审批历史/键盘操作通过。现有 5173 预览保留原进程及草稿；新代码由最新桌面包交付。完整 1:1 仍以 [验收记录](../design-qa.md)中的剩余项为准。

### 当前追加验收与候选包状态

前端完整回归 334 passed；最近完成的后端全量为 287 passed、0 failed、3 ignored。工作树复制的最新 debug 实测为 14 场景、66 checks、55 RPCs。11 个移交持久化阶段的独立进程退出恢复、真实归档失败和原生主/弹出窗口往返已验证，历史、Git 文件与草稿均保留。终端 shell 标题跟随 OSC 更新，操作固定标题不被覆盖；缺失的环境选择按参照回退。

最终 App、DMG、PKG 已更新，签名、DMG 完整性、PKG/App 二进制一致与 SHA256 清单通过。最后一轮全量出现 1 项终端关闭测试失败，同时新的 worker 和最新包启动复验停在 macOS 加载阶段，尚未完成；失败原因仍待取得诊断并修复。不能把此前候选包的运行结果当成最后制品通过。具体采样、已完成检查和剩余差异见[验收记录](../design-qa.md)。

### 2026-10-21 本地环境与工作树复制（本地流程已复验）

参照同版本安装包的 local-environment editor、settings、worktree dropdown 及 host worker。环境使用 `.codex/environments/*.toml`，按默认文件名、父目录距离和路径排序。设置页现按项目展示本地与继承配置，提供详情及编辑；新增配置依次使用 `environment.toml`、`environment-2.toml` 等空闲文件名。保存使用 SHA256 revision 比较，冲突保留草稿且要求重新加载。编辑现有配置不切换选中环境，保存不会执行设置脚本。

新工作树仅在明确选择配置时运行 Bash 设置脚本。配置映射遵循 tracked/untracked/ignored 和目标分支文件优先规则。平台脚本为空时回退通用脚本。设置失败保留工作树、输出和重试入口；停止会终止对应进程组。导出和取消导出的环境变量写入工作树 Git admin 目录，后续 shell 和顶部操作继承这些差异。聊天分支入口仍传空环境，不自动运行设置。

归档先保存清理前的可恢复快照，再运行所选环境的清理脚本。失败或取消保留 checkout；成功后另保留清理产物快照，再删除 checkout。恢复还原清理前的文件和 shell 环境。清理期间释放 registry 锁，状态和取消 RPC 可响应。创建本地工作树还按安装包的 Git 交集规则复制 `.worktreeinclude` 选中的 ignored 普通文件及 ignored `AGENTS.override.md`；不会覆盖目标文件，不经目标 symlink 目录复制。

前端全量 315 passed；环境 smoke 19 checks / 189 RPCs，fork smoke 35 checks / 34 RPCs。浏览器已操作保存、详情、新工作树设置和顶部操作，并核对真实文件及导出变量；500 × 900 下无页面水平溢出。[编辑](screenshots/53-codex-environment-editor.png)、[详情](screenshots/54-codex-environment-details.png)、[设置完成](screenshots/55-codex-environment-ready.png)及[操作结果](screenshots/56-codex-environment-action.png)已留证。首次完整后端回归暴露旧 checkout 未启用 worktreeConfig 时的归档兼容错误，以及通用创建入口错误阻止活动源聊天，均已修复。最终 Rust headless 272 passed、0 failed、3 ignored；包内 Environment 19 checks / 201 RPCs、Workspace 49 checks / 47 RPCs、Handoff 58 checks / 94 RPCs、Desktop 10 checks / 177 RPCs 通过，签名、DMG、PKG/App 一致性与 SHA256 清单通过。原生副本读取并编辑保存已有配置，磁盘与成功反馈一致。交互式 PTY、完整原生流程及同状态参照视觉仍为 pending。

### 2026-10-21 交互式终端追加

Codex 终端已从单次 shell 输出改为真实 PTY。渲染器固定为 xterm 6.0.0 / addon-fit 0.11.0，版本、integrity、可重复提取脚本及 MIT 许可证见 `src/vendor/terminal/` 与 `src/vendor/LICENSE-XTERM*.txt`。环境顶部操作在交互 shell 中运行，继承工作树保存的导出变量。终端按聊天、目录、连接隔离；页面重载可重新附加，后台输出继续接收。输入有序，重放不向 shell 回送历史设备响应，输出有背压与有界重放缓存。停止时先解除背压并释放 PTY master，再回收 shell，最后释放 checkout 租约。

后端全量 279 passed、0 failed、3 ignored。包内 Terminal 14 checks / 102 RPCs、Environment 19 checks / 188 RPCs、Workspace 49 checks / 47 RPCs、Handoff 58 checks / 94 RPCs 通过。浏览器和原生执行、中文、ANSI、重载恢复、独立 shell 状态及停止实际写入进程已留证，详见 [验收记录](../design-qa.md)。这些结果不代表完整参照视觉通过。

环境编辑器随后按参照补齐操作图标、空白行处理、缺项提示及原样脚本保存。文件格式包含同样的自动生成注释与多行字符串。图标来自安装包 `app-shared-52443cbe9ac4.js` 的 `gear-light-20`、`play-light-20`、`ladybug-light-20`、`flask-light-20`，渲染时使用主题颜色。前端全量 328 passed；5 组空白、引号及换行的 TOML roundtrip 通过，实屏保存也验证了文件内容与无执行副作用。最终 UI App、DMG、PKG 已更新；包内 Terminal 14 checks / 101 RPCs、Desktop 10 checks / 177 RPCs，以及签名、DMG、PKG/App 一致性与 SHA256 清单通过。

### Codex 分支执行位置追加实现

`thread/fork` 增加 `destination: current | newWorktree`。默认保留既有副本行为。新工作树分支采用当前 HEAD 的 detached checkout 和工作目录文件状态，保留子目录映射，不移动原目录。完整历史与有效 UI 快照写入成功后才登记聊天和工作树归属；失败会移除本次未变更的新资源。工作树有后续编辑、ignored 文件或另一归属时保留资源并返回具体路径。新分支不复制 Goal 或计划任务执行器，也不接走原草稿。

分支复制现直接遵循安装版 worker 的 Git 序列：复制 index（含 split index 的 shared index），在私有 index 中 `git add -u`，生成 binary diff，再在目标私有 index 中 `git apply --3way`。源 index 字节保持不变，目标真实 index 保持 HEAD。外部 attributes 与 filter 不参与；仓库 `.gitattributes` 仍生效。因此 CRLF、Git 可执行位和空目录按 Git 规则处理。未跟踪文件保留原始内容、权限和 symlink 目标。通用“新工作树并携带改动”保留其原有暂存区契约。

已用隔离仓库重放同版本源代码的 13 种 Git 序列，并通过真实 app-server 验证 14 种状态（66 checks / 55 RPCs）。覆盖 intent-to-add、assume-unchanged、skip-worktree、sparse index、未合并 index、split index、三种子模块状态、外部 attributes、换行/模式规范化，以及未跟踪的聊天子目录。源代码序列重放不等于原版桌面入口验收。嵌套仓库按参照规则处理：无 HEAD 时跳过；有 HEAD 时因 host 使用非递归复制而拒绝，源文件保持不变。错误文案、失败后的桌面流程及完整参照视觉仍待逐项核对。失败清理使用独立文件指纹；无法证明未变化时保留 checkout。

前端提供“分叉”子菜单、“在同一工作树中创建聊天分支”“在新工作树中创建聊天分支”与“在此工作树中新建聊天”。标题采用参照的来源族序号，归档来源也参与计算。浏览器和原生主流程已操作验证，见[浏览器菜单](screenshots/39-codex-fork-worktree.png)和[原生菜单](screenshots/40-native-codex-forks.png)。本轮分支 smoke 为 35 checks / 34 RPCs / 3 次 loopback 模型请求；11 个相关 JS 脚本 118 passed；Rust 全量 217 passed、3 ignored，另有 2 个清理边界用例通过。桌面包已重新构建。

### Handoff 参照契约与实现进度

本机 `thread-handoff-modal-5d73d64594f5.js`、`app-initial-90eb42395b7d.js` 与 `.vite/build/worker.js` 提供了以下直接证据。下面的完整契约尚未全部实现。

- 移到新/已有工作树时，参照执行 `move-thread-to-worktree`：创建或复用工作树，暂存源改动，切换本地分支，保存已有目标改动，检出工作树分支，应用源改动。目标不是默认分支；需要时用户选择本地 checkout 留下的分支。已有目标的改动不能被丢弃。
- 带回本地时，参照执行 `move-thread-to-local`：目标 checkout 必须干净。源工作树先暂存改动并 detach，目标检出对应分支，再应用改动。分支被其它工作树使用、同名分支指向不同提交、应用冲突和回滚失败均有专门反馈。
- Git 阶段完成后，参照 fork 到目标目录，再转移标题、置顶与界面状态，并归档原聊天。Git 操作完成后才出现的聊天创建或清理失败不能被冒充为完整成功。
- 正在运行的源回复有中断提示；直接移入已有工作树需要等待当前回复结束。系统中断先暂停活动 Goal。新聊天不继承 Goal；闲置的活动 Goal 与已暂停 Goal 保留在原聊天。归档后原聊天不再执行。host 的 `handleArchivedThreadSideEffects` 删除关联 heartbeat 定义，不把它们转到新聊天。官方 CLI 的隔离 fork/archive/unarchive 实验也验证了 Goal 的上述行为。

当前新增后端 `thread/handoff`，方向为 `toWorktree | toLocal`。它按聊天保存的目录执行，不接受调用方替换源目录。Git 阶段支持新建/复用工作树、源分支转移、本地分支切换、源改动暂存/应用，以及已有目标改动的独立 stash 保留。带回本地要求目标无 tracked/untracked 改动，并拒绝同名分支的提交不一致。应用使用参照的 `git stash apply`，没有 `--index`；它不恢复原有文件的暂存标记，但 Git 自身仍可能暂存新增文件。

Git 准备成功后，协调层复制历史与 UI 快照，保留标题、置顶、项目、搜索和推理强度，再登记目标归属并归档原聊天。与参照一致，Git 已移动但聊天复制失败时保留 Git 结果，由原聊天登记工作树归属，并返回警告；不再回滚已成功的 Git 移动。复制成功后的来源归档失败同样保留新聊天与 Git 结果并报告警告。警告写入持久迁移记录，超时查询可恢复同一结果。迁移记录写入 workspace registry；两侧原文件、权限、symlink、目录和原始 index 有 Git recovery refs，原暂存 blob 也由真实 tree 保持可达。Git 准备失败的回滚会核对文件/index/HEAD，新编辑或归属变化会阻止清理。`workspace/handoff/inspect` 返回待恢复记录；`workspace/handoff/rollback` 只接受同一 repository 的未完成操作，不会撤销已提交操作。

实际 stash 也保存独立 recovery ref，之后清理 stash 列表不会使其对象图失去保护。这保留了 Git 暂存期间捕获的完整数据。建立该保护失败时，回滚保留原 stash 项；错误会带迁移恢复记录 ID。

`continueExisting: true` 提供另一路径：把聊天接入选中的已有工作树，不移动任何分支、index 或工作文件；两侧有本地改动也会保留。该路径要求源回复先结束，不接受以中断方式继续。已有工作树的聊天目录也按来源子目录映射。

执行位置菜单现已接入“移动到工作树”“移动到主检出”。对话框支持新建工作树、选择已有工作树直接继续，以及带回本地。分支选择、运行中回复提示、本地 dirty 状态阻止、分步进度、失败/未知结果和成功状态均已有界面。`thread/handoff/inspect` 返回聊天资格与 Git 状态；`thread/handoff/status` 用请求 ID 查询实际协调任务或持久结果。状态查询不会返回完整文件清单。界面会保存来源主机，禁止超时后重复提交；迟到结果通过“打开”操作交回用户，不抢走新视图。

已接文字草稿、附件、文件引用、打开标签、置顶顺序和侧边栏分区的迁移。目标已有草稿时保留完整来源草稿，不混合附件。Inspector 会使用目标目录重新加载。已有文件图标和 420px compact 对话框宽度来自参照包；[原生确认界面](screenshots/41-native-codex-handoff.png)、[原生状态转移](screenshots/42-native-codex-handoff-state.png)、[既有工作树选择](screenshots/43-codex-handoff-existing.png)已留证。500 × 900 视口无水平溢出；Tab 焦点循环与 Escape 返回执行位置按钮已实测。

Goal 与 heartbeat 已按上述参照语义接入。移交使用跨进程来源锁；来源 Goal 修改/清除、heartbeat 创建/修改/删除/手动运行，以及文字和语音启动均受排他保护。活动 turn 的执行锁保留至最终快照和计数写入，移交会等待自己中断的回复完成收尾；另一窗口持有执行锁时拒绝移动。重启发现未完成 Git 记录时阻止来源续跑，需先查询和恢复该操作。成功归档保留 Goal 账本、删除 heartbeat 定义并保留 run 历史；恢复归档聊天后，原活动 Goal 才重新具备续跑资格。归档界面的“撤销”只还原聊天。语音调用和待发送队列仍阻止移交。

Handoff 追加验证：11 个持久化阶段在独立进程中直接退出，重开 registry 后均可恢复两侧原始文件、index 和分支。真实文件系统归档失败通过 7 checks / 9 RPCs，保留已完成 Git 移动和新聊天，并在重启后返回同一警告。原生开发候选已完成主窗口→新工作树→弹出窗口续聊→主窗口同步→带回本地，核对实际分支、文件、原聊天归档和草稿；见[原生往返](screenshots/66-native-handoff-roundtrip.png)。正常 Quit 退出码为 0，草稿保持。此候选只调整测试副本的 bundle 标识、本地签名与调试二进制。Git 命令执行中强制退出、页面重载和全部参照视觉状态仍未完整验收。checkout 级跨进程排他已在后续实现并回归。

继续回复的验收同时修复了标题和引用问题：已命名聊天不再被新输入覆盖标题。发送后会清理附件与引用草稿；工作区文件内容仅进入模型上下文，消息回放保留可预览的文件卡片。新记录不会把 `<workspace_file>` 展开正文显示为用户输入；历史旧记录保留原数据。

追加验证：Rust 全量 232 passed、3 ignored；随后引用呈现修复另跑 appserver 7 项通过。Handoff smoke 42 checks / 36 RPCs / 3 次 loopback 请求；Turn smoke 9 checks / 53 RPCs / 15 次 loopback 请求。前端 9 个相关脚本 93 项通过。完整范围与限制见 [验收记录](../design-qa.md)。

Goal/heartbeat 与部分失败语义修正后的追加验证：Rust 全量 233 passed、3 ignored；随后新增的 2 项 scheduler 边界测试随该模块复验通过（19 passed、1 ignored）。前端相关 9 个脚本 94 passed；另含语音与调度界面的 4 个脚本 65 passed。Handoff smoke 为 56 checks / 91 RPCs / 4 次 loopback 模型请求，覆盖真实跨进程锁、Goal 计数保留、归档后重启不续跑、显式取消归档后续跑、heartbeat 清理，以及 Git 成功/聊天复制失败后保留原聊天与持久警告。Automations 12 checks / 349 RPCs、Voice 9 groups / 74 RPCs、Turn 9 checks / 53 RPCs、Desktop 10 checks / 177 RPCs 通过，均使用隔离目录与本机 mock。浏览器实测[复制失败警告](screenshots/44-codex-handoff-warning.png)：原聊天、Goal 和文字草稿保留；警告对话框宽 420px，无水平溢出；技术原因可展开“详情”。桌面调试包已更新。

### Codex 自动审批追加实现

本机 CLI `0.163.0-alpha.10.2` 的 schema 确认了 reviewer、事件及状态；安装包的权限菜单确认了“请求批准”“帮我批准”“完全访问权限”标签。公开实现的 [reviewer 权限与工具策略](https://github.com/openai/codex/blob/main/codex-rs/ext/guardian-reviewer/src/settings.rs)和[输出协议](https://github.com/openai/codex/blob/main/codex-rs/ext/guardian-reviewer/src/assessment.rs)作为补充证据，不能替代同版本桌面的完整行为验收。

新增独立审批运行器。`guardian_subagent` 与 `auto_review` 共用此运行器，`user` 保留人工路径。审查绑定原工具参数和目录，以独立上下文调用当前所选模型。检查工具仅暴露只读、禁网的 shell；不会提供 patch、MCP、插件或升级权限。批准只放行本次动作，不修改原沙箱或 session approval cache。拒绝、非法输出与超时均不执行动作；中断会取消正在进行的请求并报告 aborted。JSON 格式错误最多尝试三次，完整审查限 90 秒，输入和输出有大小上限。实际 usage 进入 turn 与 Goal 计数，审查耗尽预算后不执行动作。

`thread/settings/update` 已支持权限字段，未支持的其它设置会明确报错。Reviewer、审批策略和沙箱随聊天保存，分支与重启保留；下一模型步骤读取最新设置，已开始的审查保留发起时的权限。界面接入三项权限选择、完全访问的产品确认，以及工具旁的审查进度和原因。新聊天读取所在主机的默认值，读取期间禁止输入；失败时不会沿用上一聊天的权限。[菜单](screenshots/45-codex-permissions.png)与[结果](screenshots/46-codex-approval-review.png)已留证。浏览器实测使用预先配置的隔离 profile 和本机 mock：审查前文件不存在，批准后实际创建，重新加载后仍显示审查记录。

完整 Auto-review 仍为 pending。普通沙箱执行、显式额外访问和路径保护已通过真实回归；当前使用固定版本官方默认策略，已接目录驱动的模型选择、持久命令规则及 `thread/approveGuardianDeniedAction` 精确授权上下文。原生 Release 已验证发送、拒绝、历史恢复和键盘展开详情。账户切换、企业自定义策略与全部原生失败情形尚未完整验收。非 macOS 主机尚缺受管只读沙箱，不会自动放行；mock 验证不代表真实模型的风险判定准确率。

新增审批 smoke：32 checks / 38 RPCs / 34 次 loopback 请求；新权限界面脚本 13 passed，相关 8 个脚本共 93 passed。Rust 全量 236 passed、3 ignored；后续 appserver 9、agent 6、session 9、API 19 项通过。API 不接受模型返回的 user、developer 或 system 消息。审批结果使用独立内部字段记录，不根据输出文字判断；该字段不进入模型上下文，拒绝的提案不计为 Goal 的实际工作。显式策略可覆盖启动默认值，权限自定义组合不会误标成自动审查。Turn、Automations 与 Handoff 回归也通过。桌面调试包已更新；完整证据与限制见 [验收记录](../design-qa.md)。

### 沙箱执行与批准时机追加修正

以同一本机官方 CLI 的独立配置、本机 Responses mock 和临时 Git 目录执行探针。模型输出通过 `response.output_item.added` 与 `done` 发送，命令确实进入官方执行链路。工作区普通写入没有审批；越界普通写入直接被沙箱拒绝，不会自动升级重试。显式 `require_escalated` 请求一次批准，批准前文件不存在，批准后写入成功。Never 策略拒绝显式升级，不弹审批。工作区内普通补丁直接应用；只读或越界补丁申请批准。文件的 `acceptForSession` 只覆盖该路径，另一文件和 shell 权限仍需独立处理。命令批准的 `cancel` 会中断 turn，模型不再收到下一次请求。普通命令不能写入工作区内的 `.git`、`.codex` 或 `.agents`。

实现已据此区分普通执行与额外访问。批准请求携带实际工作目录、理由与补丁路径；额外命令支持单次授予，以及保存原请求提供的 execpolicy 前缀。拒绝发送 `decline`，`cancel` 协议动作则中断整轮。文件授权按解析后的路径缓存，等待批准期间路径改变会拒绝应用。补丁目标的不存在后缀会先处理 `..`，避免误判为工作区内。Stop 和超时会终止本次命令的进程组，超时报告失败；输出有大小上限。持久命令规则已在后续补齐并与安装版 CLI 交叉验证；完整权限系统仍按未验收项目保留 pending。

权限 smoke 在最后两处路径保护修复前为 17 checks / 19 RPCs / 24 次本机模型请求；审批 smoke 为 33 checks / 40 RPCs / 35 次本机请求，Turn 与 Automations 回归通过。随后增加元数据保护与缺失父目录越界用例。当前新测试进程停在系统加载阶段，未进入 libtest，最新回归未完成。相同元数据保护表达式已用系统 `sandbox-exec` 单独确认拒绝写入，但不能替代 Rust 用例。桌面包仍保留上一轮已验证版本。详见 [验收记录](../design-qa.md)。

追加核对安装包 `pending-request-item-panel-a6ee45e20086.js` 与公共审批卡片：参照的“拒绝”发送 `decline`，不会因 CLI 的可选值含 `cancel` 就显示为取消；命令理由直接作为标题。Codex 卡片已修正这一区别，采用“终端”/“编辑文件”标识、灰阶样式，以及“允许一次”旁的文件授权下拉。下拉的“允许所有修改”采用参照文字；后端授权范围仍以上述真实 CLI 探针为准。命令规则尚未实现时不展示“允许类似命令”。

现有权限测试与新增审批卡片测试通过临时适配器在系统 JavaScriptCore 的独立 Realm 中运行，23 项通过；这不等于 Node 完整测试套件通过。隔离浏览器页装载生产渲染器和 `renderPendingApprovals`，实际点击验证 `acceptForSession`、`decline`、`accept`，并检查菜单键盘移动、Escape 返回和待审批记录清理。[范围菜单](screenshots/47-codex-approval-options.jpg)与[窄窗口](screenshots/48-codex-approval-narrow.jpg)已留证。500px 视口的 `scrollWidth` 为 500，菜单在边界内；控制台无 warning/error。夹具不执行 Rust 命令，不能替代最新后端回归或原生端到端验收。

`pending` 表示该项对齐验收尚未完成。实现了本地基础功能，不等于完成整个参考能力。

| 范围 | 参考依据 | 当前已落地与差异 | 对齐验收条件 | 状态 |
|---|---|---|---|---|
| 三模式与视觉外壳 | S：Chat/Work/Codex；W：模式入口和基础布局 | 三模式导航、应用栏、工作区下拉菜单、独立会话 mode、sidebar、主题、输入区、标签和侧栏已实现。采用部分静态 token，包括 4px 间距、46px toolbar、系统字体和灰阶；尚无完整桌面截图比对 | 以同版本桌面 App 为参考，在相同窗口、主题、缩放和状态下逐屏比较；覆盖首页、会话、菜单、设置、窄窗口、hover/focus、键盘操作和动画 | pending |
| 会话生命周期 | S：新建、搜索、置顶、重命名、归档、删除、分支、未读 | 本地 session lifecycle、metadata、项目归属、历史恢复和多标签已实现；运行中任务与导航状态已有修复。删除本地 chat 会清除其 Goal，并暂停关联 heartbeat，保留定义和已有 run。内部 Goal 续跑指令在历史与通知中标记 internal，模型上下文不携带私有标记。跨设备历史和官方历史结构仍有差异 | 新建→发送→停止→恢复→分支→归档→还原→删除形成完整流程；并发任务、重复提示编辑、切换草稿和重启均无串会话或数据丢失 | 本地关联清理和 internal 标记已验证；完整对齐 pending |
| Work Pages、canvas 与 Library | O：Pages 块编辑和选区审阅；S：内容模块；W：资料库入口 | 已有本地结构化富文本、选区工具栏、slash 插入、块把手、列表/表格/附件/LaTeX、查找替换、本地评论和修订、CAS 自动保存与原 Markdown 保留。真实 Responses 选区建议需手动审阅并通过 CAS 应用。Page/canvas 共用编辑器，canvas 仅为本项目旁栏布局。Markdown/JSON/Word 可导出，系统打印支持存为 PDF；公式在 Word 中为 LaTeX，文件块仅导出名称。本轮增加本地子页面/移动、Pages/Chats 引用、Prompt/Task/Instructions/Highlight、Generate/Visualize、评论请求和显式内联图片生成；云协作不在本次范围。合同见 [DOCUMENTS.md](DOCUMENTS.md) | 验证同版本参考的编辑/保存/关闭、原文与 Undo、选区/评论/修订和多窗口冲突；验证真实账户 AI、导出和全部内容类型；完成浏览器及原生逐屏交互 | 本地存储与 mock AI 主链已验证；完整对齐 pending |
| 文件、图片与生成 | S：文件/图片输入及媒体模块；W：图像入口；O：[Images API](https://developers.openai.com/api/docs/guides/image-generation) | 已有持久附件、Files Library、原件下载与提取文本预览；支持 UTF-8/UTF-16 文本、CSV/JSON、PDF 文本、DOCX、PPTX/notes、XLSX 值/公式缓存及 PNG/JPEG/WebP。消息按 id 取完整内容，历史正文与附件卡片分别保存。Images API 已接生成/编辑/mask、模型参数限制、持久任务、取消等待、手动重试和结果继续编辑。需要 API key；OAuth 不提供此通路。未做 OCR、公式重算、完整 Office/PDF 版式或音视频生成 | 上传、生成、编辑、预览、原件导出和重启形成完整流程；验证真实 provider、权限拒绝、失败恢复及参考视觉 | 本机 mock 与未配置实屏通过；完整对齐 pending |
| 模型目录与选择 | O：[Models API](https://developers.openai.com/api/reference/resources/models/methods/list)、官方 Codex models client；L：[目录合同](MODELS.md) | API key 与 ChatGPT 连接可刷新真实目录，按账户隔离缓存，失败保留同账户 stale 目录。服务未报告的 Responses/推理能力显示 unknown。只为报告的级别提供能力选项；Auto 省略 reasoning 参数。配置模型和手动设置保留。尚未覆盖订阅模型路由、全部 reasoning mode 和官方个性化推荐 | 验证真实账户目录、过期凭据、选择/刷新竞争、全部参数和菜单状态；逐屏对齐 | 本机 HTTP/RPC 与选择竞争验证通过；完整对齐 pending |
| 桌面登录与凭据 | O：[Authentication](https://learn.chatgpt.com/docs/auth)；L：auth/account | 已有桌面 API key 表单、验证后连接、OAuth PKCE 浏览器登录/取消/超时、退出、凭据来源展示和环境凭据保护。私有原子文件和刷新竞争处理已实现。真实账户登录、组织切换、订阅与账户服务仍待验收 | 本机 mock 覆盖错误、取消、刷新和换账号竞争；真实账户验证完整登录、退出、过期恢复和权限结果，且原有会话取最新连接 | pending |
| 云同步、分享与协作（本次排除） | S：Space、分享、协作者相关模块 | `~/.mcodex/` 保存本地状态；无云同步、公开分享或多人协同服务，capabilities 明确报告 unavailable | 两台设备同步同一账户；分享权限、撤销访问、冲突处理和多人编辑均有真实服务和端到端验证 | pending |
| 实时语音 | S：Voice、全局 dictation、Mini 语音命令；O：[Voice](https://learn.chatgpt.com/docs/features/voice)、[WebRTC API](https://developers.openai.com/api/docs/guides/voice-webrtc?voice-api=realtime) | 已有双向 WebRTC 媒体/数据通道与 Rust 信令、静音/打断、输入及输出转写、结束清理、同 chat 稳定转写持久化和听写到原草稿。API key 保留在 Rust；当前语音连接需要 API key，ChatGPT OAuth 不支持此 API 通路。模拟验证不等于真实设备和 provider 验收；本轮已加入全局听写和 Mini 入口；Codex 实时工具交接仍与官方语音服务有差异 | 完成设备授权、录音/静音、转写、双向语音、中断、断线恢复与结束会话，并验证桌面快捷入口 | pending |
| 连接器与插件生态 | S：Apps、Plugins、MCP 和权限设置模块 | 已有本地 MCP 配置/工具调用和部分插件发现。未覆盖官方连接器目录、OAuth、账户权限、组织策略及连接器专用产品流程 | 对选定连接器逐个验证授权、读取、写入批准、撤销、过期恢复和错误反馈；目录及设置与参考范围一致 | pending |
| Codex 工作环境与 Git | S：Local/Remote/Cloud/Worktree、分支、PR、Changes、Terminal | 已有目录、Git 状态/diff、分支、命令与 managed worktree 的创建/归档/恢复；快照保留 HEAD/index、字节、权限、symlink 和未跟踪文件。SSH 已接主机配置、指纹核对、Rust agent、同一输入区/文件/模型/批准转发、断线与重连。远端自持账户，连接关闭后 agent 停止；克隆状态导致的 ID 冲突会拒绝操作。cloud 和完整 commit/push/PR/review/handoff 仍缺 | 真实仓库与 SSH 主机验证工作区、账号隔离、批准、断线和恢复；继续验证 PR/cloud/handoff，并逐屏比对 | 本机隔离执行与 transport 已验证；完整对齐 pending |
| Scheduled tasks | S、O：独立任务、原 chat 跟进、历史和通知；W：定时任务入口 | 已有真实本地触发、RRULE/IANA 时区、cron/heartbeat、多项目结果聚合、调度 worktree、Run now、暂停/恢复、取消、通知与运行审阅历史。DST 缺失时刻跳过且不消耗 COUNT；重复时刻仅运行首次。任务计划耗尽与实际 run 成败分开。机器和 App/app-server 必须保持运行；无云端托管或连接器事件触发 | 已验证未来 DTSTART 自动触发、暂停任务手动运行、heartbeat 上下文、hasUpdate/失败通知、真实 worktree 与重启历史；继续验收多项目部分失败、长期运行、原生通知及参考界面逐屏交互 | 本地核心已实现并验证；完整对齐 pending |
| Goals | S、O：会话目标、进度行、暂停/恢复/编辑/清除 | 已有持久目标、自动续跑、真实 provider token usage、可选 token budget、人工消息优先队列、暂停/恢复/编辑/替换/清除。完成要求 evidence 非空且 remaining 为空；无实际工作工具调用会抑制续跑。相同 blocker 连续三次才 blocked；usageLimited、budgetLimited 不伪装完成，普通失败先退避。暂停/替换会停止旧 turn，编辑保留计数，重启恢复保留已授权目标 | 已验证两轮同 chat 完成、真实 usage、预算停止、无工具抑制、人工队列优先、非法编辑不改变运行目标、慢流/批准等待时取消，以及重启计数；继续验收长时间真实模型目标、批准需求和完整原生 progress row | 本地核心已实现并验证；完整对齐 pending |
| Markdown、数学与消息内容 | S：Markdown/KaTeX 和多种内容模块 | 已接入离线 markdown-it、KaTeX 和 highlight.js，覆盖列表、引用、GFM 表格/任务框/删除线/链接、行内/块公式、MathML 和 36 种语言代码高亮。字体随应用打包。脚注、Mermaid、可执行单元及官方专用内容块仍缺 | 用固定语料比较嵌套列表、表格、转义、代码、链接、公式和流式半成品；验证安全渲染、复制与导出 | pending |
| 原生快捷入口与 OS 集成 | S：Popout、Mini、全局快捷键、截图/appshot、系统能力命令 | 已有真实 App/File/Edit/View/Window/Help 菜单、WebKit 原生编辑、工作区/Files/设置等快捷键、Hide/Minimize/Zoom。CmdQ/CmdW/Dock Quit 共用保存握手；失败留窗并恢复焦点。原生页面使用私密 asset-only loopback origin，安全上下文和麦克风 API 已在本机确认；打包含麦克风用途说明。本轮已加入 Popout/Mini、全局热键和系统截图附件。系统通知中心和授权 App 内容快照仍与官方不同 | 原生安装包上验证系统快捷入口、焦点恢复、窗口生命周期、系统授权、通知和辅助功能；覆盖重启及权限拒绝 | pending |
| Rust 状态与会话基础 | L：desktop/session/appserver | 本地原子状态写入、项目/文档解除关联、并发 session ID、归档与分支持久化已有测试；活动 runner 在 resume 时保留 | 独立数据目录测试全部通过；RPC 流程覆盖并发、错误输入、重启和删除后不复活 | 本轮基础验证通过；完整产品验收仍 pending |
| Rust 流式与本地安全修复 | L：[API](../src/api.rs)、[patch](../src/patch.rs)、[preview](../src/preview.rs)、agent/appserver | SSE 按完整事件解码 UTF-8；支持拆分边界和 CRLF，处理错误/提前结束/超时。Preview 限 loopback 并校验 Host/Origin/请求边界；patch 校验路径与符号链接；Chat mode 不提供本地执行工具 | 保持单元测试通过；补充真实 turn 的增量、停止、批准、错误和继续执行验收；用浏览器验证前后端完整链路 | 单元及本机 mock turn 验证通过；真实账户和完整产品验收 pending |

## 2026-10-20 实际发送修复

最新用户会话记录显示，发送在 TLS 握手阶段失败。该 Mac 配置了系统 HTTPS 代理，App 的 reqwest 在关闭默认 feature 后同时失去了系统代理支持，因而直接连接失败。现已显式启用 `system-proxy`，保留 TLS 证书校验，不改变系统网络设置。经系统代理的无凭据请求可完成握手并收到正常的 401。

沿用用户当前 GPT-6 Luna、xhigh、priority 配置，在没有代理环境变量的隔离会话中完成真实发送并收到 `OK`。随后用最终打包的 App 二进制提供同一前端，在默认自动搜索设置下实际输入、点击发送、接收完整 `OK` 并检查落盘，未出现前端错误；[发送实屏](screenshots/32-live-send-fixed.png)。同时在 API 请求构建处统一 service_tier：auto/default 不发送，fast 转成 priority，防止旧调用路径再次发送不支持的 auto。207 项 Rust 测试通过；发送 smoke 45 RPC、模型 smoke 20 RPC 通过。

## 2026-10-20 模型与配置修正

直接使用本机 Codex App `26.1013.40934` 自带的 `0.163.0-alpha.10.2` app-server 作为参考，Mini 的 9 条模型元数据与之逐项一致，普通菜单显示同顺序的 7 个模型。输入区、设置页和定时任务按模型提供强度，Sol/Astra/Terra 支持 Ultra，Luna 到 Max；不再把人工 Auto 混入已知模型的档位。

Fast 写入并发送 `priority`，Standard 清除覆盖；模型、强度和速度随单次消息一起捕获。配置变更串行保存，失败和跨主机迟到响应不会改错当前选择。仅只读继承 Codex 的 model/model_reasoning_effort/service_tier，不导入权限或 MCP；本地覆盖可恢复为 Codex 默认值。本机实际默认值为 GPT‑6.1 Sol、xhigh、Fast。

本轮回归：207 项 Rust、206 项前端通过；模型 smoke 20 RPC、turn smoke 45 RPC 通过，包含 Ultra/Fast 实际到达本机 Responses 请求、Standard 清除 priority。模型元数据对比使用真实只读目录，不发送付费推理。浏览器已验证切换、刷新及默认值恢复。完整合同见 [MODELS.md](MODELS.md)，[强度菜单](screenshots/30-codex-reasoning-parity.png)、[设置页](screenshots/31-codex-settings-parity.png)。

## 本轮本地交付

- Mini、Popout 和主窗口共用 Rust 运行器；窗口独立草稿、附件、置顶、全局快捷键注册状态与冲突反馈、系统截图完成/取消、关闭隐藏和退出保存均已实际验收。用户另确认跨 App 全局 Mini 正常。
- 本地 Work 提供 schema v2 内容块、页面层级与移动、Page/Chat 引用、Prompt/Task/Agent Instructions/Highlight、Generate/Visualize 的显式审阅、评论请求及生成图片手动插入。原 Markdown 和 v1 历史继续可读，父页面删除保留子页面。
- 原生 WebKit 完成中文/emoji 输入、Undo/Redo、重启恢复、9→18 可视化交互和 IPC 越权拒绝。原生系统 PDF 完成真实保存与独立内容/排版检查；实际修复提前清理快照导致输出应用弹窗的问题。
- 实际阻断隔离状态目录后 Quit 被取消，文档、草稿和焦点保留；恢复后保存并正常退出。Mini→主窗口的 PNG 和文字交接、首次 Popout 的界面就绪竞争均已实测。
- Rust 205 项通过、3 个辅助入口；前端 201 项和 vendor 11 项通过。13 套 smoke 全通过，共 1264 次根服务 RPC。默认 App 和 headless agent 构建通过，全部账户及模型/图片请求为隔离本机 fixture。
- 记录与截图：[原生验收](NATIVE_LOCAL.md)、[Work](screenshots/27-native-work-local.png)、[PDF](screenshots/28-native-document-pdf.png)、[Mini](screenshots/29-native-mini.png)。以下更早记录为当时状态，出现“仍缺 Mini/Work 块/原生不可用”等字样时，以本节为准。

## 2026-10-20 续作与验收

- 已收尾中断的网页搜索：模式、策略、活动、URL 引用、完整来源、恢复和私有 UI 记录隔离。18 组搜索 smoke、174 次 RPC、42 次本机 Responses 请求通过；浏览器验证来源面板、禁用设置、重开和 760px 窄窗口无横向溢出。合同见 [SEARCH.md](SEARCH.md)，[实屏](screenshots/23-web-search.png)。
- 新增 Rust Word 导出、真实前端下载与系统打印/PDF 入口。文档 smoke 为 8 组、39 次 RPC，包含真实 Word 重新导入和 macOS 独立读取。浏览器下载已由 python-docx 打开；40 段中文及公式打印成 2 页 PDF，首尾内容和实屏均已检查。系统打印通过原生菜单/IPC 接入；本轮续作已完成原生打印面板及保存结果检查（见本地交付）。
- 202 项 Rust 测试通过，另有 3 个 helper 入口；192 项前端回归通过。13 套集成 smoke 全部通过，合计 1261 次根服务 RPC。所有模型和信令请求均使用本机 mock，不代表真实账户权限或设备验收。
- 100 个重叠新建请求的诊断全部完成，最慢约 10.2 秒。该场景串行持久写入，超过原先普通 RPC 的 10 秒测试时限；批量场景改用产品已有的 30 秒预算后通过。未削弱数据完整性、重启、删除和原子写入断言。
- 默认桌面及 headless 构建通过，已重新生成 App 和 agent 归档。App 的 ad-hoc 签名与 plist 检查通过。原生应用控制在本轮不可用；没有把浏览器结果计为原生验收。

## 此前验证记录（历史快照）

- **本轮新增：Pages 文档 smoke 7 组、30 次 RPC。** [smoke-documents.py](../scripts/smoke-documents.py) 使用真实隔离 Rust app-server，覆盖原 Markdown、富正文升级、双进程 CAS、UTF-16 评论 anchor/orphan、修订恢复、完整 JSON 导出、权限、全文检索、重启和删除；不调用模型。
- **本轮新增：选区 AI smoke 12 组。** [smoke-document-ai.py](../scripts/smoke-document-ai.py) 最近一次为 95 次 RPC、14 次本机 Responses HTTP 和 3 次 key 验证；RPC 数随取消后的短轮询变化。覆盖真实文档/附件、最终输出和 usage、不自动写文档、手动 CAS 采纳与过期冲突、取消/输出限额、工具拒绝、换号/退出、双实例 lease、EOF/崩溃恢复及有效 OAuth fixture。没有调用真实付费 API，不能代替权限、编辑质量或实屏验收。生产前端回归入口为 [保存与竞争](../scripts/test-document-persistence.mjs)、[编辑工具](../scripts/test-document-tools.mjs)。
- **已通过：177 项 Rust 全量测试。** 另有 3 个标记为 ignored 的辅助测试入口，由跨进程测试实际调用。覆盖流式与 HTTP 边界、本地状态、session、patch、RRULE/时区/DST、Goals、真实运行器、账户竞争、worktree、文件解析、语音、模型目录、Images 生成/编辑及 SSH 转发/主机指纹/关闭竞争。测试通过只覆盖对应断言，不代表所有产品能力已完成。
- **已通过：171 次 RPC smoke。** [smoke-desktop.py](../scripts/smoke-desktop.py) 使用真实 Rust app-server 和独立临时目录，覆盖三模式、CRUD、metadata、100 个重叠 session 创建请求、分支及两次重启后的状态。该脚本不调用模型，也不验证视觉或在线服务。
- **已通过：8 组 turn smoke、40 次 RPC、11 次本机 mock 请求。** [smoke-turns.py](../scripts/smoke-turns.py) 验证三模式工具差异、图片输入、项目/自定义 instructions、思考强度与 service tier、中文流、失败状态、实时会话恢复、批准列表、中断与拒绝后的继续执行，以及 Chat 返回未声明工具时不执行、不保存工具历史。它不调用外部模型，不替代真实账户和在线服务验收。
- **已通过：12 项 automations smoke、343 次 RPC、21 次本机 mock 请求。** [smoke-automations.py](../scripts/smoke-automations.py) 使用真实 Rust app-server、批准后的真实 shell 和临时 Git worktree。覆盖 Goal 自动续跑、真实 token 累加、预算、队列优先、暂停/编辑/替换/清除、无工具抑制、内部消息标记、删除 chat 后的关联清理，以及 cron/heartbeat、未来 DTSTART、API 失败、COUNT 耗尽、hasUpdate/通知和重启审阅状态。所有数据与凭据均为隔离测试数据；不调用外部模型。
- **已通过：5 项前端状态回归。** [test-desktop-state.mjs](../scripts/test-desktop-state.mjs) 执行生产函数，覆盖独立草稿、导航逆序、发送时切换视图、文档保存中的再编辑，以及内部 Goal 提示不显示用户气泡。
- **已通过：6 项安全渲染回归。** [test-rendering.mjs](../scripts/test-rendering.mjs) 验证外链协议、带引号路径、菜单、设置和文件卡片的参数传递，不把动态参数写入可执行事件代码。
- **已通过：10 项调度前端回归。** [test-scheduled-ui.mjs](../scripts/test-scheduled-ui.mjs) 覆盖调度表单、列表/详情、Goal 进度与编辑、人工 follow-up 排队、视图切换和耗尽计划的重新编辑。它们验证生产 JavaScript 的状态与 RPC 行为，不代替浏览器或原生窗口操作。
- **已通过：9 项 Markdown 回归。** [test-markdown.mjs](../scripts/test-markdown.mjs) 覆盖列表、GFM、公式、代码、高风险输入和逐字符流式前缀；浏览器已实际验证字体、公式和代码显示。
- **已通过：9 项连接/工作区前端回归。** [test-connection-workspaces.mjs](../scripts/test-connection-workspaces.mjs) 验证连接表单、环境凭据、OAuth 取消、导航竞争、保存等待及归档参数。
- **已通过：44 项工作区 smoke、45 次 RPC、2 次本机 mock。** [smoke-workspaces.py](../scripts/smoke-workspaces.py) 使用真实临时 Git 仓库验证创建、切分支、归档、重启恢复、index/原始字节/权限/符号链接、ignored 策略、活动任务保护和错误拒绝无损。
- **已通过：6 组账户 smoke、43 次 RPC。** [smoke-account.py](../scripts/smoke-account.py) 使用 2 次本机 key 验证、3 次本机 Responses 请求，验证换 key、退出时停止 Goal、取消 OAuth、只读导入、跨重启退出状态和环境凭据保护；未访问真实账户。
- **已通过：6 项附件前端回归。** [test-files-ui.mjs](../scripts/test-files-ui.mjs) 覆盖草稿恢复、导入中的导航、Library 导入不附加到聊天、关闭等待保存、图片重试去重及不可读附件拒发。
- **已通过：30 项语音前端回归。** [test-voice-ui.mjs](../scripts/test-voice-ui.mjs) 使用 fake media/peer 验证通路、静音/打断、乱序转写、稳定 prefix、原草稿、导航/结束清理、失败保存取消关闭、明确舍弃及真实模态焦点合同；不使用录音设备。
- **已通过：6 组文件 smoke、83 次 RPC、3 次本机 model mock。** [smoke-files.py](../scripts/smoke-files.py) 验证 11 种真实格式/编码 fixture、原件与全文、权限、模型内容及私有字段边界、fork/运行中恢复/重启、无文本拒发及删除。浏览器也实际导入 CSV/DOCX，DOCX 下载与原件逐字节一致。
- **已通过：9 组语音 smoke、74 次 RPC。** [smoke-voice.py](../scripts/smoke-voice.py) 使用 6 次本机 signaling/hangup、1 次转写和 2 次 Responses mock，验证会话绑定、prefix 幂等、线程保护、晚到清理、重启模型上下文、换 key 与退出。未调用外部 provider 或真实录音。
- **已通过：模型目录 4 组、17 次 RPC、10 次本机 HTTP 请求。** [smoke-models.py](../scripts/smoke-models.py) 覆盖 API/Codex 目录、缓存、未知能力、错误/大响应、换 key、刷新中与排队中的退出及配置保留；模型选择另有 7 项生产前端回归。
- **已通过：Images 9 组、73 次 RPC、9 次本机图片请求。** [smoke-images.py](../scripts/smoke-images.py) 使用真实附件存储，验证生成、原件字节/权限、multipart 编辑/mask、取消、换 key、失败与正常/崩溃重启。另有 12 项前端交互回归。未调用真实图片服务。
- **已通过：远程 transport 8 组、48 次 RPC。** [smoke-remote.py](../scripts/smoke-remote.py) 用 fake SSH 连接 3 个真实隔离 Rust 子进程，执行命令、附件读写、独立账户请求、断线和重连。额外临时 loopback sshd 验证 App 的已信任主机文件被真实 SSH 接受；未连接真实远程主机，也未声称完成远程登录验收。
- **已通过：163 项前端全量回归。** 包含本轮 20 项文档保存/异步竞争回归和 11 项真实 DOM 编辑事务回归。 包含上述状态/渲染/语音/调度等测试，以及 7 项模型选择、12 项 Images、19 项 SSH、14 项执行上下文、4 项动态菜单位置和 1 项 Rust 嵌入 JavaScript 整体解析。跨主机测试覆盖相同路径、迟到列表/预览/项目创建、旧终端/设置动作、附件归属、克隆资源 ID 冲突及固定连接的请求。
- **此前已通过：10 套集成 smoke，共 937 次根服务 RPC。** 本轮另增文档与选区 AI 两套 smoke，结果见本节首行；desktop 的 171 次 RPC 也已重跑。 全部使用隔离状态、真实 Rust app-server、本机 HTTP mock 与临时仓库。远程部分使用 fake SSH 承载真实第二个 Rust 进程；该数值不代表真实外部服务验收。
- **已通过：默认桌面与 headless 构建及归档检查。** 原生 App 的 ad-hoc 签名与 plist 检查通过。headless 归档为真实 `x86_64-apple-darwin / debug`，binary/source/archive SHA256 已核对；无 Linux 二进制或已公证 release 的声明。
- **已通过：本机原生交互。** 隔离 home 下验证 Undo/Redo、模式与 Files 快捷入口、Minimize/Hide/Zoom 和恢复；真实保存失败时 CmdQ 保持文档/草稿与焦点，恢复后 CmdW 成功退出且落盘内容准确。修复 Tao 0.31 无 keyWindow 时 Command-keyUp 崩溃。App 的 ad-hoc 签名与 plist 检查通过。
- **此前已通过：Images/SSH 原生入口与保存退出。** 相同二进制的独立测试 bundle 验证 Images/SSH 设置、未连接状态和 CmdW 退出码 0，未发送草稿已落盘，测试副本已清理。工具曾另外启动默认环境实例，已停止且未继续操作；缺少该实例网络日志，不能据此排除它曾外联。最终菜单异步锚点问题另经浏览器实测修复，当前页面无新 console error。
- **此前富文本验收的历史状态。** 本轮已补原生 WebKit 中文/emoji、Undo/Redo、可视化、打印和保存退出；真实 IME 组合输入及像素级逐屏比较不作无证据声明。
- **桌面视觉与交互验收：pending。** 浏览器已实际验证 Chat/Work/Codex 首页、中文文档编辑/预览、Library 和重启后的文档恢复。截图见下表。同版本桌面逐屏参考、真实语音设备/provider 和其余系统能力验收仍未完成。

维护时以实际源码、测试输出和截图更新对应行。只有验收条件有结果证据后才关闭 `pending`；不要用功能数量、完成分数或“全功能”概述替代具体记录。


## 浏览器验证截图

以下截图来自当前 Rust preview。它们验证实现页面可显示，不代表与桌面参考逐像素一致。

| 步骤 | 检查内容 | 结果与证据 |
|---|---|---|
| 1 | Chat 首页与输入区 | 可用；[截图](screenshots/02-chat-home.png) |
| 2 | Work 首页与文档入口 | 可用；[截图](screenshots/03-work-home.png) |
| 3 | 中文编辑、预览、保存后重开、Library/Canvas | 可用；[截图](screenshots/04-work-library-canvas.png) |
| 4 | Codex 首页、工作目录与工具入口 | 可显示；完整 Git/终端工作流仍待验收；[截图](screenshots/05-codex-home.png) |
| 5 | 浅色/深色主题、设置页 | 可切换并保存；[截图](screenshots/06-settings-dark.png) |
| 6 | Cmd+K 搜索 | 浏览器中可打开搜索框并查到保存的文档 |
| 7 | Scheduled 列表、筛选、任务详情与 Previous runs | 暂停任务及空运行历史可显示；[截图](screenshots/07-scheduled.png)；真实触发由上述 RPC smoke 验证 |
| 8 | Goals 列表与侧栏编辑器 | 暂停目标、可选 token budget、Revert/Save 可显示；[截图](screenshots/08-goal-editor.png)；实际续跑、usage 和取消由上述 smoke 验证 |
| 9 | 应用栏、工作区菜单、Chat 首页 | 浏览器可切换；[截图](screenshots/09-desktop-shell.png) |
| 10 | 桌面登录与 API key 表单 | 未输入真实凭据；[截图](screenshots/10-account.png) |
| 11 | 列表、表格、公式、Rust 高亮 | 实际预览可见，数学字体已加载，控制台无错误；[截图](screenshots/11-markdown-math.png) |
| 12 | 工作区与分支管理 | 实际读取当前仓库；[截图](screenshots/12-worktrees.png)；创建/归档/恢复由隔离真实仓库 smoke 验证 |
| 13 | 735px 窄窗口与关闭侧栏 | 内容无横向溢出，焦点回到侧栏入口，关闭面板从可访问树隐藏；[截图](screenshots/13-narrow-codex.png) |
| 14 | 持久文件库与 DOCX 预览/原件 | 实际导入，原件下载与 591-byte fixture 一致；[截图](screenshots/14-files-docx.png) |
| 15 | 语音未连接状态与模态焦点 | 正确提示 API key 需求；焦点在 Connect，背景从可访问树移除，关闭后还原；未申请麦克风；[截图](screenshots/15-voice-setup.png) |
| 16 | 附件与文字草稿恢复 | Work→Chat→Work 及 reload 后准确恢复；未发送模型；[截图](screenshots/16-file-draft.png) |
| 17 | 模型能力菜单 | 未报告的能力标为未知；配置强度与 Auto 分开，菜单已在视口内；[截图](screenshots/17-model-catalog.jpg) |
| 18 | 图片创建/编辑 | 实际未配置状态、禁用提交和模型参数联动；未调用真实图片服务；[截图](screenshots/18-image-editor.jpg) |
| 19 | SSH 设置 | 实际未配置状态与新增主机入口，未连接真实主机；[截图](screenshots/19-ssh-connections.jpg) |
| 20 | 原生 SSH 设置 | 隔离空库 native App，同字节二进制；[截图](screenshots/20-native-connections.png) |
| 21 | 结构化 Page、全宽表格、选区格式、中文正文和保存状态 | 浏览器实屏已验证；[截图](screenshots/21-work-page.jpg) |
| 22 | 同一文档切换聊天旁栏、保留正文、恢复聊天视图与草稿 | 浏览器实屏已验证；[截图](screenshots/22-work-canvas.jpg) |
| 23 | 网页搜索活动、中文回答、引用和完整来源 | 本机 mock 实屏通过；[截图](screenshots/23-web-search.png) |
| 24 | Word 下载、JSON/Markdown 与打印入口 | 实际 Word 下载及独立读取通过；[截图](screenshots/24-document-export.png) |
| 25 | 长正文打印排版、公式和中文 | 2 页 PDF 已检查，图为首屏；[截图](screenshots/25-document-pdf.png) |

仍需检查：原生剪贴板与各类型导出、更多窗口尺寸、完整键盘/辅助功能、本地化和完整桌面参考逐屏比对。命令行与浏览器验证不能替代这些检查。

## 2026-10-22 顶部工具栏修复

- Open 读取后端 `targets`，每次打开重新检测应用。加载失败和 SSH 未接通会显示原因。菜单绑定打开时的工作区，过期响应不会重开菜单或覆盖新结果。后端检查应用、路径和启动退出状态。
- Review 每次打开重新读取 Git 状态，并同步增删行数。关闭面板后忽略延迟结果。Files 和 Review 的加载错误会显示提示。
- Terminal 的延迟列表或创建响应不会重新打开已关闭的面板，也不会覆盖随后选择的 Files。会话仍可在返回时恢复。
- 更多菜单可通过第二次点击关闭。弹出窗口转移只有文件引用的草稿；保存期间切换聊天时会取消转移。空白辅助窗口返回主窗口时同步模式。截图入口会请求 macOS 屏幕录制权限，未授权时显示设置路径。
- 实际浏览器 → Rust preview 验证了 Open 应用列表和 Finder 启动请求、终端命令执行、外部改动进入 Review、文件预览与引用、更多菜单开关和重命名。[截图](screenshots/26-toolbar-fixed.png)
- 前端回归 344 项通过，其中工具栏相关专项 46 项通过；Rust appserver 9 项、terminal 7 项通过；桌面 smoke 10 checks / 177 RPCs，终端 smoke 14 checks / 108 RPCs；真实 Open RPC 应用发现及三个错误分支通过。
- 当前工具无法操作原生窗口。Mini、弹出窗口和系统截图仍缺本次原生实屏验收；跨窗口草稿逻辑已由自动化测试验证。Apple Silicon 仍需 M 系列真机启动验证。这些结果不代表整体 1:1 复刻已完成。

## 2026-10-22 左侧功能导航修复

- 实际复现：从 Codex 点击资料库会切换成 Work，改变右侧布局并隐藏工作树入口。现在资料库保留当前模式、项目和执行上下文，显示本地全局资料库。
- 设置和资料库的延迟响应会检查当前导航及请求序号，不能覆盖之后选择的页面。设置等待文档保存期间切到首页时，不再跳回设置。
- 资料库一次读取后直接绘制目录树，取消卡片与目录树的二次读取、二次绘制。延迟回调不再向后来的资料库重复添加操作入口。
- 导航高亮与当前页面一致；账户有独立选中态，计划和设置开始加载时立即同步选中入口。
- `scripts/test-shell-navigation.mjs` 覆盖模式保持、账户选中、旧设置/资料库响应、保存期间离开设置、全局资料库项目上下文和单次目录树绘制。
- `scripts/smoke-shell-navigation.py` 在真实浏览器和 Rust preview 中完成 3 轮七个入口切换（21 次完整检查）、7 次快速切换；模式、草稿、侧栏宽度和主区域边界保持稳定，始终只有一个主页面可见，资料库操作入口无重复。[截图](screenshots/27-sidebar-navigation.png)
- 这些是功能导航验收，不代表所有功能、原生系统集成或 M 系列真机验收全部完成。
