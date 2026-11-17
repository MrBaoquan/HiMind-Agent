# Agent 桌面 UI 优化交付

承接上级目录审查的 UI-01 至 UI-10，修复布局、滚动归属、状态表达与视觉细节。保留现有导航、13px 正文、32px 控件与克制的 SaaS 风格。

## 工作区与边界

- 独立 worktree：`C:/Users/Administrator/.codex/worktrees/agent-desktop-ui/himind-agent`
- 分支：`codex/agent-desktop-ui`，基线与回滚点：`6e59856`。
- 产品改动仅在该 worktree；原始审查材料保留在上级目录，不改写审查历史。
- 正常桌面、100% 页面缩放。主窗口 1280×820，设置窗口 980×720，DPR 1。
- 跳过 200% 缩放、无障碍专项、移动端、极窄窗口与高 DPI／多显示器矩阵。
- 未执行任务、审批、安装、删除、取消授权或服务配置修改；未推送、合并、部署。

## 十项改动与证据

| 编号 | 改前 | 改后 | 验证 |
| --- | --- | --- | --- |
| UI-01 | 长待办列表把摘要压至 2px | 页面独立布局，摘要不可收缩，最小高 76px | 12 项内存待办实测 76px；[截图](fixture/inbox.png) |
| UI-02 | 从任务页进入待处理时标题滚出视口 | 主导航切换复位滚动；任务详情返回仍保留列表位置与筛选；常驻 AI 会话保持挂载 | 浏览器跨页、详情返回、后台刷新和持久挂载断言；真实任务页滚动 400px 后切换待处理为 0；原生会话宿主未卸载 |
| UI-03 | 同屏“连接异常”与“已连接”含义混用 | 分成“工作台账号／工作台任务”，明确授权与任务连接状态 | 连接异常受控场景；真实开发配置未授权场景 |
| UI-04 | 协作任务行约 150–195px，状态、责任人与动作重复 | 三行信息与固定动作列；工作区末段显示，完整路径可悬停查看 | 12 行实测 81–82px，首屏至少 5 行；[截图](fixture/tasks.png) |
| UI-05 | 空审批、空工作流组仍占双列空间 | 只渲染非空分组；仅一组时占满宽度；全部为空仍用整体空态 | 内存待办单列宽 1036px；真实开发配置整体空态 |
| UI-06 | 包 ID、英文错误直接铺开，缺少恢复路径 | 短名称、中文原因与可展开完整诊断；提供“检查已装插件”入口 | 依赖未响应、内容变化、配置错误三类；展开诊断与插件导航断言；[截图](fixture/workflow-diagnostic.png) |
| UI-07 | AI 服务未读完就显示无服务／不可用 | 加载、成功为空、错误、已有数据刷新失败分别表达；摘要保持 86px | 加载／错误／重试／空态断言；真实设置加载完成显示 4 项；[加载截图](fixture/ai-loading.png) |
| UI-08 | 同级标题 22px/600 与 14px/700 混用 | 新增页面标题语义令牌，统一 20px/600/26px；区块令牌仍为 14px | 九个真实主页面与 AI 连接标题计算样式 |
| UI-09 | 就绪卡片、图标与状态点无限呼吸发光 | 保留静态就绪标记，移除装饰循环与光晕 | 就绪受控场景连续 10 秒计算动画为 none、样式稳定；[截图](fixture/home-ready.png) |
| UI-10 | “全部工具”被截为“全部…” | 该入口最小宽 120px，短标签不收缩，仍高 32px | 一位原生计数与两位受控计数下，文字宽度足够；真实窗口 label clientWidth=scrollWidth=48 |

## 验收结果

- [x] `npm --prefix frontend run check:ui`：全量现有自检通过，包含文案、布局、任务模型与设计令牌检查。
- [x] `npm --prefix frontend run build`：TypeScript 与 Vite 构建通过。
- [x] `cargo build --locked --release --bin himind-agent --bin himind-agent-mcp`：Release 程序与 MCP companion 构建通过。
- [x] `check:desktop-ui-browser`：9 组回归通过，无页面异常；另断言长标题不挤出动作列、跨导航不卸载常驻会话。
- [x] 最终签名 Release 的真实 development 窗口复验及运行日志复查：9 个主页面、1 个设置页面通过，无页面异常；最终实例 stderr 为 0 字节，验收时段事件无警告或错误。
- [x] 两个程序均为有效 Authenticode 签名，源文件与临时运行副本 SHA-256 一致；真实 Windows 窗口句柄可见。

浏览器回归使用真实组件与内存 API 数据；它证明受控边界场景的布局和交互。真实窗口复验连接独立 Agent 进程的 WebView2，没有改用浏览器预览替代原生验收。开发配置目前没有协作任务或待办，也没有工作流读取错误；这些场景的数值来自受控回归，不伪称来自真实业务数据。

原生验收沿用 ADR 0025 的 development 配置、18082 端口、签名 Release、双程序临时副本与哈希验证。生产 CDP 9311 已占用，本次仅临时改用 loopback 9312。规范启动器的旧证书名称与本机现有名称不符，临时适配为已受信任的 `HiMind Software Publisher`；未新建证书或修改信任存储。临时启动器、原始构建和启动日志保存在 `.himind/long-tasks/20261117-desktop-ui-optimization/evidence/`。

最终原生实例 PID `37816`，330 项能力；程序 SHA-256 为 `97A9F29366FF8130D545FB3E02C78CB11BC607D7EF5920B5AFD079D0CACF8B69`。构建、签名、测试及源码散列见 [validation.json](validation.json)。验收实例保留运行，便于直接检查。

## 证据与复跑

- [受控回归结果](fixture/result.json)：每项检查及行高、摘要尺寸。
- [真实窗口结果](native-result.json)：页面尺寸、标题样式、滚动位置与实际配置内容。
- [真实主窗口](01-home.png)、[真实待处理空态](03-inbox.png)、[真实工作区](04-workspaces.png)、[真实 AI 服务设置](10-ai-services.png)。
- [Windows 原生窗口截图](native-window.png)及[进程／窗口句柄记录](native-window.json)：系统窗口标题栏与 WebView2 内容一并捕获，不是浏览器模拟外框。
- [34 项自检日志](checks/ui-checks.log)、[前端构建](checks/build.log)、[Release 构建](checks/release-build.log)、[原生启动校验](checks/native-launch.log)、[运行事件摘要](checks/native-event-summary.json)。
- [浏览器回归脚本](../../../../frontend/scripts/check-desktop-ui.browser.mjs)：`npm --prefix frontend run check:desktop-ui-browser`。
- [真实窗口采样脚本](inspect-native.cjs)：连接本次验收实例的 9312，不执行业务写操作。

浏览器回归复用已有 Playwright 运行时，未新增产品依赖。复跑时设置 `PLAYWRIGHT_MODULE` 指向 Playwright 的 `index.mjs`，并以 `SERVICE_UI_RUNTIME_ROOT` 指定隔离的截图输出目录。本次受控回归输出已复制到 `fixture/`。

## 影响范围与限制

组件：Shell、InboxPage、TaskCenterPage、DashboardPage、AiConnectionsPage、AiProvidersPanel、WorkflowsPage；样式：全局语义令牌与对应页面样式。新增 `workflowIssuePresentation.ts` 仅将诊断转为用户摘要，不改变依赖校验或执行行为。

Vite 仍提示已有的大 chunk；Rust 仍有既有编译警告。验收期间自动检查更新曾遇到 GitHub 403 限流，首页因此显示已有更新失败提示；该外部状态不属于本次 UI 修复。最终实例的 stderr 与验收时段事件检查均通过。开发配置未授权，稳定就绪态使用受控数据验证，未为截图修改账号授权。

本次结果以未提交 diff 保留在独立分支。回滚可按基线逐文件恢复本次 UI 改动；不要重置原始工作区或删除其他任务文件。
