# 本地窗口与 Work 验收

更新日期：2026-10-20。对应本轮 Mini、全局入口、Work 本地内容类型和原生验收交付。原生验收使用真实 macOS App、WebKit 和系统面板。状态、HOME、凭据、模型服务和工作目录均为隔离测试环境；模型与图片请求只到本机 fixture。

## 窗口与入口

- 主窗口、Mini、Popout 共用一个 Rust app-server。Mini 默认置顶并可跨桌面；Popout 可切换置顶。辅助窗口有独立、持久的文字、文件、引用和搜索草稿。
- 默认全局入口为 Mini `⌥Space`、听写 `⌃⌥Space`、截图 `⌥⇧Space`。设置可改为其他预置组合或关闭；注册失败显示系统返回的冲突，不静默回退。只有 App 运行时有效。验收版使用 `⌘⇧Space` 避开其他应用已占用的组合；用户已确认在其他 App 中可正常显示/隐藏 Mini。
- 原生菜单、主窗口按钮、Mini/Popout 返回主窗口均可用。关闭窗口先保存再隐藏；Dock 重开显示主窗口。Quit 等待所有窗口保存，再停止后台 runner。
- 草稿交接携带文字、附件、引用、模式、模型、目录和项目。接收窗口保存后才确认；来源内容未变化才清空。超时、保存失败或交接期间新增编辑会保留来源草稿。首次创建窗口先确认界面就绪，再处理交接。
- 截图使用系统交互选区。只在完成后导入本地附件库并加入原草稿；取消恢复窗口；导航已改变时只保存在 Library。临时图像和私有临时目录在完成、取消或退出时清理。

## 实际通过的原生流程

| 流程 | 结果 |
|---|---|
| 全局 Mini 显示/隐藏 | 系统 HID 测试通过；用户另确认其他 App 内的真实快捷键通过 |
| 原生文件选择器 | 选择项目 PNG，导入后准确显示附件 |
| Mini → 主窗口 | 文字与 PNG 一起转移；主窗口旧 Work 草稿保留；确认保存后来源清空 |
| 主窗口 → 首次 Popout | 窗口就绪后收到草稿并持久保存；来源清空；真实验收修复了早到请求丢失 |
| Popout 置顶 | 系统窗口状态与按钮状态一起切换 |
| CmdW 与全局重开 | 关闭只隐藏；进程继续运行；全局重开后原草稿准确恢复 |
| 截图完成/取消 | 框选隔离 App 窗口后产生真实 PNG 附件；取消不增加文件，恢复原草稿 |
| WebKit 富文本 | 中文、emoji、表格显示与编辑；Undo/Redo 准确；保存后重启可恢复 |
| 系统打印/PDF | 从真实打印面板保存 PDF；独立提取、Unicode 归一化检查和 Poppler 渲染通过；没有侧栏、菜单或导出弹窗 |
| 保存失败 + Quit | 临时阻断隔离状态目录后 Quit 被取消；Mini 草稿、文档与焦点保留；恢复目录后保存退出码 0，落盘内容准确 |
| WebKit 可视化 | 输入 9 后结果为 18；主页面访问失败；原生 IPC 探针未写入设置 |
| 浏览器 Work | 子页面、移动、刷新、全部本地块、Generate/Visualize 审阅、Prompt 草稿、可视化交互及显式图片插入通过 |

首次 PDF 检查发现 WKWebView 在面板仍使用快照时提前发出 afterprint，导致最终文件包含应用外壳。修复后重新保存并逐项检查。浏览器打印仍按正常 afterprint 清理。原生隐藏快照在下次导航或打印时清理。

实屏：[原生 Work 可视化](screenshots/27-native-work-local.png)、[原生 PDF 输出](screenshots/28-native-document-pdf.png)、[Mini](screenshots/29-native-mini.png)。

## 存储与边界

原生窗口使用私密 loopback asset origin。所有原生 RPC 验证来源和主 frame 随机令牌。可视化 iframe 为 `allow-scripts` 的独立源，子页面 CSP 禁止网络、外部资源、嵌套 frame 和表单；主页面的 `frame-src about:` 同时禁止它导航到网络地址；无同源、顶层导航与弹窗能力。macOS WebKit 的子 frame 导航经 Wry 的新窗口回调，需要显式允许 `about:srcdoc`；外部链接通过主页面的点击和带令牌的命令处理。

本轮不调用真实付费模型、图片或语音服务。真实账户权限、语音设备质量和外部连接器需要相应服务；云同步、分享、多人协作及云执行已由用户移出范围。本记录不声称完整官方云端产品或像素级一致。

## 可复现检查

```bash
cargo test
EDITOR_TEST_NODE_MODULES=/absolute/temp/node_modules node --test scripts/test-*.mjs
python3 scripts/smoke-documents.py
python3 scripts/smoke-document-ai.py
./scripts/package-macos.sh --debug --app-only
./scripts/package-agent.sh --debug --offline
codesign --verify --deep --strict 'dist/Mini ChatGPT.app'
plutil -lint 'dist/Mini ChatGPT.app/Contents/Info.plist'
```

最终回归：Rust 205 项通过、3 个辅助入口；前端 201 项通过；editor vendor 11 项通过。13 套 smoke 全部通过，合计 1264 次根服务 RPC。RPC 数可能因取消后的轮询次数变化。另用真实浏览器验证不透明子页面可执行本地脚本，但导航到本机探针地址被父页面 frame-src 拒绝。全部测试数据均为临时 fixture。

源码入口：[native windows](../src/gui_native.rs)、[global shortcuts](../src/native_integration.rs)、[draft isolation](../src/gui_native_windows.js)、[native controls](../src/gui_native_controls.js)、[Work local](../src/gui_work_local.js)、[Pages contract](DOCUMENTS.md)。
