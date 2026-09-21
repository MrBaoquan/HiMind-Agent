# 微信小程序开发闭环

> 分发与创作事实源：`MrBaoquan/himind-extensions/workflows/wechat-miniprogram-delivery`。
> 本目录只作为 `himind-agent` 跨语言契约测试和离线解析快照，不单独发布。

该 Workflow Package 使用真实小程序仓库、Git Candidate、Runtime Step、Capability、开发者工具或 `miniprogram-ci` 完成：

1. 需求和验收解析。
2. 工作区、依赖和工具链准备。
3. 内层开发 Loop：AI 修改、测试、构建和开发自检；`rejected` 后暂停并记录用户反馈，下一轮 Runtime 读取 `last_user_feedback` 继续修改。
4. 冻结不可变 Git Candidate。
5. 预览和体验版上传，并绑定同一 Candidate。
6. 人工验收失败时由 `fail_when` 阻断后续步骤；通过后进入组织审批、微信审核准备与提交。
7. 正式发布和条件回滚。

`workflow.json` 是包契约。微信平台能力由独立插件 `com.himind.wechat-miniprogram-tools` 提供，Workflow 只负责步骤 DAG、审批、Artifact 校验和 Run 投影。

运行前 Preflight 会验证 `wechatide-skill`、独立插件、Connector、Runtime Provider 和本地工具。Skill 未安装或显式 Runtime Provider 不可用时，Run 不得启动；`runtime.provider=auto` 只要任一声明的候选 Provider 可用即可。

上传私钥、AppSecret、平台 Token 和测试账号密码不得写入本目录，必须通过 Agent Credential Broker 或 Dashboard Managed Credential 注入。Workflow 输入只保存 `credential_handles`，由 Capability Gateway 在调用前解析为本机文件或 Secret；真实路径和 Secret 不得进入 Run、Event 或 Dashboard Projection。

## 本地工程选择与配置

Workflow 将源码仓库和编译后的小程序工程分开：

1. `workspace_root`：Git 仓库根目录，Candidate 冻结、代码修改和开发 Loop 的事实源。
2. `source_root`：执行依赖安装、测试和构建的目录；通常等于 `workspace_root`，省略时自动回退。
3. `project_root`：包含 `project.config.json` 的微信编译产物目录。
4. `app_id`：必须与本次构建产生的 `project.config.json` 一致。
5. `build_script`：真实构建命令，例如 `build:dev` 或面向具体展馆的 `build:szkjg`。
6. `credential_handles.private_key_path`：Credential Broker 中的上传私钥句柄，不是私钥路径本身。

以 `F:\WebProjects\kerun_user` 的 MPX 多展馆工程为例：

```json
{
  "workspace_root": "F:\\WebProjects\\kerun_user",
  "source_root": "F:\\WebProjects\\kerun_user",
  "project_root": "F:\\WebProjects\\kerun_user\\dist\\wx",
  "app_id": "wx534ac49f5db83057",
  "build_script": "build:szkjg",
  "package_manager": "npm",
  "install_mode": "install",
  "scripts": ["lint"],
  "credential_handles": {
    "private_key_path": "wechat-upload-private-key"
  }
}
```

该工程的 AppID 映射位于 `scripts/venue-config.js`，构建脚本位于 `package.json`，服务器环境位于 `.env.local`。Workflow 的 `build_script` 必须选择同一展馆对应的脚本，避免 `project.config.json` 的 AppID 与 Workflow 输入不一致。

首次使用时可从 Agent 工作流的“启动前检查”中配置。检查结果会列出缺失的
`wechat-upload-private-key`，选择本地上传私钥文件后保存，并自动复检。

如需在脚本或其他自动化流程中注册上传私钥，可使用：

```powershell
himind-agent credential set-file `
  wechat-upload-private-key `
  wechat-miniprogram `
  <上传私钥文件>
```

仓库内提供了可直接修改后使用的输入样例：`examples/kerun-user.szkjg.input.json`。
