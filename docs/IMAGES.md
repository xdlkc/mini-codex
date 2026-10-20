# 本地 Images 合同

核实日期：2026-10-19。实现使用标准 Images API。它要求 API key。ChatGPT OAuth 不用于该接口。模型是否向当前账号开放，仍由服务端决定。

## 模型与参数

默认模型为 `gpt-image-2.5-sunburst`。同时支持 Flare、两者的 `2026-09-08` 快照、GPT Image 2 及其 `2026-04-21` 快照，以及官方接口列出的旧模型。`chatgpt-image-latest` 使用保守的公共参数集，不假定别名对应某个固定版本。[官方生成接口](https://developers.openai.com/api/reference/resources/images/methods/generate)

| 模型族 | quality | size | 透明背景 |
| --- | --- | --- | --- |
| GPT Image 2.5 Sunburst / Flare | auto、low、medium、high、xhigh、max | 标准尺寸或自定义 | PNG / WebP |
| GPT Image 2 | auto、low、medium、high | 标准尺寸或自定义 | 官方标为 preview；PNG / WebP |
| GPT Image 1.5 / 1 / 1-mini | auto、low、medium、high | auto、1024x1024、1536x1024、1024x1536 | PNG / WebP |

2.x 和 2.5 的自定义宽高须为 16 的倍数。单边不超过 3840。长短边比不超过 3:1。像素总量为 655,360 至 8,294,400。高于 2560x1440 的分辨率属于官方实验范围。本模块不发送 `input_fidelity`，也不将旧模型的该参数套用到 2.x。[官方图像指南](https://developers.openai.com/api/docs/guides/image-generation)

输出支持 PNG、JPEG、WebP。JPEG 不支持透明。编辑以 multipart 发送附件原件。mask 必须是带 alpha 的 PNG，尺寸须与第一张输入图相同。API 以 mask 引导编辑，不保证精确沿轮廓修改。[官方编辑接口](https://developers.openai.com/api/reference/resources/images/methods/edit)

## 本地限制与任务状态

本地上限比 API 的部分上限更小：同时 2 个任务；每任务生成 1–4 张；编辑输入 1–8 张、每张 10 MiB、合计 40 MiB；mask 小于 4 MiB；输入不超过 32 百万像素，输出不超过 8,294,400 像素；HTTP 响应不超过 64 MiB；请求最长 180 秒。图像须完成 base64 解码和完整图像解码。远程结果 URL 不会被自动下载。

`images/generate` 和 `images/edit` 返回 `{job}`，不等待网络完成。读取、列表和取消使用 `images/job/read`、`images/job/list`、`images/job/cancel`。通知为 `images/job/updated {job}`。状态为 queued、running、completed、failed、cancelled 或 interrupted。`revision` 单调递增，UI 应据此忽略旧状态。

取消只停止本机等待。它不能保证服务端停止处理或停止计费。取消后到达的结果不会被记为成功。退出、换账号和重启都不会自动重试，以免重复产生费用。重启将未完成记录标为 interrupted。

任务写入 `MCODEX_HOME/images/jobs.json`。图像进入现有私有附件库。完成任务包含真实 `attachmentIds` 和附件元数据；预览、下载和下一次编辑复用这些附件。写入使用私有权限和原子替换。损坏的任务文件不会被空状态覆盖。
