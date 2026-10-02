# GitHub 发布

不对接 AI 工作台时，Agent 从 GitHub Release 获取自更新包，首次安装使用 GitHub Release 中的独立安装器；运行时仍可访问 GitHub、第三方 AI 服务和已发布的首方 Runtime Release。

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

## Runtime 安装

HiMind AI Runtime 与 Agent Release 解耦，使用独立标签和制品：

| 制品 | 文件 | 用途 |
| --- | --- | --- |
| Runtime 安装索引 | `himind-runtime-release.json` | Runtime 版本、兼容区间、大小、SHA-256 和签名 |
| Runtime 包 | `deepseek-harness-runtime-<version>-windows-x64.zip` | 隔离安装到 Agent 本机 Runtime 目录 |

Runtime Release 标签为 `runtime-v<runtime-version>`。为了避免旧版 Agent 的 `/releases/latest` 自更新逻辑把 Runtime Release 误判为 Agent 版本，Runtime Release 在 GitHub 上统一标记为 prerelease；新版 Agent 不再只看 latest，而是扫描 Release 列表并按 `himind-runtime-release.json` 资产识别 Runtime。Agent Release 与 Runtime Release 因而可以在同一仓库独立发布。

Runtime 可被独立发现的前提是 Agent 自身包含产品资产扫描逻辑。已发布旧版 Agent 如果仍只读取 `/releases/latest`，不会自动获得该能力；需要先发布包含该逻辑的 Agent 版本。

未对接 AI 工作台时的安装路径：

1. 从 GitHub Release 安装 `himind-agent-<version>-setup.exe`，安装器默认写入 `independent`。
2. 首次启动 Agent。设置页中 HiMind AI 显示“需要安装”时，点击“安装运行时”。
3. `auto` 来源选择 GitHub Provider，下载 Runtime Release 的签名清单和 ZIP，校验大小、SHA-256、RSA-PSS-SHA256、Agent 兼容区间后执行隔离安装。
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

- 未对接 AI 工作台：统一更新状态机使用 GitHub Release provider，下载地址必须是 `github.com/.../releases/download/...`。
- 已对接 AI 工作台：使用 Dashboard software-distribution provider，并保留设备级进度上报。

两个 provider 共享版本状态、下载进度、SHA-256 校验、签名校验、暂存、原子替换和失败回滚。不对接 AI 工作台只是不启用 Dashboard 控制面，不是离线模式。

## 扩展源

插件和 Skill 的 GitHub 源与 Agent Release 相互独立。用户可以在 Agent 中配置一个仓库链接，也可以直接导入带 `?path=/subdir#ref` 的 GitHub URL。仓库存在 `.himind/catalog.json` 时，导入会自动建立扩展源并保存 provenance；开启该源的自动更新后，Agent 会按目录清单更新插件和 Skill。没有目录清单的仓库仍支持一次性导入，但不会伪装成可自动更新源。

## 扩展分发

扩展的 GitHub 分发由 Agent 内置发布器完成，与 Agent 自身的 Release 互不影响（标签、资产名各自独立）。扩展清单里的 `distribution_targets`（`workbench` / `github`）是硬约束：发布器只能在该范围内收窄，不能越界补发。

| 项目 | 取值 |
| --- | --- |
| Tag | `<kind>/<id>@<version>`，如 `plugin/com.himind.x@1.0.0` |
| 主制品 | `<id>-<version>.<hmpkg\|hmskill\|hmwf>` |
| 发布清单 | `<id>@<version>.json` |
| 签名 | 内嵌在发布清单的 `signature` 字段，不作为独立资产上传 |

一次发布只有两个资产：制品和发布清单（Workflow 多一个扩展锁 `<id>-<version>.extension-lock.json`）。签名放在发布清单里，安装侧读到的签名与它校验的制品是同一份事实，不会出现「清单和签名资产各说各话」。

发布命令（UI 在扩展开发工作区，同一实现）：

```powershell
himind-agent extension distribution preview  <kind> <id> <version>   # 只读，看会发到哪里
himind-agent extension distribution publish  <kind> <id> <version> --yes
```

制品签名使用与扩展仓脚本同一套环境变量：

```powershell
$env:HIMIND_EXTENSION_SIGNING_PRIVATE_KEY_PATH = "C:\keys\himind-extension-private.pem"
$env:HIMIND_EXTENSION_SIGNING_KEY_ID = "himind-production-2026"
```

两者都配置时，发布的制品带 RSA-PSS/SHA-256 签名，签名写进发布清单的 `signature` 字段（`signature_key_id`、`signature_algorithm` 同处一行记录）；未配置时按未签名发布，`preview` 会显示 `未配置私钥，按未签名发布`。私钥只在本机读取，不写入状态、不上传、不记日志。

安装侧的口径一致：发布清单里出现 `signature` 就必须验签通过，否则整个安装失败并回滚；没有 `signature` 时是否放行由 `HIMIND_REQUIRE_SIGNED_EXTENSIONS` 决定，默认与 Agent 更新一致——内嵌了生产公钥就要求签名，未内嵌（开发构建）只做「有签名就校验」。自建分发可显式设 `HIMIND_REQUIRE_SIGNED_EXTENSIONS=false` 关闭要求。受信公钥优先取 `HIMIND_TRUSTED_SIGNING_KEYS_DIR/<key-id>.pem`，其次取内嵌公钥。

```powershell
himind-agent extension distribution plan   <repository> <tag> <id> <version>            # 只读依赖与来源
himind-agent extension distribution install <repository> <tag> <id> <version> --dry-run  # 下载 + 摘要 + 验签，不落盘
himind-agent extension distribution install <repository> <tag> <id> <version> --yes      # 真正安装
```

发布清单本身有契约文件 `contracts/agent-core/v1/extension-release-manifest.schema.json`，发布侧写、消费侧读，字段漂移会在测试里暴露。

## 本地存储与保留策略

安装物落在 `%LOCALAPPDATA%\HiMindAgent`（`HIMIND_AGENT_HOME` 可整体覆盖，开发档位落在 `profiles/<name>`）：

| 资产 | 目录 | 布局 |
| --- | --- | --- |
| 插件 | `plugins/<plugin-id>/` | `versions/<version>/` 原件，`current` / `previous` 是运行副本 |
| 技能 | `skills/managed/<skill-id>/` | `versions/<version>/`，`current.json` / `previous.json` 是指针 |
| 工作流 | `workflows/<workflow-id>/` | 同上，指针写在安装元数据里 |
| 状态 | `data/` | 扩展锁 `extension.lock.json`、来源记录 `extension-provenance/`、工作区租约 `workspace-leases.json` |

保留策略：

- 插件与技能只保留 `current` + `previous` 两版：安装提交后立即收敛，之后不再更新的资产由启动巡检兜底（`sweep_plugin_versions`、`sweep_skill_versions`）。
- 技能额外保留渲染收据引用的版本，避免删掉软链接的落点；读不出 `previous` 时整体跳过——宁可留着旧版本，也不动回退要用的那一份。
- 工作流保留全部已装版本，历史版本目录就是版本列表，回滚按版本号直接切换，不做收敛。
- 暂存目录分两种口径：安装期只清自己刚建的（`TransientPolicy::Remove`），后台巡检只清超过一小时的残留（`RemoveStale`），避免和并发安装抢文件。
- 卸载同时清扩展锁、来源记录和本地目录；删除来源时按 `asset_kind` 反查，把该源装出来的资产记录一并清掉。

插件为什么留原件和运行副本两份：`versions/<version>/` 是发布制品的不可变原件，`current` / `previous` 是插件自己的运行目录，插件会往里写状态。分开之后回滚、卸载、重装都不需要改动原件。代价是磁盘占用翻倍（实测软件分发插件 59.7 MB 原件对应 59.6 MB 运行副本），这是刻意取舍；换成目录联接需要重新处理运行中的可执行文件与联接清理顺序，属于该实现最容易踩坑的部分，暂不做。

来源记录（`himind-agent source provenance`）不是审计留痕，而是自动更新的依据：`reconcile_auto_updates` 靠它判断某个资产由哪个源装出来、该不该跟着源更新。CLI 与 MCP 都可读取，卸载时会清理对应条目。
