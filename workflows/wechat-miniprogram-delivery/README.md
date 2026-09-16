# 微信小程序开发闭环

该 Workflow Package 使用真实小程序仓库、AppID、开发者工具或 `miniprogram-ci` 完成：

1. 需求和验收解析。
2. 工作区、依赖和工具链准备。
3. 页面、组件、API、资源和配置实现。
4. 测试、构建和预览。
5. 体验版上传和人工验收。
6. 组织审批、微信审核准备与提交。
7. 正式发布、监控和回滚。

`workflow.json` 是包契约。上传私钥、AppSecret、平台 Token 和测试账号密码不得写入本目录，必须通过 Agent Credential Broker 或 Dashboard Managed Credential 注入。
