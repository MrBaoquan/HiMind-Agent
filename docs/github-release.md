# GitHub 独立发布

HiMind Agent 的 Independent 模式不依赖 Dashboard。它从 GitHub Release 获取 Agent 自更新包，首次安装使用同一 Release 中的 Independent 安装器；运行时仍可访问 GitHub 和第三方 AI 服务。

## Release 契约

每个版本必须包含以下两个运行制品：

| 制品 | 文件 | 用途 |
| --- | --- | --- |
| 首次安装 | `himind-agent-<version>-setup.exe` | 新用户安装或覆盖修复，默认 `independent` |
| 自更新 | `himind-agent-update.zip` | 已安装 Agent 的原子更新，`directory-zip` |

`himind-agent-update.zip` 的根目录只能包含以下文件：

```text
himind-agent.exe
himind-agent-mcp.exe
himind-agent-updater.exe
himind-agent-launcher.exe
himind-ai.vsix       # 可选
```

Release 还必须发布 `himind-agent-update.json`，其 `product` 为 `himind-agent`、`package_type` 为 `directory-zip`、`file_name` 为 `himind-agent-update.zip`，并包含包大小、SHA-256、渠道和签名元数据。标签必须是 `v<version>`，且与索引中的版本一致。

## 本地发布

正式入口是 Agent 仓库中的 PowerShell 脚本，不依赖 GitHub Actions：

```powershell
./scripts/publish-github-release.ps1
```

脚本会构建前端和 Rust 二进制、生成完整便携包和严格自更新包、生成 Independent 安装器、签名更新包、生成清单和校验文件，并通过本机 `gh release create` 发布到 `MrBaoquan/HiMind-Agent`。发布前会执行安装器/自更新包配对校验；缺少签名材料时正式流程会失败。仅本地联调可显式使用 `-AllowUnsigned -SkipGhRelease`。

签名材料通过进程环境变量或参数提供：

```powershell
$env:HIMIND_SIGNING_PRIVATE_KEY_PATH = "C:\keys\himind-agent-private.pem"
$env:HIMIND_SIGNING_PUBLIC_KEY_PATH = "C:\keys\himind-agent-public.pem"
$env:HIMIND_SIGNING_KEY_ID = "release-2026"
./scripts/publish-github-release.ps1
```

私钥只用于本机签名，不会写入 Release、Agent 状态或日志。构建时公钥会嵌入 Agent，并由安装器写入 `trusted-keys`，更新器据此校验签名。

## 更新源选择

- Independent：统一更新状态机使用 GitHub Release provider，下载地址必须是 `github.com/.../releases/download/...`。
- Connected：继续使用 Dashboard software-distribution provider，并保留设备级进度上报。

两个 provider 共享版本状态、下载进度、SHA-256 校验、签名校验、暂存、原子替换和失败回滚。Independent 只是不启用 Dashboard 控制面，不是离线模式。

## 扩展源

插件和 Skill 的 GitHub 源与 Agent Release 相互独立。用户可以在 Agent 中配置一个仓库链接，也可以直接导入带 `?path=/subdir#ref` 的 GitHub URL。仓库存在 `.himind/catalog.json` 时，导入会自动建立扩展源并保存 provenance；开启该源的自动更新后，Agent 会按目录清单更新插件和 Skill。没有目录清单的仓库仍支持一次性导入，但不会伪装成可自动更新源。
