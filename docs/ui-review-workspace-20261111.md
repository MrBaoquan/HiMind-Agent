# 工作区页面审查 · 2026-11-11

2026-11-12：WUI-01～09 及 API-01 已实施源码整改，结果、回归证据与原生验证边界见 [整改记录](ui-review-workspace-remediation-20261112.md)。下文保留审查当时的结论与证据。

当前页面已具备项目树、目录关联和会话历史的基本结构，但仍有 **9 项界面问题：P1 3 项、P2 5 项、P3 1 项**。另外发现 **1 项 P1 接口契约问题**，单独移交。优先处理会话预览的位置、编辑抽屉的焦点及遮挡问题，再统一字阶和状态反馈。

本次交付是审查报告、合成夹具和可复核证据，没有修改生产界面或接口，也没有变更此前 A3/A4 的人工验收记录。

## 范围、平台与证据边界

- 产品：Windows HiMind Agent（Tauri/WebView），真实客户端观察到的版本为 0.3.92。覆盖工作区的项目、目录清单、待确认目录、项目编辑抽屉、会话列表与正文预览。
- 真实界面：观察项目与目录清单，确认来源状态出现 `undefined`。未在真实登记中执行新增、关联、删除或打开会话正文。
- 当前源码：Chrome 渲染真实 React 组件和产品样式，替换数据服务为内存合成数据；持久化方法统一拒绝。包含 3 个项目、长名称/长路径、55 条会话、五种来源、读取失败和 2.2 秒延迟。
- 1280/1024px 使用产品现有 `.shell/.sidebar/.main/.main-content` 样式构造桌面外壳；768/560/400/320px 主矩阵为独立页面。另用同一桌面外壳补测 768px。合成外壳没有原生窗口按钮或实际菜单内容，不能等同于完整客户端 E2E。
- 原生窗口默认 1280×820，最小 1024×680，见 [ui.rs](/F:/WebProjects/项目看板/himind-agent/src/app/ui.rs:908)。768px 以下属于组件适配及本地审查规范要求的压力测试，不表示当前原生窗口可以直接缩到该宽度。
- 200% 使用 CSS `zoom:2` 仿真，**不是浏览器或 WebView 原生缩放验收**。已执行键盘焦点、页签方向键、空态、筛选无结果、错误态、文本间距和系统深色偏好检查；未执行屏幕阅读器、Windows 高 DPI 或原生 200% 缩放的完整验收。

证据：[主矩阵及交互度量](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/result.json)、[桌面外壳补测及源码 SHA-256](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/supplement.json)。证据只含合成数据。源码取自当前有未提交改动的工作区，指纹用于区分后续变化。

## 判据与冲突处理

第一依据是 [产品审查规范](/F:/WebProjects/项目看板/himind-agent/docs/ui-review-profile.json)，其次为 Windows 桌面约定、WCAG 2.2 和 ARIA APG：

| 项目 | 本次采用的判据 |
| --- | --- |
| 字阶 | 元信息 ≥12px、正文 ≥13px、标题 ≥14px；引用 `--fs-meta/body/title` |
| 颜色 | 普通文字 ≥4.5:1；用于辨识控件的边界 ≥3:1 |
| 目标 | 本产品要求命中区 ≥24×24 CSS px；桌面常用操作建议保持现有 32px 控件高度 |
| 容纳性 | 400/560/768/1024px 无非必要横向溢出；长文本不能挤掉动作 |
| 模态 | 打开后焦点进入、Tab 限定内部、Esc 关闭、关闭后回到触发处 |
| 反馈 | 操作后 100ms 内有可感知反馈；超过 1 秒持续显示等待状态 |

两项本地约定需明确适用边界：

1. 产品明确固定浅色，系统深色偏好下仍为 `color-scheme:light`。本次记录为已知能力限制，不新增“必须实现深色主题”的整改项，也不宣称深色主题通过。
2. “无数据整块不渲染”适合辅助模块；工作区首用主页面仍需提供“新建项目”，筛选为空仍需反馈结果。保留此类空态是合理例外，建议在规范中明确，避免删除主要操作入口。

WCAG 不规定本产品的 12/13/14px 字号下限；小字问题依据产品字阶判定。WCAG 2.5.8 有间距等例外，不能仅凭某按钮高度不足 24px 就断言 WCAG 必然失败；本产品的命中区规则更严格。WCAG 2.4.13 属 AAA，本次仅把其焦点外观指标作为建议，不计作 AA 强制项。

## 问题清单

### WUI-01 · P1 · 编辑抽屉声明模态，键盘行为没有实现模态

**方向：无障碍；适用：Windows 桌面/Web 组件。**

- 现象与证据：点击“新建项目”后，焦点仍停在背景按钮；Esc 后抽屉仍存在；在最后一个表单控件按 Tab 后焦点落到 `BODY`；取消后也没有回到触发按钮。`result.json → modal` 已记录四项结果。[抽屉截图](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/editor-1024.png)。
- 根因：只设置了 `role="dialog" aria-modal="true"`，没有初始焦点、焦点约束、背景不可交互、Esc 和焦点归还。
- 修改位置：[WorkspaceRegistryPage.tsx:618](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceRegistryPage.tsx:618)，`.workspace-registry-editor`。建议复用统一的可访问对话框/抽屉基础组件，并覆盖新建、编辑、完成添加目录三种入口。
- 验收：打开后焦点进入标题或首个可编辑字段；连续 Tab/Shift+Tab 20 次均在抽屉内部；背景无法点击和聚焦；Esc 关闭；取消、关闭和保存成功后焦点回到触发元素（触发元素被删除时回到合理后继）。保存期间的关闭行为需一致定义。
- 依据：ARIA APG Dialog (Modal) Pattern；WCAG 2.4.3 焦点顺序、4.1.2 名称/角色/值。

### WUI-02 · P1 · 抽屉标题和关闭按钮被顶部菜单栏覆盖

**方向：布局；适用：带现有菜单栏的桌面外壳。**

- 现象与证据：1024px 外壳中，关闭按钮位于 `(976,12)`，尺寸 32×32；其中心点的命中元素为 `.app-menu-bar`，而非关闭按钮。菜单栏 `z-index:900`，抽屉为 `30`。[遮挡截图](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/editor-error.png)，`supplement.json → drawer`。这是当前样式的合成复现，尚未在真实客户端打开抽屉复验。
- 根因：抽屉 `position:fixed;top:0` 从窗口顶部开始，但没有参与应用外壳的层级和顶部安全区约定。
- 修改位置：[styles.css:6197](/F:/WebProjects/项目看板/himind-agent/frontend/styles.css:6197) 与 [菜单栏样式:1332](/F:/WebProjects/项目看板/himind-agent/frontend/styles.css:1332)。采用共享浮层层级令牌；明确抽屉覆盖菜单栏还是避让 42px 菜单栏，避免仅增大一个魔法数。
- 验收：1024×680、1280×820 下标题完整可见；关闭按钮中心及四角内缩 2px 的命中点均属于该按钮；焦点轮廓不被菜单遮挡。随后在真实 WebView 重验。
- 依据：技能“布局与响应式容纳性”；键盘聚焦时同时核对 WCAG 2.4.11 焦点不被遮挡。

### WUI-03 · P1 · 会话预览出现在长列表末尾，点击后不可见

**方向：交互与状态；适用：Windows 桌面/Web 组件。**

- 现象与证据：55 条合成会话、首屏显示 50 条时，点击第一条“预览”，正文区域顶部为 **3664.58px**，视口高度为 900px；主滚动位置仍为 0，焦点仍在原“预览”按钮。关闭预览后焦点落到 `BODY`。[点击后首屏](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/preview-after-click.png)，`result.json → states.preview/previewAfterClose`。
- 根因：预览在列表和分页之后挂载，打开/关闭时均未管理可见位置和焦点。
- 修改位置：[WorkspaceSessionHistoryPanel.tsx:111](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceSessionHistoryPanel.tsx:111)、[预览渲染:289](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceSessionHistoryPanel.tsx:289)。优先采用列表旁的详情区，窄容器使用单独详情视图；若保留底部预览，打开时滚入视口并聚焦可读标题，关闭后返回原按钮。避免两个未经管理的抽屉叠加。
- 验收：分别在 1/50 条列表中用鼠标和键盘打开第一条、最后一条，300ms 内看到预览标题或加载状态；无需查找页面末尾；关闭后焦点和列表位置可恢复。正文内容和分页本身不作为本问题的失败结论。
- 依据：Nielsen 系统状态可见、用户控制；WCAG 2.4.3 焦点顺序。

### WUI-04 · P2 · 历史组件按视口断点适配，无法适应实际详情列宽度

**方向：布局；适用：窄容器/缩放后的布局；原生 1024px 常规窗口未复现关键按钮裁切。**

- 现象与证据：768px 桌面外壳中，历史工具栏可用宽度 **286px**，内容滚动宽度 **438px**；刷新按钮右缘为 **878px**，超出 768px 视口。详情列形成 318px 可视宽、454px 内容宽的内部横向滚动。[768px 外壳截图](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/desktop-shell-768.png)。独立页面 320/400px 主矩阵未出现整页横向溢出，不能据此推断所有嵌入宽度都合格。
- 根因：工具栏本身不换行，仅在全局 `max-width:420px` 改网格；列表也仅按视口 `720px` 切换布局。详情列窄于视口时，媒体查询不能表达真实空间。
- 修改位置：[WorkspaceSessionHistoryPanel.css:30](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceSessionHistoryPanel.css:30)、[断点:120](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceSessionHistoryPanel.css:120)，以及 [styles.css:6209](/F:/WebProjects/项目看板/himind-agent/frontend/styles.css:6209)。使用容器查询或可换行网格；搜索占一整行，来源/排序/刷新在余下行排列；列表元信息随实际列宽换行。
- 验收：历史容器宽度 240/280/320/400/500/660px 分别测量，工具栏和操作盒右缘不得越过父级内容盒；容器 `scrollWidth ≤ clientWidth + 1px`（排除视觉隐藏文本和焦点轮廓造成的度量噪声）；搜索、排序、来源、刷新均可见且可键盘完成。完整外壳 1024px 必测，768px 作为压力回归。
- 依据：产品 `narrow_window`；技能容器适配规则；WCAG 1.4.10 重排的相关要求。

### WUI-05 · P2 · 字阶和间距绕过已有令牌

**方向：视觉；适用：Windows 桌面/Web 组件；一致性为同一根因，不重复计数。**

- 现象与证据：实测历史标题 13px、会话标题 12px、目录和时间元信息 10px；项目树类型/目录计数 10px，路径 11px。低于产品规定的标题 14px、正文 13px、元信息 12px。源码还出现 7/10/14/18/22px 间距和独立阴影等值。[1024px 页面](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/desktop-shell-1024.png)，`supplement.json → text`。
- 根因：工作区样式位于全局约束之后，以更具体的选择器和裸数值覆盖统一字阶；独立历史 CSS 延续了另一套密度。
- 修改位置：[styles.css:6112](/F:/WebProjects/项目看板/himind-agent/frontend/styles.css:6112)、[项目树:6136](/F:/WebProjects/项目看板/himind-agent/frontend/styles.css:6136)、[路径:6158](/F:/WebProjects/项目看板/himind-agent/frontend/styles.css:6158)、[历史 CSS:28](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceSessionHistoryPanel.css:28)。统一引用 `--fs-* / --space-* / --control-h-* / --shadow-*`；特殊密度若确有理由，应新增有文档的语义令牌。
- 验收：按元素角色检查计算字号，元信息 ≥12px、正文 ≥13px、标题 ≥14px；新增间距均来自 4/8/12/16/24/32 阶梯；修改字阶后重跑长名称、50 条列表和窄容器，不能把阅读问题转成裁切问题。
- 依据：产品 `typography/tokens_only`，不是 WCAG 最小字号要求。

### WUI-06 · P2 · 工作区刷新没有进行中反馈

**方向：交互与状态；适用：Windows 桌面/Web 组件。**

- 现象与证据：模拟刷新等待 2.2 秒，点击后 239ms 按钮仍启用，页面无 `aria-busy`、无“读取工作区”反馈。`result.json → states.refresh`。会话历史自己的刷新按钮有禁用状态，本问题指页面右上角“刷新工作区”。
- 根因：[load():108](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceRegistryPage.tsx:108) 没有在再次读取时设置加载状态，而 [刷新按钮:394](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceRegistryPage.tsx:394) 依赖该状态禁用。
- 修法：区分初次加载和保留内容的刷新状态；进入读取立即显示刷新状态、阻止重复请求；结束显示成功或失败，错误后可重试。
- 验收：50ms/500ms/2200ms 三档延迟下，100ms 内反馈；连续点击 5 次最多启动一轮刷新；等待期间旧内容仍可读，结束后控件恢复，失败状态有重试路径。
- 依据：技能交互状态要求；Nielsen 0.1/1/10 秒原则。采用 `role=status` 时同时核对 WCAG 4.1.3 状态消息。

### WUI-07 · P2 · 错误提示缺少可执行的恢复路径，表单错误未就近展示

**方向：内容与文案；适用：Windows 桌面/Web 组件。**

- 现象与证据：历史错误直接显示 `SESSION_CURSOR_INVALID`；登记读取失败显示 `Error: ...`；来源读取失败只有状态名称。合成保存失败出现在抽屉外的页面顶部，提示区域延伸到抽屉背后，而且没有 `role=alert/status`。[历史错误](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/history-error.png)、[表单错误位置](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/editor-error.png)。合成保存失败的英文内容由审查夹具故意产生，仅用于检验错误展示位置。
- 根因：历史错误直接 `String(reason)`；页面共用一个错误状态，抽屉内没有表单级错误区域；来源的 `error_code` 没有映射到下一步动作。
- 修改位置：[历史错误处理:93](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceSessionHistoryPanel.tsx:93)、[来源文案:46](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceSessionHistoryPanel.tsx:46)、[登记错误:406](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceRegistryPage.tsx:406) 和编辑表单。统一错误呈现规则，区分页面读取、来源不可用和表单提交；诊断码放在可复制的详情内。
- 文案示例：游标失效为“会话列表已变化，请刷新后重试”，配“刷新列表”；保存失败为“项目未保存，请重试”，保留输入；来源不可读时按实际原因给“检查本地客户端/重试读取”等动作，不猜测原因。
- 验收：游标失效、路径冲突、目录选择失败、保存失败、来源不可读各有现状和下一步；错误出现在相关区域内；提交失败被辅助技术宣布且输入不丢失；页面不以内部码作为唯一说明。
- 依据：技能错误文案规则、Nielsen 帮助识别并恢复错误；WCAG 4.1.3；输入错误适用时核对 3.3.1/3.3.3。

### WUI-08 · P2 · 目录所属项目按钮命中区不足产品下限

**方向：无障碍；适用：桌面指针/Web 组件。**

- 现象与证据：目录清单内两个“审查项目”按钮均为 **48×18px**，高度小于 24px，见 `result.json → states.smallTargets`。
- 根因：项目名称用无内边距的文本按钮，未继承通用按钮的最小高度。
- 修改位置：[styles.css:6184](/F:/WebProjects/项目看板/himind-agent/frontend/styles.css:6184)，`.workspace-registry-project-link`。设置最小命中区，保留文本链接视觉；建议 `min-height:var(--control-h-sm)`，并用令牌设置内边距。
- 验收：单字和 40 字项目名下，按钮宽高均 ≥24px；推荐高度 30px；键盘轮廓完整，表格行距和长文本不受损。
- 依据：产品 `hit_target`。WCAG 2.5.8 的间距例外需另测，不在本次直接判为 WCAG 失败。

### WUI-09 · P3 · 删除失败文案引导到页面不存在的动作

**方向：内容与文案；适用：Windows 桌面/Web 组件；证据等级：静态确认。**

- 现象：`PROJECT_HAS_CHILDREN` 被翻译成“先把子项目移走或强制删除”，但工作区没有“强制删除”入口。
- 根因：错误文案描述了当前界面未提供的操作。
- 修改位置：[WorkspaceRegistryPage.tsx:82](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceRegistryPage.tsx:82)，`friendlyRegistryError`。改为“该项目还有子项目，请先调整子项目的上级，再删除此项目。”不因此扩大删除能力。
- 验收：模拟该错误时，提示只引用实际可完成的动作；通过编辑子项目上级能够完成恢复流程。此流程尚未在真实数据执行。
- 依据：技能内容规则；Nielsen 帮助识别并恢复错误。

## 接口问题移交（不计入 9 项 UI 问题）

### API-01 · P1 · OpenCode 来源键不一致

- 真实界面观察到 `undefined · 未检测到`；合成夹具输入实际序列化键后复现 `undefined · 读取失败`。[复现截图](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/wire-source.png)。
- 根因：[SessionSource:9](/F:/WebProjects/项目看板/himind-agent/src/session_history.rs:9) 的 `rename_all="snake_case"` 把 `OpenCode` 序列化为 `open_code`；其 `id()` 返回 `opencode`；[前端类型:1364](/F:/WebProjects/项目看板/himind-agent/frontend/src/services/agentApi.ts:1364) 与 [sourceLabel:36](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceSessionHistoryPanel.tsx:36) 只识别 `opencode`。仅修标签映射会掩盖契约分歧。
- 移交：统一 Rust 序列化、筛选参数和 TypeScript 类型中的正式键；明确兼容策略；前端未知来源显示可读兜底。审查未修改接口。
- 验收：使用 Rust **实际序列化的响应**贯穿前端验证五种来源；OpenCode 在状态、筛选、列表、正文角色名中均正确；未知来源不显示 `undefined`。既有手写 `opencode` 合成数据不能替代此契约验证。

## 七方向汇总与系统性根因

| 主方向 | 问题 | 结论 |
| --- | --- | --- |
| 信息架构与导航 | 无独立问题 | 项目 → 项目目录/会话历史的层级可理解；目录清单是按路径检索的补充视图 |
| 布局与响应式 | WUI-02、04 | 浮层未接入外壳层级；视口断点无法代表详情区的真实宽度 |
| 视觉层次与间距 | WUI-05 | 局部样式覆盖统一令牌，形成第二套字阶和密度 |
| 内容与文案 | WUI-07、09 | 错误说明与用户可执行的恢复动作脱节 |
| 交互与状态 | WUI-03、06 | 数据已经加载不等于用户已经感知结果 |
| 无障碍与包容性 | WUI-01、08 | 模态只有 ARIA 声明；文本按钮缺少统一命中区约束 |
| 一致性与规范 | 不重复计数 | WUI-05 和抽屉基础能力是跨页面约束问题，不能逐个选择器临时补丁 |

待确认目录的选择框 `onChange` 会立即执行关联（[代码:596](/F:/WebProjects/项目看板/himind-agent/frontend/src/pages/WorkspaceRegistryPage.tsx:596)）。其占位文字已说明“选择项目并添加目录”，因此本次不直接判定为 WCAG 3.2.2 失败。作为防误操作建议，可改为“选择项目 → 添加目录”两步；这是 UI 交互建议，未调用真实关联接口。

## 验证结果

| 检查 | 结果与实际边界 |
| --- | --- |
| 原生客户端观察 | 已观察工作区项目/目录页；来源名称异常可见；未完成原生抽屉及缩放验收 |
| 桌面外壳 1280/1024px | 常规页面无整页横向溢出；抽屉遮挡及预览不可见复现 |
| 独立页面 768/560/400/320px | 主矩阵整页宽度均等于视口；长路径可换行，待确认目录在 320px 未超页 |
| 外壳 768px 压力测试 | 详情区内部横向溢出，历史排序和刷新超出可视区（WUI-04）；原生最小窗口为 1024px |
| 目录清单 320px | 表格最小宽度 620px，由局部容器滚动；二维数据表属于 WCAG 1.4.10 可考虑的例外，不直接记作整页重排失败 |
| 空数据/无匹配结果 | 首用空态有新建入口；项目无匹配与会话无匹配均有明确结果说明 |
| 键盘 | 页签方向键切换及选中态实测有效；抽屉与预览焦点失败，见问题清单；未声称完整无障碍通过 |
| 文本间距 | 注入行高 1.5、段距 2em、字距 .12em、词距 .16em；1024px 未新增整页溢出。原有固定宽度工具栏问题仍需按容器回归 |
| 200% | CSS zoom 仿真出现内容越界；受外壳最小宽度及媒体查询行为影响，仅作风险线索，原生缩放结论待补 |
| 浅色实际来源徽标对比 | 灰色文字/浅灰底 5.44:1，绿色 6.62:1，棕色 6.19:1，均 ≥4.5:1；不代表所有控件和状态全量通过 |
| 系统深色偏好 | 页面仍使用浅色，符合当前已知产品限制；无独立深色主题可验 |
| `check:design-tokens` | 通过；是静态全局检查，未阻止工作区局部 10–11px 字号 |
| `check:text-layout` | 通过（4737 条规则/6 组路径）；不能替代实际容器度量和键盘检查 |
| `check:copy` | 未通过：报告当前有 7 条 40+ 字文案。属全前端结果，不能全部归因于工作区；本页顶部说明仅 17 个汉字，符合 20 字预算 |
| 构建/完整业务测试 | 本次只新增审查文档与证据，没有为此重新运行构建及业务测试，不借用此前实现测试宣称本次 UI 全面通过 |

## 整改顺序与复验入口

1. 先处理 WUI-01/02/03，并协调 API-01。统一抽屉基础能力和预览位置后，再调整布局，减少重复改动。
2. 处理 WUI-04/05/08：集中维护工作区和历史组件的容器、字阶、命中区约束；使用现有令牌，避免在多个媒体查询中继续叠加裸值。
3. 处理 WUI-06/07/09：建立初次加载/刷新/表单提交的状态与错误规则，错误保留诊断详情，同时提供用户能完成的下一步。
4. 补充有代表性的自动断言：50 条列表预览可见性、抽屉焦点循环及关闭回还、菜单遮挡命中测试、五来源真实序列化契约、不同容器宽度下的操作可见性。它们验证用户结果，不能仅断言代码中存在某个样式字符串。
5. 在真实客户端 1024×680、1280×820 和原生 200% 缩放下完成回归；若计划支持浏览器/窄屏，再验完整外壳 400/560/768px，而不是只验独立组件。

本地合成复验命令（仓库根目录）：

```powershell
node target/workspace-ui-audit-20261111/audit.mjs
node target/workspace-ui-audit-20261111/supplement.mjs
```

本次已停止自建审查服务和浏览器。临时运行目录位于 ignored `target/`；[夹具快照](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/fixture.tsx)、[主采集脚本快照](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/audit.mjs)、[补充脚本快照](/F:/WebProjects/项目看板/himind-agent/docs/ui-reviews/workspace-20261111/supplement.mjs) 与截图、度量已归档到 `docs/ui-reviews/workspace-20261111/`。快照保留原运行路径；清理 target 后复现需将三个脚本还原到上述运行目录，并具备本机 Chrome、前端依赖及既有 Playwright 运行环境。

参考：[ARIA APG 模态对话框](https://www.w3.org/WAI/ARIA/apg/patterns/dialog-modal/)、[WCAG 2.2](https://www.w3.org/TR/WCAG22/)、[Nielsen 响应时间阈值](https://www.nngroup.com/articles/response-times-3-important-limits/)。
