# AI 服务架构专项审查（修复交付）

## 结论与问题模型

本报告只修复审查交付，不整改产品源码，不自行宣布 Mission 或正式 VERIFY 通过。原报告实际为 `ai.json={}`、`ai.md` 不存在，无有效文字或引用可复用。worker 首次执行原命令 `python contracts/check.py ai` 返回退出码 **1**，实际 traceback 指向 check.py:15 的 `KeyError: 'area'`。因此本次实际补齐完整契约字段、六项 coverage、带原文的五项发现和数据流说明，而非增加失败声明或降低检查标准。

约束：仅写本专项两个报告；仅定向读取授权源码与契约；不联网、不读取真实凭据、不构建或修改程序、不修改检查脚本。保护目标按顺序为配置一致性、秘密不跨客户端边界、故障可定位与可恢复、预算计量可信、团队可分期交付。confirmed 表示静态代码行为有原文依据，并不表示已经做了运行复现。每项后果均依赖明确触发条件。

## 现状组件、所有权和数据流

```text
服务编辑/导入
  -> store/ai_services.rs
     ai-services.json: 服务账号+加密凭据+endpoints+模型/媒体能力目录
     ai-service-selection.json: capability_routes 有序主选/备用与模型覆盖
  -> 客户端配置投影 -> 本机URL+本机token（不是上游真实Key）
  -> app/inference_gateway.rs 环回监听
     token/模型/协议授权 -> 并发槽 -> 兼容矩阵/媒体规格化
     -> 同Provider+同上游模型+同协议候选切换/瞬态重试
     -> Chat / Responses / Anthropic 上游与相应协议互译
     -> 响应模型重写/流式扫描 -> 客户端
     -> store/local_usage 请求级台账

Mission 指派 -> assignment 校验职责/专家摘要/执行器/服务模型组合
  -> runtime/builtin AIRuntimeAdapter -> deepseek_harness 进程边界
     -> builtin_ai_router 独立进程寿命URL+token
     -> 每请求服务目录解析 -> 同一推理网关实现 -> 上游
  -> run级session投影聚合usage -> mission_events(run_usage_recorded)
  -> 预算门控 -> waiting -> request_id+计划哈希保护的预算扩展 -> active
```

* **目录不是运行健康证明**：`store/ai_services.rs:34-74` 定义 openai-chat、openai-responses、anthropic；135-220 明确服务是账号与凭据、endpoints 是事实源，按协议族去重；1092-1095 将旧标量投影跟随首端点。`builtin_ai_router.rs:64-131` 从已配置模型建立 `himind:服务:模型` 别名和默认选择；并未证明远端模型在线。服务默认模型与能力路由固定模型应区分，99-110 按聊天路由首项求默认别名。
* **路由不是无限回退**：`inference_gateway.rs:1530-1614` 先按本机令牌和模型授权，筛选同 Provider、同协议、同上游模型；1001-1055 对401/403/408/429/指定5xx及网络错误切换下一候选。不能把该实现描述成任意跨供应商/跨模型自动降级。写出前可切换，写出后无缝续流本次未验证。
* **协议边界确实有互译**：文件头“P1只直通”的历史说明不代表当前全部实现。1598-1605 列出 Anthropic→Chat/Responses、Responses→Chat、Chat→Responses；2390-2417 展示规格化后 Chat 请求发送，3432-3473 用客户端/供应商/模型矩阵规格化并记录动作。兼容回退不同于服务故障切换，不保证语义完全不损失；能力观测也不能替代真实端到端协议测试。
* **客户端不等于执行器**：客户端持有本机推理入口；执行器负责进程、工作目录和事件契约。`runtime/builtin.rs:14-63` 用适配接口隔离引擎，`builtin_ai_router.rs:41-61` 为 DSH 启动独立环回路由。`mission/assignment.rs:5-25` 校验专家版本/摘要，29-75 验证可用执行器及模型/service_id组合；ACP 的空服务/模型可保留客户端原模型。模型绑定不是文件或工具执行授权，专家引用亦不是权限提升。
* **凭据与计量是不同数据面**：`credentials.rs:571-596` 封装当前用户保护/解保护；`ai_services.rs:1082-1087` 空Key更新保留旧密文。`inference_gateway.rs:4253-4265` 白名单请求头不透传本机认证，3933-3948 日志摘要先脱敏。但客户端错误体另有边界缺口（AI-003）。4505-4536 为请求级台账记录 request_id、绑定、上游模型、usage_unreported；不能将它等同 Mission 预算已准确入账。
* **续跑保留现有保护**：`runtime/deepseek_harness.rs:1499-1517` 从 run 级目录读 usage，`mission/checks.rs:99-132` 包含 cache 读写 token。`mission/store.rs:973-1049` 事务记录并按 run_id 查询去重；435-528 对预算扩展做 request_id 重放摘要、计划哈希、waiting、无运行任务检查，事务更新预算及事件。币种为空时成本门控保守阻断（1794-1803），不应误报为“零成本可续跑”。本次没有执行真实任务/重启/预算扩展。

## 确认问题与可复核原文

以下行号均相对当前工作区 `source/`，JSON含机器可检查的对应行段及原文。严重度由影响与触发条件决定，不按文件大小或模块数量判断。

### AI-001 / P1：服务目录非原子覆盖

证据 `source/src/store/ai_services.rs:561-568`：
```rust
fs::write(path, serde_json::to_vec_pretty(&persisted)?)?;
```
`528-534` 的读取为：
```rust
let persisted: BTreeMap<String, PersistedCustomAIService> =
    serde_json::from_slice(&fs::read(path)?)?;
```
保存路径1115-1116先插入内存目录再调用save_all。触发：覆盖写中断、部分I/O失败。影响：整份JSON不可解析可阻断目录及依赖路由，故P1；未声称当前已有损坏。改法：锁内完整读改写、临时文件写入刷新并原子替换、保留可验证上一版本及恢复事件。验收：每个写阶段故障注入，重启后至少可读旧或新版；并发保存不同服务不能互相丢失。不能仅“捕获错误后继续”。

### AI-002 / P1：删除服务与清路由不是一次提交

证据 `source/src/store/ai_services.rs:1182-1188`：
```rust
let removed = services.remove(id).is_some();
if removed {
    save_all(&services)?;
```
随后1193-1202才清理引用并执行：
```rust
write_selection(&selection)?;
```
触发：目录已写、路由读取或写入失败，或两步间终止。重试删除时removed=false，清路由逻辑不再执行。影响：默认/备用可悬空且重试不自愈，P1。短期清理操作无论服务是否存在都应幂等执行，加启动一致性检查与诊断；长期同事务或可恢复操作日志。验收：两步之间中断后重启再删除，悬空引用消失、其余候选顺序不变。只调换写顺序仍有反向不一致窗口，不能算根治。

### AI-003 / P1：日志脱敏没有覆盖客户端翻译错误

证据 `source/src/app/inference_gateway.rs:2423-2424`：
```rust
let raw_error = read_error_body(response);
let detail = String::from_utf8_lossy(&raw_error).to_string();
```
非400/422/404分支2476-2487构造并发出：
```rust
"error": { "type": "api_error", "message": detail },
```
对比3933-3936的日志摘要路径：
```rust
let redacted = crate::approval::manager::redact_message(text);
```
触发：Anthropic→Chat的401/403/500等错误体回显密钥、请求头或敏感内容。影响：秘密经客户端及其日志扩散，P1；本报告未接触真实凭据，未确认真实泄漏事件。建议统一公共错误DTO，只带安全代码/request_id/脱敏限长说明，直通和互译共用；用虚构秘密测试各错误码，确保响应和日志均不含原值又能追踪请求。

### AI-004 / P2：非流式响应吞掉读取错误

证据 `source/src/app/inference_gateway.rs:4279-4287`：
```rust
let _ = reader.read_to_end(&mut raw);
```
未超限就返回Ok(raw)。2338-2361仍发上游状态且JSON解析失败仅默认usage：
```rust
let usage = serde_json::from_slice::<Value>(&raw)
    .map(|value| extract_usage(&response_binding.protocol, &value))
    .unwrap_or_default();
```
4529按状态记账：
```rust
status: if status < 400 { "success" } else { "failure" }.to_string(),
```
触发：200响应头后body读取发生I/O错误、已读部分未超限。影响：客户端收到截断的“成功”体、台账状态失真，P2。改法：读取返回明确I/O错误/超限/完整三态，写出前失败回502；流式另外记录完成/中断原因，不只记录HTTP状态。验收：伪上游200后截断/超时，不出现静默成功；保留usage不完整标记和request_id。未执行这些故障测试。

### AI-005 / P2：Mission成本跨币种直接相加

证据 `source/src/mission/store.rs:1774-1786` 读取currency并只验证非空，之后：
```rust
estimated_cost += cost.unwrap_or(0.0);
```
1801按总额与max_cost比较；1028-1046累计所有运行成本，但返回总额的币种沿用：
```rust
currency: usage.currency.clone(),
```
触发：同一Mission多个运行上报不同且非空币种。影响：金额单位不一致导致限额判断和展示失真，P2。当前DSH聚合currency为空会保守阻断，所以不把混币风险断言为本次已发生账单错误。建议预算固定唯一币种或分币种记账、拒绝隐式比较；换汇若必须引入，应固定汇率版本/时间/精度并保留原币。验收：USD+CNY不作为单币总额比较，空币种仍阻断，预算扩展与usage重放保持幂等。

## 方案比较、演进路径与回退

1. **第一阶段：最小安全修复**。后端配置维护者负责AI-001锁内原子写及AI-002幂等清引用；网关维护者负责AI-003公共错误边界与AI-004读取错误类型。优先于大规模拆模块，复用现有atomic_file设施（selection写法见1945-1957），但原子替换本身不解决跨文件事务或读改写竞争。回退：保留版本化目录/路由备份和旧格式读取，不回退脱敏安全边界；出现新写路径问题应暂停编辑并显式恢复快照，而非无声覆盖。
2. **第二阶段：计量契约**。Mission/Runtime维护者共同定义币种、计量完整性、attempt与request关联，修复AI-005；保留现有计划哈希、请求幂等和空成本保守阻断。验收独立测试混币、缓存token、usage缺失、run重放、预算扩展重放、旧计划拒绝和进程恢复。不能用提高预算掩盖usage缺失。回退读旧事件时标记计量未验证，不能补成零成本。
3. **第三阶段：按证据决定存储演进**。比较继续双文件+恢复日志与迁入统一事务存储：前者改动小且兼容现有客户端，但恢复协议复杂；后者一致性边界清晰但引入迁移/回滚成本。先完成故障矩阵与并发测试，再以实际失败率和维护成本决定，不为“统一”而大迁移。双写期间必须有单一写所有者、版本号及校验，不能长期并存两个事实源。

观测要求：稳定request_id/run_id/config版本，安全的路由候选结果、transport_outcome、usage完整性、币种异常、恢复动作；不记录Key和完整请求体。团队交付时每个修改PR附原文对应的回归用例、故障注入预期及回退演练结果；本次报告只是这些工作的输入。

## 观察边界与检查记录

* 当前代码与安装界面分离：BRIEF说明安装版UI为v0.3.92（e630a08）。本专项未读取/引用截图作发现证据，故无安装版可见现象被当作当前源码缺陷。未做UI交互、键盘、窄屏、动画验证。
* 定向查找未找到`source/src/inference`目录或顶层inference文件，已采用实际`source/src/app/inference_gateway.rs`与必要调用链；不据此宣称全仓没有其他推理入口。
* 未运行产品单元测试、真实网络调用、ACP/Codex/DSH执行、故障注入、真实预算与重启演练。上面的验收都是下一步要求，不是完成事实。本报告没有需装外部技能的依赖。
* 宿主历史事件 `mission_1794443924791_2d612a63:293`：原命令退出1、216ms、output_sha256=`e07c8c4a5c2b9cd2c371e53472324a6f411cf59e3b78be74207fea60a29a8205`；未提供正文或数字TAP汇总。原review-ai运行超过900秒终止。不猜测历史失败条目，不把历史记录冒充worker执行。
* 本次修复前worker检查退出1（KeyError: area）。补齐报告后，worker实际执行 `python contracts/check.py ai` 成功，输出 `{"passed": true, "stage": "ai", "source_files_unchanged": true, "meaning": "Structural and evidence checks only; independent semantic review still required."}`；退出码0。未输出数字TAP汇总，不编造用例数量。检查仅验证源码清单、报告结构与引用匹配，不代表生产问题整改或正式独立VERIFY。

## 下一步

执行并记录原必需命令，若输出指出字段/引用错误则在这两个报告内修复；本阶段完成后由宿主另行正式VERIFY。后续产品整改按上述三阶段、负责人边界和可观察验收推进，不扩大本worker写入权限。
