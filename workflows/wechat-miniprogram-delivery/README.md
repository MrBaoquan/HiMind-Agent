# 微信小程序开发闭环

该 Workflow Package 使用真实小程序仓库、Git Candidate、Runtime Step、Capability、开发者工具或 `miniprogram-ci` 完成：

1. 需求和验收解析。
2. 工作区、依赖和工具链准备。
3. 内层开发 Loop：AI 修改、测试、构建和开发自检。
4. 冻结不可变 Git Candidate。
5. 预览和体验版上传，并绑定同一 Candidate。
6. 人工验收、组织审批、微信审核准备与提交。
7. 正式发布和条件回滚。

`workflow.json` 是包契约。微信平台能力由独立插件 `com.himind.wechat-miniprogram-tools` 提供，Workflow 只负责步骤 DAG、审批、Artifact 校验和 Run 投影。

上传私钥、AppSecret、平台 Token 和测试账号密码不得写入本目录，必须通过 Agent Credential Broker 或 Dashboard Managed Credential 注入。当前首版只接受本机 `private_key_path`，后续应替换为短期 Credential Handle。
