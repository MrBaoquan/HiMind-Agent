# main 整合验证

2026-11-17 按用户要求，将桌面 UI 优化合入本地 `main` 并删除 `codex/agent-desktop-ui`。合并前将 UI 提交从原验收基线 `6e59856` 重放到最新本地 main `84d40eb`，得到产品提交 `baaee7b`，避免带入原基线尚未进入 main 的其他 Mission 提交。main 已获取远程状态，整合前领先 `origin/main` 6 个提交且没有落后。

`AiProvidersPanel.tsx` 和 `ServiceUi.css` 的重放冲突已处理：保留 main 的两步服务接入、模型选择、克隆提示及对应样式，同时保留桌面 UI 的加载／错误／空态区分和摘要高度。产品范围仍为 16 个前端源码及回归文件。

合并前验证：

- [35 项 UI 自检](checks/integration-ui.log)：通过。
- [TypeScript 与 Vite 构建](checks/integration-build.log)：通过，保留已有大 chunk 提示。
- [桌面浏览器回归](checks/integration-desktop.log)：9 组通过，无页面异常；任务行 81–82px，待处理摘要 76px、单组宽 1036px。包含导航复位、详情返回、后台刷新保持滚动及会话挂载检查。完整结果见 [desktop-result.json](integration/desktop-result.json)。
- [AI 服务接入浏览器回归](checks/integration-onboarding.log)：9 组通过，无页面异常；覆盖预设接入、用途选择、保存失败重试、重复提交保护、旧请求隔离、编辑和手工回退。完整结果见 [onboarding-result.json](integration/onboarding-result.json)。
- [差异空白检查](checks/integration-diff.log)：通过。检查日志仅规范化行尾空白，不改写结果。

浏览器回归运行真实 React 组件与内存宿主回调，未调用真实服务推理或修改账号配置。本轮为 main 整合兼容性验证，未重新构建或验收原生 Release；原生验收历史仍见 [原交付记录](README.md) 和 `validation.json`，不能作为 `baaee7b` 的原生构建证明。

合并使用本地快进；没有推送、发布或部署。原长任务的忽略文件与原始证据在 worktree 清理前保留到 main checkout 的 `.himind/long-tasks/20261117-desktop-ui-optimization/`。
