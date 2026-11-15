# 当前服务界面阶段交付

设计结论（direction）：延续克制、紧凑的本机设置工具。保守场景为重复管理 AI 服务与工具接入的本机操作者，首先扫描可运行/需行动数量和健康目录可信度；记忆点是同一时间快照的并列数字。对标项目现有设置行，不新增视觉体系。本轮只修检查入口，既有 B06/B07/B10/B11/B12/B13 生产修复通过当前真实浏览器回归确认，不算本轮新增实现。

结构与状态（state_coverage）：保留五页签 id、键盘与深链、父页唯一可见 acp tabpanel；服务页主操作为新增 AI 服务。表单沿连接→能力与模型→高级与用量推进；ModalSurface 负责嵌套隔离、Esc、返回焦点与忙态，不新增 document 级 Tab 接管。加载、初始空、筛选空、错误、部分可用、成功、无权限均实测；健康错误与无权限仍保留对话及仅语音两项服务，重试恢复。重复工具 id 合并为一行，ready 与需行动并列，冲突先显示 diff 且不自动同步。跨步错误链接回字段，非密钥数据保留，预设 radio/pressed/current 行为保持。

取值与依据（token_values）：项目令牌优先：字号 12/13/14px，间距 4/8/12/16/24/32px，圆角 4/6/8/10px；shadow-3 为 0 12px 24px rgba(17,24,39,.10), 0 24px 48px rgba(17,24,39,.12)。沿用局部容器 <480px 摘要上下布局。正文采样阈值 4.5:1、输入边界/焦点 3:1、命中区最低 24px，预设最低 32px，焦点 2px 轮廓与 2px 偏移；减少动效为 opacity 80ms、无变换动画，未新增缓动。规范来源为固定任务契约、remediation-input/backlog.json、frontend/styles.css 和 ServiceUi.css；未加载宿主未固定外部技能，不宣称整站 AA。

因果证据：通过只读 MCP 取得 mission_1794473598404_ffe1e069:559 原事件及 delivery-5 local-run。宿主五项为 0/1/1/1/1，test_summary 全为空，事件只有输出 SHA，没有 stdout。delivery-5 的 exit=1、turn.failed、completed=false 单独不证明源码原因。本任务复现 browser 与 build 均 exit=1：前者在深宿主资源根下生成 Playwright 临时目录时 ENAMETOOLONG，后者 TypeScript 7.0.2 不导出 typescript/bin/tsc，报 ERR_PACKAGE_PATH_NOT_EXPORTED。缩短任务运行目录后仍在 chromiumdev_profile 前缀失败，保留该次记录。改用宿主根内显式短 profile/artifacts 路径后完整浏览器 exit=0；按公开 package.json 的 bin.tsc 定位编译器后类型检查和 Vite build exit=0。原五命令、断言与场景未删除或放宽，未改依赖版本、生产数据模型或宿主。

验证（render_evidence）：当前原浏览器入口加载 AiConnectionsPage 及生产子组件，API 为虚构隔离数据。320/390/480/620/880px、长 URL、50% 文案膨胀、200% CSS zoom 下真实指针/键盘、五页签四方向/Home/End 与深链、唯一 acp、模态/预设/保存忙态、错误定位、字号、合成色对比度和各降级态均有断言及 41 张截图。另目视复核 services-320、expanded-form-320、zoom-200-pointer、full-service-list-denied：窄摘要主动作下排、弹窗滚动内容不遮挡底部操作、200% API Key 错误焦点可见、无权限不清空两项服务。实现后按评审立场核查保留全部原断言和业务路径。启动异常、1ms deadline 故意失败均 exit=1，cleanup 记录浏览器/服务关闭且仅任务 profile 被移除；证据保留。B13 UI-006 先行测量及旧失败仍保留在 repair3/attempt4 和 browser-1794501571165，没有改标成当前结果。

固化与重跑：业务证据、result.json 与索引先固化，再执行原两条结果检查；最终五项原命令只读取候选，日志、SHA-256 文件集合前后快照与实际退出码写入索引引用的宿主资源 final-replay.json。成功、异常、超时路径另有只读快照对比，生成器只在工作阶段显式调用，原检查不调用它。本地范围快照不冒充 host:candidate-integrity；最终实际结论以外部 final-replay.json 为准，不通过最终重跑自动更新本报告。

假设与风险（unverified）：200% 为 CSS zoom，不是浏览器工具栏原生缩放；深色系统偏好保留项目固定 light 主题；未测实际读屏语音、真实后端/凭据、部署。npm ci 的 4 项 high advisory 与构建原有 >500kB chunk warning 未在本范围升级依赖处理。保护文件只读：当前 mission_dispatch.rs 哈希匹配 check-runtime-host-integration 记录；provenance 文档在原始基线无条目且 Git 未跟踪，外部备份未获授权读取，归属/正式基线交宿主裁定，不声称 protected_file_check 通过。宿主原始 stdout 不可由当前事件获取，根因依据本任务真实复现。未调用正式 VERIFY；本阶段只提出送验建议，不声明全 Mission 或部署通过。
