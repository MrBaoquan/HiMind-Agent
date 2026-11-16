# 插件 AI 服务选择验证记录

验证日期：2026-11-16 至 2026-11-17（Asia/Shanghai）。
分支：`codex/20261116-plugin-ai-service-selection`。
基线/回滚点：`87137c71aff80f343c399d5add79d532ca3362f0`。

## 结果与证据来源

| 验证 | 结果 | 实际覆盖 |
| --- | --- | --- |
| Rust 定向测试 | 88 项去重通过，1 项忽略 | 92 次成功执行，过滤集存在重叠，未累加为独立用例 |
| SDK Node 测试 | 3 项通过 | 偏好优先级、作用域隔离、批任务固定、音色不支持状态、视图/Broker 传输 |
| Chromium 浏览器 | 7 个场景通过 | 真实 SDK/示例 UI；宿主接口使用内存替身 |
| 前端构建 | 通过 | 当前 TypeScript 与 Vite 构建 |
| AI 服务路由检查 | 通过 | `npm run check:ai-service-routing` |
| 差异检查 | 通过 | `git diff --check` |

后端过滤集为 `plugin_ai`、`ai_selection::tests`、
`ai_capability_bus::tests`、`store::ai_services::tests`、
`capability_broker::tests`、`local_image::tests`、`local_speech::tests`、
`vision_describe::`、`local_video::tests` 和
`local_image_generation_invokes_the_local_openai_compatible_service`。
去重按完整测试名称完成；没有测试的过滤集不计数。

关键行为证据：

- 图片/音频适配器实际发出 HTTP 请求至本机假 Provider，检查模型字段、产物和
  内联图片结果中的实际选择。跨服务回退测试检查显式允许前后行为及各自模型。
- 公共 Gateway 检查目录、计划、非法字段；插件 Broker 使用本机 HTTP 到真实
  Gateway，检查声明范围，确认发现能力不授予执行或管理权限。
- 宿主 SDK 资源检查有效视图、启用状态和来源；视觉批任务检查固定模型序列化、
  默认值变化与恢复，旧记录缺失固定模型时拒绝悄悄换用新默认。
- 浏览器覆盖图片/音频固定选择、项目偏好重载、能力隔离、失效选择可见且阻止执行、
  过时异步响应、Agent 断线后刷新恢复，以及没有全局路由写入。
- 服务保存会规范化裸 URL；Fish 音色目录和 Ark/Wan 视频路径已修正，相关回归通过。

## 构建身份与原始记录

环境：Windows，Node `v22.18.0`，Cargo/Rust `1.97.1`。
测试二进制 SHA-256：
`063398560CBA25527BAA3DB304556A56F5960C4C6ECB09E73D4946C4DB416C16`。

命令与原件保存在本 worktree 的忽略目录：
`.himind/long-tasks/20261116-plugin-ai-service-selection/evidence/`。
关键文件：`source-manifest.json`、`final-test-build.log`、
`final-backend-summary.json`、`final-unique-tests.json`、
`plugin-client-final.log`、`final-selector-browser.log`、
`plugin-selector-browser.json`、`plugin-selector-ready.png`、
`plugin-selector-stale.png`、`final-frontend-build.log`、`final-routing-check.log`。
迭代中失败的 Fish/视频路径原件另存为 `pre-fix-*`，未伪装为成功结果。
这些原始记录不随 Git 自动分发；本记录和可重跑测试随代码提交。

Rust 构建复用旧 worktree 的 `target-integration-check` 缓存，但从本分支源码
重新构建最终二进制；所有后端结果来自该最终二进制。
前端复用现有依赖，不改依赖锁文件。前端和浏览器结果产生后仅修正了 Rust
视频路径；其相关源文件未变化，按文件身份复用结果。
构建仍有现有 Rust 和前端体积等警告，没有将这些警告声称为已消除。

## 验收边界与兼容变化

忽略项为 `live_fish_audio_tts_and_asr_round_trip`，需要真实密钥与网络。
本次未验证商业 Provider、已安装第三方插件、安装包或多设备偏好同步。
目录资格来自本机配置；适配器/协议元数据不等于逐模型认证，上游健康显示未测试。

插件接入需要包含本功能的 Agent 构建并声明使用的能力；现有插件不会自动新增选择器。
旧 `service/model` 字段继续支持，但显式选择默认不再跨服务回退，须明确允许。
缺少固定模型记录的旧视觉批任务需要重建，以避免恢复时执行目标漂移。
