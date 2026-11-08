# UI 审查发现

来源：`com.himind.workflow.ui-craft-team` v1.1.2 的 `audit` 入口，
运行 `workflow_run_1794142960977_7000`（2026-11-08）。
判据：`docs/ui-review-profile.json`。

## 这次审查的边界

**只做了静态阅读。** 审查员能读源码，但跑不了命令、渲染不了页面：

- `personal.codex` 的 `workspace-write` 沙箱在本机无法启动
  （`setup helper exited with status Some(143)`）
- `himind.builtin`（DSH）的沙箱设 ACL 失败
  （`SetNamedSecurityInfoW failed (Win32 5)` → 拒绝访问）

因此 `npm run check:copy` / `check:text-layout` 未执行，四个视口未实测。
结论全部来自源码常量与行号，**可以核对，但没有运行证据**。

## 覆盖情况

| 分支 | 状态 | 产出 |
|---|---|---|
| 信息架构（ia） | succeeded | 15 条 |
| 工艺（craft） | succeeded | 14 条 |
| 无障碍（a11y） | **failed（超时 1800s）** | 0 条 |

无障碍分支从 `tool_started` 到 `error` 之间没有任何事件，整段 30 分钟无产出。
**无障碍维度未被审查**，这是本次的明确缺口。

---

## 信息架构（可定位）

### 1. 同一入口在侧栏与页头用了两个名字

- **id**：`ia-7d7f09`
- **位置**：frontend/src/navigation.ts:45,68；InboxPage.tsx:48；ExtensionDevelopmentPage.tsx:395
- **状态**：open

现象：侧栏写「待处理」，页面 h1 写「需要我处理」；侧栏写「开发工作区」，页面 h1 写「扩展开发」。原因：导航模型与页面自述各自维护文案；pageLabel:98 的兑底表里也写着「扩展开发」，与导航项冲突且因条目已在 navigationItems 中而永不生效。建议：文案只留一份，页头 h1 直接取 pageLabel(currentPage)，并删掉失效兑底项。

### 2. 「待处理」徽标数与页内条目数不一致

- **id**：`ia-b02adf`
- **位置**：frontend/src/main.tsx:2081-2085,2293；Shell.tsx:642；InboxPage.tsx:43
- **状态**：open

现象：徽标 = 审批数 + waiting 且排除 external_wait；页内「全部事项」= 审批数 + 全部 waiting，含 external_wait。存在「等待外部状态」的运行时会看到徽标少于页内条数。原因：计数口径写了两遍。建议：抽一个共用选择器供两处调用，或在页内把 external_wait 单列为「等外部信号」并排除在总数之外。

### 3. 「需处理 / 待处理 / 需要我处理」三个近义标签指向不同集合

- **id**：`ia-13290c`
- **位置**：TaskCenterPage.tsx:68；navigation.ts:45；InboxPage.tsx:48
- **状态**：open

现象：「我的任务 → 需处理」= failed + canceled + waiting；「待处理」页 = 审批 + waiting；页头叫「需要我处理」。三个词只差一个字，集合各不同。原因：状态筛选与待办入口共用近义中文，没有术语表。建议：统一为一个词，并在文案里带上集合边界，例如「待我处理（审批 2 · 运行等待 3）」。

### 4. 同一个审批决定在四个界面用了四个词

- **id**：`ia-844b32`
- **位置**：ApprovalsPage.tsx:96；InboxPage.tsx:81；TaskCenterPage.tsx:441；WorkflowsPage.tsx:1869
- **状态**：open

现象：审批页与待处理页写「允许」，我的任务行内写「批准」，运行详情写「批准并继续」。原因：四处各自实现按钮文案。建议：统一动词为「批准 / 拒绝」；需要表达后续动作时用「批准并继续」作为同一动词的扩展，不再引入「允许」。

### 5. 「审批」页是隐藏二级页，缺返回路径与导航高亮

- **id**：`ia-626a0f`
- **位置**：main.tsx:2297,2299；ApprovalsPage.tsx:134
- **状态**：open

现象：只能从「待处理」页头的「审批记录」进入；PageKey='approvals' 不在 navigationSections，进入后侧栏无任何高亮，页面无面包屑、无返回按钮，只能改点其它一级项离开。原因：页面被收进待处理，但入口与返回关系没有一并补上。建议：改为「待处理」页内的「待审批 / 历史记录」两个页签；或保留独立页但补进导航并给返回链接。

### 6. 运行记录有两个平行主入口

- **id**：`ia-eecf75`
- **位置**：TaskCenterPage.tsx:196；WorkflowsPage.tsx:1544
- **状态**：open

现象：「我的任务」与「工作流 → 运行记录」都列运行号、状态、步骤、可打开运行；在我的任务点「查看运行」会跳到另一个一级入口下的页签，侧栏高亮随之丢失。原因：两份实现覆盖同一份运行台账。建议：指定一个为主入口，另一处只做上下文摘要并显式标注「在运行记录中打开」。

### 7. 工作流清单在两处都可启停

- **id**：`ia-d66f3f`
- **位置**：WorkflowsPage.tsx:1648-1701；main.tsx:2628
- **状态**：open

现象：同一组件以 mode='runs' 的「工作流」页签和 mode='library'（我的能力 → 工作流）两个实例渲染，两处都有行尾启停开关与运行入口，没有「哪处是主入口」的约定。原因：库与运行两件事被拆到两个一级入口，但组件复用让两侧能力对等。建议：「自动化 → 工作流」保留全部动作；「我的能力 → 工作流」改成只读清单 + 「去运行」链接。

### 8. 「首页」命名与内容不符，说明承诺的入口不存在

- **id**：`ia-40d070`
- **位置**：DashboardPage.tsx:91-97
- **状态**：open

现象：页头写「从这里开始使用 AI 对话、自动化工作流和能力拓展。」，但页面只渲染更新横幅、同步状态、账号授权、用量与版本信息，没有这三类入口；同时首次启动落点是 AI 对话，与「首页」争夺起点。「从这里开始」属填充语，与 ADR 0010「写后果不写介绍」相惖。建议：标题改为「运行状态」，说明改成陈述后果的一句；若要保留「首页」，就真的补上三类任务入口。

### 9. 市场上已安装项目规则的「管理」按钮是空操作

- **id**：`ia-b24b43`
- **位置**：ExtensionsPage.tsx:967 与 794
- **状态**：open

现象：已安装的项目规则条目会渲染「管理」按钮，但 onManage 对 kind==='instruction' 直接 return，点击既无跳转也无任何提示。原因：instruction 没有对应的「我的能力」页签，守卫拦掉了动作却没拦按钮。建议：instruction 不渲染该按钮，或改为指向「扩展开发 → 规则库」。

### 10. 删除启动方案的后果文案与定时计划页自相矛盾

- **id**：`ia-fe2c56`
- **位置**：WorkflowsPage.tsx:2489-2490 vs SchedulesPage.tsx:1524
- **状态**：open

现象：删除确认框说「引用它的定时计划会因方案缺失无法启动」；定时计划详情说「引用的启动方案已不存在，这条计划只会按自带的参数运行」。同一件事两种结论。原因：两处分别手写后果。建议：以后端真实行为为准收敛成一条常量，两处引用同一句。

### 11. 空态句式三套范式混用

- **id**：`ia-89e397`
- **位置**：LogsPage.tsx:22；ApprovalsPage.tsx:78,106；WorkflowsPage.tsx:1601,1707,2117；SchedulesPage.tsx:1126,1133；ExtensionsPage.tsx:777,855；PluginsPage.tsx:154-155；SkillsWorkspacePage.tsx:182；ExtensionDevelopmentPage.tsx:467-484
- **状态**：open

现象：同时存在「暂无 X」「还没有 X」「没有匹配的 X」三种句式，部分只有名词式标题没有下一步动作。原因：空态由各页各写，没有两种范式。建议：固化两个变体——「还没有 X + 如何产生第一条」与「没有匹配的 X + 清除筛选」，在 EmptyState 上做成两个命名导出，禁止第三套写法。

### 12. 分区级「暂无…」占位违反 empty_state 判据

- **id**：`ia-18e343`
- **位置**：InboxPage.tsx:86,110；GatewayProfilesPanel.tsx:282,365,412；AiProvidersPanel.tsx:777；BuiltinAiPage.tsx:592,609；SettingsPage.tsx:1051；SkillsWorkspacePage.tsx:304,591
- **状态**：open

现象：判据要求「无数据时整块不渲染，不留暂无占位」，但这些分区在空时仍渲染一行「暂无操作审批。」「暂无等待中的工作流。」「暂无入口」「暂无模型」「暂无已发布的项目规则」「暂无活动会话」「暂无已安装工作流连接器」等。原因：分区没有遵守 approvalEvidence 已经用过的那条口径（没数据的分组整块不出现）。建议：这些分区在空时 return null，整页无数据时只保留一处带下一步的空态。

### 13. 主从两栏同时渲染同一句空态

- **id**：`ia-75c493`
- **位置**：WorkflowsPage.tsx:1714 与 2117；SchedulesPage.tsx:1133 与 1208
- **状态**：open

现象：宽窗口下左栏说「暂无工作流。」「还没有定时计划」，右侧详情区又用整块空态说「还没有安装工作流」「还没有定时计划」，同一事实同屏两遍。原因：列表空态与详情空态各自判断。建议：左栏空时只留一行浅色提示或直接不渲染，结论与下一步只交给右侧（工作流页注释里已经写明这个口径，但左栏仍留了「暂无工作流。」）。

### 14. 页头说明超出 copy_budget 且绕过自检

- **id**：`ia-44cf76`
- **位置**：aiConnectionsTabs.ts:21；settingsModel.ts:40,48,49；SettingsPage.tsx:768；AiConnectionsPage.tsx:165
- **状态**：open

现象：判据 pageHeader ≤20 汉字，实际「接入本机 AI 工具，管理 AI 服务、模型网关、能力路由与运行环境。」=23、「查看并管理本机需要的账号和凭据；缺失或待验证项目会优先显示。」=28、「设置本机是否接收工作台任务、访问范围与执行工具。」=22、「设置哪些操作需要确认，以及提醒方式与超时时间。」=21。原因：check-copy 只统计 JSX 里的字面量，description={CONST} 完全不参与，自检因此通过而预算已超。建议：让 check-copy 对常量取值也计数（或把这些常量登记进检查白名单并逐个比对），同时把 4 条压到 20 字内。

### 15. 「方案 / 启动方案 / 预设」同一概念三种写法

- **id**：`ia-2ea20f`
- **位置**：WorkflowsPage.tsx:2032,2086,2140,2253,2360；SchedulesPage.tsx:1297,1486
- **状态**：open

现象：界面同时出现「存为方案…」「还没有方案」「启动方案」「方案管理」「引用的启动方案」，代码里叫 preset。原因：改名只改了部分文案位。建议：选定「启动方案」为正式名，按钮等窄位用「方案」作缩写并在常量里只存一份，其余位置不再混写。

## 工艺（可量化）

### 1. 字阶令牌形同虚设：9/10/11px 元信息占全部字号声明 58%

- **id**：`craft-c41263`
- **位置**：frontend/styles.css（font-size 声明全文件）
- **状态**：open

判据 typography：元信息 ≥ --fs-meta 12px。改前：font-size:9px 7 处、10px 78 处、11px 511 处，合计 596 处（约占 1032 条 font-size 声明的 58%），var(--fs-meta) 仅少量使用。代表点：.task-center-row-action 9px（1937）、.task-center-section-kicker 9px（1784）、.task-center-filters button small 9px（1973）、.task-center-filters.sources button 9px（1936）、.task-center-summary-item span 10px（1774）、.workflow-process-node-index 10px（547）、.workflow-process-event-list small 10px（576）。改后：上述全部 → var(--fs-meta) 12px；9px/10px 归零；11px 归并为 12px 或显式登记新令牌 --fs-compact 11px。根因：check-design-tokens.mjs 第 65-67 行只断言令牌值 ≥12px，不断言使用点，596 处实际用法对这道门禁完全不设防。

### 2. 间距阶梯被绕过：gap 2/3/5/6/9px 共 286 处

- **id**：`craft-41c693`
- **位置**：frontend/styles.css（--space-* 使用面）
- **状态**：open

判据：间距只用 4/8/12/16/24/32，文件自己在 4642 行写着「只有这六级，页面级样式不许写阶梯外的值」。改前：gap:2px 93 处、gap:3px 58 处、gap:6px 129 处、gap:5px 4 处、gap:9px 2 处（528、3930），合计 286 处非阶梯值；margin-top/margin-bottom:10px 34 处；padding:6px 8px 20+ 处（87、572、901、917、989、1201、1213、1342、1379、3208、3348、5298、5347）。改后：2px/3px → var(--space-1) 4px（文字行内堆叠若必须保留 2px，另立 --space-half 并登记）；6px → var(--space-2) 8px；9px → var(--space-2) 8px；10px → var(--space-2) 8px 或 var(--space-3) 12px。

### 3. 颜色绕过令牌：169 处 background 十六进制字面量

- **id**：`craft-207e5e`
- **位置**：frontend/styles.css（颜色声明）
- **状态**：open

判据 tokens_only：颜色只能引用 --* 令牌。改前：background 十六进制字面量 169 处，其中 #fffbeb 22 处、#fef2f2 18 处、#fbfcfe 14 处、#f8fafc 9 处；文字色 #92400e 20 处、#b91c1c 22 处。这些都有现成令牌：--warning-soft（#fff7ed）、--danger-soft（#fef2f2）、--surface-muted（#eef0ed）、--warning-strong（#92400e）、--danger（#b42318）。改后：#fffbeb/#fef2f2 → var(--warning-soft)/var(--danger-soft)；#fbfcfe/#f8fafc → var(--surface-muted)；#92400e → var(--warning-strong)；#b91c1c → var(--danger)。一次性装饰色（角色链 --chain-done/live/idle，1854-1856）已合规，保留并保留注释。

### 4. 圆角写死 5px/6px，绕过 --radius 四档

- **id**：`craft-72d270`
- **位置**：frontend/styles.css:148、661、847、3196、3197
- **状态**：open

改前：.workflow-inline-error 与 .workflow-preset-manager-message 用 border-radius: 0 5px 5px 0；.skill-row-reason 用 border-radius: 0 6px 6px 0；.extension-source-acquisition .btn:first-child 用 6px 0 0 6px、:last-child 用 0 6px 6px 0。5px 不在四档（4/6/8/10）内，6px 是字面量而非 var(--radius)。改后：5px → var(--radius-sm) 4px（或统一 var(--radius) 6px，二者取一并全站一致）；6px 字面量 → var(--radius)。另 .workflow-run-list > button.is-live::before 的 border-radius: 2px（323）、.task-center-row.is-live::before 的 0 3px 3px 0（1961）、.ai-usage-bar 的 2px 2px 0 0（5246）同属阶梯外值，改 var(--radius-sm) 4px。

### 5. 同类徽标两套形状：4px 方标与 999px 胶囊同屏混用

- **id**：`craft-211053`
- **位置**：.pill(2367) / .tag(2755) / .skill-state-label(1109) / .task-center-status(1805) / .plan-tag(1205)
- **状态**：open

判据：同类同值。改前：.pill 用 var(--radius-sm) 4px、.tag 4px、.skill-state-label 4px；同时 .task-center-status、.task-center-source、.task-center-parent、.task-center-child-kind、.plan-tag、.notification-channel-state、.market-update-chip 用 999px，全文件 999px 共 31 处。同一行里 .task-center-source（999px）与相邻的其他 4px 标签形状冲突。改后：状态/标签类统一 999px 胶囊（与 .task-center-role、.pill.live 一致），.pill/.tag/.skill-state-label 一并改 999px；或反向全部收成 4px。不允许同屏两套。

### 6. 命中区不足 24×24：9px 文字按钮与 18px 图标按钮

- **id**：`craft-cd45f0`
- **位置**：.task-center-row-action(1937)、.builtin-ai-mcp-probe-retry(1725)、.workflow-topic-tag button(5316)、.topic-group-chip button(5341)
- **状态**：open

判据 hit_target：可点击元素命中区 ≥24×24。改前：.task-center-row-action 为 padding:0;border:0;font-size:9px，行盒高约 9×1.5=13.5px、宽约 27px，高度不足；.builtin-ai-mcp-probe-retry min-height:18px；.workflow-topic-tag button 与 .topic-group-chip button 显式 18×18；.task-center-filters.sources button min-height:25px 且字号 9px。改后：.task-center-row-action 加 min-height:24px; padding:3px 2px（与 5365 行 .text-action 同口径）；.builtin-ai-mcp-probe-retry → min-height:24px；两个 chip 按钮保留 18px 视觉，用 ::after { inset:-3px } 扩到 24×24；.task-center-filters.sources button → min-height:28px、font-size:var(--fs-meta)。

### 7. 焦点环三套并存：实色 outline 与 10% 透明阴影混用

- **id**：`craft-33c1bc`
- **位置**：.task-center-search:focus-within(1976)、.builtin-ai-mcp-map:focus(1710)、select,input:focus(2653)、.workflow-preset-option:focus-visible(731)
- **状态**：open

改前：全局 focus-visible 是 outline: 2px solid var(--primary)（4667-4670），--focus-ring 是 1px 白隔离环 + 4px 实色主色（4657）；但 .task-center-search:focus-within 用 box-shadow: 0 0 0 2px rgba(31,78,121,.1)、.builtin-ai-mcp-map:focus 与 select,input:focus 用 0 0 0 3px rgba(31,78,121,.1)（10% 透明）、.workflow-preset-option:focus-visible 用 outline:0 加 0 0 0 2px var(--primary-soft)。环宽 2px/3px/4px 三种，颜色为 10% 透明。改后：四处统一 var(--focus-ring)（1px 白 + 4px var(--primary)），环宽收成唯一一档 4px。

### 8. 过渡时长七种并存，三档 motion 令牌只覆盖少数声明

- **id**：`craft-f1bd72`
- **位置**：frontend/styles.css（transition/animation 声明）
- **状态**：open

改前：实际出现 100ms（令牌）、150ms（5831、3667、3935、3991、5635）、160ms/.16s（569、1024、1402、1722、2517、2718、3217、3230、3243、4219）、.2s（1625、1951、2551、3642）、.22s（1372）、240ms（486）、520ms（478）共七种；--motion-fast/base/slow（4654-4656）只被 1383、2072 两处引用。改后：150ms → var(--motion-base) 160ms；.16s/160ms → var(--motion-base)；.2s → var(--motion-slow) 220ms；.22s → var(--motion-slow)；240ms → var(--motion-slow)；520ms → var(--motion-slow)。目标：全站只剩 100/160/220 三个值。

### 9. 过渡打在 width/flex-basis 上，非合成层属性

- **id**：`craft-92c187`
- **位置**：.workflow-run-progress i(486)、.sidebar(1372)、.builtin-ai-runtime-progress-track span(1625)、.task-center-progress span(1951)、.runtime-install-progress span(2551)、.agent-update-progress span(3642)
- **状态**：open

改前：进度条一律 transition: width 240ms / width .2s ease；.sidebar 同时过渡 width .22s 与 flex-basis .22s（双动画，布局属性）。改后：进度条改用 transform: scaleX() + transform-origin: left，transition: transform var(--motion-base) ease-out；.sidebar 只保留 transition: width var(--motion-slow) ease-out，去掉 flex-basis 分支。注：全文件未出现 transition: all（0 处，已达标）。

### 10. 数字未对齐：计数器与徽标缺 tabular-nums

- **id**：`craft-076a45`
- **位置**：.task-center-summary-item strong(1775)、.section-count(2065)、.menu-badge(1991)、.quick-access-count(1452)、.approval-tab-count(2382)、.ai-usage-kpi strong(5232)、.task-center-filters button small(1973)
- **状态**：open

改前：全文件只有 16 处 font-variant-numeric: tabular-nums（332、357、363、483、677、1043、1838、1952、1962、2108、2114、3695、4229、5275、5311、5336）。所有随数据变动的计数与徽标均未声明：20px 概览数字、18px 徽标、筛选计数、KPI 大数字（该处只有 letter-spacing:-.01em）。改后：上述 7 个选择器统一加 font-variant-numeric: tabular-nums，并同步 .task-center-summary-item strong 的 line-height 从 1 改为 1。保持不变宽同宽。

### 11. 字重 650 非标准档，跨平台会掉档

- **id**：`craft-16a92d`
- **位置**：.task-center-team b(1843)、.task-center-role-step.is-running/is-waiting/is-danger(1910-1912)、.task-center-progress-label(1952)
- **状态**：open

改前：5 处 font-weight: 650。系统 UI 字族与 Consolas 通常只有 400/500/600/700，650 会被就近映射，导致「已运行 12:03」与相邻 600 文字在 Windows/macOS 上粗细不一致，同一行内出现肉眼可见的跳变。改后：5 处统一 font-weight: 600（与 .card-header 2062、列表标题 4742 同档）。

### 12. 全大写小标签四种字距，同一角色三种取值加一处缺失

- **id**：`craft-10dc66`
- **位置**：.app-menu-section-label(1349)、.extension-source-columns(3154)、.extension-source-notice-reason(3234)、.development-rail-head strong(3256)、.ai-overview-eyebrow(3858)、.sidebar-section-label(1392)
- **状态**：open

改前：同一角色（小号全大写分组标签）使用 letter-spacing: .02em（1349）、.03em（3234）、.04em（3154、3256、3858）三种取值，.sidebar-section-label 完全未设（继承 0）。改后：统一 letter-spacing: .04em；.sidebar-section-label 补上 .04em。数字类 letter-spacing 保持现状（.device-code .12em、h1 -.01em 属不同角色，不必归一）。

### 13. 粗描边模拟层级：8 处 3px 左边线配字面量底色

- **id**：`craft-a67630`
- **位置**：.skill-row-reason(148)、.market-inline-warning(245)、.workflow-market-reason(890)、.mcp-catalog-risk(1693)、.skill-inline-warning(1045)、.full-access-warning(2662) 等
- **状态**：open

改前：border-left: 3px solid var(--warning)/var(--danger) 配 background: #fffbeb / #fef2f2，共 8 处；另有 border-left: 2px（917、1923、3317）。判据：描边只用于 1px 分隔与焦点环，层级由背景分层表达，不用粗描边。改后：border-left: 3px → border: 1px solid var(--warning-border)（danger 同理用 var(--danger-border)）；底色 #fffbeb → var(--warning-soft)、#fef2f2 → var(--danger-soft)；区分度交给背景色与 1px 边框，不再靠线宽。

### 14. 阴影字面量绕过三档：quick-access-bar / expert-studio-modal / 焦点阴影

- **id**：`craft-810dae`
- **位置**：.quick-access-bar(1439)、.expert-studio-modal(1551)、.plugin-debug-grid textarea:focus(2655)、.workspace-status-* keyframes(2789-2794)
- **状态**：open

改前：box-shadow: 0 1px 5px rgba(15,23,42,.04)（1439）、0 20px 50px rgb(15 23 42 / 18%)（1551）、0 0 0 3px rgba(31,78,121,.1)（2655）；--shadow-1/2/3 三档已在 4650-4652 定义但未被引用。改后：0 1px 5px → var(--shadow-1)（0 1px 2px rgba(17,24,39,.06), 0 1px 3px rgba(17,24,39,.08)）；0 20px 50px → var(--shadow-3)；3px 焦点阴影 → var(--focus-ring)；workspace-status 呼吸关键帧内的 rgba 阴影保留但数值对齐 --shadow-2 的分段。

---

## 这份文档的来源与更新方式

审查台账本体在 `%LOCALAPPDATA%\HiMindAgent\ui-audit-ledger.json`，
按「类别+位置+标题」派生稳定 id，重复审查不会新增重复条目，只刷新确认时间。

这份文档是从台账生成的快照。判断某条问题是否已修时：

1. 以台账的 `status` 为准（`open` / `fixed` / `wontfix`）
2. 修复后由守门步骤调用 `engineering.ui-audit.mark-fixed` 标记
3. 本文件重新生成

**注意**：审查台账按「本轮没再报」不自动关闭问题——
审查范围缩小也会漏报，那不叫修好了。
