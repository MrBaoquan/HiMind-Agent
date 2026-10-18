# 插件凭据（S1 已实现 / S2 契约）

插件经常需要「用户自己的账号或密钥」（API key、OAuth、Cookie、账号密码）。
这些值**不应该**散落在各插件目录的配置文件里：那样无法轮换、无法审计、插件一升级
就可能覆盖用户填过的值，卸载后还会留下孤儿秘密。

## S1：声明 + Agent 统一保管（已实现）

- 插件清单声明需求（`plugin.json`）：

  ```json
  "credentials": [
    {
      "id": "media.cookies",
      "label": "下载站点 Cookie",
      "kind": "cookie",
      "fields": ["cookie"],
      "required": false,
      "description": "需要登录才能下载的站点：把浏览器里的 Cookie 粘到这里。",
      "docs_url": "https://example.com/help"
    }
  ]
  ```

- Agent 侧统一录入（插件详情 → 凭据），值以 **DPAPI 按当前 Windows 用户加密**落盘
  （`profiles/<profile>/plugin-credentials.json`），**按插件 ID 隔离**。
- 界面与插件只能看到「配没配 / 配了哪些字段 / 更新时间」，**永远不回显明文**。
- 删除即失效；插件卸载时调用 `remove_all_for_plugin` 清掉它名下的条目。
- 内核侧取值入口 `store::plugin_credentials::secret_fields`（仅供内核/未来 broker）。

## S2：插件如何真正拿到值（待实现，两个候选）

**方案 A（推荐）：反向 broker 调用**

插件运行期向 Agent 申请短期凭据：`secret.broker.acquire(credential_id, purpose)`。
Agent 校验「该插件是否声明 + 是否获授权 + purpose 是否在范围内」后，返回**短期令牌或
一次性值**；每次取用写审计（谁、哪个插件、哪条凭据、何时）。

- 前提：插件运行时目前是「Agent → 插件」单向 JSON-RPC，**没有反向通道**。需要新增：
  1. Agent 启动插件时注入一次性会话令牌（环境变量，进程级）；
  2. Agent 暴露只监听环回的凭据端点（`127.0.0.1:<port>/plugin-credentials`），按令牌
     认证、限流、只允许该插件自己的凭据；
  3. 插件 SDK（`sdk/jsonrpc`）加一个 `acquire` 客户端与缓存（只缓存短期令牌）。
- 优点：最小下发、可审计、可轮换、插件不落盘；缺点是协议要加一层。

**方案 B（过渡）：启动时一次性注入**

Agent 在 spawn 插件进程时，把**已授权且插件声明过**的字段解密后注入进程环境变量
（如 `HIMIND_CRED_<ID>_<FIELD>`），插件启动时读取。

- 优点：零协议改动、插件只读环境变量；缺点：环境变量对整个进程树可见、只能启动时
  更新（轮换需重启插件）、审计粒度只到「本次启动」。
- 仅建议作为短期过渡，且必须：只注入声明过的字段、每次启动写审计、日志里绝不打印值。

## 落地顺序

1. 选一个**真正需要用户密钥**的插件（目前仓库里没有：微信小程序读的是项目自带的
   `project.config.json`，GitHub 令牌由 Agent 自己管理），先确认需求真实存在；
2. 实现反向 broker（方案 A）+ 该插件的读取改造；
3. 补审计视图（凭据 → 使用方/最近取用）与轮换策略。

在此之前，S1 的「声明 + 统一管理 + 状态可见」已经能解决「配置散落、无处审计」的问题，
并且不会把明文交给任何插件。
