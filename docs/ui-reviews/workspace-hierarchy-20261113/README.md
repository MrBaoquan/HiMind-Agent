# 工作区父子项目与中文显示名优先 · 实施记录

适用平台：HiMind Agent Windows 桌面工作区及 Web 预览。本任务未部署原生安装版；独立原生候选已编译成功。本记录区分源码、合成运行态与原生人工验证。

## 本轮交付

- 界面统一使用“项目别名”，中文优先展示。兼容既有登记：已有中文名称优先，否则使用唯一中文别名，多个中文别名由用户选定展示别名，没有中文配置时回退原名。原名仍可搜索，项目 ID、目录绑定 ID 和 cwd 不变。
- 项目树、子项目列表、标题、面包屑、窄窗选择器、目录归属和会话归属共用名称规则。中文、技术名、别名和目录均可检索。
- 左侧使用单行别名导航，只保留父子层级、选中状态与子项目数量。技术名移至悬浮提示，目录路径与目录数量在详情中查看；移除逐行技术名、类型说明和“未添加目录”等信息。编辑和整理表单均使用“项目别名 / 其他名称”。
- 父项目默认显示具名子项目列表，子项目默认进入会话历史。返回父级不依赖目录名或缩进，逐项目记忆内容视图；本地记忆异常时安全回退。
- 父级的目录、五来源会话列表及正文/相关文件搜索共用“仅此项目 / 包含子项目”。统一去重、排序和分页；正文读取、复制恢复命令和继续会话均使用真实所属项目。
- 提供新建子项目和已有目录的批量整理：整体预览、版本校验、冲突拦截、原子保存、独立恢复快照。恢复时拒绝覆盖后续登记变更。首次目录与新建项目也在同一次事务保存。
- 760px 以下内容宽度切换单栏项目选择；320px 宽采用紧凑操作。极窄或极矮窗口允许项目内容滚动以保留控制和阅读区；常规窗口的树、目录、历史、正文分别滚动，边界不带动页面。来源诊断优先收缩，列表与分页保留阅读空间。

## IODevice 整理建议

| 主中文名称 | 保留的技术身份 | 关系 |
| --- | --- | --- |
| IO 设备平台 | IODevice / MrBaoquan/IOToolkit | 原父项目 |
| 核心驱动与 SDK | IODevice SDK | 直接子项目 |
| 设备配置与编排 | IOStudio | 直接子项目 |
| 设备插件 | IOUI | 直接子项目 |
| 动感影院播控 | CinemaControl | 直接子项目 |
| 交互播放器 | UNIPlayer | 直接子项目 |

[迁移提案](iodevice-migration-preview.json)保留真实登记中的原父 ID、六个入口 ID 与路径，调整其中五个入口的逻辑归属。它是建议文件，不能直接作为执行请求。应用前需要从最新登记获取 revision 并重新预览；本轮未应用真实登记。

UNIPlayer 的 F 盘目标与 O 盘符号链接应属于同一逻辑项目。提案保留现有 F 盘入口，没有自动新增链接或重命名磁盘目录。跨盘身份及路径保持的合成测试已通过；本轮没有实际创建或修改符号链接。

## 验证与验收边界

2026-11-13 反馈调整：用户确认中文名应理解为项目别名，要求收敛左侧的文件夹名等信息。上述单行导航与表单术语已据此修改；用户随后明确“新版中文别名和导航通过验收”，[人工确认记录](evidence/manual-alias-navigation-acceptance.json)覆盖合成页面的中文别名与左侧导航，不包含原生界面。最新层级 12 项、工作区 24 项、构建、令牌和文字布局回归均已重跑通过。Rust 与完整历史报告保留上一轮的通过证据，本次未修改其实现。

证据事实源：`../../../.himind/long-tasks/20261113-workspace-project-hierarchy/evidence/`。源码基线 `e630a087eb86ee8e2b64951eb4428e7978e31c29` 之外原有大量未提交改动；只对本任务增量负责。

[逐项完成审计](completion-audit.md)已核对当前源码、候选程序和证据覆盖范围；完整目标仍缺原生人工验收。

| 验收项 | 已有证据 | 状态 |
| --- | --- | --- |
| A1–A3 中文主身份、明确回退、检索与稳定标识 | [14 项模型断言](evidence/model.log)、[Rust 登记测试](evidence/rust-registry.log)、[中文/技术名操作链](evidence/hierarchy-browser.json) | 自动检查通过 |
| A4 子项目导航、计数、祖先与窄窗上下文 | [层级操作链与宽度测量](evidence/hierarchy-browser.json)，含刷新后收起状态保留 | 自动检查通过 |
| A5 后代范围、统一分页、真实 owner 操作、搜索任务失效 | [50 项历史 Rust 回归](evidence/rust-history.log)、[范围/正文/恢复操作](evidence/hierarchy-browser.json) | 自动检查通过 |
| A6 整理预览、冲突、并发与保护恢复 | [28 项登记 Rust 回归](evidence/rust-registry.log)、[整理和恢复完整操作链](evidence/hierarchy-browser.json) | 自动检查通过 |
| A7 IODevice 提案 | [1 父 5 子、6 个唯一原入口一致性断言](evidence/proposal-check.json)，JSON 提案标注未应用 | 提案已交付；合成中文别名已人工确认，未授权应用真实登记 |
| A8 运行态与人工体验 | [层级 12 项](evidence/hierarchy-browser.json)、[既有工作区 24 项](evidence/workspace-browser.json)、[完整历史浏览器回归](evidence/history-browser.json)、[构建](evidence/build.log)、[令牌](evidence/tokens.log)及[文字布局](evidence/text-layout.log) | 自动检查及合成别名/导航人工验收通过；原生页面待验收 |

最新层级测量使用 680px 高视口：

| 宽度 | 历史列表高度 | 横向溢出 | 页面滚动 |
| --- | --- | --- | --- |
| 1280px | 198px | 无 | 无 |
| 1024px | 198px | 无 | 无 |
| 768px | 159px | 无 | 无 |
| 320px | 148px | 无 | 无 |

层级浏览器报告包含真实测量值。200% 检查用 640×450 CSS px 等效视口验证 1280×900 窗口放大后的重排，历史列表保留 148px、无横向溢出；不能替代原生 WebView 的实际缩放和人工体验验收。完整历史回归中的 console/HTTP 错误均为空。构建通过，仍有现有主 bundle 超过 500kB 的提示。

交付截图：[父级子项目视图](evidence/hierarchy-children-1280.png)、[768px](evidence/hierarchy-768.png)、[320px](evidence/hierarchy-320.png)、[200% 等效视口](evidence/hierarchy-zoom-200.png)。

所有会话和页面操作使用内存合成数据。恢复按钮只记录模拟动作，没有启动真实客户端或读取私人会话正文。本轮没有部署、重启、提交或推送。

## 复验与预览

当前可交互预览：[工作区父子项目](http://127.0.0.1:5174/scripts/fixtures/workspace-hierarchy.html)。前端开发服务器启动后打开 `/scripts/fixtures/workspace-hierarchy.html`。页面完全模拟登记和会话 API，刷新即可恢复原合成数据；额外的 `NewTool` 入口用于练习“整理子项目”，因此合成页面为 7 个目录，与真实 IODevice 提案的 6 个入口口径不同。

合成页面的中文别名与左侧导航已人工确认。[原生候选](evidence/native-candidate.json)已编译成功，尚未由本任务安装或启动。用户已确认“当前安装版没有新版界面”，[部署计划](deployment-plan.md)及旧程序备份已准备；等待部署与重启授权后再进行原生缩放、键盘和滚动验收，详见[原生候选与验收记录](native-acceptance.md)。

在 `frontend` 运行 `npm run check:workspace-project-model`、`npm run check:workspace-hierarchy-browser` 和 `npm run build`。层级浏览器脚本默认使用 `target/session-history-playwright/node_modules/playwright/index.mjs` 与系统 Chrome，也支持 `PLAYWRIGHT_MODULE` / `CHROMIUM_EXECUTABLE` 覆盖。既有工作区与历史浏览器回归沿用相同环境变量，并分别支持独立证据目录。

主要改动：`WorkspaceRegistryPage` / `workspaceProjectModel`、`WorkspaceSubprojectsDialog`、历史面板与检查组件、前端 API、Tauri 命令接线、登记与历史 Rust 模型、相关合成 fixture 和检查脚本。

回滚以任务目录 `before/` 的逐文件副本和 `before-manifest.json` 为依据，只撤销本轮增量，保留其他任务原有改动。登记恢复快照写在用户执行整理时的登记文件旁，当前 UI 恢复入口保留在本次页面会话中；后续修改发生时会拒绝整表覆盖恢复。
