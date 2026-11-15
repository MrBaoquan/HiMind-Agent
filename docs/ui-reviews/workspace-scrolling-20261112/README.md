# 工作区滚动与目录入口验证

日期：2026-11-12。适用范围：Windows 上的 Chrome、真实 React 工作区组件、内存合成项目与会话。实现和后续功能评估见 [工作区体验方案](../../workspace-experience-plan-20261112.md)。

## 本轮结果

| 检查 | 结果 | 证据 |
| --- | --- | --- |
| 工作区浏览器回归 | 24 组通过；含真实滚轮、PageDown、滚动边界、位置恢复、刷新保位、筛选归零、诊断展开、打开目录、剪贴板读回及原模态/错误回归 | `browser.log`、`result.json` |
| 历史预览回归 | 16 项通过；五来源、搜索、排序、列表/正文分页、失败重试、关闭及 390px 横向容纳性 | `history-browser.log`、`session-history-browser-result.json` |
| 前端构建 | TypeScript 与 Vite 通过；仍有既有主 bundle 超过 500kB 的提示 | `build.log` |
| 设计令牌 | 通过 | `tokens.log` |
| 文本布局 | 通过，4621 条规则 / 6 组路径 | `layout.log` |
| 全局文案 | 未通过；其他页面有 6 条既有 ≥40 字文案，工作区未新增 | `copy.log`、`copy-diagnostic.log` |

超长文案位置：`MissionTeamPanel.tsx` 2 条、`missionView.ts` 1 条、`RuntimeWorkspaceDialog.tsx` 3 条；本轮没有修改这些文件或放宽检查阈值。上一轮记录为 7 条，本轮工作树现状为 6 条。

浏览器没有 page/console 错误；历史预览没有 HTTP 错误。打开目录通过 mock 记录传给现有真实 API 入口的路径，不打开用户实际目录；复制通过实际浏览器剪贴板读回。Rust 逻辑本轮未改，使用已有 Rust 测试生成的响应结构夹具贯穿前端，未重跑或重复宣称上轮的 37 项 Rust 测试。

## 关键滚动断言

- 历史列表滚动后，应用 `.main-content.scrollTop` 保持 0；工作区页头和底部分页的位置不变。
- 长项目树滚动不影响历史；项目搜索固定；长目录清单在自己的区域滚动。
- 列表末项可操作；预览在同一内容区呈现。正文独立滚动，返回恢复原列表位置和按钮焦点；正文翻页从顶部开始。
- 刷新时保留列表且禁用旧记录预览，完成后保位；筛选与列表翻页后从顶部开始。
- 1024×680 和 768×680 下展开全部诊断后，列表高度分别约 **137.8px / 106.6px**，底部分页在视口内。修复前固定 180px 的诊断区会把列表挤到 0px。
- 窗口宽度覆盖 1280、1024、768，以及独立组件的 560、400、320px；历史容器分别实测为 240、280、320、400、500、660px，`scrollWidth` 不超过实际宽度。测试显式禁用容器的 flex 增长并断言实际宽度，防止只设置 width 而未真正缩窄的假覆盖。

## 截图

| 文件 | 场景 |
| --- | --- |
| `workspace-1280.png`、`workspace-1024.png` | 项目树与历史分区，分页固定 |
| `workspace-768.png` | 窄容器用项目选择器 |
| `source-diagnostics-1024.png`、`source-diagnostics-768.png` | 680px 高窗口展开诊断后保留列表和分页 |
| `preview-after-click.png` | 列表区域切换正文 |
| `editor-1024.png`、`editor-error.png` | 既有模态与错误操作未回退 |
| `session-history-desktop.png`、`session-history-mobile.png` | 独立历史预览的桌面与窄屏 |

## 复验

仓库根目录运行，下列路径使用本轮已有的工具安装，不新增项目依赖：

```powershell
$env:PLAYWRIGHT_MODULE = Join-Path (Get-Location) 'target/session-history-playwright/node_modules/playwright/index.mjs'
$env:CHROMIUM_EXECUTABLE = 'C:/Program Files/Google/Chrome/Application/chrome.exe'
$env:WORKSPACE_SOURCE_WIRE_FIXTURE = Join-Path (Get-Location) 'target/workspace-source-wire.json'
$env:WORKSPACE_UI_EVIDENCE = Join-Path (Get-Location) 'target/workspace-scroll-regression'
$env:SESSION_HISTORY_EVIDENCE = Join-Path (Get-Location) 'target/workspace-scroll-history-regression'
npm --prefix frontend run check:workspace-ui-browser
npm --prefix frontend run check:session-history-browser
npm --prefix frontend run build
npm --prefix frontend run check:design-tokens
npm --prefix frontend run check:text-layout
npm --prefix frontend run check:copy
```

最后一项在当前共享工作树预期报告上述 6 条。每套浏览器脚本使用自己的 Vite 缓存目录，避免与另一套夹具或开发服务器共享预构建缓存；历史独立夹具已改成有明确高度的 flex 容器，与生产组件的布局约定一致。工作区脚本每次开始清理旧结果文件，只有整套通过后才写入新的 `result.json`。

`source-hashes.json` 记录本轮最终源码哈希；`source-wire.json` 保存实际使用的响应夹具。未归档中途失败截图为最终结果。

## 验证边界

本轮完成源码和浏览器自动验证，没有部署或重启实际 Agent。全局应用 shell 仍有既有 680px 最小宽度，320px 的结论只针对独立组件。当前产品为浅色；未将组件宽度或 CSS 缩放冒充原生 WebView、Windows 高 DPI、200% 缩放、深色主题或屏幕阅读器验收。真实原生「打开目录」以及后续「继续会话」仍需要实际客户端联调。

之前的人工 A3/A4 验收属于历史里程碑，本轮没有借用它声称用户已验收新的滚动体验。
