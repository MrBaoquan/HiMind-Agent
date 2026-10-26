# 插件凭据

插件经常需要用户自己的账号或密钥。凭据由 Agent 统一登记、保管、验证和按需下发；
插件目录不保存长期秘密，插件清单也不能凭借“声明”自动获得访问权。

## 清单声明

`plugin.json` 的 `credentials` 同时兼容旧字符串字段和新对象字段：

```json
{
  "credentials": [
    {
      "id": "demo.account",
      "label": "Demo 服务账号",
      "kind": "userpass",
      "required": true,
      "description": "用于 Demo 服务同步。",
      "docs_url": "https://example.com/help",
      "verify_capability": "demo.credential.check",
      "fields": [
        {
          "key": "username",
          "label": "账号",
          "type": "text",
          "secret": false,
          "required": true
        },
        {
          "key": "password",
          "label": "密码",
          "type": "password",
          "secret": true,
          "required": true
        }
      ]
    }
  ]
}
```

旧写法 `"fields": ["api_key"]` 仍可读取，并归一化为秘密密码字段。新清单应使用对象写法。

字段规则：

- `key` 在一条凭据内唯一，只能使用字母、数字、点、短横线和下划线；
- `secret=false` 的字段可显示为账号名、地址或租户等元数据；
- `secret=true` 的字段只保存 DPAPI 密文，UI 和 broker 状态接口都不回显；
- `type` 支持 `text`、`password`、`url`、`file`、`cookie`；
- 保存时，未在清单声明的字段会被拒绝。

## 存储与状态

- 密文落在 profile 的 `plugin-credentials.json`；
- 每条 profile 记录声明指纹；插件更新后若声明变化，状态变为“需确认”；
- 状态只暴露配置字段、公开字段、更新时间，不暴露秘密值；
- 插件卸载时清理其名下所有凭据；
- `credential-audit.jsonl` 记录 consumer、credential、capability、结果和时间，不记录任何值。

## 运行时 broker

外部插件按能力调用启动短生命周期进程。若插件声明了凭据且具备
`secret.broker.acquire` 权限，Agent 会为本次调用启动仅监听 `127.0.0.1` 的 broker：

```text
HIMIND_CREDENTIAL_BROKER_URL=http://127.0.0.1:<port>/acquire
HIMIND_CREDENTIAL_BROKER_TOKEN=<one-invocation token>
```

插件请求：

```http
POST /acquire
Authorization: Bearer <token>
Content-Type: application/json

{"credential_id":"demo.account","purpose":"demo.sync"}
```

`purpose` 必须等于本次调用的 capability ID。成功时返回公开字段和秘密字段；会话在插件
进程结束后立即关闭，token 不能跨调用复用。

Broker 同时校验：

1. 一次性会话 token 和截止时间；
2. plugin ID 与 capability；
3. 凭据 ID 是否由当前插件清单声明；
4. 保存时的声明指纹是否仍匹配；
5. 插件是否具备 `secret.broker.acquire`；
6. 请求次数上限。

## 信任边界

Broker 解决的是最小权限、按次下发、轮换、撤销和审计。它不是对恶意插件的强沙箱：
插件拿到值以后仍可能泄露。插件安装审查、扩展签名和组织策略仍是信任链的一部分。

共享账号、通用 OAuth 授权引擎和 secretless egress proxy 不在当前阶段范围内。工作流连接器
使用同一套 consumer 授权与审计语义，但由 Agent 在 workflow capability 执行前解析。
