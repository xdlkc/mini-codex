# SSH 远程连接

桌面通过 SSH stdio 启动远端的 Rust `mcodex app-server`。聊天、命令、Git、文件解析和工具审批在选定主机运行。账户凭据留在各自机器。本机账户、界面偏好、文档和 Images 仍在本机。

## 安装远端 agent

远端需要 SSH 服务、可用的密钥认证和 `mcodex`。连接使用 `BatchMode=yes`，不显示密码提示。可以使用已有 SSH host alias、用户、端口和 identity file 路径。应用不读取或上传私钥内容。

优先使用与远端操作系统和 CPU 一致的二进制包。参见 [HEADLESS_AGENT.md](HEADLESS_AGENT.md)，校验外部 manifest 中的 archive SHA-256，以及解包后内部 manifest 中的 binary SHA-256。Darwin 包不能当作 Linux 包使用。

也可以传输源码包，在远端构建：

```sh
cargo build --release --locked --no-default-features --bin mcodex
install -d "$HOME/.local/bin"
install -m 755 target/release/mcodex "$HOME/.local/bin/mcodex"
```

Headless 构建不需要 GTK 或 WebKit。当前连接页不会自动上传或安装 agent。`agentPath` 填远端可执行文件的绝对路径，或 SSH 非交互环境中能找到的 `mcodex`。路径中的 `$HOME` 和 `~` 不会被应用展开。

## 配置与首次信任

1. 打开 **Settings → SSH connections → Add SSH host**。填写 host、远端绝对工作目录及 agent 路径。保存 profile 不会连接主机。
2. 选择 **Inspect host key**。扫描结果只表示发现了公钥，不表示主机身份已验证。
3. 通过主机管理员或其他可信渠道核对 SHA-256 指纹。确认后勾选核对项，再选择 **Trust and connect**。

扫描与信任绑定同一 profile、host 和 port，结果在五分钟后过期。信任只写入本机 `MCODEX_HOME/remote-known-hosts`，使用私有权限和原子替换。应用不修改系统或用户的 known_hosts。

连接始终使用 `StrictHostKeyChecking=yes`。系统和用户已信任的 known_hosts 仍有效；应用文件通过固定的 `KnownHostsCommand` 追加。该命令只读取应用文件，不执行扫描内容。若文件路径含 OpenSSH 环境变量表达式 `${...}`，应用会拒绝使用该路径。空格、单引号和 `%` 按字面路径处理。

OpenSSH 说明：[`KnownHostsCommand`](https://man.openbsd.org/ssh_config#KnownHostsCommand) 会补充系统与用户文件；[`ssh-keyscan`](https://man.openbsd.org/ssh-keyscan) 本身不能验证扫描结果的身份。

## 账户与界面作用域

在远端终端配置该主机自己的账户。连接页的 **Remote account** 只读取远端连接状态。应用不把本机 API key 发给远端，也不在本机代办远端 OAuth 回调。

本机和远端默认各自使用其机器上的 `~/.mcodex`。需要独立目录时，可在远端准备一个 launcher，再把其绝对路径填入 `agentPath`：

```sh
#!/bin/sh
export MCODEX_HOME="$HOME/.mcodex-ssh"
export MCODEX_NO_CODEX_IMPORT=1
exec /absolute/path/to/mcodex "$@"
```

Codex 顶部的位置菜单选择 Local 或 SSH profile。已有远端聊天按其 profile 路由，后台审批也保留来源。远端任务创建的 Chat/Work 聊天同样保留远端模型、设置和文件作用域。切换到新的本机 Chat/Work 会恢复本机环境，并保留后续 Codex 可选的 SSH profile。

远端 folder picker 使用手工绝对路径；不会打开本机文件选择器。远端 IDE 启动和语音未接入，界面会明确说明。语音在申请麦克风前拒绝 SSH 上下文。

不要把同一 `MCODEX_HOME` 克隆后同时当作不同主机的独立工作区。当前 UI 发现跨主机相同资源 ID 时会阻止含糊操作，不会把本机命令改发远端。需要为不同主机使用独立 store；ID 命名空间迁移尚未提供。

## 断开与恢复

SSH child 不是 daemon。应用退出、主动断开、协议错误或连接掉线会终止连接，并使所有未完成的代理 RPC 失败。已连接 profile 需要先断开，才能编辑或删除。

聊天、已保存的附件、Goal 和定时任务定义留在远端 store。重新连接会启动新的 agent 进程，并恢复已保存内容。中断的 Goal 会按现有 Goal 恢复规则重新检查是否继续；未完成的模型输出和外部命令不保证能恢复，界面不会声称它们已成功。定时任务只在 SSH 连接和远端 agent 运行期间执行。

同 profile 重连会替换 connection ID。后台聊天和附件继续按 profile 绑定；关闭的连接不会退回本机执行。当前没有开机自启、后台常驻服务、断线任务接管或 Cloud 执行。

## 已验证范围

- Rust 单元/进程测试：真实 `mcodex` 经 fake SSH 完成 initialize、命令执行、审批、附件、会话恢复和重连；验证帧大小、超时、进程退出与 shutdown 竞态。
- `scripts/smoke-remote.py`：真实本机 dispatcher → fake SSH → 第二个真实 Rust app-server，使用独立 HOME 和账户，验证完整 RPC 链及 EOF 清理。没有连接用户已有主机。
- 临时 loopback sshd：只用临时 host key 和端口，验证含空格、单引号的 app known_hosts 路径。HostKey 校验通过后，按测试配置拒绝所有登录认证。没有据此宣称真实主机认证或真实网络稳定性已验收。
- `scripts/test-remote-ui.mjs`：验证来源路由、草稿、异步导入、审批、掉线拒绝及跨 host ID 冲突防护。
