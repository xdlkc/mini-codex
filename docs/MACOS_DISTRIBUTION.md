# macOS 正式发布

其他电脑出现“无法验证开发者”或“无法验证是否包含恶意软件”，原因是旧版本只做了 ad-hoc 临时签名，未经过 Apple 公证。每次更新都是新的二进制，旧版的手动允许运行不能作为新版的发布验证。

正式分发需要有效的 **Developer ID Application** 证书及其私钥。PKG 另需 **Developer ID Installer** 证书及其私钥。Apple Developer Program 的证书和公证流程见 [Developer ID](https://developer.apple.com/developer-id/) 与 [Apple 公证说明](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)。

## 配置

在构建机钥匙串中安装这两类证书及私钥。使用 `security find-identity -v -p codesigning` 检查应用签名身份。不要把私钥、证书导出密码或 Apple 密码写进仓库或聊天。

用交互式命令创建公证凭据配置。命令会单独提示输入 app-specific password：

```bash
xcrun notarytool store-credentials mini-codex-notary \
  --apple-id '<Apple ID>' --team-id '<Team ID>'
```

构建环境只需引用证书名称和钥匙串配置名称：

```bash
export MACOS_APP_SIGN_IDENTITY='Developer ID Application: <Name> (<Team ID>)'
export MACOS_INSTALLER_SIGN_IDENTITY='Developer ID Installer: <Name> (<Team ID>)'
export MACOS_NOTARY_PROFILE='mini-codex-notary'
./scripts/package-macos.sh
```

默认 release 构建执行正式发布流程。缺少身份、私钥或公证凭据会在构建前停止。应用采用 hardened runtime 和 secure timestamp 签名，先公证并附加 App 票据，再生成、签名、公证并附加 DMG / PKG 的票据。最后检查两类安装包和 App 的 Gatekeeper 评估。

只有全部步骤通过才生成标记为已验证的 `dist/distribution-manifest.json`。从干净 `git archive` 构建时，需通过 `MACOS_SOURCE_COMMIT` 提供完整源提交 SHA。

## 发布

提交并推送源码，准备 `dist/release-notes.md` 后执行：

```bash
./scripts/publish-macos-release.sh
```

发布入口核对源提交、干净工作区、版本、两个安装包的哈希、双架构、附加票据和 Gatekeeper 评估。任何检查失败都不会创建 Release。不得用临时签名包代替完成公证的正式包。

`notarytool` 超时时，Apple 端可能仍在处理。保留 `dist/notarization/*.submission.json`，根据其中的提交 ID 查询结果后再决定是否重试。拒绝或非 `Accepted` 状态不会继续附加票据或发布。

## 本地测试

```bash
./scripts/package-macos.sh --debug --app-only
./scripts/package-macos.sh --signing ad-hoc
```

这两种是未公证的本地测试包。安装包文件名包含 `-unnotarized`，不能经正式发布入口发布。它们不解决其他电脑的 Gatekeeper 警告。

Developer ID 签名和公证可处理未识别开发者及未公证软件的阻止提示。macOS 对从互联网下载的软件仍可能显示普通首次打开确认，不应把它与“无法验证安全性”的阻止提示混为一谈。
