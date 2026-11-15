# 视觉专项修复审查与离线实现交付

## 设计结论
保留克制、紧凑的桌面设置工具方向：先扫到可用与待行动状态，再选择服务、模型和执行环境。记忆点是明确区分“可运行”与“需要更新”，不以装饰色代替事实。保守假设用户反复管理本机 AI 工具与服务；未取得使用频次访谈。正面对标 Windows 设置的任务分组和就近反馈，仅为交互惯例参考，未联网对比。

## 检查诊断（历史与本轮分开）
最新宿主实录为事件 `mission_1794443924791_2d612a63:263`：`python contracts/check.py visual` exit_code=0、elapsed_ms=210、output_sha256=`2ac5e7759b6e3430364a372eae5748cc90d5c1d1ad38ec42615bb03200a2dcf1`；test_summary 与 error 均为空，没有数字 TAP totals。该结果适用于本轮修改前交付，不作为修改后通过依据。历史 VERIFY 输入超限及缺 Markdown 失败仅作背景，本阶段不处理宿主输入或执行正式 VERIFY。最终版本必需检查见本会话执行记录。保留原型既有优化，仅修复 hidden 隐藏规则；不宣称产品已修复。

## 证据边界
本轮对原始证据逐张调用 media.image.read，成功获得七张图像像素；只读解码，不改名、不转换证据。tasks 预览1282×822，其余982×752；这些是工具返回预览尺寸，不是测量得到的目标视口。此前read_image格式不匹配属于历史观察限制，已被本轮补充观察更新。AX为安装版 v0.3.92（e630a08）静态快照，source含未提交修改，仍不可认定同版。未启动产品或原型做交互测试。

## 结构与状态
结构建议维持 AI 连接 → 工具接入 / AI 服务 / 模型网关 / 能力路由 / 运行环境。工具接入先显示可用及需更新事实，主动作是处理待更新；服务列表主动作是新增服务；新增表单每步只有下一步或保存为主动作。六状态分别为：加载保留列表结构并显示持续文字；初始空提供新增，筛选空提供清除筛选；错误说明影响和重试；部分可用保留已知配置并标记健康未知；成功把结果留在列表；无权限说明只读范围与申请路径。权限实际产品边界未确认，以上为设计要求而非已验证。

## 取值与依据
本地 styles.css:4776-4778 的 meta/body/title 为 12/13/14px，优先于通用字号建议；正文行高 1.5，标题 1.3。间距采用 4/8/12/16/24/32px。建议桌面动作命中 32px，离线示例采用 44px；原28px不能仅凭尺寸判为 WCAG 2.5.8 失败。圆角控件6px、面板8px；面板无阴影，对话框单档阴影。反馈过渡只用透明度120ms ease-out，减少动效不做位移或缩放。正文对比度目标4.5:1、焦点与非文本3:1，未实测合成色，不声称AA合规。依据 WCAG 1.4.3/1.4.11/1.4.4/1.4.10、APG Dialog 与 Tabs；具体问题、原文、行号、优先级及整改建议保存在 visual.json，下表给出逐项映射。

## 验证与风险
本轮核实并保留 reports/prototype.html:3-12 的已有离线实现，仅在:3补充hidden隐藏规则（前序创建时文件不存在是历史记录，不是当前文件状态）。原型不请求网络，不加载外部资产，仅用虚构数据演示。未进行渲染验证；没有目标视口截图或浏览器测试产物。键盘、焦点、窄屏及动画均未验证，不能标为通过。结构检查不代替独立语义复核，不代表产品已经整改或 Mission 验收完成。

## 专项问题与原型映射

完整逐字源码引用见 visual.json 的 evidence；下列路径均相对工作区，行号以受保护快照为准。confirmed 指代码事实已确认，不指产品交互缺陷已经复现。

| 编号 / topic | 代码事实与影响 | 具体整改、原型对应和验收 |
| --- | --- | --- |
| UI-001 / modal_and_focus | source/frontend/src/pages/AiProvidersPanel.tsx:258-283 中 `document.addEventListener('keydown', onKeyDown);` 与814行自管section模态并存。source/frontend/src/components/ModalSurface.tsx:22已有 `dialog.showModal();`。实现分叉增加嵌套焦点冲突风险，P1；实际穿透未复现。 | 产品应复用ModalSurface，删除document级循环。原型#editor使用原生dialog/showModal，记录opener，close事件归还焦点，打开聚焦标题。待测Tab/Shift+Tab循环、Esc、取消、归还及读屏背景隔离；不以静态aria-modal自证通过。 |
| UI-002 / forms_and_selection | 同文件844、851行的button仅以active类体现选择，未暴露选择状态；安装AX service-create:305-316为普通按钮。P2，选中反馈受损。 | 分类按钮加aria-pressed，预设改原生radio，步骤aria-current。原型fieldset/legend下用同名radio，第一步aria-current=step；仅连接步骤可操作，后续步骤不是伪完成向导。待验证AX所选状态及方向键行为。 |
| UI-003 / forms_and_selection | 同文件473-481 `setFormError(error);`，823行只有全局alert，863行URL输入无字段错误关联。P2，修正定位成本高；错误态未复现。 | 产品按字段建映射并跨步定位。原型#form仅验证名称，失败显示摘要链接、aria-invalid、aria-describedby并聚焦摘要；成功持久显示“未请求服务、未保存配置”。待测空提交→摘要→名称→修正，不声称完整业务校验。 |
| UI-004 / contrast_and_accessibility | source/frontend/src/pages/AiConnectionsPage.tsx:186及AcpProfilesPanel.tsx:168重复 `id="ai-panel-acp" role="tabpanel"`；runtime.ax.txt:46-47同时出现相同面板。P2，控制关系不唯一，未实听重复播报。 | 产品只由父页持有tabpanel；原型不模拟运行环境页签，未演示此整改。待测该id数量为1及AX只有一个面板。依据APG Tabs、WCAG1.3.1/4.1.2。 |
| UI-005 / hierarchy_and_density | source/frontend/styles.css:4776-4778定义12/13/14字号；5585事实行为11px，5683备用模型9.5px与opacity .75。P2，重要信息绕过本地令牌；没有断言对比度失败。 | 事实行与备用模型统一meta12、名称body13、标题14。原型--meta/--body/--title复用数值，未重建产品主题；数值tabular-nums。待200%及长名称实测；原型h1另为22px页面标题，非产品令牌修改。 |
| UI-006 / responsive_layout | styles.css:5594的三列布局、5605动作锁第三列及2991双列表单构成窄容器挤压风险。P2 hypothesis，不是已见溢出。 | 产品建议容器480px以下动作换行、表单440px以下单列。原型480px媒体查询单列表单、动作整行，copy可收缩并overflow-wrap:anywhere；仅页面级演示，不等价产品容器查询。待测320/390/480/620/880px内容宽、200%、120字符URL与50%膨胀；检查边界遮挡及scrollWidth。 |
| UI-007 / feedback_and_motion | source/frontend/src/pages/AiToolsPanel.tsx:140单独计算pendingClientSync，158-161摘要只依赖attention。ai-connections.ax.txt:49-50“工具已就绪”与75-78“1个待更新”并存。P2，第一眼下一步不清晰，不判业务分类错误。 | 原型#summary分列可用与待更新事实，#sync仅更新虚构计数及持久status。产品应将待同步作为行动摘要并降级无关主动作。待覆盖ready+needs_update/failed/conflict；原型未演示全部组合。 |
| UI-008 / feedback_and_motion | AiProvidersPanel.tsx:246失败 `setProviderCatalog(null)`，287用模型数量兜底，191返回“凭据已配置”。P2，健康未知未显式呈现，不等同文案声称连接成功。 | 原型部分可用保留已知列表并显示健康未知、重试；错误、真实空、筛选空分开。产品保留旧数据时间并支持局部重试，上层状态交所有者确认，不改接口。待注入目录失败并验证status可感知。 |

## state_coverage：实现与观察严格分离

| 状态 | 原型行为（源码检查） | 产品/原型运行验证 |
| --- | --- | --- |
| 加载 | #state=loading显示持续文字、#rows aria-busy=true；列表隐藏但外层区域保留，可切换状态取消等待。没有异步任务或真实进度，未实现骨架和固定列表高度。 | 均未验证；不能据此声称加载结构完全稳定。 |
| 空 | empty显示新增服务指引；success/partial下筛选无匹配显示清除筛选，清除后请求聚焦搜索。 | 均未验证。 |
| 错误 | error显示目录失败影响与重试；按钮恢复虚构success。 | 未注入产品错误，原型运行未验证。 |
| 部分可用 | partial保留已知列表并提示健康暂不可用、配置仍可编辑与重试。 | 产品故障映射未确认；运行未验证。 |
| 成功 | 默认显示列表；同步完成保留计数和提示；表单仅显示格式演示确认。 | 没有真实保存、连接或同步，未验证交互。 |
| 无权限 | denied说明只读边界与联系管理员，禁用新增并隐藏编辑所在列表。 | 产品权限边界为假设；原型未验证。没有实际授权申请入口。 |

## render_evidence 与逐张核对记录

七张均由 media.image.read 返回像素，本轮观察如下；相关AX已定点复读。图像不能证明键盘、读屏、原型或当前源码实际行为。

| 输入 | 实际图像观察及AX定位 | 边界 |
| --- | --- | --- |
| evidence/ui/tasks.png | 左侧任务导航、顶部搜索/筛选，列表并列完成及失败项，失败项带错误文字与重试 | 无tasks AX；不扩大为任务页问题 |
| evidence/ui/ai-connections.png | 绿色“工具已就绪”区与主按钮“接入全部”，下方另有待更新橙色提示及“更新全部” | AX:40-78；支持UI-007的状态竞争，不判断业务分类错误 |
| evidence/ui/services.png | 概览、工作台未配置区、本机服务搜索及密集事实行；首屏行尾四个图标动作可见 | AX:46-92；不证明窄屏裁切或对比度失败 |
| evidence/ui/gateway.png | 单个网关卡片及三项操作，下方能力观测表为两个记录 | AX:46-95；只覆盖首屏 |
| evidence/ui/routes.png | 四列表格，服务/模型下拉、状态及行尾动作；视频等行显示无可用服务及去配置 | AX:46-79；无窄屏证据，UI-006仍为hypothesis |
| evidence/ui/runtime.png | 可用概览和本机列表，下方两列运行环境卡片部分可见 | AX:46-69确认重复面板；截图本身不能证明重复播报 |
| evidence/ui/service-create.png | 居中白色表单覆盖灰色背景，步骤1与手动配置深色强调，URL/接口双列，底部取消/下一步及内部滚动条可见 | AX:289-336；不证明背景隔离、焦点循环或错误恢复 |

未进行原型渲染验证，没有新增原型截图；未安装浏览器依赖或重建产品。本轮命令仅用于交付整理与给定检查。待人工在1440×900、390×844、320px宽、200%、长文本与各状态下实测；深浅/高对比主题、键盘焦点、读屏、减少动效、合成色对比度仍为unverified。安装截图观察不等于上述测试通过。

## 前序修复检查记录（历史，不代表本轮执行）
补齐Markdown及原型、更新JSON边界说明后，实际再次运行 `python contracts/check.py visual`，退出码0，stdout为：

```json
{"passed": true, "stage": "visual", "source_files_unchanged": true, "meaning": "Structural and evidence checks only; independent semantic review still required."}
```

上述仅为前序版本记录，不能绑定本轮最终文件。没有数字 TAP totals，不扩写成测试数量或独立通过。本轮检查与文件摘要见执行记录；所有源码修改建议均待产品所有者实施，综合方案与 Mission 独立验收由后续角色负责。

## 简短送验交接（按路径读取）
- 变更：visual.json/md更新七图观察、最新宿主事件和版本边界；prototype.html:3仅增加 `[hidden]{display:none!important}`，防止.row display:flex覆盖hidden。对应UI-008及六状态/筛选隐藏，静态确认，运行待测。无产品源码修复。
- 保留：UI-001–008全部问题及原文/行号/影响/建议见visual.json findings；六主题见coverage。原型:3保留12/13/14字号、可收缩布局及480px回落；:9、11保留原生dialog、radio、错误关联及焦点归还；:6、12保留可用/待更新摘要及六状态。UI-004没有原型演示。
- 证据：evidence/ui/README.md:3-7定义安装版边界；七图及六AX定位见上表。没有更改原图或AX。源码引用已逐项核对。
- 检查来源：最新历史宿主:263，exit_code=0、210ms、SHA256见本报告检查诊断，仅覆盖变更前版本。本轮修改后三文件已实际运行 python contracts/check.py visual，显式 VISUAL_CHECK_EXIT_CODE=0；stdout passed=true、stage=visual、source_files_unchanged=true，仅为结构与证据检查，无数字TAP totals。加入本记录后再次运行以绑定最终落盘版本，真实结果及三文件摘要见本会话执行记录。
- 余项：未实测原型/产品渲染、键盘、焦点、响应式、200%、长文本、读屏、主题、对比度及动效。权限为假设。正式VERIFY由宿主随后独立执行，不要求本角色控制宿主输入，不声明Mission通过。

## 独立只读补充复核（非宿主正式 VERIFY）
独立子代理 `c002fd30-e426-4300-92dc-8106780e5154` 已核对四份合同/检查器、visual.json/md、prototype.html、八项定点源码与三份 AX，结论为未发现阻断 visual 专项合同符合性的缺陷。六主题及 UI-001–008 原文/行段可核对；原型优化真实存在；UI-004未演示、UI-002分类不完整、UI-003仅名称验证、UI-006媒体查询非产品容器实现等限制保留。独立执行必需检查显式 exit_code=0，但执行早于最后报告措辞整理，不冒充最终版本检查。复核未重新观察七张PNG、未核验历史宿主事件，也未运行浏览器、键盘、焦点、读屏或动效测试。此记录只作补充证据，不替代宿主记录实际输入长度并完成正式 VERIFY；前序记录不代表本阶段最终送验结果。最终落盘后必需检查与三文件 SHA256 以本会话随后执行记录为准。

## 假设与剩余风险
用户场景按高频本机配置保守推进，频次与权限模型未确认；若实际是新手首次安装，应另评估渐进披露，不自行重排产品优先级。源码中已有令牌优先，原型新设中性颜色仅示意，没有测得对比度值。安装版与当前树有版本差异；本轮未确认两者视觉一致。原型的radio、原生dialog和响应式规则为实际HTML/JS实现，但未执行真实交互，不能作为已修复产品的证据。未访问凭据或真实服务，未改业务逻辑、数据模型或接口。
