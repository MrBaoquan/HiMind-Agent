# repair-service-ui-delivery-4：阶段交付未就绪

## 设计结论（direction）

保守场景：本机操作者反复管理 AI 服务、默认来源和工具接入；优先扫到服务数量、健康目录可信度及需要处理的工具。延续克制、紧凑的本地设置界面，不新增风格或依赖。记忆点仍为同一快照的可运行/需行动并列数字。正面对标为项目已有设置行，而非外部着陆页。

本轮主要修复检查副作用，不重做界面。另依据受保护 provenance 文件中既有宿主观察，修复窄服务摘要被按钮挤成约 80px 列的实际缺口：给服务组件自己的容器设置小于 480px 的上下布局，不再依赖外层 agent-main 容器存在。本次未运行浏览器，因此不声称修复已获得渲染验收。

## 结构与状态（state_coverage）

保留五页签和深链，父页持有唯一 acp tabpanel。AI 服务页主操作仍是新增 AI 服务；健康失败时只局部重试，不清空对话或媒体配置。表单保留连接→能力与模型→高级与用量、跨步错误定位、非密钥数据、预设原生 radio、ModalSurface 嵌套焦点/Esc/返回/忙态。

加载、初始空、筛选空、错误、部分可用、成功、无权限均保留原生产实现及原浏览器断言，没有删除或以静态断言替代。B06/B07/B10/B11/B12 的源代码复查与历史证据不能代替本次真实浏览器回归。B13 增加摘要可读宽度和动作上下布局断言，覆盖五宽默认/膨胀文案，以及 880px 视口内 296px 的局部容器。

## 取值与依据（token_values）

- 延续事实/备用模型 12px、名称 13px、局部标题 14px；间距 4/8/12/16/24/32px；不改变现有圆角和阴影档位。
- 窄摘要断点为局部容器最大 479.98px，内边距复用 space-3（12px）。动作放在摘要下方；浏览器新增 copy 宽度至少 min(200px, 摘要总宽−76px)、无横向裁切、窄容器动作位于主内容下方的真实几何断言。
- 保留正文 4.5:1、输入边界/焦点 3:1、24px 最低命中区、预设 32px 的原浏览器检查，不声明整站 AA。减少动效继续只允许 opacity 过渡，时长 80ms；不修改现有缓动令牌。
- 依据优先级：本任务契约及输出授权→项目本地组件/CSS→既有 backlog。没有调用外部设计数据或引入新设计体系。固定专家方法与权限冲突时以任务契约为准；本地技能操作说明只用于核对方法，不作为新授权。

## 输出隔离

新运行入口只接受 SERVICE_UI_RUNTIME_ROOT 指向的、宿主已明确授权且预先存在的独立绝对目录。缺失、相对路径、工作区内目录、工作区祖先、规范化后指回工作区的路径均失败关闭；不推定 TEMP/TMP、OS 全访问或旧报告中的绝对路径为授权。

浏览器截图/JSON/失败材料、Vite 优化缓存、浏览器 profile/download 临时目录和构建 dist 均放在该根下各次唯一运行目录。构建复用现有 vite.config.ts 的配置对象，通过 configFile:false 避免临时配置包落入候选；不修改 vite.config.ts。原五项命令没有变化，不需要新命令参数。

scripts/remediation/service-ui-checks.py 为入口配置适配器：在获得目录授权后，设置 TEMP/TMP/TMPDIR、npm_config_cache 和 npm_config_update_notifier，再原样运行五项命令。旧 service-ui-delivery3.py 委托此入口，不再自动固化 result.json 或写入 repair3。直接由宿主启动 npm 命令时也必须预先设置同等环境；npm 自身在 Node 入口运行前就可能写 cache/log，不能靠 npm lifecycle 钩子事后补救。没有修改 npm ci 的语义、候选选择规则或宿主代码。npm ci 的 frontend/node_modules 是既有依赖安装位置，不移动或建立符号链接；其授权/实际冻结范围须由宿主确认，不把相对依赖安装许可扩展为任意绝对写许可。

普通 npm build 的产物现在位于独立运行目录而非 frontend/dist。下游打包若依赖固定 dist，须由有授权的构建/部署所有者消费该明确产物；本任务不擅自修改 tauri.conf.json、部署流程或候选范围。

## 验证（render_evidence / unverified）

当前授权记录 approved_absolute_read_paths 与 approved_absolute_write_paths 均为空，没有可用的宿主确认外部输出目录。SERVICE_UI_RUNTIME_ROOT 未提供；TEMP 只是环境值，不是业务授权。因此 npm ci 和 npm run build 未执行，浏览器原命令仅执行了无目录时的失败关闭路径，未启动服务或浏览器；两个报告断言检查如实失败。目录门禁测试和语法检查是当前本地检查，不是浏览器业务通过。

原五项完整重跑、成功/启动失败/超时的真实资源清理回归、五宽截图、200% 指针操作、长 URL、文案膨胀、AX、深浅偏好、对比度、减少动效及新增摘要断言均未在本次运行验证。保留历史 repair3/attempt4 的 UI-006 先行实测及之后回归材料，不将历史截图改标为本次结果。

新增的检查入口能将前后文件集合/类型/SHA-256 快照和完整日志写入独立运行目录；该本地 task-scope 对比不是 host:candidate-integrity。本次只能验证安全失败子集的候选内容未漂移，不能宣称已完成“五项检查重跑后集合与内容不变”。阶段证据仅在工作阶段明确固化，原五项检查和兼容旧入口均不再自动改写这些交付文件。

## 历史关联与保护文件

失败业务运行 local-run:workflow_run_mission_mission_1794473598404_ffe1e069_repair-service-ui-delivery-3 与核验运行同名加 _verify 保留原身份。宿主事件 :537 的细项原件不可访问，不猜测其检查总数。用户明确给出的最新事件 mission_1794473598404_ffe1e069:543 五项退出码为 0、0、0、0、0，五项 test_summary 均为空；仅列为用户提供的历史宿主记录，不是本 worker 执行，也不补数值 TAP 总计。

docs/mixed-executor-controlled-host-patches.json 可读记录说明 repair3 五项命令退出 0、但新增/改写 53 个 scoped report 文件导致 candidate-integrity guard 拒绝。保留 browser-1794501571165 的全部文件与原始引用，不清理历史。两受保护文件只读；源文件当前 SHA 与 check-boundary-host-integration 记录 after_sha256 匹配，可证明记录的一致性，不能独立验证未获授权的控制端备份。provenance 文档在原始 baseline-manifest 中无条目、Git 未跟踪，无法据此裁定其正式基线或所有差异归属。完整当前 hash、Git 可访问差异及记录链见 repair4/protected-provenance.json。

## 假设与风险

宿主须提供预先存在且明确授权、候选之外的专用运行根，并配置 npm cache/temp 环境；本任务没有自行申请或假设额外绝对路径授权。环境变量是配置接口，不是权限凭证。宿主应保留被证据索引引用的运行目录，待引用解除并按宿主保留策略清理；脚本不自动删除运行证据，只由浏览器/Vite 清理本次拥有的进程和临时 profile，不处理其他会话或宿主生命周期。

生产后端、真实凭据、屏幕阅读器语音、部署及正式 VERIFY 不在本阶段实测结果内。阶段 passed=false 的原因是缺少目录授权、完整必需检查和本次业务回归证据，并非因为未来正式 VERIFY 尚未发生。业务候选重新冻结、protected_file_check 的基线裁定和正式 VERIFY 留给宿主；本 worker 不调用、不等待、不冒充该核验。
