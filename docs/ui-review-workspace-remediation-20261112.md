# 工作区页面整改 · 2026-11-12

已完成 [原审查报告](ui-review-workspace-20261111.md) 中 WUI-01～09 和 API-01 的源码整改。工作区的模态、预览位置、容器布局、字阶、反馈、错误恢复及来源契约统一处理；原审查及 A3/A4 人工验收记录保留。

适用范围为 Windows HiMind Agent 的工作区页面及共享确认框。验证使用真实 React 组件、产品样式、Chrome 和内存合成数据；来源状态与会话摘要由 Rust 的实际响应结构序列化生成。本次未部署、重启真实 Agent 或修改真实登记和会话内容。

## 逐项整改

| 编号 | 实施结果 | 验证 |
| --- | --- | --- |
| WUI-01 | 新增 `ModalSurface`，通过原生 `dialog.showModal()` 提供背景隔离和顶层模态；显式初始焦点、Tab 循环、Esc、提交后焦点回还。保存和目录选择期间禁用关闭 | 正反向各 25 次 Tab、背景聚焦隔离、取消/保存后回还；嵌套确认及旧版模态兼容 |
| WUI-02 | 抽屉进入浏览器 top layer，覆盖应用菜单；共享确认框可叠加在抽屉之上，无需竞争 z-index | 关闭按钮中心和四角内缩 2px 命中；1024px 抽屉截图；原生 WebView 复验边界见下文 |
| WUI-03 | 正文详情替换会话列表，立即滚入可见位置并聚焦标题；返回恢复原预览按钮和滚动位置 | 1/50 条列表首尾，鼠标/Enter、慢正文加载、Esc/返回列表；正文错误重试 |
| WUI-04 | 页面和历史使用容器查询；工具栏先搜索整行，筛选/排序/刷新换行；窄列表降为单列。32px 刷新按钮取消多余的外扩命中伪元素，避免内部滚动 | 1280/1024/768px 外壳、560/400/320px 独立页面；240/280/320/400/500/660px 列表及正文容器；文本间距压力测试 |
| WUI-05 | 工作区样式集中至局部 CSS，引用现有 12/13/14px 字阶和间距、控件、圆角、阴影令牌；修正目录计数与全局 `empty` 类的冲突 | 计算字号断言、项目行高度断言、项目树折叠/展开、截图复查、令牌与布局自检 |
| WUI-06 | 刷新进入即设加载状态，保留旧内容，阻止重复读取并提供进行中/完成反馈；读取期间禁用登记修改，避免并发覆盖 | 2.2 秒延迟，重复点击只发一轮读取；操作禁用、内容保留和结束恢复 |
| WUI-07 | 页面读取、表单提交与正文错误分别就近展示；友好提示附可选择的诊断详情。游标错误刷新重置列表、正文可重试、来源状态有恢复建议 | 登记/游标/正文/保存/目录选择失败、首目录冲突；保留输入和已创建项目，更换目录后不重复创建 |
| WUI-08 | 目录所属项目链接使用现有控件尺寸与内边距 | 实测命中区宽高均 ≥24px |
| WUI-09 | 删除被拒时提示调整子项目上级，不再引用不存在的强制删除入口 | 合成 `PROJECT_HAS_CHILDREN` 错误提示断言；保留既有删除保护 |
| API-01 | Rust 正式序列化键统一为 `opencode`，反序列化兼容 `open_code`；前端兼容旧键，未知来源有可读兜底 | Rust 枚举 roundtrip 与 ID 一致；实际序列化的五来源状态/摘要贯穿筛选、列表及正文角色名；旧键及未知来源回归 |

额外完成原报告中的防误操作建议：待确认目录先选择所属项目，再按“添加目录”执行关联，选择框变化不直接写入。

## 验证记录

最终命令结果和截图归档于 [workspace-remediation-20261112](ui-reviews/workspace-remediation-20261112/)。`result.json` 保存页面检查和宽度度量，`source-wire.json` 保存 Rust 实际序列化夹具，`source-hashes.json` 标识本次验证的源码。

| 检查 | 结果 |
| --- | --- |
| 工作区浏览器回归 | 18 组通过，页面运行错误 0；六档视口无整页溢出，六档历史容器的 `scrollWidth = clientWidth`；列表与正文均检查 |
| 既有会话历史浏览器回归 | 通过；搜索、排序、来源筛选、列表/正文分页、错误状态和关闭预览 |
| `cargo test --bin himind-agent session_history` | 37 项通过；覆盖来源契约、项目归属、分页、五类来源及只读边界 |
| `npm run build` | TypeScript 与 Vite 通过；仍有既有 bundle 大于 500kB 的提示 |
| `check:design-tokens` | 通过；正文/弱化文字及控件边框的既有对比度断言通过 |
| `check:text-layout` | 通过，4621 条规则 / 6 组路径 |
| `check:ui` | 前 32 项通过，随后 `check:copy` 因既有 7 条 ≥40 字文案失败；最后的令牌检查已单独通过 |
| `git diff --check`（本次涉及的已跟踪文件） | 通过 |

7 条超长文案位于 `MissionTeamPanel.tsx`（3 条）、`missionView.ts`（1 条）、`RuntimeWorkspaceDialog.tsx`（3 条），与原审查数量一致；未修改其内容或放宽阈值。明细见 `copy.log`。工作区没有新增该类失败。

## 复验命令

在仓库根目录执行，浏览器脚本使用本机现有 Playwright；没有改变前端依赖锁文件。

```powershell
$env:WORKSPACE_SOURCE_WIRE_FIXTURE = Join-Path (Get-Location) 'target/workspace-source-wire.json'
cargo test --bin himind-agent session_history
$env:PLAYWRIGHT_MODULE = Join-Path (Get-Location) 'target/session-history-playwright/node_modules/playwright/index.mjs'
$env:CHROMIUM_EXECUTABLE = 'C:/Program Files/Google/Chrome/Application/chrome.exe'
npm --prefix frontend run check:workspace-ui-browser
npm --prefix frontend run check:session-history-browser
npm --prefix frontend run build
```

`PLAYWRIGHT_MODULE` 可指向其他既有安装，`CHROMIUM_EXECUTABLE` 可指向其他本机 Chromium；安装了 Playwright 默认浏览器时可省略后者。夹具仅使用合成项目、目录及正文，不连接运行中的 Agent。

## 修改位置与回滚

- `frontend/src/components/ModalSurface.{tsx,css}`：共享原生模态基础；`ConfirmDialog.tsx` 接入，`useModalFocus.ts` 避让顶层原生确认框。
- `frontend/src/pages/WorkspaceRegistryPage.{tsx,css}`：工作区布局、抽屉、刷新、登记操作和项目树；原全局工作区样式迁入局部文件。
- `frontend/src/pages/WorkspaceSessionHistoryPanel.{tsx,css}`：历史容器、正文详情、焦点和位置恢复、来源标签及错误恢复。
- `frontend/src/pages/WorkspaceFeedback.{tsx,css}`：工作区统一错误呈现及恢复文案。
- `frontend/styles.css`：新增模态遮罩语义令牌，移除迁出的工作区样式块；其他已有改动保留。
- `src/session_history.rs`：OpenCode 键兼容与真实结构响应夹具测试。
- `frontend/scripts/check-workspace-ui.browser.mjs` 及其夹具：浏览器回归；已有历史脚本更新错误文案/诊断位置断言；`frontend/package.json` 增加两个独立检查命令。

实施前文件快照位于 `.himind/long-tasks/20261112-workspace-ui-remediation/evidence/before/`。工作区原本有大量未提交修改；回滚应仅反向应用本次差异，并撤除本次新增文件和两个检查命令，不能整体恢复仓库或覆盖随后修改。没有创建提交或推送。

## 验证边界

本次属于源码整改及真实浏览器组件回归，不能等同于已安装 Tauri 客户端的完整 E2E。没有把 CSS zoom、设备像素比或组件宽度测试称为原生 200% 缩放验收。

发布前仍需在真实 WebView 的 1024×680、1280×820 窗口确认抽屉与原生菜单关系，并补 Windows 高 DPI、原生 200% 缩放及屏幕阅读器检查。全局 `.shell` 仍有既有 680px 最小宽度；本次 320px 结论针对独立工作区组件。产品固定浅色，本次没有引入深色主题。

依据沿用原报告的产品 UI 规范、ARIA APG 模态规则、WCAG 2.2 及 Nielsen 状态反馈要求；没有新增 UI 问题编号或改写此前人工验收结果。
