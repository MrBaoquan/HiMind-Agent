# MCP 业务调用指南

## 运行边界

外部 AI 工具通常通过 `himind-agent-mcp` 的 stdio companion 接入。这个进程只启动 Agent 能力网关，不启动组织调度 Worker。因此看到以下状态属于预期：

```json
{
  "mcp_transport": "stdio",
  "local_service_expected": false,
  "dashboard_worker_state": "not_applicable",
  "dashboard_worker_expected": false,
  "dashboard_worker_online": false,
  "dashboard_worker_reason_code": "stdio_companion_gateway_only"
}
```

只有 Connected 模式的本地 Agent 应用服务才需要 Dashboard Worker。Worker 状态不影响 MCP 直接调用 Agent 本地能力，也不影响已授权的同步业务能力；需要组织控制面时，Agent 会单独报告 `control_plane_required` 或授权错误。

外部工具应按 `dashboard_worker_expected` 判断是否需要诊断 Worker：`false` 表示当前进程不托管 Worker，直接忽略 `dashboard_worker_online`；只有 `true` 时才根据 `dashboard_worker_state`（`connecting`、`online`、`offline`）判断连接状态。`dashboard_worker_reason_code` 是机器可分支的原因码，避免依赖日志或产品名称猜测运行拓扑。

`initialize` 的 `_meta.himind.runtime` 会直接给出本次 companion 的结构化运行事实，包括 `mode`、`dashboardEnabled`、`transport` 和 `controlPlane.enabled`。`InvocationSource`（例如 MCP 客户端）与 `transport` 是两个独立维度；客户端应读取 `transport`，不要仅凭调用来源推断 stdio。

健康结果中的 `runtime_schema_version=1` 和 `dashboard_worker_online_semantics=legacy_boolean_use_expected_state` 表示旧的 `dashboard_worker_online` 仅为兼容字段；新客户端应以 `dashboard_worker_expected`、`dashboard_worker_state` 和 `dashboard_worker_reason_code` 为准。

## 展项调用顺序

展项有两个不同语义的标识：`pid` 是业务 API 路由 ID，`exhibit_id`（例如 `EX-0021`）是给人看的展示编号。后续展项读取、人员、需求、关联和工作区能力统一使用 `pid`，不要把 `EX-xxxx` 当作参数。

```text
1. business.project.list 或 context.resolve
2. business.exhibit.list(project="深圳科技馆")
3. 从返回项读取 item.pid
4. business.exhibit.get(exhibit_id=item.pid)
5. business.people.search(q="郑怡媛")，读取 person.id
6. business.exhibit.crew.append(
     exhibit_id=item.pid,
     add_developer_user_ids=[person.id]
   )
7. 再次 get/list 回查结果
```

如果误传 `EX-0021`，Agent 返回结构化错误：

```json
{
  "code": "EXHIBIT_ROUTE_ID_REQUIRED",
  "field": "exhibit_id",
  "display_id": "EX-0021",
  "hint": "先调用 business.exhibit.list 或 context.resolve，使用返回项的 pid"
}
```

这个错误是参数语义错误，不是 Dashboard Worker 离线。不要通过猜测名称或顺序自行替换 ID。
