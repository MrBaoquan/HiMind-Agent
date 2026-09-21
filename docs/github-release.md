# GitHub 独立发布

HiMind Agent 的 Independent 模式不依赖 Dashboard。它从 GitHub Release 获取 Agent 自更新包，首次安装使用 GitHub Release 中的 Independent 安装器；运行时仍可访问 GitHub、第三方 AI 服务和已发布的首方 Runtime Release。

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

## Runtime 独立安装

HiMind AI Runtime 与 Agent Release 解耦，使用独立标签和制品：

| 制品 | 文件 | 用途 |
| --- | --- | --- |
| Runtime 安装索引 | `himind-runtime-release.json` | Runtime 版本、兼容区间、大小、SHA-256 和签名 |
| Runtime 包 | `deepseek-harness-runtime-<version>-windows-x64.zip` | 隔离安装到 Agent 本机 Runtime 目录 |

Runtime Release 标签为 `runtime-v<runtime-version>`。为了避免旧版 Agent 的 `/releases/latest` 自更新逻辑把 Runtime Release 误判为 Agent 版本，Runtime Release 在 GitHub 上统一标记为 prerelease；新版 Agent 不再只看 latest，而是扫描 Release 列表并按 `himind-runtime-release.json` 资产识别 Runtime。Agent Release 与 Runtime Release 因而可以在同一仓库独立发布。

Runtime 可被独立发现的前提是 Agent 自身包含产品资产扫描逻辑。已发布旧版 Agent 如果仍只读取 `/releases/latest`，不会自动获得该能力；需要先发布包含该逻辑的 Agent 版本。

Independent 模式的安装路径：

1. 从 GitHub Release 安装 `himind-agent-<version>-setup.exe`，安装器默认写入 `independent`。
2. 首次启动 Agent。设置页中 HiMind AI 显示“需要安装”时，点击“安装运行时”。
3. `auto` 来源在 Independent 模式选择 GitHub Provider，下载 Runtime Release 的签名清单和 ZIP，校验大小、SHA-256、RSA-PSS-SHA256、Agent 兼容区间后执行隔离安装。
4. 已安装 Runtime 后，同一入口显示“检查更新”或“更新到 vX”，无需 Dashboard。

命令行等价入口：

```powershell
himind-agent --mode independent runtime install
himind-agent --mode independent runtime check-update
himind-agent --mode independent runtime update
```

企业内网或离线环境继续使用本地签名发布清单：

```powershell
himind-agent runtime install --manifest .\himind-runtime-release.json
```

Runtime Release 通过主仓库脚本发布：

```powershell
./scripts/release/sign-artifact.ps1 `
  -ArtifactPath .\manual-releases\deepseek-harness-runtime\deepseek-harness-runtime-0.1.5-rc.2-windows-x64.zip `
  -PrivateKeyPath C:\keys\himind-runtime-private.pem `
  -KeyId himind-production-2026 `
  -PackageType directory-zip

./scripts/runtime/publish-github-runtime-release.ps1 `
  -PackagePath .\manual-releases\deepseek-harness-runtime\deepseek-harness-runtime-0.1.5-rc.2-windows-x64.zip `
  -SignatureMetadataPath .\manual-releases\deepseek-harness-runtime\deepseek-harness-runtime-0.1.5-rc.2-windows-x64.zip.signature.json
```

脚本会生成并交叉校验 `himind-runtime-release.json`，然后创建 `runtime-v<version>` GitHub Release。正式发布要求签名材料；`-SkipGhRelease` 只用于本地暂存和自动化验证。

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
