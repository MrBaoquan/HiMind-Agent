# 内置插件架构与升级约定

本文记录 Agent 内置插件的唯一身份、版本和升级边界。它约束扩展开发工具、软件分发工具以及后续新增的内置插件。

## 1. 一个插件 ID 只有一个家

- 内置插件源码在 `HiMind-Agent`，随 Agent 二进制发布。
- 外部插件源码在 `himind-extensions` 或其他扩展仓，走独立的构建、签名和发布流程。
- 同一个插件 ID 不允许同时存在内置和外置两套实现。
- 注册表按 ID 去重，`source=builtin` 的条目恒定胜出；外部同 ID 副本不加载。
- 安装入口（本地导入、目录安装、分发策略）在落盘前拒绝外部同 ID 副本。

这样能力归属、停用/卸载语义和升级路径才可判定；否则同一个 ID 会出现两个实现，调用方无法知道真正执行的是哪一份。

## 2. 版本不是 Agent 版本

内置插件有两个独立版本概念：

- `PluginRegistryItem.version`：插件对外契约版本，例如扩展开发工具 `1.4.3`、软件分发工具 `1.3.0`。
- `crate::VERSION`：Agent 自身版本，例如 `0.3.75`。

Skill/Workflow 的 `min_version` 比较的是插件契约版本，不是 Agent 版本。新增或修改内置能力时：

1. 在对应模块里定义插件契约版本常量。
2. 注册能力时使用 `registration_versioned`，并把同一个版本写进能力描述。
3. 更新 `docs` 和对应测试，避免注册表版本与网关版本漂移。

## 3. 能力注册必须与插件清单同时存在

只在 `builtin_plugin_items()` 里写能力 ID 不够。能力网关还必须注册同名能力，否则 UI 会显示“插件存在但没有 AI 工具”，MCP `tools/list` 也拿不到工具。

当前扩展开发工具注册 12 条能力：

- `extension.environment.preflight`
- `extension.plugin.scaffold/validate/build/package`
- `extension.skill.scaffold/validate/package`
- `extension.workflow.scaffold/validate/build/package`

软件分发工具注册 4 条能力：

- `distribution.project.inspect`
- `distribution.artifact.inspect`
- `distribution.channel.resolve`
- `distribution.release.resolve`

发布能力 `distribution.release.publish` 与 GitHub 候选 `distribution.release.publish.github` 由 Agent 内核单独提供。

## 4. 构建不能执行真实业务能力

候选插件的运行时契约测试只验证传输层：

- 能启动入口进程；
- 能接收一行 JSON-RPC；
- 能返回一行 JSON 响应；
- 能正常退出。

它不执行插件声明的网络、写文件或长任务能力。否则 `tech-radar.collect` 这类 300 秒网络能力会把“打包”变成不可控的长任务。

## 5. 升级槽（后续）

当前内置插件随 Agent 二进制升级，外部同 ID 副本不允许安装。后续若要支持独立升级，建议采用 Unity/VS Code 式“官方签名槽位”：

- 只允许官方签名、受信 key ID 的升级包；
- 升级包仍占用同一个插件 ID，不产生第二个注册表条目；
- Agent 始终保留内置兜底版本；
- 升级包失败时回滚到内置版本；
- 升级包必须通过能力契约版本和权限校验。

在实现升级槽之前，内置插件的发布节奏就是 Agent 的发布节奏。

## 6. 停机检查

修改内置插件后至少运行：

```powershell
cargo fmt --check
cargo check --locked
cargo test --bin himind-agent builtin_
cargo test --bin himind-agent extension_
cd frontend && npm run check:ui && npm run build
```

页面验收至少确认：

- 插件列表同一 ID 只有一条；
- 内置条目显示“系统内置”；
- 没有卸载/停用入口；
- 能力数量与注册表一致；
- 外部同 ID 安装被拒绝。
