# HiMind Agent 桌面 UI 设计审查

现有侧栏、工具栏和主从工作区已经形成稳定的桌面工具结构，主要问题集中在内容区域的高度分配、滚动归属和状态表达。建议先修复两处布局缺陷，再压缩任务列表与错误信息，最后统一标题和静态状态的视觉细节。无需更换配色或重新设计导航。

本次确认 **10 项：P1 两项、P2 七项、P3 一项**。P1 表示正常桌面尺寸下信息被隐藏或导航落点错误；P2 表示影响理解、扫描效率或一致性；P3 为局部工艺问题。这些是审查结论，尚未实施 UI 修复。

## 检查条件与依据

| 项目 | 本次条件 |
| --- | --- |
| 平台 | Windows，正在运行的已安装 HiMind Agent；不是浏览器模拟页面 |
| 程序 | `C:/Users/Administrator/AppData/Local/HiMindAgent/versions/0.3.92/himind-agent.exe` |
| 版本 | v0.3.92，本地构建标识与审查时源码 HEAD 均为 `069a79a` |
| 主窗口内容视口 | 1280 × 820 CSS px，DPR 1，页面缩放 1 |
| 设置窗口内容视口 | 980 × 720 CSS px，DPR 1，页面缩放 1 |
| 实际检查 | 首页、我的任务、待处理、工作区、工作流、定时计划、市场、我的能力、开发工作区；设置的通用与 AI 连接／AI 服务 |
| 方法 | 真实桌面窗口观察与截图；通过现有 WebView2 CDP 连接运行中的页面，测量元素边界盒、计算样式和滚动位置；结合对应源码判断根因 |
| 操作范围 | 页面导航、滚动与读取。未执行任务、审批、安装、删除、取消授权或修改服务配置 |
| 按本次要求省略 | 200% 缩放、无障碍专项、移动端／极窄窗口、键盘合规、减少动效专项、多显示器与高 DPI 矩阵 |

尺寸指 WebView 的实际内容视口，不是含系统外框的窗口尺寸。审查期间应用仍接收其他任务的状态更新，数量可能变化；截图和 JSON 中的数值只代表各自采样时刻。

采用 [software-ui-spec](C:/Users/Administrator/.agents/skills/software-ui-spec/SKILL.md) 的结构化审查、[ui-craft](C:/Users/Administrator/.agents/skills/ui-craft/SKILL.md) 的具体数值建议，以及 [windows-desktop-e2e](C:/Users/Administrator/.agents/skills/windows-desktop-e2e/SKILL.md) 对真实进程与自动化边界的区分。后者明确要求 WebView2 的 HTML 层使用浏览器自动化，因此本次直接连接已运行桌面程序的 WebView，没有另外启动网页替身。

规范优先级：Agent 自有令牌与 [ui-review-profile.json](F:/WebProjects/项目看板/himind-agent/docs/ui-review-profile.json) → [DESIGN.md](F:/WebProjects/项目看板/DESIGN.md) → 两项 UI 技能的通用规则。文案同时依据 [ADR 0010](F:/WebProjects/项目看板/himind-agent/docs/adr/0010-user-visible-copy-states-consequences.md)。正文 13px、元信息 12px和控件 32px沿用本地体系，不套用通用技能的更大字号、40–44px命中区。旧版 MASTER 中的蓝色渐变方向与当前 DESIGN.md 冲突，本次采用后者的克制风格。放大与无障碍相关技能要求按用户明确指定的范围跳过。

## 问题概览

| 编号 | 优先级 | 位置 | 现象 | 主方向 |
| --- | --- | --- | --- | --- |
| UI-01 | P1 | 待处理 | 四项摘要被压缩成 2px 细线 | 布局 |
| UI-02 | P1 | 主导航切换 | 新页面继承旧滚动位置，标题消失 | 导航 |
| UI-03 | P2 | 首页 | 同一屏同时表示工作台连接异常与已连接 | 状态反馈 |
| UI-04 | P2 | 我的任务 | 普通任务行 150.5px，多次重复状态与动作 | 内容与文案 |
| UI-05 | P2 | 待处理 | 固定双列让空分组占据半屏 | 布局 |
| UI-06 | P2 | 工作流 | 原始包 ID 与英文错误抢占列表，缺少恢复路径 | 内容与文案 |
| UI-07 | P2 | AI 服务 | 加载中先显示“没有可用服务”，随后出现 8 项 | 状态反馈 |
| UI-08 | P2 | 跨页面标题 | 工作区标题 14px/700，其他页面 22px/600 | 一致性 |
| UI-09 | P2 | 首页就绪卡片 | 静态就绪状态持续呼吸与发光 | 视觉层次与工艺 |
| UI-10 | P3 | 顶部全部工具 | 四字标签被截为“全部…” | 视觉层次与工艺 |

## UI-01 · P1 · 待处理摘要被压成细线

**现象与影响：** 1280 × 820、页面位于顶部、有 10 项协作待办时，四项摘要容器只有 **2px** 高；内部单元格各为 **73.5px**，被 `overflow: hidden` 整体裁切。用户只看到一条横线，读不到分项统计。

**证据：** [顶部截图](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/03-inbox-top.png)；`desktop-details.json` 的 `03-inbox-top`：摘要位于 x214/y182，1021 × 2，`flex-shrink: 1`；列表网格高 852.5px。

**根因与位置：** [InboxPage.tsx:50](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/InboxPage.tsx:50) 以 Fragment 把页头、摘要、列表作为共享纵向 Flex 容器的直接子项；[styles.css:1448](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:1448) 与 [styles.css:2124](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:2124) 未明确摘要的高度所有权。长列表占据最小内容高度，摘要则可收缩至边框高度。

**修改建议：** 为页面建立明确的纵向容器，让页头与摘要 `flex: 0 0 auto`；摘要建议最小高度 **76px**，同时允许内容自然撑高。列表负责产生正常纵向滚动。现有四列保留，不用固定整页高度掩盖溢出。

**验收：** 同尺寸、10 项及更多待办时，四个数字与标签全部可见；摘要实测高度 ≥76px；列表滚动不裁切摘要；零待办仍采用既有整体空态。

**依据：** software-ui-spec「布局与响应式容纳性」要求内容不被父容器裁切；ui-craft「先结构后装饰」。

## UI-02 · P1 · 跨页面滚动位置串用

**复现：** 我的任务 → 将 `.main-content` 滚到 500 → 点击侧栏待处理。新页面保留 **scrollTop=269**（被新页面最大值截断），页头的 y 坐标为 **−155px**。首屏直接落在列表中段，标题与操作入口均不在可视范围。

**证据：** [切换后的截图](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/12-navigation-scroll.png)；`desktop-details.json` 的 `12-navigation-scroll`。同页面回到顶部时页头 y=114，证明不是标题未渲染。

**根因与位置：** [Shell.tsx:741](F:/WebProjects/项目看板/himind-agent/frontend/src/components/Shell.tsx:741) 的共享滚动宿主跨导航保留；[TaskCenterPage.tsx:213](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/TaskCenterPage.tsx:213) 已有任务列表的滚动记忆，但缺少与一级页面切换配套的统一归属规则。

**修改建议：** 按页面管理滚动状态。首次进入另一个一级页面从顶部开始；任务列表进入详情再返回时，保留列表原位置和筛选。滚动恢复只在相应导航时机发生，不在每次数据刷新时执行。

**验收：** 上述路径切到待处理后 `scrollTop=0`、页头完整可见；任务列表 → 任务详情 → 返回仍能恢复原列表位置；后台刷新不改变用户当前滚动位置。

**依据：** software-ui-spec「信息架构与导航」「一致性与规范对齐」中的可预测导航与上下文保留。

## UI-03 · P2 · “已连接”没有明确区分授权与任务连接

**现象与影响：** 连接波动的同一帧中，首页身份卡写“连接异常，本机服务仍在运行，但工作台任务连接尚未就绪”，底部运行信息却写“AI 工作台：已连接”，侧栏账号也显示“已连接”。用户无法据此判断远程任务现在是否可用。

**证据：** [同屏矛盾状态](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/01-home.png)；`desktop-observations.json` 的 `01-home`。稍后身份卡恢复就绪，说明这是实际状态切换时的表达问题，不是持续离线。

**根因与位置：** [DashboardPage.tsx:81](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/DashboardPage.tsx:81) 与 [DashboardPage.tsx:148](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/DashboardPage.tsx:148)：底部使用 Provider 可用性，身份卡另用 worker 连接状态，但都以宽泛的“工作台连接”表达。

**修改建议：** 身份区域明确写“账号已授权”；执行连接明确写“任务连接异常／已就绪”，由 worker 状态决定。底部摘要复用相同状态投影或省去重复的工作台格，避免新增一套解释段落。保留本机可用这一后果信息。

**验收：** 在账号有效、任务连接异常这一状态下，同屏不再无范围地宣称“工作台已连接”；用户可直接读出“账号有效、本机可用、远程任务受影响”。恢复后相关摘要同步显示就绪。

**依据：** software-ui-spec「系统状态可见」「同一概念同名」；ADR 0010 要求文案表达可判断的状态与后果。本条只审查表达，不诊断或修复连接故障。

## UI-04 · P2 · 任务列表的扫描成本过高

**现象与影响：** 普通协作任务行实测 **150.5px** 高，长内容行可达 **194.5px**；列表从 y301 开始，820px 高窗口仅完整显示 3 行。同一待办行同时展示“预算已用尽”“增加预算并继续”“需要你处理”“阶段：暂停或等待”，尾部又重复“增加预算并继续”。完整绝对路径优先占据元信息行。

**证据：** [任务列表](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/02-tasks-top.png)；`desktop-details.json` 的 `02-tasks-top`；较长条目见初次任务截图与记录。

**根因与位置：** [TaskCenterPage.tsx:346](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/TaskCenterPage.tsx:346) 将动作、责任、阶段分别渲染成纵向块；[styles.css:1919](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:1919) 的 `.task-center-row`、`.task-center-row-meta` 和 `.task-row-next` 共同决定行高。

**修改建议：** 普通行收为三层：标题与单一状态；项目短名／来源／时间；必要原因或进度。单一下一步动作放在右侧固定 **112–144px** 操作列，32px 高，文字列允许收缩。普通行以 **72–88px** 为目标，长错误允许自然增高。路径默认显示项目短名，完整路径保留在悬停或详情中；错误原因不能为了压行而删除。

**验收：** 相同视口至少完整显示 5 条普通记录；每行同一动作只出现一次；能直接辨认任务、状态和必要下一步；长路径不挤走动作；详细责任与阶段仍能在任务详情获取。

**依据：** DESIGN.md「Dense but calm」「Agent 任务中心」；ADR 0010 的删除无决策增量文案；ui-craft 的列对齐与间距层级。72–88px 是本次建议目标，不是既有硬性规范。

## UI-05 · P2 · 单一待办分组仍被限制为半屏

**现象与影响：** 当前 10 项全部属于协作待办，左列仅 **502.5px** 宽；右列是“操作审批 0／暂无操作审批”，其下大片空白。“工作流等待 0”仍被排到下一行，起点 y914，增加整页滚动。

**证据：** [待处理布局](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/03-inbox-top.png) 与 [页面底部](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/12-navigation-scroll.png)；`03-inbox-top` 的网格实测为 502.5px + 16px + 502.5px。

**根因与位置：** [InboxPage.tsx:77](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/InboxPage.tsx:77)、[InboxPage.tsx:102](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/InboxPage.tsx:102) 在其他分组有内容时仍渲染空卡片；[styles.css:2129](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:2129) 固定双列。

**修改建议：** 仅一个非空分组时占满 **1021px** 内容宽度；无事项分组不渲染大卡片，摘要中的零计数足以表达现状。多个非空分组时保留双列；所有分组为空时显示一处整体空态。

**验收：** 当前数据下仅有一张全宽协作待办列表，不再为两个空分组额外产生滚动；混合分组时内容和操作完整；总数与分项仍可核对。

**依据：** Agent ui-review-profile 的 `empty_state` 规则、software-ui-spec「概览与元信息自适应」和 ADR 0010「没有信息则不渲染」。与 UI-01 同页但根因不同，修复摘要高度不会消除空列。

## UI-06 · P2 · 工作流错误只有诊断原文与移除动作

**现象与影响：** 左侧工作流库前三项都是红色错误卡，每项 **89.5px** 高，连同间隔占约 **280.5px**。标题是被截断的 `com.himind.workflow…`；错误区只有约 **176px** 宽，英文原文两行截断。可见操作只有垃圾桶，用户难以判断应修依赖还是移除工作流。

**证据：** [工作流截图](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/05-workflows.png)；`desktop-final.json` 的 `05-workflow-issues`。实际错误分别包含依赖插件返回空响应、依赖插件内容已变化。

**根因与位置：** [WorkflowsPage.tsx:1623](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkflowsPage.tsx:1623) 直接以 `issue.package_id`、`issue.message` 充当用户文案；[styles.css:296](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:296) 的 `.workflow-list-issue` 与 `.workflow-list-issue-reason` 控制窄列截断。

**修改建议：** 有可用元数据时显示工作流名称，否则用短 ID；原因改为“依赖插件未响应”或“依赖插件内容已变化”等简短结论。提供“查看详情”展开完整可复制诊断，并能前往已有插件管理入口检查依赖；移除保持次要操作和现有确认。默认行以 **64–72px** 为目标，不在列表中展开放完整日志。不要凭错误文字实现未经支持的自动修复。

**验收：** 不悬停也能知道不可运行的原因；能完整读取诊断并找到非删除的检查路径；原始包 ID、版本与日志仍可取得；成功工作流不再被大段错误挤到首屏下方。

**依据：** software-ui-spec「帮助识别与修复错误」「用户语言」「错误信息与下一步」；ADR 0010 要求说明后果而非堆过程。

## UI-07 · P2 · AI 服务把加载态表现为空态

**现象与影响：** 打开 AI 服务时，顶部先显示“还没有可用的 AI 服务”、本机数量 0、工作台服务不可用；下方同时写“正在读取 AI 服务配置”。读取完成后，同窗口显示 **8 个本机 AI 服务**和工作台服务。用户在正常加载期间收到了一次错误的配置结论。

**证据：** [加载帧](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/11-ai-services.png) → [读取完成](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/11-ai-services-ready.png)。这是加载闪现，不代表服务持续不可用。

**根因与位置：** [AiProvidersPanel.tsx:774](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/AiProvidersPanel.tsx:774) 仅用数量是否为 0 决定顶部空态；[AiProvidersPanel.tsx:830](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/AiProvidersPanel.tsx:830) 的下方列表才区分未取得数据与真正为空。

**修改建议：** 顶部、工作台服务和列表共用明确的 loading／ready／empty／error 状态。加载时写“正在读取 AI 服务…”，数量显示“—”或骨架；初次请求成功且确实为空后才显示空态。摘要容器保留现有约 **86px** 高度，避免整个页面先塌缩再展开。

**验收：** 首次打开到读取完成的全过程不出现错误的“没有服务／不可用”结论；成功空结果、加载失败、筛选无匹配分别表达；既有 8 项配置读取后计数正确。

**依据：** software-ui-spec「加载、空态、错误态分别设计」；ui-craft「加载反馈」与稳定布局。

## UI-08 · P2 · 同级页面标题有两套字阶

**现象与影响：** 首页、我的任务、工作流及 AI 连接等页面标题为 **22px/600，行高28.6px**；工作区的同级标题为 **14px/700，行高18.2px**，视觉上更像区块标签。同级页面切换时层级明显跳变。

**证据：** [工作区标题](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/04-workspaces-ready.png)、首页和设置截图；`desktop-final.json` 的标题测量。工作区截图中会话历史子区域仍在加载，本条仅采用已渲染标题，不以子区域加载状态判断缺陷。

**根因与位置：** [styles.css:2166](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:2166) 的页标题写死22px；[WorkspaceRegistryPage.css:9](F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceRegistryPage.css:9) 用了区块级 `--fs-title: 14px`，并继承 h2 的700字重。

**修改建议：** 增加明确的页面标题语义令牌，按照 DESIGN.md 统一为 **20px/600/26px**；工作区使用同一页标题样式；区块标题继续 **14px/600**，正文继续13px。不要把现有 `--fs-title` 全局改成20px，它还用于大量区块标题。

**验收：** 上述一级页面标题计算样式一致为20/600/26；原14px区块标题不被连带放大；设置类节标题仍按本地单独约定，不增加多层分组标题。

**依据：** DESIGN.md「Typography Rules」；ui-craft「同级标题同值」；software-ui-spec「语义令牌」。Agent 的14px标题最低值仍满足，但不能替代页面级层次定义。

## UI-09 · P2 · 稳定就绪态持续发光

**现象与影响：** 首页没有状态变化时，就绪卡片仍运行 **4.8s 无限循环**呼吸动画，图标另有2.2s光晕循环。就绪状态获得了接近运行告警的注意权重。

**证据：** [首页就绪帧](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/01-home-top.png)；`desktop-details.json` 的 `01-home-top` 与 `desktop-final.json` 的 `01-home-final` 均记录实际计算动画 `workspace-status-breathe … infinite`。静态截图冻结动画，连续运动依据运行时样式而非单帧推断。

**根因与位置：** [styles.css:2893](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:2893) 与 [styles.css:2921](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:2921)：卡片、顶线、图标及伪元素各自循环；卡片在中点变为绿色 **0 8px 28px rgba(16,185,129,.16)** 阴影。

**修改建议：** 就绪态保留静态状态点与1px边框，取消该卡片及其装饰伪元素的无限循环；若要表达状态到达，使用一次 **150–200ms** 的颜色或透明度过渡。阴影沿用既有静态层级令牌，不用绿色发光表达“已就绪”。

**验收：** 稳定就绪后观察10秒，卡片边框、阴影、图标不再周期变化；真正运行中的任务仍有清晰进度或运行反馈。本条是普通桌面视觉审查，不包含减少动效或无障碍专项。

**依据：** DESIGN.md「几乎不用渐变和厚阴影」「禁止发光」；ui-craft「动效服务因果」「少而准」。

## UI-10 · P3 · 工具入口的短标签不应省略

**现象与影响：** 顶部工具入口只有 **112 × 32px**，四字“全部工具”的文字容器宽 **44.31px**，实际文字需要48px，结果显示“全部…”。工具栏中间仍有充足留白。

**证据：** 各主窗口截图右上角；`desktop-final.json` 记录标签 `clientWidth=44`、`scrollWidth=48`。

**根因与位置：** [Shell.tsx:715](F:/WebProjects/项目看板/himind-agent/frontend/src/components/Shell.tsx:715)；[styles.css:1455](F:/WebProjects/项目看板/himind-agent/frontend/styles.css:1455) 的通用按钮 `max-width:112px`，以及 `.quick-access-button-label` 的可收缩、省略规则，共同挤压该特殊入口。

**修改建议：** 仅对 `.quick-access-all-button` 解除112px上限，设置 **min-width:120px**；其固定短标签不收缩，计数列保留至少22px，高度仍32px。其他可能很长的工具名称继续允许省略。

**验收：** 本次桌面尺寸下“全部工具”四字完整，图标与数量均可见；一位及两位计数不挤压标签；不扩大所有工具按钮。

**依据：** software-ui-spec「行内文本与操作容纳」；ui-craft「同类同值，差异来自实际内容」。

## 共性根因与实施顺序

| 主方向 | 数量 | 对应问题 | 共性根因 |
| --- | --- | --- | --- |
| 布局 | 2 | UI-01、05 | 页面纵向尺寸和分组布局缺少内容感知 |
| 导航 | 1 | UI-02 | 共享宿主与页面滚动记忆没有统一边界 |
| 状态反馈 | 2 | UI-03、07 | 不同来源或阶段被合并成同一个用户结论 |
| 内容与文案 | 2 | UI-04、06 | 内部信息直接平铺，缺少按决策价值分层 |
| 一致性 | 1 | UI-08 | 页面与区块复用同一标题令牌 |
| 视觉层次与工艺 | 2 | UI-09、10 | 装饰状态权重过高、公共按钮约束缺少特殊入口处理 |

建议按以下顺序实施，每一步在同一真实窗口重新复核：

1. **修复高度与滚动：** UI-01、02；同时验证列表与详情返回的滚动记忆。
2. **统一状态：** UI-03、07；先确认状态口径，再同步摘要与局部提示。
3. **提高扫描效率：** UI-04、05、06；保留必要原因和恢复入口，减少重复与空占位。
4. **收敛视觉：** UI-08、09、10；页面标题独立令牌，静态就绪态，完整短标签。

以上建议可在现有组件和样式中完成，不需要新增设计库。不要追加另一套大范围样式覆盖来抵消已有规则；应合并相同选择器的职责，优先修复布局和状态来源。

## 已检查但未列为缺陷的部分

侧栏分组、选中态、账号区和顶部工具条保持稳定；市场与我的能力在数据完成读取后使用列表／详情结构，没有把初次零计数或未完成读取的图标当作故障。定时计划和扩展开发的当前空态／错误态已留证，本次未据此推断后端能力缺失。用量数字已有等宽数字机制，不重复提出数字抖动问题。

轻量文案打磨还可把首页 P95 `64,902ms`、平均 `19,872ms` 显示成 `64.9s`、`19.9s`，详情保留毫秒；这属于可选可读性改进，未计入10项问题。

## 产物与验证边界

- [desktop-observations.json](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/desktop-observations.json)：九个主页面的首轮文本、尺寸与样式快照。
- [desktop-details.json](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/desktop-details.json)：摘要压缩、任务行高、加载态和滚动串用的详细记录。
- [desktop-final.json](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/desktop-final.json)：补充标题、工作流错误、已加载服务和当前动画的实测值。
- [inspect-desktop.cjs](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/inspect-desktop.cjs)、[inspect-details.cjs](F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/desktop-20261117/inspect-details.cjs)：本次采样脚本，连接既有本机9311端口，依赖已有的会话 Playwright 运行时。它们是检查记录，不是自动回归测试；部分首轮截图包含真实加载中间态，已在对应结论中明确标注。

本次只新增审查文档、截图与采样记录，没有修改产品 UI 源码，没有运行构建或完整 E2E 套件。当前产品数据由运行中的其他工作持续更新，复验应优先断言布局与状态关系，不能依赖固定任务总数。登录、支付、凭据编辑、实际审批和任务执行结果不在此次验证范围内；设置其余分类与独立模式未逐项审查。后续修复应采用开发配置进行需要写入数据的测试，并按每条验收条件回到真实窗口检查。
