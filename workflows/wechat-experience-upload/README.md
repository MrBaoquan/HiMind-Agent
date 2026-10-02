# 微信小程序体验版上传

按下发展馆和服务器环境，构建 `kerun_user` 小程序并把产物上传成微信体验版。
工作流只做构建与上传，不包含 AI 开发环节。

启动时通常只需要确认两件事：

1. 展馆（默认邯郸成语博物馆）。
2. 服务器环境（默认开发环境）。

版本号、说明、工作区目录、上传通道都有默认值，留空即可。

执行步骤：

1. 按展馆和环境执行真实构建（`node scripts/wx-cli.js build --env <venue> --server <environment>`）。
2. 冻结 Git Candidate。
3. 校验目标产物与上传环境。
4. 按上传通道上传体验版并记录 Artifact。

## 启动参数

启动表单只展示展馆与服务器环境，其余都在「高级」里且已带默认值：

- `workspace_root` 默认指向 `F:\WebProjects\kerun_user`，换仓库时改这一项。
- `source_root` 留空等于 `workspace_root`，构建脚本在它下面执行。
- `project_root` 留空按 `source_root\dist\wx` 推导，它必须包含 `project.config.json`。
- `version` 留空按 `1.YY.MMDD` 生成，口径与仓库 `scripts/wx-cli.js` 一致。
- `description` 留空生成「版本号 + 上传时间」。
- `upload_channel` 默认 `auto`：本机能读到上传私钥就走 CI 直传，读不到才用开发者工具。

## 上传通道与人工确认

两条通道的差别只有一个：**要不要有人在开发者工具窗口点一次「确认」**。

- `ci`：用上传私钥直传，全程无人值守。私钥由 Agent Credential Broker 保存，
  Workflow 输入只保存句柄：

```json
{
  "credential_handles": {
    "private_key_path": "wechat-upload-private-key"
  }
}
```

- `wechatide`：不需要私钥，但上传确认发生在微信开发者工具窗口内，Agent 无法代按。
  未确认时步骤如实返回「待确认」并记下任务号，**不会**写成上传成功；
  下次重跑会先续等同一任务，不会重复发起上传、也不会重复弹确认框。

想彻底免人工，就在插件配置里登记上传私钥（微信公众平台「开发管理 → 开发设置 →
小程序代码上传」生成的密钥文件），通道保持 `auto` 即可自动走 CI。

本地输入样例见 `examples/kerun-user.szkjg.input.json`（只覆盖展馆，其余走默认值）。

## 版本

1.4.3：启动参数收敛到「确认展馆与环境」，上传通道默认自动（有私钥走 CI 直传），
引入插件 0.3.19 的通道自动推断与待确认续等。

1.4.0：移除 DSH 开发环节，工作流只负责构建与上传；启动参数补齐默认值，
版本号与说明留空自动生成，通常确认展馆即可启动。

1.3.1：依赖锁改为按依赖制品的载荷摘要计算，组织分发安装不再把已安装的
`com.himind.wechat-miniprogram-tools` 判成内容变更。

1.3.0：从官方扩展仓迁入本仓，改由 `MrBaoquan/himind-ext-projects` 分发。
