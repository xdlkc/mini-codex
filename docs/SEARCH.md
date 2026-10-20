# Hosted Web search 合同

更新日期：2026-10-20。本轮使用 Responses 的托管 `web_search` 工具。搜索由模型服务执行；Rust 不将它伪装成本机 shell，也不从普通回答文字推测搜索活动。完整 ChatGPT 产品和视觉对齐仍未完成。

## 官方依据与观察边界

- [Web search API](https://developers.openai.com/api/docs/guides/tools-web-search) 说明工具声明、缓存/实时控制、URL 引用和完整来源列表。标准 API key 请求有直接示例。
- [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events) 定义搜索进度及引用增量事件。
- [ChatGPT Web search](https://learn.chatgpt.com/docs/web-search?surface=app) 说明桌面 transcript 中的搜索活动和 Codex 的本地搜索配置。

安装包 `26.1013.40934` 的公开静态资源另有搜索设置、引用 pill 和 Sources 面板。`_virtual_settings-search-documents-e5d0b2e60b03.js` 包含自动搜索和禁用选项；`local-conversation-sources-side-panel-tab-d38ea7378311.js` 包含 Sources、Web search 和搜索次数标签；`chatgpt-sources-side-panel-tab-60b332fe3329.js` 包含来源列表、空状态和加载失败标签。静态存在不表示这些控件在当前账户实际可见；本轮没有据此声称视觉实测通过。

## 偏好、有效模式与全局设置

每个 chat 保存 `webSearchMode`，并返回解析后的 `effectiveWebSearchMode`。派生的 `webSearch` bool 表示有效模式是否允许搜索。

| 保存的偏好 | 有效行为 |
|---|---|
| `auto` | Chat/Work 向模型提供工具，由模型决定是否使用。Codex 优先读取 `config.toml` 的 `web_search`；未配置时，full access 为 live，其他沙箱为 cached |
| `cached` | 工具带 `external_web_access: false`，仅使用服务的缓存/索引 |
| `live` | 工具允许实时访问 |
| `required` | 本次 turn 的首个 Responses 请求使用 `tool_choice: {type: "web_search"}`。后续本地工具循环恢复 auto；服务未返回真实搜索调用时失败 |
| `disabled` | 请求不声明 hosted search；任何未授权的搜索调用或进度返回均失败 |

新 chat 保存的偏好为 `auto`。兼容参数 `webSearch: false/true` 分别映射到 `disabled/auto`。当前 Codex 配置支持 `cached/live/disabled`；其他配置值不能当作已支持的模式。

全局 `preferences.webSearchAllowed` 默认为 `true`。设为 `false` 后，下一轮的有效模式为 disabled，但保留各 chat 偏好；显式要求 required 的 RPC 返回错误，不假称已搜索。全局开关只约束 hosted search，不改变本地 shell、MCP、连接器或模型请求自身的网络权限。运行中修改搜索偏好影响下一轮；停止当前工作仍使用正常的 Stop 操作。

Chat 只增加 hosted search，不因此获得 shell 等本地执行工具。Work/Codex 可组合 hosted search 和原有工具，仍遵守原工具的沙箱和批准条件。

## 搜索活动、引用与 Sources

真实 `web_search_call` 形成活动项。支持 `search/open_page/find_in_page`，保留查询、目标 URL、查找 pattern 和服务报告的状态。进度来自真实 SSE 事件；done/final 更新同一个直播 UI item，不生成重复活动或答案。

`message.content[].annotations` 的 `url_citation` 是回答使用的引用。`include: ["web_search_call.action.sources"]` 请求的 sources 是服务报告的完整 consulted URL 列表，可以多于被引用的页面。两者分别保存；没有 sources 时不从回答或引用虚构“完整来源”。关闭新搜索后，普通回答仍可合法引用历史资料，这不构成新的搜索活动。

引用 URL 和标题随所属 message part 保存。原始 `start_index/end_index` 按 provider 值持久化，公共投影标记 `indexUnit: "provider"`。官方 schema 只称它们为 character 索引，没有明确 UTF-16、UTF-8 或 code-point 单位。官方 [Python 示例](https://developers.openai.com/cookbook/examples/deep_research_api/introduction_to_deep_research_api) 使用字符串切片，但不是含 emoji 的索引规范证明。本实现不猜单位、不切正文；引用链接紧邻对应 part 或回答末尾，完整 consulted 来源单独显示。

展示链接只接受无凭据的绝对 HTTP(S) URL。不执行服务给出的脚本、文件 URL 或任意本机路径。provider 伪造的 `_mcodex_` 控制字段不能变成可信 UI 标记。原始合法 message、annotations 和搜索输出保留在模型历史，用于后续请求、fork 和 resume。

失败、停止和不完整流不会伪装成成功搜索。已观察的搜索进度和 partial answer 通过独立的私有 `uiTurn` 记录保存，供重启、resume 和 fork 后显示；它们不会变成 Responses 原始输出或进入模型请求。恢复时规范化显示 ID，新直播 item 使用独立随机 ID，避免新 delta 覆盖旧行。停止只结束本机等待，不承诺服务已停止处理；搜索客户端不自动重发该请求。

## RPC

| 方法 | 主要字段 |
|---|---|
| `thread/start`、`turn/start` | 可选 `webSearchMode`，兼容 `webSearch` bool；turn 覆盖会保存为后续偏好 |
| `thread/webSearch/set` | `{threadId, mode}`，或兼容 `{threadId, enabled}`；返回偏好、有效模式和派生 bool |
| `thread/read/resume/list/fork` | thread 包含 `webSearchMode/effectiveWebSearchMode/webSearch`；fork 保留来源 chat 的偏好 |
| `web/search/capabilities` | `available` 仅表示账户已连接；`permissionsVerified: false`、`defaultModes`、`policy.allowed`、可选 `reason`。可用 `sandboxMode` 查询对应 Codex 默认模式 |
| `desktop/settings/update` | `{preferences: {webSearchAllowed: boolean}}` |

活动使用标准 item 通知，类型为 `webSearch`，包含唯一 UI `id`、`providerId`、`status`、`queries`、`sources`，以及可选 `actionType/url/pattern`。回答类型为 `agentMessage`；`parts` 保存各段文本和引用，顶层 `citations` 提供聚合视图。代码入口为 [web_search.rs](../src/web_search.rs)、[api.rs](../src/api.rs)、[agent.rs](../src/agent.rs) 和 [appserver.rs](../src/appserver.rs)。

## 隔离验收

```bash
cargo build
python3 -B scripts/smoke-search.py
```

[smoke-search.py](../scripts/smoke-search.py) 使用真实 app-server、临时 allowlisted HOME 和 loopback Responses fixture。验收范围包括模式与策略、真实工具声明、Chat 工具隔离、SSE 和 Unicode、原始引用、完整 Sources、批准后的真实本地工具循环、停止/失败、fork/resume 和 OAuth fixture。运行结果应以脚本实际输出为准，不能把测试用例存在当作通过。

本轮独立验收已通过 **18 组、174 次 JSONL RPC、42 次本机 Responses HTTP 请求**。成功历史的完整来源、引用、原始模型上下文和搜索偏好覆盖跨进程重启。新增检查实际验证了：

- 同一个 response 内，有 ID 和匿名消息的中文/emoji delta 可跨 reasoning/search 交错；活跃消息不会被提前 completed，done→final 的引用更新使用原 UI item；两条相同文字的不同消息仍保留两项，重启后没有重复答案。
- 停止和失败后的已观察 query、partial answer 及终态经 EOF 正常退出、重启、resume 和 fork 后保留；后续原 chat 的 cached 请求及 fork 的 disabled 请求均不携带私有 UI wrapper、partial 正文/查询/推理或虚构的成功搜索。恢复的 item ID 不与新直播 item 冲突。

没有调用真实付费 API、实际搜索引擎或用户账户。OAuth fixture 只验证已配置 endpoint 的认证与协议通路，不证明 ChatGPT 订阅或选定模型已获得 hosted search 权限。真实账户、检索质量、计费、站点可访问性、参考桌面逐屏视觉和完整交互仍待验收。

2026-10-20 浏览器补验：主动搜索、中文与 emoji 回答、2 个引用和 3 个 consulted 来源均实际显示；Search 设置可禁用后续搜索并保留原偏好。760px 窄窗口没有横向溢出，Sources 面板可关闭。截图见 [搜索与来源](screenshots/23-web-search.png)。本轮 provider 仍为隔离本机 mock；没有进行真实搜索或同版本原生逐屏验收。
