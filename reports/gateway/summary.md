# B03/B04 SSE 完整事件定向修复

## 结论

本轮修复 `relay_stream` 的单行提前转发路径：以空行作为事件边界，按 SSE 规则去除字段值首个空格、用换行合并全部 data 字段，再判定错误及终止标记。完整分类前不向客户端写出该事件的字段。事件缓存上限 1 MiB；未完成、不可解析 JSON、超时、超限及 I/O 故障均不回退到原文转发。支持 LF、CRLF 和网络分块。

有效 JSON 规范化为单个 data 字段后传给现有原生清洗器和四条互译适配器，保持解码载荷及 usage 语义；拒绝错误不依赖虚构密钥正则。写出前安全 502，写出后安全 SSE error；不重放、不生成成功结束标记、不把不完整 usage 标成完整。

## 可复现与证据

运行 `python scripts/remediation/gateway-multidata-check.py`，隔离测试目录为 `reports/gateway/multidata-test-home`，设置 RUST_MIN_STACK=16777216、RUST_TEST_THREADS=1。脚本实际执行两条指定 cargo 测试、整个网关测试集以及两条 result.json 断言，保存真实输出及退出码。最终状态以 `result.json` 为准。

- `multidata-boundaries.log`：保留的真实 loopback 边界回归。
- `multidata-errors.log`：新增 `remediation_stream_multidata_error_no_leak`，覆盖三种原生及四条互译路径、首事件/正常事件后错误、LF/CRLF、7 字节网络写入及多个事件同块、EOF 未结束、有效多行事件及事件超限；检查实际客户端响应和捕获日志的完整密钥与秘密片段，检查安全 code/request_id、usage 不完整和不重放。
- `multidata-suite.log`：全网关回归，包含旧的单行错误、401/403/500、HTML 限长、完整/截断/超时/超限/I/O/已写流故障。
- `multidata-result-check-1.log`、`multidata-result-check-2.log`：两条必需 JSON 断言。

宿主历史事件 mission_1794473598404_ffe1e069:455 曾记录原报告检查退出码 0，但安全核验失败，且没有 TAP 总数。历史结果不作为本轮执行证据；本轮日志是 Rust 测试输出，不编造 TAP 数量。旧 gateway-check.py 与 patch/finalize 脚本仅属上一轮材料，不用于本次复现。

## 限制与下一步

仅表示本任务本地交付就绪，不代表 Mission、正式 VERIFY 或部署通过。未访问生产服务；未逐互译路径注入确定性的下游 TCP reset；CR-only SSE 方言不在此次修复范围。Provider relay 只接受可分类 JSON 或协议 [DONE] 数据，不可分类正文安全失败。

下一步：将 result.json、源码及本轮原始日志交给控制端，由宿主另行正式 VERIFY；部署仍由控制端负责。
