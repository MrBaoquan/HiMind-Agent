# 引擎工程构建

用本机已安装的 Unity 或 Unreal 工具链构建工程，不经过任何云端构建机。

## 运行前提

- 本机装有对应版本的引擎编辑器；团队环境变量优先，其次取「设置 → 本机工具与技能 → 开发工具」里指定的路径，最后才扫描常规安装目录。
- 目标平台需要引擎侧已装模块：Unity 看 `PlaybackEngines`，Unreal 需要 `RunUAT`。缺模块时能力会直接报缺少的组件名。

## 输入

| 字段 | 说明 |
| --- | --- |
| 工程目录 | 必需。Unity 目录需含 `Assets/ProjectSettings`，Unreal 目录需含 `.uproject` |
| 引擎 | 默认自动识别，识别不准时手动指定 |
| 引擎版本 | 留空按工程自身声明匹配（Unity `ProjectVersion.txt`、Unreal `EngineAssociation`） |
| 构建方式 | 默认自动；`原生引擎` 强制走引擎 CLI，`项目脚本` 走 `.himind/build.ps1|cmd|bat` |
| 目标平台 / 架构 / 配置 | 传给引擎的构建参数 |
| 输出目录 | 留空使用工程默认输出 |
| 清理输出 | 勾选后先删除输出目录再构建 |

## 执行语义

这是一个单步骤工作流，步骤以 `wait=true` 调用 `exhibit.workspace.build`：
能力启动构建进程后阻塞等待终态，因此工作流运行态与真实构建一致，失败时日志尾部会带在结果里。

需要取消正在跑的构建时，用 `exhibit.workspace.build.cancel` 传 `job_id`。

## 依赖

仅使用 Agent 内置能力 `exhibit.workspace.build`，无插件、连接器与运行时依赖。
