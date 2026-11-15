设计结论（direction）：延续克制、紧凑的本机配置工具。保守使用场景是反复管理 AI 服务与工具接入的本机操作者，首先扫描可运行/需行动数量与健康目录可信度。记忆点是同一工具身份快照的并列数字。正面对标 VS Code Settings 的紧凑设置行；本产品保留服务能力、分发和健康状态的项目既有行结构。本轮已核实并复用 B06/B07/B10/B11/B12/B13，没有发现必须新增源码修复的缺口，不将既有改动算本轮新增成果。

结构与状态（state_coverage）：五页签 id、四向箭头/Home/End 和深链保持；父页唯一可见 acp tabpanel。服务页主操作为新增 AI 服务，表单沿连接→能力与模型→高级与用量推进。工具按 id 去重，可运行与需行动可重叠；冲突先看 diff，不自动同步。健康目录加载、错误、部分可用、成功、无权限均有实测，失败时全量两项（对话及仅语音）配置仍在；初始空与筛选空分开。表单和确认复用 ModalSurface，服务表单不安装 document 级 Tab 监听；嵌套焦点、Esc、返回触发点、忙态、预设 radio/pressed/current、跨步错误定位及非密钥内容保留已实测。默认来源说明保持作用域，不改模型解析或数据契约。

取值与依据（token_values）：项目令牌优先，字号 12/13/14px，间距 4/8/12/16/24/32px，圆角 4/6/8/10px，沿用 shadow-3（0 12px 24px rgba(17,24,39,.10), 0 24px 48px rgba(17,24,39,.12)）。局部容器 <480px 摘要动作下排。正文阈值 4.5:1、输入边界/焦点 3:1，命中区至少24px、预设至少32px，焦点轮廓2px、偏移2px；减少动效为 opacity 80ms，无位移/缩放动画，不新增缓动。当前表单正文采样最小5.4417:1、输入边界最小3.6579:1。依据为固定任务契约、backlog、项目 styles/ServiceUi/ModalSurface；未加载宿主未固定外部技能，不声称整站 AA。

验证（render_evidence）：本任务原 ci、browser、build 实际退出码0/0/0，原始日志及运行目录见索引。Chrome 157.0.8103.50 加载生产 AiConnectionsPage 与子组件，API 是虚构隔离数据，不冒充真实后端。320/390/480/620/880px、长 URL、50% 文案膨胀、200% CSS zoom 的真实指针/键盘、五 tab、唯一 acp、模态/预设/错误定位、计算字号、合成色对比度及降级态均有行为断言和41张截图，pageerror为空。另以评审立场目视查看 services-320、expanded-form-320、zoom-200-pointer、full-service-list-denied：摘要按钮下排，表单底部动作可见，放大后的错误焦点位于视口，健康无权限仍留两项服务。本轮没有新实现，不自签独立验收。B13 UI-006 先行实测及既有回归目录保持原样，旧失败与引用未删除。

失败线索与来源：本轮只读 MCP 取得 :559/:566/:573/:589 原事件及 delivery-8 local-run。按时间区分：:559 为0/1/1/1/1，:566、:573、最新:589 均为0/0/0/0/0；各五项 test_summary 均是空字符串，没有数值 TAP 总计。事件只给输出哈希，无原始 stdout 或运行目录。delivery-8 仍是 worker exit=1、cli_error_event=error、error_notifications=6、terminal_event=turn.failed、completed=false、final_observed=true、failed=true；runtime_report=null、step_evidence为空。另观察到 Dashboard projection 的 HTTP 400 invalid json，只记录为独立线索，不能据此定因。业务检查全绿与 worker 失败并存，不归咎于等待 VERIFY 或历史捕获长度问题。按固定 personal.codex/remediation.frontend 绑定执行，未采用旧目标中冲突的 himind.builtin 指示。

固化顺序：首次固化时 passed=true 只表示待结果检查确认的业务阶段状态；随后原两项结果检查已实际退出0/0，本任务五项初轮退出码为0/0/0/0/0。结果检查日志写宿主资源，确认后显式更新阶段报告，再固化完整候选并执行全部五项原命令；重跑前后文件集合与SHA-256快照仅写外部资源。最终重跑结果以索引 final_read_only_replay_evidence 为准，检查入口不会自动改写报告。成功、启动异常及1ms超时均检查 cleanup；异常/超时为预期非零，浏览器和服务已关闭、profile已清理、证据保留，局部候选快照无变化。

假设与风险（unverified）：具体重复使用场景未另行提供，保守延续现有工具密度，不代替产品改优先级。200%实测是CSS zoom而非浏览器工具栏缩放；深色系统偏好下保留项目固定light，未新增暗色主题。真实读屏语音、真实后端/凭据、部署未验。依赖4项high advisory及既有大chunk警告保留披露。本任务证据全部写入分配资源根，只有本交付报告写获准reports路径。两保护文件只读核查：dispatch哈希匹配最后已记录宿主integration；provenance文档在原始基线无条目且未跟踪，正式归属/基线交宿主裁定，不声称protected_file_check通过。本地比较只覆盖授权候选范围，不冒充host:candidate-integrity。正常返回供宿主冻结及正式VERIFY；本阶段不执行、不等待正式VERIFY，不宣称Mission、跨执行器成熟度或部署通过。


更正轨迹（repair-service-ui-delivery-11，2026-11-13）：保留以上 repair9 原文作为历史记录。“没有发现必须新增源码修复的缺口”未覆盖普通/发布构建，不能延伸为发布兼容结论。本任务在实际资源根源码副本复现缺变量时 createRuntime 前置失败（exit=1），并复现第六项因 service-ui-build-modes.mjs 缺失而无法启动（exit=1）。已在授权 frontend/scripts 内实现普通/显式双模式并新增三模式回归，原 TS7 公开 package.json/bin.tsc 解析、依赖与浏览器断言保留。当前证据与源码差异见 ../repair11/evidence-index.json 和 ../repair11/source-diff.patch；历史 :619 只有摘要/哈希，不将本次根因倒推为其已证实原因。


更正轨迹（repair-service-ui-independent-qa-1，2026-11-13）：保留 repair9/repair11 原文。原“未发现必须新增源码修复”也未覆盖原 service 浏览器入口 Refresh 初始化缺口。本阶段将全部原场景保留到原 service 入口并显式 preamble，原候选入口直接成功；原生 Chrome 200% 与产品 NVDA 语音仍未验证，不能沿用历史 passed。本次 passed=false，原因与工具阻断详见 ../independent-qa-1/delivery.md，历史日志未删除。
