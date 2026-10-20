# 本地 Pages 与选区建议

更新日期：2026-10-20。本地 Work 提供结构化编辑、页面层级、可复用内容块及需审阅的 Responses 建议。本文记录本轮本地交付与实际验收。云同步、分享、多人协作和云执行已由用户排除；它不代表官方云端 Pages 或像素级一致。

## 参考对象

官方 [Work with pages](https://learn.chatgpt.com/docs/space/pages) 说明 Pages 的块编辑、选区工具栏和内容组织。[Work with agents in Space](https://learn.chatgpt.com/docs/space/agents) 明确要求先审阅选区修改，再接受建议。[Collaborate in Space](https://learn.chatgpt.com/docs/space/collaboration) 另行说明共享权限与多人协作。这些是产品依据，不是当前账户已开通服务的证明。

本项目的 **Page** 是本地文档。**canvas** 只是将同一编辑器放在聊天旁栏的布局名称。两种布局共用文档、修订和编辑状态；它们不代表实现了官方所有 canvas 或 Space 内容类型。文件查看器也与 Pages 编辑器分开；官方 [Work with files](https://learn.chatgpt.com/docs/artifacts-viewer) 不能作为本地 Pages 导出或协作能力的验收证据。

## 当前编辑范围

编辑器使用随应用打包的 Tiptap/ProseMirror。运行时不依赖 CDN。当前合同为 `schemaVersion: 2`，兼容读取 v1；实际节点、属性和依赖见 [schema.json](../src/vendor/editor/schema.json)、[编辑器说明](../src/vendor/editor/README.md) 和 [manifest.json](../src/vendor/editor/manifest.json)。Rust 同样校验正文结构。

| 能力 | 当前实现 | 限制 |
|---|---|---|
| 文本与块 | 段落、标题、引用、代码块、分隔线、有序/无序列表、嵌套 checklist；块拖动把手；工具栏与可搜索的 `/` 插入菜单 | 标题菜单提供 H1–H3，schema 接受 H1–H6。本地复用块和生成入口见下文；在线人员/贡献者模块不在本地范围 |
| 行内格式 | 加粗、斜体、下划线、删除线、行内代码、链接；选区浮动工具栏 | 链接只接受符合校验的 HTTP、HTTPS、mailto。富 HTML 粘贴按纯文本处理 |
| 表格 | 插入表格、增删行列、表头、拖动列宽；Normal/Flexible/Full 布局；Fit columns | 当前 Fit 清除所选表格的固定列宽，不等于官方全部列宽分配方式。schema 可保存合并单元格属性，当前工具栏未提供完整合并/拆分流程 |
| 图片与文件 | 从本地 Library 选择、导入或拖入；替换附件；图片 alt、title 和尺寸；显式打开原件；图片编辑入口 | 正文只保存 `attachmentId` 和显示属性，不保存任意路径、远程图片 URL 或 base64。页面内可打开生成器；生成结果先留在 Library，显式点击 Insert in page 才插入；导航或正文变化会阻止旧位置插入 |
| 数学 | 行内与块级 LaTeX，支持插入和编辑；本地 KaTeX 显示 | 不执行公式代码，不提供计算表格或可执行单元 |
| 查找与替换 | 当前文档内查找、大小写选项、上/下一项、单项/全部替换；富文本跨 mark 搜索；全部替换可作为一次 Undo 撤销 | 使用字面文本匹配；不跨附件、公式等非文本节点，不提供正则表达式搜索 |
| Undo/Redo | 使用真实编辑事务；各文档编辑器独立；在页面/旁栏切换及挂载/卸载时保留当前进程的历史栈 | Undo 栈不跨进程重启。已保存修订提供另一条恢复路径 |
| 本地审阅 | 评论、回复、编辑/删除、解决/重开；版本列表、预览和恢复；选区 AI 建议 | 评论作者显示为本机用户；没有邀请、共享、实时协同、贡献者归属或 `@` 人员通知 |

当前格式 mark 为 `bold`、`italic`、`underline`、`strike`、`code`、`link`。正文还包含 `image`、`fileAttachment`、`inlineMath`、`blockMath` 和 `workBlock` 节点。未知节点、属性、危险链接和无效结构会被拒绝。图片原件通过 Rust 附件存储读取；编辑器不会自行请求远程图片。

## 本地内容组织与生成

Library 按 `parentId` 显示页面层级。页面提供 New subpage、Move page 和返回父页面。Rust 拒绝缺失父页面与循环；移动需要 CAS，删除父页面会把子页面移到上一级并保留正文与修订。

`workBlock` 支持 Prompt、Task、Agent Instructions、Highlight、Page reference、Chat reference 和 Visualization，属性为 `kind/title/text/targetId/html`。这些块可编辑、删除和 Undo。Markdown 采用 `mcodex-block` JSON fence 无损往返。v1 不允许这些节点；v2 不能直接降级为 v1，历史恢复仍可明确恢复旧版本。

Prompt 和 Task 打开本地草稿，分别使用 Work 和 Codex；同页 Agent Instructions 作为可见指导文字加入草稿。Page/Chat reference 打开本地目标。评论的 Ask ChatGPT 以评论为要求，仍走建议审阅；失效 anchor 只生成插入草稿，不猜测原选区。

Generate 与 Visualize 使用 `purpose: generate | visualize`，允许经过校验的空插入点。模型输出先保存为建议，用户审阅后才插入。Visualization 使用 `sandbox="allow-scripts"` 的独立 iframe。子页面 CSP 禁止网络、嵌套 frame、表单提交和外部资源；主页面另用 `frame-src about:` 禁止子页面导航到网络地址；不开放 same-origin、popup、导航顶层或本地工具。原生 IPC 另校验来源和只注入主 frame 的随机令牌。Word 导出保留 Work 块文本、引用或 HTML 源码并提示限制；交互行为只在 App 内运行。

## 保存、原文和修订

文档保存在 `MCODEX_HOME/desktop.json`，默认是 `~/.mcodex/desktop.json`。Rust 使用私有文件、原子替换和跨进程文件锁。正文的关键字段为：

```text
id, title, kind, projectId, parentId
format: markdown | richtext
schemaVersion: 1 | 2
body: ProseMirror JSON | null
content: 当前 Markdown 或富正文的 Markdown 投影
sourceMarkdown: 保留的原始 Markdown | null
revision, createdAt, updatedAt
```

旧 Markdown 只有在导入器确认支持且可往返时，才在首次保存时升级为富正文。原字符串保留在 `sourceMarkdown`，包括原有换行。包含不支持结构、原始 HTML、脚注、远程图片或无法保留的链接标题时，界面继续使用源文本编辑，不静默转换或执行 HTML。`kind: code` 使用源文本编辑。

编辑后约 600ms 自动保存。保存队列会继续写入保存期间的新输入。切换文档、离开页面和关闭窗口会等待未保存内容。失败或 CAS 冲突会保留本地编辑；冲突界面提供另存为新文档、审阅最新版本或明确舍弃后加载的操作。两个窗口可以读取同一份存储，但不会自动合并正文。

富正文写入和删除必须带 `expectedRevision`。评论、恢复及项目解除关联也参与修订计数。过期写入返回 `conflict.expectedRevision/actualRevision`，不改动原文件。修订恢复产生一个新修订，不把当前版本号倒退；当前评论不会随旧正文一起回退。

选区评论使用 ProseMirror UTF-16 位置和精确 quote。正文改变后，已有选区评论标为 `orphaned`，不会猜测新位置。保留上限为每文档 100 个修订、32 MiB 修订记录；超出时从最旧记录裁剪，并报告已裁剪数量。评论最多 1000 条，单条正文上限 16 KiB。富正文与其 Markdown 投影合计受 8 MiB 限额约束，节点最多 100000 个，深度最多 64。

导出提供 Markdown 和 JSON，也可复制 Markdown。Markdown 不能保留全部表格布局、列宽、合并单元格和图片/文件属性；完整结构应保留 JSON。JSON 导出包含选定修订与原 Markdown，但不是包含附件原件的自包含归档。Word（DOCX）由 Rust 离线生成，保留标题、段落、行内格式、链接、代码、列表及起始编号、清单、表格合并与列宽、PNG/JPEG 图片。WebP 在内存中转为 PNG；图片只从本地 Library 读取并校验，不请求外部地址。重复图片只嵌入一次；原件缺失时整个导出失败。公式转为 LaTeX 文本，文件块仅保留名称，评论和修订不进入 Word；响应的 `warnings` 会在界面显示这些限制。源 Markdown 不静默转换，按字面文本导出并提示。XML 最多 15 MiB，图片合计与最终文件各最多 20 MiB。

“Print / Save as PDF”和文档内 `Cmd/Ctrl+P` 先保存，再建立独立打印快照。原生 App 使用 WebKit 系统打印面板，由用户选择保存 PDF；浏览器使用自己的打印面板。快照保留标题、正文、KaTeX 和本地图片，等待字体与图片就绪，并隐藏应用外壳、编辑把手和编辑器占位图片。保存失败、图片缺失或切换文档会阻止打印。它不提供 headless PDF RPC。原生系统面板已实际存出 PDF，正文、表格、中文和 emoji 经独立提取与渲染检查。WKWebView 可能在面板仍使用快照时发出 afterprint；原生快照保留到下一次导航/打印，避免输出应用界面。

## 真实选区 AI 通路

`Ask for change` 先保存当前文档，再提交选区、版本和修改要求。Rust 核对 `expectedRevision`、UTF-16 边界和 canonical quote。定位 quote 使用 `textBetween(from, to, '\n', '\uFFFC')`；不能用 DOM 文本或 Unicode code-point 下标替代。

发给模型的内容只包含选区和用户要求。选中的换行、LaTeX 会转成相应语义文本；选中的图片或文件节点会经真实附件校验，加入原图或完整提取文本。未选中的文档正文、仓库和聊天历史不会附加。选中一个文件节点意味着该文件的完整可读内容进入请求；不可读或无文本的附件会在发请求前失败。

请求通过 Rust `api::Client` 的有界 Responses 通路发送。支持已连接的 API key 或 ChatGPT OAuth，凭据不会返回前端。请求使用 `tools: []`、`store: false`；任何观察到的工具或不支持的输出都会使建议失败，不能靠最终文本隐藏。Auto reasoning 省略 reasoning 参数。账户已连接不代表模型权限已验证，capabilities 明确保留 `permissionsVerified: false`。

建议状态为 `queued/running/ready/failed/cancelled/interrupted`。`ready` 只保存可审阅的 Markdown，不写文档。界面显示原选区与建议。用户可保持文档不变、复制 Markdown，或明确应用到原选区。应用前再次核对文档版本和编辑状态，然后走普通文档 CAS 保存。过期建议标为 `stale`；不能直接覆盖旧选区。不支持无损导入的建议只能复制 Markdown。

建议记录保存在私有的 `MCODEX_HOME/document-ai/proposals.json`。实际 provider 返回 token usage 时才记录 usage。最多同时运行 2 项，最多保留 200 项；选区 quote 最多 32 KiB，修改要求最多 4000 字符，完整输入最多 32 MiB、20 个附件，替换文本最多 64 KiB。请求限制为 8192 output tokens、2 MiB 响应和 120 秒等待。流式超限会取消正在等待的 future，不只是截断显示。

取消只停止本机等待；无法保证 provider 停止处理或停止计费。晚到结果不会变成成功，也不会自动重试。换账号、退出和服务关闭会取消旧 generation。进程重启将遗留活动记录标为 `interrupted`。控制器长持 OS 文件锁；同一个 home 的第二个 app-server 可读历史，但不能接管或取消第一个实例仍在处理的建议。

本地文档始终通过本机 RPC 和本机账户处理。即使页面显示在 SSH 聊天旁边，也不会因此使用远端账户或向远端写入文档。

## RPC 与代码入口

| 范围 | 方法 | 主要合同 |
|---|---|---|
| 文档 | `desktop/document/create/read/list/update/delete` | richtext 使用 `body`；修改和删除用 `expectedRevision`；列表返回 `items` 摘要 |
| 评论 | `desktop/document/comments/list/create/update/delete` | `id` 为文档；修改带 `expectedRevision`；选区 anchor 含 `revision/from/to/quote`；回复带 `parentId` |
| 修订与导出 | `desktop/document/history/list/read`、`desktop/document/restore`、`desktop/document/export` | 恢复用旧 `revision` 和当前 `expectedRevision`；导出 format 为 `markdown`、`json` 或 `docx`，可用 `revision` 固定版本；Word 返回 `dataBase64/mimeType/filename/warnings` |
| AI 建议 | `desktop/document/ai/capabilities/propose/read/list/cancel` | propose 带 `id/expectedRevision/selection/instructions/model?/purpose?`；read/cancel 用 `proposalId`；list 返回 `data`；通知为 `desktop/document/ai/updated {proposal}` |

存储与校验见 [documents.rs](../src/documents.rs) 和 [desktop.rs](../src/desktop.rs)。AI 作业见 [document_ai.rs](../src/document_ai.rs)。界面与保存握手见 [gui_documents.js](../src/gui_documents.js)，选区、插入和附件工具见 [gui_document_tools.js](../src/gui_document_tools.js)，实际编辑事务见 [bridge.js](../src/vendor/editor/bridge.js)。

## 验证与未完成范围

工具回归需要 JSDOM。请使用 Node 22+/npm 10+，在仓库根目录执行下列命令。依赖按 vendor lockfile 安装到临时目录；`EDITOR_TEST_NODE_MODULES` 指向该目录，不能省略。子 shell 退出时自动清理目录。已有完整 npm 缓存时，可给 `npm ci` 增加 `--offline`。

```bash
(
  set -eu
  EDITOR_TEST_DEPS="$(mktemp -d)"
  trap 'rm -rf "$EDITOR_TEST_DEPS"' EXIT
  cp src/vendor/editor/package.json src/vendor/editor/package-lock.json "$EDITOR_TEST_DEPS/"
  npm ci --prefix "$EDITOR_TEST_DEPS" --ignore-scripts --registry=https://registry.npmjs.org
  EDITOR_TEST_NODE_MODULES="$EDITOR_TEST_DEPS/node_modules" \
    node --test scripts/test-document-persistence.mjs scripts/test-document-tools.mjs
)
python3 scripts/smoke-documents.py
python3 scripts/smoke-document-ai.py
```

这里只验证已提交的离线 bundle；重建及 vendor 自身测试见 [编辑器构建说明](../src/vendor/editor/README.md#rebuild-and-test)。两个 Python smoke 需要先 `cargo build`，也可用 `--binary` 指定已构建的 app-server。

- 文档 smoke 10 组、48 次真实 JSONL RPC 通过。覆盖原 Markdown、v1/v2、全部本地 Work 块、父子层级、循环拒绝、父页面删除后的子页面保留、跨进程 CAS、评论、修订、JSON/Word 导出、权限和重启。Word 经 ZIP/XML、真实重新导入和 macOS textutil 独立检查。
- 选区 AI smoke 12 组、96 次 RPC、14 次本机 Responses HTTP 和 3 次 key 验证通过。另有 Rust 插入生成/可视化测试，覆盖空插入点、UTF-16、显式审阅、原选区隔离和历史兼容。
- 最终前端集 201 项通过；Rust 205 项通过，另有 3 个由跨进程测试使用的辅助入口。编辑器离线 vendor 11 项测试通过。浏览器实测层级、移动、刷新、Prompt 草稿、Generate/Visualize 审阅、可视化交互及显式图片插入。
- 原生 WebKit 实测中文与 emoji、Undo/Redo、重开恢复、系统 PDF 保存、多窗口草稿/附件交接，以及真实保存失败取消退出和恢复后保存退出。原生可视化另验证不访问主页面、网络或 IPC；见 [本地验收记录](NATIVE_LOCAL.md)。

全部数据、账户和 provider 都是隔离 fixture。**未调用真实付费 API，也未验证真实订阅、模型权限或修改质量。** 前端回归执行生产函数和真实编辑事务，但不能替代浏览器、原生 WebKit、输入法、辅助功能和同版本桌面逐屏验收。

云同步、分享权限、多人协作、人员引用和贡献者归属需要在线服务，已移出本次范围。Prompt/Task 是可审查的本地聊天草稿入口；它们不会自动发送或启动工具。图片、语音和模型的真实服务权限仍需用户账户；本轮使用本机 fixture 验证。完整产品差异保留在 [总体差异矩阵](PARITY.md)。
